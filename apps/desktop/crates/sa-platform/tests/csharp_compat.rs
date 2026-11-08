//! Wire compatibility of the single-instance pipe with the C# app (Windows): a .NET `CurrentUserOnly` client
//! forwards files to the Rust pipe server, and the Rust client forwards to a .NET server, through the fixture tool
//! `apps/desktop/tools/fixtures/PlatformCompat` (the C# app's client/server code on a test pipe name, so a running
//! app is never contacted). Also checks the user-name sanitization against .NET `char.IsLetterOrDigit`.
//!
//! Opt-in: `SA_DOTNET_FIXTURES=1 cargo test -p sa-platform --test csharp_compat` (needs the .NET 10 SDK; the tool is
//! built on first use with `dotnet build -c Release`). Without the variable every test returns early.
#![cfg(windows)]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use sa_core::platform::{ExternalOpenSource, InstanceRole};
use sa_platform::win::single_instance::{InstanceNames, SingleInstanceCoordinator, sanitize_token};

fn enabled() -> bool {
    let on = std::env::var("SA_DOTNET_FIXTURES").is_ok_and(|v| v == "1");
    if !on {
        eprintln!("skipped: set SA_DOTNET_FIXTURES=1 to run the C# compatibility tests");
    }
    on
}

fn tool_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/fixtures/PlatformCompat")
}

fn tool_exe() -> PathBuf {
    let exe = tool_dir().join("bin/Release/net10.0/PlatformCompat.exe");
    let built = std::fs::metadata(&exe).and_then(|m| m.modified());
    let stale = ["Program.cs", "PlatformCompat.csproj"].iter().any(|source| {
        match (&built, std::fs::metadata(tool_dir().join(source)).and_then(|m| m.modified())) {
            (Ok(built), Ok(source)) => *built < source,
            _ => true,
        }
    });
    if stale {
        let status = Command::new("dotnet")
            .args(["build", "-c", "Release", "-nologo", "-v", "q"])
            .current_dir(tool_dir())
            .status()
            .expect("dotnet is on PATH");
        assert!(status.success(), "building the PlatformCompat fixture tool failed");
    }
    exe
}

fn test_names(tag: &str) -> InstanceNames {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    InstanceNames::for_user(&format!("sa compat {tag} {}-{n}", std::process::id()))
}

