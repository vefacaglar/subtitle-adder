//! CUDA 12 runtime libraries for NVIDIA GPUs on Windows/Linux (port of `cuda.py`, spec models.md §5).

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use se_core::ProgressFn;

use crate::http::{Opener, RuntimeReporter, UreqOpener, download_to_file_with};

/// Inference device of CTranslate2 models (`device="auto"|"cpu"|"cuda"`), shared by `se-asr` and
/// `se-translate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device {
    Auto,
    Cpu,
    Cuda,
}

impl Device {
    /// `auto` → `Cuda` if [`cuda_usable`] else `Cpu`; `Cuda` also calls [`register_libraries`].
    pub fn resolve(self) -> Device {
        match self {
            Device::Auto => {
                if cuda_usable() {
                    Device::Cuda
                } else {
                    Device::Cpu
                }
            }
            Device::Cuda => {
                register_libraries();
                Device::Cuda
            }
            Device::Cpu => Device::Cpu,
        }
    }

    /// The CTranslate2 device string.
    pub fn as_str(self) -> &'static str {
        match self {
            Device::Auto => "auto",
            Device::Cpu => "cpu",
            Device::Cuda => "cuda",
        }
    }
}

/// Override of the CUDA library folder.
pub const CUDA_DIR_ENV_VAR: &str = "SUBTITLE_ENGINE_CUDA_DIR";
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// Windows libraries probed by [`cuda_usable`].
pub const WINDOWS_LIBS: &[&str] = &["cublas64_12.dll", "cublasLt64_12.dll", "cudnn64_9.dll"];
/// Linux libraries (load order).
pub const LINUX_LIBS: &[&str] = &["libcublasLt.so.12", "libcublas.so.12", "libcudnn.so.9"];

/// A pinned NVIDIA wheel (`_CudaWheel`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CudaWheel {
    /// Folder under `nvidia/` that marks this wheel as extracted.
    pub subdir: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

/// Pinned wheels for `win32` (verbatim from `cuda.py`, including the 63-hex nvrtc digest — see risk R9).
pub static WINDOWS_WHEELS: &[CudaWheel] = &[
    CudaWheel {
        subdir: "cublas",
        url: "https://files.pythonhosted.org/packages/20/e2/fc9a0e985249d873150276d5afb02e39a66817fedbf1a385724393e505ed/nvidia_cublas_cu12-12.9.2.10-py3-none-win_amd64.whl",
        sha256: "623f43027d40d44ceadf0043f002bd25cf353e8f13ce90b9a87057019f560661",
        size: 553_162_896,
    },
    CudaWheel {
        subdir: "cuda_nvrtc",
        url: "https://files.pythonhosted.org/packages/52/de/823919be3b9d0ccbf1f784035423c5f18f4267fb0123558d58b813c6ec86/nvidia_cuda_nvrtc_cu12-12.9.86-py3-none-win_amd64.whl",
        sha256: "72972ebdcf504d69462d3bcd67e7b81edd25d0fb85a2c46d3ea351766636349",
        size: 76_408_187,
    },
    CudaWheel {
        subdir: "cudnn",
        url: "https://files.pythonhosted.org/packages/aa/38/f856579877f7c1c5066e61182e7de7bc27bf35a78c8d1b0fa592e6985bc4/nvidia_cudnn_cu12-9.27.0.42-py3-none-win_amd64.whl",
        sha256: "06e9b0026f3bad97d2b58666330fabec04fe1672f776661ecb0ce0029c27f142",
        size: 743_068_852,
    },
];

/// Pinned wheels for `linux`.
pub static LINUX_WHEELS: &[CudaWheel] = &[
    CudaWheel {
        subdir: "cublas",
        url: "https://files.pythonhosted.org/packages/7b/ea/9b6c36aef90766eebb4763aded157039e797ae9789c9c216e960d1d3cd47/nvidia_cublas_cu12-12.9.3.13-py3-none-manylinux_2_27_x86_64.whl",
        sha256: "d7d51055f161b317c7105ddaae90e71eda059026074ffef93cf7cde2886bfe72",
        size: 581_217_458,
    },
    CudaWheel {
        subdir: "cuda_nvrtc",
        url: "https://files.pythonhosted.org/packages/b8/85/e4af82cc9202023862090bfca4ea827d533329e925c758f0cde964cb54b7/nvidia_cuda_nvrtc_cu12-12.9.86-py3-none-manylinux2010_x86_64.manylinux_2_12_x86_64.whl",
        sha256: "210cf05005a447e29214e9ce50851e83fc5f4358df8b453155d5e1918094dcb4",
        size: 89_568_129,
    },
    CudaWheel {
        subdir: "cudnn",
        url: "https://files.pythonhosted.org/packages/65/e4/c5a205d48ff00ed8b27882bb45d338d8138e976c76385f328a326bbfaeda/nvidia_cudnn_cu12-9.27.0.42-py3-none-manylinux_2_27_x86_64.whl",
        sha256: "0a4aa3a7d2264256506c6857fc41fc0c499982f78d70195b1cfcc9055c1957cd",
        size: 766_178_381,
    },
];

