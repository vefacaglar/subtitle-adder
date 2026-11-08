//! The engine's scratch folder `<temp>/SubtitleAdder` (mac-enginekit §10; Windows cleanup rules in
//! win-viewmodels §17 / win-sync §3.4 are applied by the view model, which knows about broker files and other
//! instances).

use std::path::{Path, PathBuf};

/// `<temp>/SubtitleAdder` (`--out-dir` of every run).
pub fn work_dir(temp: &Path) -> PathBuf {
    temp.join("SubtitleAdder")
}

/// `create_dir_all` (errors ignored).
pub fn ensure_exists(temp: &Path) -> PathBuf {
    let dir = work_dir(temp);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// macOS rule: removes the whole folder and every `subtitle-engine-*.log` in `temp`. Only while no job runs.
pub fn clean_up(temp: &Path) {
    let _ = std::fs::remove_dir_all(work_dir(temp));
    let Ok(entries) = std::fs::read_dir(temp) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with("subtitle-engine-") && name.ends_with(".log") && entry.file_type().is_ok_and(|t| t.is_file()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// `<temp>/subtitle-engine-<UUID>.log`: upper-case hyphenated as Swift's `uuidString` (macOS), lower-case hyphenated
/// as .NET's `Guid.ToString()` (Windows).
pub fn new_log_path(temp: &Path) -> PathBuf {
    let id = uuid::Uuid::new_v4().hyphenated().to_string();
    let id = if cfg!(windows) { id } else { id.to_uppercase() };
    temp.join(format!("subtitle-engine-{id}.log"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folder_lives_in_temp_and_is_created() {
        let temp = tempfile::tempdir().unwrap();
        let dir = ensure_exists(temp.path());
        assert_eq!(dir, temp.path().join("SubtitleAdder"));
        assert!(dir.is_dir());
    }

    #[test]
    fn clean_up_removes_the_folder_and_the_engine_logs_only() {
        let temp = tempfile::tempdir().unwrap();
        let dir = ensure_exists(temp.path());
        std::fs::write(dir.join("a.wav"), b"x").unwrap();
        std::fs::create_dir_all(dir.join("translations")).unwrap();
        std::fs::write(dir.join("translations").join("chunk-1.json"), b"{}").unwrap();
        let log = new_log_path(temp.path());
        std::fs::write(&log, b"log").unwrap();
        std::fs::write(temp.path().join("keep.txt"), b"k").unwrap();
        std::fs::write(temp.path().join("other.log"), b"o").unwrap();
        clean_up(temp.path());
        assert!(!dir.exists());
        assert!(!log.exists());
        assert!(temp.path().join("keep.txt").exists());
        assert!(temp.path().join("other.log").exists());
        clean_up(temp.path());
    }

    #[test]
    fn log_names_follow_the_platform_uuid_spelling() {
        let name = new_log_path(Path::new("t")).file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("subtitle-engine-") && name.ends_with(".log"));
        let id = &name["subtitle-engine-".len()..name.len() - 4];
        assert!(uuid::Uuid::parse_str(id).is_ok());
        if cfg!(windows) { assert_eq!(id, id.to_lowercase()) } else { assert_eq!(id, id.to_uppercase()) }
    }
}
