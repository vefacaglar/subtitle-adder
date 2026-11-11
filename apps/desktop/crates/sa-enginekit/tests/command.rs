//! Locating the engine (C# EngineKitTests locate cases, Swift EngineKitTests locate cases) and media file types.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sa_enginekit::command::ENGINE_FILE_NAME;
use sa_enginekit::media_types::{AUDIO, VIDEO, is_audio, is_media, media_extensions};
use sa_enginekit::EngineCommand;

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
}

fn touch(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[cfg(windows)]
fn app_exe(root: &Path) -> PathBuf {
    root.join("app").join("SubtitleAdder.exe")
}

#[cfg(windows)]
fn bundled_path(root: &Path) -> PathBuf {
    root.join("app").join("subtitle-engine").join(ENGINE_FILE_NAME)
}

#[cfg(not(windows))]
fn app_exe(root: &Path) -> PathBuf {
    root.join("Subtitle Adder.app").join("Contents").join("MacOS").join("SubtitleAdderApp")
}

#[cfg(not(windows))]
fn bundled_path(root: &Path) -> PathBuf {
    root.join("Subtitle Adder.app").join("Contents").join("Resources").join("subtitle-engine").join(ENGINE_FILE_NAME)
}

#[test]
fn the_environment_variable_wins_without_an_existence_check() {
    let path = if cfg!(windows) { r"D:\engine\se.exe" } else { "/opt/engine/subtitle-engine" };
    let located = EngineCommand::locate_with(&env(&[("SUBTITLE_ENGINE", path)]), None, None).unwrap();
    assert_eq!(located, EngineCommand::new(path));
    assert!(located.arguments.is_empty());
    // Trimmed (UNIFIED.md §3.7 FIX-W).
    assert_eq!(EngineCommand::locate_with(&env(&[("SUBTITLE_ENGINE", &format!("  {path}\n"))]), None, None), Some(EngineCommand::new(path)));
}

#[test]
fn an_empty_or_blank_engine_path_counts_as_unset() {
    let temp = tempfile::tempdir().unwrap();
    let exe = app_exe(temp.path());
    let without = EngineCommand::locate_with(&env(&[]), Some(&exe), None);
    for blank in ["", "   ", "\t\n"] {
        assert_eq!(EngineCommand::locate_with(&env(&[("SUBTITLE_ENGINE", blank)]), Some(&exe), None), without, "{blank:?}");
    }
}

#[cfg(windows)]
#[test]
fn the_variable_name_is_case_insensitive_on_windows() {
    assert_eq!(EngineCommand::locate_with(&env(&[("subtitle_engine", r"C:\e.exe")]), None, None), Some(EngineCommand::new(r"C:\e.exe")));
}

#[test]
fn finds_the_bundled_engine_next_to_the_app() {
    let temp = tempfile::tempdir().unwrap();
    let exe = app_exe(temp.path());
    assert_eq!(EngineCommand::bundled_engine(&exe), None);
    assert_eq!(EngineCommand::locate_with(&env(&[]), Some(&exe), None), None);
    touch(&bundled_path(temp.path()));
    assert_eq!(EngineCommand::bundled_engine(&exe), Some(bundled_path(temp.path())));
    assert_eq!(EngineCommand::locate_with(&env(&[]), Some(&exe), None), Some(EngineCommand::new(bundled_path(temp.path()))));
}

#[test]
fn the_dev_engine_uses_the_staged_dist_of_the_nearest_cargo_repo() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let exe = root.join("apps").join("x").join("bin").join(if cfg!(windows) { "SubtitleAdder.exe" } else { "SubtitleAdder" });
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    // A pyproject-only engine folder does not count.
    touch(&root.join("engine").join("pyproject.toml"));
    assert_eq!(EngineCommand::find_repo_engine(exe.parent().unwrap()), None);
    // Cargo.toml without a staged dist: the repo is found, but there is no engine.
    touch(&root.join("engine").join("Cargo.toml"));
    assert_eq!(EngineCommand::find_repo_engine(exe.parent().unwrap()), Some(root.join("engine")));
    assert_eq!(EngineCommand::locate_with(&env(&[]), Some(&exe), None), None);
    // With the staged dist: located.
    let dist = root.join("engine").join("dist").join(ENGINE_FILE_NAME);
    touch(&dist);
    assert_eq!(EngineCommand::locate_with(&env(&[]), Some(&exe), None), Some(EngineCommand::new(&dist)));
    // A nearer engine/Cargo.toml wins even without its own dist (no fallback to the outer repo).
    touch(&root.join("apps").join("engine").join("Cargo.toml"));
    assert_eq!(EngineCommand::find_repo_engine(exe.parent().unwrap()), Some(root.join("apps").join("engine")));
    assert_eq!(EngineCommand::locate_with(&env(&[]), Some(&exe), None), None);
}