/// Python's `sys.platform` of this build (`win32`, `linux`, `darwin`, other).
pub fn platform() -> &'static str {
    if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        std::env::consts::OS
    }
}

/// `_CUDA_WHEELS.get(platform, ())`.
pub fn wheels_for(platform: &str) -> &'static [CudaWheel] {
    match platform {
        "win32" => WINDOWS_WHEELS,
        "linux" => LINUX_WHEELS,
        _ => &[],
    }
}

fn lib_subdir(platform: &str) -> &'static str {
    if platform == "win32" { "bin" } else { "lib" }
}

fn env_nonblank(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| se_core::pyfmt::strip(&v).to_string()).filter(|v| !v.is_empty())
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// `cuda_cache_dir()`: `$SUBTITLE_ENGINE_CUDA_DIR`, else Windows `%LOCALAPPDATA%\SubtitleAdder\cuda`, else
/// `$XDG_CACHE_HOME/subtitle-adder/cuda` (`~/.cache` fallback).
pub fn cuda_cache_dir() -> PathBuf {
    if let Some(dir) = env_nonblank(CUDA_DIR_ENV_VAR) {
        return PathBuf::from(dir);
    }
    user_cache_base().join(if cfg!(windows) { "SubtitleAdder" } else { "subtitle-adder" }).join("cuda")
}