#[test]
fn a_dotnet_client_forwards_files_to_the_rust_server() {
    if !enabled() {
        return;
    }
    let exe = tool_exe();
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("film ü.mp4");
    let b = dir.path().join("film.tr.srt");
    std::fs::write(&a, b"x").unwrap();
    std::fs::write(&b, b"x").unwrap();
    let names = test_names("server");
    let mut owner = SingleInstanceCoordinator::with_names(names.clone());
    assert_eq!(owner.acquire_or_forward(&[]), InstanceRole::Primary);
    let (tx, rx) = std::sync::mpsc::channel();
    owner.set_sink(Box::new(move |p| {
        let _ = tx.send(p);
    }));
    let quoted = format!("\"{}\"", b.display());
    let out = Command::new(&exe)
        .args(["client", &names.pipe, a.to_str().unwrap(), &quoted, r"C:\missing\x.mp4"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ok", "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), vec![a, b]);
    owner.dispose();
    // Nobody listens any more: the .NET client fails within its timeout.
    let out = Command::new(&exe).args(["client", &names.pipe, "x"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "fail");
}

#[test]
fn the_rust_client_forwards_files_to_a_dotnet_server() {
    if !enabled() {
        return;
    }
    let exe = tool_exe();
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("Ğ film.mkv");
    std::fs::write(&a, b"x").unwrap();
    let names = test_names("client");
    let mut server = Command::new(&exe).args(["server", &names.pipe, "2"]).stdout(Stdio::piped()).spawn().unwrap();
    let mut lines = BufReader::new(server.stdout.take().unwrap()).lines();
    assert_eq!(lines.next().unwrap().unwrap(), "ready");
    assert!(SingleInstanceCoordinator::forward(&names, &[a.clone(), dir.path().join("missing.srt")]));
    assert_eq!(lines.next().unwrap().unwrap(), "batch|1");
    assert_eq!(lines.next().unwrap().unwrap(), format!("path|{}", a.display()));
    assert!(SingleInstanceCoordinator::forward(&names, &[]));
    assert_eq!(lines.next().unwrap().unwrap(), "batch|0");
    assert!(server.wait().unwrap().success());
}

#[test]
fn user_name_sanitization_matches_dotnet() {
    if !enabled() {
        return;
    }
    for user in ["User", "Vefa Çağlar", "first.last@corp", "名前", "a-b_c", "   ", "x²y", "हिन्दी", "Ⅻ٣", "a😀b"] {
        let out = Command::new(tool_exe()).args(["names", user]).output().unwrap();
        let text = String::from_utf8(out.stdout).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let names = InstanceNames::for_user(user);
        assert_eq!(lines[0], sanitize_token(user), "token of {user:?}");
        assert_eq!(lines[1], names.mutex);
        assert_eq!(lines[2], names.pipe);
    }
}

// ---- file associations (win-ui-main §3): the same registry tree, written and removed by either side ------------

/// `HKCU\<scratch>` for one test, removed (with everything below it) on drop.
struct Scratch(String);

impl Scratch {
    fn new(tag: &str) -> Self {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        Scratch(format!(r"Software\SubtitleAdderDesktopTests-compat-{tag}-{}-{n}", std::process::id()))
    }

    /// Another app already owns `.mkv` (the registrar must never steal an existing default).
    fn seed_foreign_mkv(&self) {
        let status = Command::new("reg")
            .args(["add", &format!(r"HKCU\{}\Software\Classes\.mkv", self.0), "/ve", "/d", "VLC.mkv", "/f"])
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = Command::new("reg").args(["delete", &format!(r"HKCU\{}", self.0), "/f"]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
}

fn tool(args: &[&str]) -> String {
    let out = Command::new(tool_exe()).args(args).output().unwrap();
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

fn dump(s: &Scratch) -> String {
    tool(&["dump", &s.0])
}

#[test]
fn file_associations_write_and_remove_the_same_registry_tree_as_csharp() {
    use sa_core::platform::FileAssociations;
    use sa_platform::win::associations::FileAssociationRegistrar;
    if !enabled() {
        return;
    }
    let exe = r"C:\Program Files\Subtitle Adder\SubtitleAdder.exe";
    for icon in [None, Some(r"C:\Program Files\Subtitle Adder\Assets\AppIcon.ico")] {
        let cs = Scratch::new("cs");
        let rs = Scratch::new("rs");
        cs.seed_foreign_mkv();
        rs.seed_foreign_mkv();
        let rust = |s: &Scratch| FileAssociationRegistrar::with_root(&s.0, PathBuf::from(exe), icon.map(str::to_owned));
        let mut register_args = vec!["assoc-register", cs.0.as_str(), exe];
        register_args.extend(icon);

        assert_eq!(tool(&register_args).trim(), "ok");
        assert!(rust(&rs).register());
        let written = dump(&cs);
        assert!(written.contains(r"Software\Classes\SubtitleAdder.Media\shell\open\command||String|"), "{written}");
        assert!(written.contains(r"Software\Classes\.mkv||String|VLC.mkv"), "{written}");
        assert_eq!(dump(&rs), written, "Rust and C# registrations differ (icon {icon:?})");

        // Each side recognizes the other's registration, and registering again changes nothing.
        assert!(rust(&cs).is_registered());
        assert_eq!(tool(&["assoc-registered", &rs.0]).trim(), "true");
        assert_eq!(tool(&register_args).trim(), "ok");
        assert!(rust(&rs).register());
        assert_eq!(dump(&cs), written);
        assert_eq!(dump(&rs), written);

        // Cross removal: Rust removes what C# wrote, C# removes what Rust wrote; both leave the same remains.
        assert!(rust(&cs).unregister());
        assert_eq!(tool(&["assoc-unregister", &rs.0]).trim(), "ok");
        let removed = dump(&cs);
        assert_eq!(dump(&rs), removed);
        assert!(!removed.contains("SubtitleAdder.Media"), "{removed}");
        assert!(removed.contains(r"Software\Classes\.mkv||String|VLC.mkv"), "{removed}");
        assert!(!rust(&cs).is_registered());
        assert_eq!(tool(&["assoc-registered", &rs.0]).trim(), "false");
        // Removing twice is fine on both sides.
        assert!(rust(&rs).unregister());
        assert_eq!(tool(&["assoc-unregister", &cs.0]).trim(), "ok");
        assert_eq!(dump(&rs), dump(&cs));
    }
}

// ---- fonts (win-ui-main §20.2) -------------------------------------------------------------------------------------

#[test]
fn installed_font_families_match_the_csharp_list_and_order() {
    if !enabled() {
        return;
    }
    let cs: Vec<String> = tool(&["fonts"]).lines().map(str::to_owned).collect();
    let rs = sa_platform::win::fonts::families();
    assert!(cs.len() > 10, "{cs:?}");
    let first_difference = cs.iter().zip(rs.iter()).position(|(a, b)| a != b);
    assert_eq!(first_difference, None, "first difference at {first_difference:?}: C# {:?} / Rust {:?}",
        first_difference.map(|i| &cs[i.saturating_sub(2)..(i + 3).min(cs.len())]),
        first_difference.map(|i| &rs[i.saturating_sub(2)..(i + 3).min(rs.len())]));
    assert_eq!(cs.len(), rs.len());
}

// ---- path rules: roots, extensions, explorer folders, storage readiness ----------------------------------------------

#[test]
fn path_rules_match_dotnet() {
    use sa_platform::dialogs::{DialogFlavor, has_extension, suggested_save_name};
    use sa_platform::win::folders::{normalize_directory_path, path_root, storage_available};
    if !enabled() {
        return;
    }
    // A drive letter that does not exist here (DriveInfo.IsReady false).
    let missing = (b'D'..=b'Z').map(|c| format!("{}:\\", c as char)).find(|r| !Path::new(r).exists()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let inputs: Vec<String> = [
        r"C:\Users\x\film.tr.srt",
        r"C:\Users\x\film",
        r"C:\Users\x\film.",
        r"C:\Users\x\.srt",
        "film.tr.srt",
        "film",
        r"C:\Windows\",
        r"C:\Windows\System32//",
        "  C:/Windows/System32/  ",
        r"C:\",
        "C:",
        "C:relative.mp4",
        r"\Films\a.mp4",
        r"relative\a.mp4",
        r"\?\C:\Windows",
        "D:/x//y/",
        "   ",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .chain([format!("{missing}film.mp4"), dir.path().join("a.mkv").display().to_string()])
    .collect();
    let mut args = vec!["paths"];
    args.extend(inputs.iter().map(String::as_str));
    let out = tool(&args);
    let rows: Vec<serde_json::Value> = out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(rows.len(), inputs.len());
    for (input, row) in inputs.iter().zip(&rows) {
        let expect_root = row["root"].as_str().unwrap_or("");
        assert_eq!(path_root(input), expect_root, "GetPathRoot({input:?})");
        assert_eq!(has_extension(Path::new(input)), row["hasExtension"].as_bool().unwrap(), "HasExtension({input:?})");
        assert_eq!(
            suggested_save_name(input, DialogFlavor::Windows),
            row["withoutExtension"].as_str().unwrap_or(""),
            "GetFileNameWithoutExtension({input:?})"
        );
        assert_eq!(normalize_directory_path(input), row["normalizedDirectory"].as_str().unwrap(), "NormalizeDirectoryPath({input:?})");
        assert_eq!(storage_available(Path::new(input)), row["storageAvailable"].as_bool().unwrap(), "storage of {input:?}");
    }
}