#[test]
fn the_bundled_engine_wins_over_the_dev_dist() {
    let temp = tempfile::tempdir().unwrap();
    let exe = app_exe(temp.path());
    touch(&temp.path().join("engine").join("Cargo.toml"));
    touch(&temp.path().join("engine").join("dist").join(ENGINE_FILE_NAME));
    touch(&bundled_path(temp.path()));
    assert_eq!(EngineCommand::locate_with(&env(&[]), Some(&exe), None), Some(EngineCommand::new(bundled_path(temp.path()))));
}

#[test]
fn no_engine_without_a_repo() {
    let temp = tempfile::tempdir().unwrap();
    assert_eq!(EngineCommand::locate_with(&env(&[]), Some(&temp.path().join("a.exe")), None), None);
    assert_eq!(EngineCommand::locate_with(&env(&[]), None, None), None);
}

#[test]
fn the_compile_time_repo_is_the_fallback_when_the_exe_lives_outside_any_repo() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    touch(&repo.join("engine").join("Cargo.toml"));
    let exe = temp.path().join("target").join("SubtitleAdder.exe");
    assert_eq!(EngineCommand::locate_with(&env(&[]), Some(&exe), Some(&repo)), None);
    let dist = repo.join("engine").join("dist").join(ENGINE_FILE_NAME);
    touch(&dist);
    assert_eq!(EngineCommand::locate_with(&env(&[]), Some(&exe), Some(&repo)), Some(EngineCommand::new(&dist)));
}

#[test]
fn locates_the_real_dev_engine() {
    // The compile-time repo root is this repository; its staged engine is used when present.
    let root = EngineCommand::repo_root();
    assert!(root.join("engine").join("Cargo.toml").is_file(), "{}", root.display());
    let dist = root.join("engine").join("dist").join(ENGINE_FILE_NAME);
    let mut vars: HashMap<String, String> = std::env::vars().collect();
    vars.retain(|k, _| !k.eq_ignore_ascii_case("SUBTITLE_ENGINE"));
    let located = EngineCommand::locate(&vars, std::env::current_exe().ok().as_deref());
    if dist.is_file() {
        assert_eq!(located.map(|c| c.executable.file_name().map(|n| n.to_owned())), Some(Some(ENGINE_FILE_NAME.into())));
    } else {
        assert_eq!(located, None);
    }
}

#[test]
fn media_file_types() {
    for ext in VIDEO.iter().chain(AUDIO.iter()) {
        assert!(is_media(Path::new(&format!("a.{ext}"))), "{ext}");
        assert!(is_media(Path::new(&format!("a.{}", ext.to_uppercase()))), "{ext}");
    }
    for name in ["a.txt", "a.srt", "a.json", "a.jpg", "a", "mp4", ".mp4", "x.mp4.bak"] {
        assert!(!is_media(Path::new(name)), "{name}");
    }
    assert!(AUDIO.iter().all(|a| is_audio(Path::new(&format!("a.{a}")))));
    assert!(!is_audio(Path::new("a.mp4")));
    for ext in ["vob", "ogv", "mxf", "rmvb", "m2ts"] {
        assert!(is_media(Path::new(&format!("a.{ext}"))) && !is_audio(Path::new(&format!("a.{ext}"))), "{ext}");
    }
    for ext in ["mka", "ac3", "aiff", "ape", "dsf"] {
        assert!(is_audio(Path::new(&format!("a.{}", ext.to_uppercase()))), "{ext}");
    }
    assert_eq!(media_extensions().count(), VIDEO.len() + AUDIO.len());
    assert_eq!(media_extensions().collect::<Vec<_>>(), sa_core::media_types::MEDIA_EXTENSIONS.to_vec());
    assert!(media_extensions().count() > 22, "wider than the C#-era 22");
    let mut all: Vec<_> = media_extensions().collect();
    all.sort_unstable();
    all.dedup();
    assert_eq!(all.len(), VIDEO.len() + AUDIO.len());
    assert_eq!(media_extensions().next(), Some("mp4"));
}