/// `%LOCALAPPDATA%` (or `~/AppData/Local`) on Windows, `$XDG_CACHE_HOME` (or `~/.cache`) elsewhere.
pub(crate) fn user_cache_base() -> PathBuf {
    if cfg!(windows) {
        env_nonempty("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| crate::fsx::home_dir().join("AppData").join("Local"))
    } else {
        env_nonempty("XDG_CACHE_HOME").map(PathBuf::from).unwrap_or_else(|| crate::fsx::home_dir().join(".cache"))
    }
}

/// `_nvidia_dirs()`: `nvidia/<pkg>/bin|lib` folders under the executable's folder (and its `_internal`
/// bundle folder) and [`cuda_cache_dir`], children sorted, without duplicates.
pub fn nvidia_dirs() -> Vec<PathBuf> {
    let sub = lib_subdir(platform());
    let mut roots = Vec::new();
    if let Some(exe_dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        roots.push(exe_dir.join("_internal"));
        roots.push(exe_dir);
    }
    roots.push(cuda_cache_dir());
    let mut dirs: Vec<PathBuf> = Vec::new();
    for root in roots {
        let nvidia = root.join("nvidia");
        let Ok(rd) = fs::read_dir(&nvidia) else { continue };
        let mut children: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        children.sort();
        for d in children {
            let lib = d.join(sub);
            if lib.is_dir() && !dirs.contains(&lib) {
                dirs.push(lib);
            }
        }
    }
    dirs
}

static REGISTERED: Mutex<bool> = Mutex::new(false);
static USABLE: Mutex<Option<bool>> = Mutex::new(None);

/// `register_libraries()` (once per process; reset after a download): make the cached libraries findable
/// (Windows: prepend to `PATH` and `AddDllDirectory`; Linux: preload by full path with `RTLD_GLOBAL`).
pub fn register_libraries() {
    let mut done = REGISTERED.lock().unwrap_or_else(|e| e.into_inner());
    if *done {
        return;
    }
    *done = true;
    let dirs = nvidia_dirs();
    if dirs.is_empty() {
        return;
    }
    #[cfg(windows)]
    {
        let old = std::env::var_os("PATH").unwrap_or_default();
        let mut joined = std::ffi::OsString::new();
        for d in &dirs {
            joined.push(d.as_os_str());
            joined.push(";");
        }
        joined.push(old);
        // SAFETY: called while loading models on the engine's main thread, before worker threads use the
        // environment (the same moment Python mutates os.environ).
        unsafe { std::env::set_var("PATH", joined) };
        for d in &dirs {
            native::add_dll_directory(d);
        }
    }
    #[cfg(target_os = "linux")]
    {
        for name in LINUX_LIBS {
            if let Some(d) = dirs.iter().find(|d| d.join(name).exists()) {
                native::dlopen_global(&d.join(name));
            }
        }
    }
}

fn reset_memos() {
    *REGISTERED.lock().unwrap_or_else(|e| e.into_inner()) = false;
    *USABLE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

static GPU_PROBE: AtomicUsize = AtomicUsize::new(0);

/// Installs the CUDA device-count probe (`ct2rs::sys::get_device_count(Device::CUDA)`).
pub fn set_gpu_probe(probe: fn() -> usize) {
    GPU_PROBE.store(probe as usize, Ordering::SeqCst);
}

/// `has_nvidia_gpu()`: never on macOS; else CTranslate2 reports at least one CUDA device.
///
/// se-models has no CTranslate2 dependency: `se-asr` installs the probe with [`set_gpu_probe`]. Without a
/// probe the CUDA driver API (`nvcuda.dll` / `libcuda.so.1`, `cuDeviceGetCount`) is asked instead.
pub fn has_nvidia_gpu() -> bool {
    if cfg!(target_os = "macos") {
        return false;
    }
    let raw = GPU_PROBE.load(Ordering::SeqCst);
    if raw != 0 {
        // SAFETY: only `set_gpu_probe` stores into GPU_PROBE, always a valid `fn() -> usize`.
        let probe: fn() -> usize = unsafe { std::mem::transmute::<usize, fn() -> usize>(raw) };
        return probe() > 0;
    }
    native::driver_device_count() > 0
}

/// `cuda_usable()` (memoized): a GPU is present and the CUDA 12 / cuDNN 9 libraries load.
pub fn cuda_usable() -> bool {
    if let Some(v) = *USABLE.lock().unwrap_or_else(|e| e.into_inner()) {
        return v;
    }
    let value = compute_usable();
    *USABLE.lock().unwrap_or_else(|e| e.into_inner()) = Some(value);
    value
}

fn compute_usable() -> bool {
    if !has_nvidia_gpu() {
        return false;
    }
    register_libraries();
    let names = if platform() == "win32" { WINDOWS_LIBS } else { LINUX_LIBS };
    names.iter().all(|n| native::load_by_name(n))
}

/// `needs_cuda_download()`: not strict offline, Windows/Linux, a GPU, and the libraries are not usable.
/// Tests stub this to `false` (conftest `_no_implicit_cuda_download`) through the CLI's `Services`.
pub fn needs_cuda_download() -> bool {
    needs_cuda_download_with(platform(), &has_nvidia_gpu, &cuda_usable)
}

/// [`needs_cuda_download`] with injectable platform / GPU / usability checks.
pub fn needs_cuda_download_with(platform: &str, has_gpu: &dyn Fn() -> bool, usable: &dyn Fn() -> bool) -> bool {
    if se_core::offline::offline_requested() {
        return false;
    }
    !wheels_for(platform).is_empty() && has_gpu() && !usable()
}

/// The seams of [`ensure_cuda_with`] (the Python test replaces all of them).
pub struct CudaEnv<'a> {
    pub platform: &'a str,
    pub wheels: &'a [CudaWheel],
    pub cache_dir: PathBuf,
    pub opener: &'a dyn Opener,
    pub needs_download: &'a dyn Fn() -> bool,
    /// `cuda_usable()` (after the memo reset).
    pub usable: &'a dyn Fn() -> bool,
    /// `register_libraries.cache_clear(); cuda_usable.cache_clear(); register_libraries()`.
    pub refresh: &'a dyn Fn(),
    pub interval: Duration,
}

/// `ensure_cuda(on_progress)`: downloads and extracts the pinned NVIDIA wheels; any failure prints
/// `CUDA runtime download failed, falling back to CPU: {exc}` on stderr and returns false (no error event).
pub fn ensure_cuda(on_progress: Option<ProgressFn<'_>>) -> bool {
    let refresh = || {
        reset_memos();
        register_libraries();
    };
    let env = CudaEnv {
        platform: platform(),
        wheels: wheels_for(platform()),
        cache_dir: cuda_cache_dir(),
        opener: &UreqOpener,
        needs_download: &needs_cuda_download,
        usable: &cuda_usable,
        refresh: &refresh,
        interval: PROGRESS_INTERVAL,
    };
    ensure_cuda_with(&env, on_progress, &mut io::stderr())
}

/// [`ensure_cuda`] with injectable seams and stderr.
pub fn ensure_cuda_with(env: &CudaEnv<'_>, on_progress: Option<ProgressFn<'_>>, stderr: &mut dyn io::Write) -> bool {
    if !(env.needs_download)() {
        return (env.usable)();
    }
    let root = &env.cache_dir;
    let sub = lib_subdir(env.platform);
    let mut missing: Vec<&CudaWheel> = env.wheels.iter().filter(|w| !wheel_installed(root, w, sub)).collect();
    if missing.is_empty() {
        missing = env.wheels.iter().collect();
    }
    let total: u64 = missing.iter().map(|w| w.size).sum();
    let mut reporter = RuntimeReporter::new(on_progress, total, env.interval);
    let result = (|| -> Result<(), String> {
        fs::create_dir_all(root).map_err(|e| se_core::EngineError::win(&e, Some(root)).message)?;
        let mut downloaded = 0u64;
        for wheel in &missing {
            let tmp = root.join(format!(".{}.whl.tmp", wheel.subdir));
            let outcome = (|| -> Result<(), String> {
                let base = downloaded;
                let mut seen = 0u64;
                let digest = download_to_file_with(env.opener, wheel.url, &tmp, &mut |n| {
                    seen = n;
                    reporter.report(base + n);
                })
                .map_err(|e| e.message)?;
                downloaded = base + seen;
                if digest.to_lowercase() != wheel.sha256.to_lowercase() {
                    return Err(format!("SHA-256 mismatch for {} CUDA wheel", wheel.subdir));
                }
                extract_wheel_libs(&tmp, root, sub).map_err(|e| e.to_string())
            })();
            let _ = fs::remove_file(&tmp);
            outcome?;
        }
        Ok(())
    })();
    if let Err(exc) = result {
        let _ = writeln!(stderr, "CUDA runtime download failed, falling back to CPU: {exc}");
        return false;
    }
    (env.refresh)();
    reporter.finish();
    (env.usable)()
}


/// `_wheel_installed(root, wheel)`: `root/nvidia/<subdir>/<bin|lib>` is a non-empty folder.
fn wheel_installed(root: &Path, wheel: &CudaWheel, sub: &str) -> bool {
    let dir = root.join("nvidia").join(wheel.subdir).join(sub);
    fs::read_dir(&dir).map(|mut rd| rd.next().is_some()).unwrap_or(false)
}

/// `_extract_wheel_libs(whl, root)`: writes every `nvidia/<pkg>/<bin|lib>/...` member below `root`
/// (later members overwrite earlier ones); everything else is skipped. Unsafe member paths are skipped too.
pub fn extract_wheel_libs(whl: &Path, root: &Path, sub: &str) -> io::Result<()> {
    let file = fs::File::open(whl)?;
    let mut zip = zip::ZipArchive::new(file).map_err(io::Error::other)?;
    for i in 0..zip.len() {
        let mut member = zip.by_index(i).map_err(io::Error::other)?;
        if member.is_dir() {
            continue;
        }
        let name = member.name().to_string();
        let parts: Vec<&str> =
            name.split(|c| c == '/' || (cfg!(windows) && c == '\\')).filter(|p| !p.is_empty() && *p != ".").collect();
        if parts.len() < 4 || parts[0] != "nvidia" || parts[2] != sub {
            continue;
        }
        if parts.iter().any(|p| *p == ".." || p.contains(':')) {
            continue;
        }
        let target = parts.iter().fold(root.to_path_buf(), |acc, p| acc.join(p));
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = fs::File::create(&target)?;
        let mut buf = Vec::new();
        member.read_to_end(&mut buf)?;
        io::Write::write_all(&mut out, &buf)?;
    }
    Ok(())
}

/// `is_cuda_library_error(exc)`: the lowercased message contains `cublas`, `cudnn`, `cuda` or `no kernel image`.
pub fn is_cuda_library_error(message: &str) -> bool {
    let m = message.to_lowercase();
    ["cublas", "cudnn", "cuda", "no kernel image"].iter().any(|k| m.contains(k))
}

/// Native loader calls (hand-written FFI; se-models links no native library).
mod native {
    #[cfg(windows)]
    mod win {
        use std::ffi::c_void;
        use std::os::windows::ffi::OsStrExt;
        use std::path::Path;

        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn AddDllDirectory(new_directory: *const u16) -> *mut c_void;
            fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
            fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
        }

        fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
            s.encode_wide().chain(std::iter::once(0)).collect()
        }

        pub fn add_dll_directory(dir: &Path) {
            let w = wide(dir.as_os_str());
            // SAFETY: `w` is a NUL-terminated wide string that outlives the call.
            unsafe { AddDllDirectory(w.as_ptr()) };
        }

        pub fn load(name: &str) -> *mut c_void {
            let w = wide(std::ffi::OsStr::new(name));
            // SAFETY: NUL-terminated name; flags 0 = standard search order (incl. PATH). The module stays loaded.
            unsafe { LoadLibraryExW(w.as_ptr(), std::ptr::null_mut(), 0) }
        }

        pub fn symbol(module: *mut c_void, name: &[u8]) -> *mut c_void {
            // SAFETY: `module` is a loaded module handle and `name` is NUL-terminated.
            unsafe { GetProcAddress(module, name.as_ptr()) }
        }
    }

    #[cfg(unix)]
    mod nix {
        use std::ffi::{CString, c_char, c_int, c_void};

        unsafe extern "C" {
            fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
            fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        }
        const RTLD_NOW: c_int = 2;
        #[cfg(target_os = "linux")]
        const RTLD_GLOBAL: c_int = 0x100;
        #[cfg(not(target_os = "linux"))]
        const RTLD_GLOBAL: c_int = 0x8;

        pub fn open(name: &str, global: bool) -> *mut c_void {
            let Ok(c) = CString::new(name) else { return std::ptr::null_mut() };
            let flags = RTLD_NOW | if global { RTLD_GLOBAL } else { 0 };
            // SAFETY: valid C string; the handle is intentionally never closed (libraries stay loaded).
            unsafe { dlopen(c.as_ptr(), flags) }
        }

        pub fn symbol(handle: *mut c_void, name: &[u8]) -> *mut c_void {
            // SAFETY: `handle` is a live dlopen handle and `name` is NUL-terminated.
            unsafe { dlsym(handle, name.as_ptr().cast()) }
        }
    }

    #[cfg(windows)]
    pub fn add_dll_directory(dir: &std::path::Path) {
        win::add_dll_directory(dir);
    }

    #[cfg(target_os = "linux")]
    pub fn dlopen_global(path: &std::path::Path) {
        let _ = nix::open(&path.to_string_lossy(), true);
    }

    /// Loads a library by bare name with the platform's standard search (kept loaded).
    pub fn load_by_name(name: &str) -> bool {
        #[cfg(windows)]
        {
            !win::load(name).is_null()
        }
        #[cfg(unix)]
        {
            !nix::open(name, false).is_null()
        }
        #[cfg(not(any(windows, unix)))]
        {
            let _ = name;
            false
        }
    }

    type CuInit = unsafe extern "system" fn(u32) -> i32;
    type CuDeviceGetCount = unsafe extern "system" fn(*mut i32) -> i32;

    /// CUDA devices reported by the driver API (0 when the driver is missing or fails).
    pub fn driver_device_count() -> usize {
        #[cfg(windows)]
        let (module, init, count) = {
            let m = win::load("nvcuda.dll");
            if m.is_null() {
                return 0;
            }
            (m, win::symbol(m, b"cuInit\0"), win::symbol(m, b"cuDeviceGetCount\0"))
        };
        #[cfg(target_os = "linux")]
        let (module, init, count) = {
            let m = nix::open("libcuda.so.1", false);
            if m.is_null() {
                return 0;
            }
            (m, nix::symbol(m, b"cuInit\0"), nix::symbol(m, b"cuDeviceGetCount\0"))
        };
        #[cfg(not(any(windows, target_os = "linux")))]
        let (module, init, count): (*mut std::ffi::c_void, *mut std::ffi::c_void, *mut std::ffi::c_void) =
            (std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
        let _ = module;
        if init.is_null() || count.is_null() {
            return 0;
        }
        // SAFETY: the symbols come from the CUDA driver library with these documented signatures.
        unsafe {
            let init: CuInit = std::mem::transmute::<*mut std::ffi::c_void, CuInit>(init);
            let count_fn: CuDeviceGetCount = std::mem::transmute::<*mut std::ffi::c_void, CuDeviceGetCount>(count);
            if init(0) != 0 {
                return 0;
            }
            let mut n = 0i32;
            if count_fn(&mut n) != 0 {
                return 0;
            }
            n.max(0) as usize
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cuda_library_errors_are_recognized() {
        assert!(is_cuda_library_error("Library cublas64_12.dll is not found"));
        assert!(is_cuda_library_error("no kernel image is available"));
        assert!(!is_cuda_library_error("out of memory"));
    }

    #[test]
    fn wheels_are_pinned_per_platform() {
        assert_eq!(wheels_for("win32").len(), 3);
        assert_eq!(wheels_for("linux").len(), 3);
        assert!(wheels_for("darwin").is_empty());
        // Verbatim parity with cuda.py: the Windows nvrtc digest has 63 hex characters (risk R9).
        assert_eq!(WINDOWS_WHEELS[1].sha256.len(), 63);
    }
}
