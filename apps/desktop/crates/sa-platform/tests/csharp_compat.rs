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
