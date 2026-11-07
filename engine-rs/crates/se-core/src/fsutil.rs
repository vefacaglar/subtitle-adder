//! File-system helpers shared by the crates: Python `os.replace`, atomic writes, quiet unlink,
//! `tempfile.gettempdir()` parity, the app temp folder and the (overridable) process id.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

/// Name of the engine's temp subfolder (`<temp>/SubtitleAdder`), shared with the apps.
pub const APP_TEMP_DIR_NAME: &str = "SubtitleAdder";

static PID_OVERRIDE: AtomicU32 = AtomicU32::new(0);

/// The process id used in temp/fallback file names (`os.getpid()`); tests can override it.
pub fn pid() -> u32 {
    match PID_OVERRIDE.load(Ordering::SeqCst) {
        0 => std::process::id(),
        p => p,
    }
}

/// Test hook: `Some(pid)` pins [`pid`], `None` restores the real process id.
pub fn set_pid_override(pid: Option<u32>) {
    PID_OVERRIDE.store(pid.unwrap_or(0), Ordering::SeqCst);
}

/// Python `os.replace(src, dst)`: rename, replacing an existing file.
pub fn replace(src: &Path, dst: &Path) -> io::Result<()> {
    fs::rename(src, dst)
}

/// Remove a file, ignoring every error (`unlink(missing_ok=True)` inside `suppress(OSError)`).
pub fn unlink_quiet(path: &Path) {
    let _ = fs::remove_file(path);
}

/// Write `bytes` to `<path>.<pid>.tmp` and rename it over `path`; the temp file is removed on failure.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = PathBuf::from(format!("{}.{}.tmp", crate::paths::py_path_str(path), pid()));
    let result = (|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.flush()?;
        drop(f);
        replace(&tmp, path)
    })();
    if result.is_err() {
        unlink_quiet(&tmp);
    }
    result
}

static TEMPDIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Python `tempfile.tempdir = value` (tests): `Some` pins [`gettempdir`], `None` makes the next call search again.
pub fn set_tempdir(value: Option<PathBuf>) {
    *TEMPDIR.lock().unwrap_or_else(|e| e.into_inner()) = value;
}

fn candidate_tempdirs() -> Vec<String> {
    let mut dirs = Vec::new();
    for var in ["TMPDIR", "TEMP", "TMP"] {
        if let Ok(v) = std::env::var(var) {
            if !v.is_empty() {
                dirs.push(v);
            }
        }
    }
    if cfg!(windows) {
        if let Some(home) = home_dir() {
            dirs.push(format!("{}\\AppData\\Local\\Temp", home));
        }
        if let Ok(root) = std::env::var("SYSTEMROOT") {
            dirs.push(format!("{root}\\Temp"));
        } else {
            dirs.push("%SYSTEMROOT%\\Temp".to_string());
        }
        for d in ["c:\\temp", "c:\\tmp", "\\temp", "\\tmp"] {
            dirs.push(d.to_string());
        }
    } else {
        for d in ["/tmp", "/var/tmp", "/usr/tmp"] {
            dirs.push(d.to_string());
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(crate::paths::os_to_py_string(cwd.as_os_str()));
    }
    dirs
}

fn home_dir() -> Option<String> {
    if cfg!(windows) {
        if let Ok(p) = std::env::var("USERPROFILE") {
            return Some(p);
        }
        match (std::env::var("HOMEDRIVE"), std::env::var("HOMEPATH")) {
            (Ok(d), Ok(p)) => Some(format!("{d}{p}")),
            _ => None,
        }
    } else {
        std::env::var("HOME").ok()
    }
}

fn writable(dir: &Path) -> bool {
    for i in 0..100u32 {
        let name = format!("tmp{}{:08x}", pid(), i.wrapping_mul(2654435761) ^ (std::process::id()));
        let probe = dir.join(name);
        match fs::OpenOptions::new().write(true).create_new(true).open(&probe) {
            Ok(mut f) => {
                let ok = f.write_all(b"blat").is_ok();
                drop(f);
                let _ = fs::remove_file(&probe);
                return ok;
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied && cfg!(windows) && dir.is_dir() => continue,
            Err(_) => return false,
        }
    }
    false
}

/// Python `tempfile.gettempdir()`: the first writable of `TMPDIR`, `TEMP`, `TMP`, the platform defaults
/// (Windows `~\AppData\Local\Temp`, `%SYSTEMROOT%\Temp`, `c:\temp`, `c:\tmp`, `\temp`, `\tmp`; POSIX `/tmp`,
/// `/var/tmp`, `/usr/tmp`) and the cwd, made absolute (`abspath`). Cached for the process like Python.
pub fn gettempdir() -> PathBuf {
    let mut guard = TEMPDIR.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(dir) = guard.as_ref() {
        return dir.clone();
    }
    let mut found = None;
    for d in candidate_tempdirs() {
        let abs = crate::paths::abspath(Path::new(&d));
        if writable(&abs) {
            found = Some(abs);
            break;
        }
    }
    let dir = found.unwrap_or_else(|| crate::paths::abspath(Path::new(".")));
    *guard = Some(dir.clone());
    dir
}

/// Development override of the native runtime folder (sherpa-onnx C API + its onnxruntime).
pub const SHERPA_DIR_ENV_VAR: &str = "SUBTITLE_ENGINE_SHERPA_DIR";

/// Folders searched for a bundled native library (`onnxruntime`, `sherpa-onnx-c-api`), in order: the
/// executable's folder, then `$SUBTITLE_ENGINE_SHERPA_DIR` (dev copy, e.g. the venv's `sherpa_onnx/lib`).
/// `ORT_DYLIB_PATH` (a full file path) is checked by the ONNX Runtime loader before these.
pub fn native_library_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        dirs.push(dir);
    }
    if let Ok(dir) = std::env::var(SHERPA_DIR_ENV_VAR) {
        let dir = crate::pyfmt::strip(&dir);
        if !dir.is_empty() {
            dirs.push(PathBuf::from(dir));
        }
    }
    dirs
}

/// The first existing `<dir>/<file_name>` among [`native_library_dirs`].
pub fn find_native_library(file_name: &str) -> Option<PathBuf> {
    native_library_dirs().into_iter().map(|d| d.join(file_name)).find(|p| p.is_file())
}

/// `<gettempdir()>/SubtitleAdder` (not created).
pub fn app_temp_dir() -> PathBuf {
    let base = crate::paths::py_path_str(&gettempdir());
    PathBuf::from(crate::paths::py_join(&base, APP_TEMP_DIR_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[serial_test::serial]
    fn test_tempdir_override_and_app_dir() {
        let dir = tempfile::tempdir().unwrap();
        set_tempdir(Some(dir.path().to_path_buf()));
        assert_eq!(gettempdir(), dir.path());
        assert!(app_temp_dir().ends_with(APP_TEMP_DIR_NAME));
        set_tempdir(None);
        assert!(gettempdir().is_absolute());
    }

    #[test]
    #[serial_test::serial]
    fn test_write_atomic_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.json");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
