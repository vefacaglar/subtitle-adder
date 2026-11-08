//! Locating the engine (mac-enginekit §2, win-enginekit §2, product rule 7, UNIFIED.md §3.7 "`SUBTITLE_ENGINE`
//! trim" FIX-W).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Executable plus arguments prepended to every subcommand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineCommand {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
}

/// The engine binary's file name.
pub const ENGINE_FILE_NAME: &str = if cfg!(windows) { "subtitle-engine.exe" } else { "subtitle-engine" };

/// The environment variable that overrides the engine location.
pub const ENGINE_ENV_VAR: &str = "SUBTITLE_ENGINE";

impl EngineCommand {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self { executable: executable.into(), arguments: Vec::new() }
    }

    /// A command with prefix arguments (placed before the subcommand; tests pass a fake-engine script this way).
    pub fn with_arguments(executable: impl Into<PathBuf>, arguments: Vec<String>) -> Self {
        Self { executable: executable.into(), arguments }
    }

    /// Order: `SUBTITLE_ENGINE` (trimmed; blank = unset; no existence check; Windows looks the name up
    /// case-insensitively) → bundled engine ([`bundled_engine`](Self::bundled_engine)) → repo
    /// `engine/dist/subtitle-engine[.exe]` (development; see [`repo_engine_near`](Self::repo_engine_near)) → `None`
    /// (callers map it to `EngineError::NotFound`).
    pub fn locate(env: &HashMap<String, String>, exe_path: Option<&Path>) -> Option<EngineCommand> {
        Self::locate_with(env, exe_path, Some(&Self::repo_root()))
    }

    /// `locate` with an explicit compile-time repository fallback (`None` disables it; tests).
    pub fn locate_with(
        env: &HashMap<String, String>,
        exe_path: Option<&Path>,
        compile_time_repo: Option<&Path>,
    ) -> Option<EngineCommand> {
        if let Some(value) = env_value(env, ENGINE_ENV_VAR) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(EngineCommand::new(value));
            }
        }
        if let Some(exe) = exe_path
            && let Some(bundled) = Self::bundled_engine(exe)
        {
            return Some(EngineCommand::new(bundled));
        }
        Self::repo_engine_near(exe_path, compile_time_repo).map(EngineCommand::new)
    }

    /// `locate` with the process environment and `std::env::current_exe()`.
    pub fn locate_default() -> Option<EngineCommand> {
        let env: HashMap<String, String> = std::env::vars().collect();
        Self::locate(&env, std::env::current_exe().ok().as_deref())
    }

    /// Windows: `<exe dir>\subtitle-engine\subtitle-engine.exe` if the file exists. macOS: only when the exe lives
    /// in `<X>.app/Contents/MacOS/`: `<X>.app/Contents/Resources/subtitle-engine/subtitle-engine` if executable, else
    /// `<X>.app/Contents/MacOS/subtitle-engine` if it exists.
    pub fn bundled_engine(exe_path: &Path) -> Option<PathBuf> {
        let exe_dir = exe_path.parent()?;
        if cfg!(target_os = "macos") {
            let contents = exe_dir.parent()?;
            let is_bundle = exe_dir.file_name().is_some_and(|n| n == "MacOS")
                && contents.file_name().is_some_and(|n| n == "Contents")
                && contents.parent().and_then(Path::extension).is_some_and(|e| e == "app");
            if !is_bundle {
                return None;
            }
            let resources = contents.join("Resources").join("subtitle-engine").join(ENGINE_FILE_NAME);
            if is_executable_file(&resources) {
                return Some(resources);
            }
            let aux = exe_dir.join(ENGINE_FILE_NAME);
            return aux.exists().then_some(aux);
        }
        let bundled = exe_dir.join("subtitle-engine").join(ENGINE_FILE_NAME);
        bundled.is_file().then_some(bundled)
    }

    /// `<repo>/engine/dist/subtitle-engine[.exe]` when usable (development fallback), from the compile-time repo.
    pub fn repo_engine() -> Option<PathBuf> {
        Self::dist_engine(&Self::repo_root().join("engine"))
    }

    /// Development lookup: the nearest ancestor of the exe directory holding `engine/Cargo.toml` (win-enginekit §2:
    /// nearest wins, no fallback to an outer repo); when no ancestor has one, the compile-time repository root
    /// (mac-enginekit §2 step 3; the Rust build's target directory may live outside the repo). The binary must be an
    /// executable file in that engine's `dist`.
    pub fn repo_engine_near(exe_path: Option<&Path>, compile_time_repo: Option<&Path>) -> Option<PathBuf> {
        if let Some(engine_dir) = exe_path.and_then(Path::parent).and_then(Self::find_repo_engine) {
            return Self::dist_engine(&engine_dir);
        }
        let repo = compile_time_repo?;
        let engine_dir = repo.join("engine");
        if !engine_dir.join("Cargo.toml").is_file() {
            return None;
        }
        Self::dist_engine(&engine_dir)
    }

    /// Walks from `start` (inclusive) up to the root; the first `dir` with a file `dir/engine/Cargo.toml` gives
    /// `dir/engine`. A `pyproject.toml`-only engine directory does not count.
    pub fn find_repo_engine(start: &Path) -> Option<PathBuf> {
        let start = std::path::absolute(start).unwrap_or_else(|_| start.to_path_buf());
        start.ancestors().map(|dir| dir.join("engine")).find(|engine| engine.join("Cargo.toml").is_file())
    }

    fn dist_engine(engine_dir: &Path) -> Option<PathBuf> {
        let dev = engine_dir.join("dist").join(ENGINE_FILE_NAME);
        is_executable_file(&dev).then_some(dev)
    }

    /// The repository root derived from `CARGO_MANIFEST_DIR` at compile time (`apps/desktop/crates/sa-enginekit`
    /// → four levels up).
    pub fn repo_root() -> PathBuf {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        manifest.ancestors().nth(4).map_or_else(|| manifest.join("../../../.."), Path::to_path_buf)
    }
}

/// The value of `name`: exact key first, then (Windows) the first key equal case-insensitively.
fn env_value<'a>(env: &'a HashMap<String, String>, name: &str) -> Option<&'a String> {
    env.get(name).or_else(|| if cfg!(windows) { env.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v) } else { None })
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    if !path.is_file() {
        return false;
    }
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return false };
    // SAFETY: `c` is a valid NUL-terminated path.
    unsafe { libc::access(c.as_ptr(), libc::X_OK) == 0 }
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}
