//! Port of `tests/test_cuda.py` (the translator fallback test lives in se-translate).

use std::cell::Cell;
use std::io::{Cursor, Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use serial_test::serial;
use sha2::{Digest, Sha256};

use se_models::cuda::{CUDA_DIR_ENV_VAR, CudaEnv, CudaWheel, cuda_cache_dir, ensure_cuda_with, needs_cuda_download_with};
use se_models::http::Opener;

fn make_fake_wheel(subdir: &str, lib_folder: &str, filename: &str) -> (Vec<u8>, String) {
    let mut buf = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        zip.start_file(format!("nvidia/{subdir}/{lib_folder}/{filename}"), opts).unwrap();
        zip.write_all(b"dll-bytes").unwrap();
        zip.start_file(format!("nvidia/{subdir}/include/header.h"), opts).unwrap();
        zip.write_all(b"// ignored").unwrap();
        zip.finish().unwrap();
    }
    let raw = buf.into_inner();
    let sha = hex::encode(Sha256::digest(&raw));
    (raw, sha)
}

struct BytesOpener(Vec<u8>);
impl Opener for BytesOpener {
    fn open(&self, _url: &str) -> Result<Box<dyn Read>, String> {
        Ok(Box::new(Cursor::new(self.0.clone())))
    }
}

struct FailingOpener;
impl Opener for FailingOpener {
    fn open(&self, _url: &str) -> Result<Box<dyn Read>, String> {
        Err("network unreachable".to_string())
    }
}

#[test]
#[serial]
fn test_cuda_cache_dir_respects_env_override() {
    let dir = tempfile::tempdir().unwrap();
    let custom = dir.path().join("custom-cuda");
    // SAFETY: serial test.
    unsafe { std::env::set_var(CUDA_DIR_ENV_VAR, &custom) };
    let got = cuda_cache_dir();
    unsafe { std::env::remove_var(CUDA_DIR_ENV_VAR) };
    assert_eq!(got, custom);
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

#[test]
fn test_ensure_cuda_downloads_and_extracts_wheel_binaries() {
    let dir = tempfile::tempdir().unwrap();
    let cache_dir = dir.path().join("cuda-cache");
    let (raw, sha) = make_fake_wheel("cublas", "bin", "cublas64_12.dll");
    let wheel = CudaWheel { subdir: "cublas", url: "https://example.invalid/cublas.whl", sha256: leak(sha), size: raw.len() as u64 };
    let wheels = [wheel];
    let refreshed = Cell::new(false);
    let refresh = || refreshed.set(true);
    let env = CudaEnv {
        platform: "win32",
        wheels: &wheels,
        cache_dir: cache_dir.clone(),
        opener: &BytesOpener(raw),
        needs_download: &|| true,
        usable: &|| true,
        refresh: &refresh,
        interval: Duration::from_millis(100),
    };
    let mut progress = Vec::new();
    let mut cb = |f: f64| progress.push(f);
    let mut stderr = Vec::new();
    assert!(ensure_cuda_with(&env, Some(&mut cb), &mut stderr));
    assert_eq!(std::fs::read(cache_dir.join("nvidia/cublas/bin/cublas64_12.dll")).unwrap(), b"dll-bytes");
    assert!(!cache_dir.join("nvidia/cublas/include/header.h").exists());
    assert_eq!(*progress.last().unwrap(), 1.0);
    assert!(refreshed.get());
    assert!(!cache_dir.join(".cublas.whl.tmp").exists());
}

#[test]
fn test_ensure_cuda_returns_false_on_network_error() {
    let dir = tempfile::tempdir().unwrap();
    let env = CudaEnv {
        platform: "win32",
        wheels: se_models::cuda::WINDOWS_WHEELS,
        cache_dir: dir.path().join("cuda-cache"),
        opener: &FailingOpener,
        needs_download: &|| true,
        usable: &|| false,
        refresh: &|| {},
        interval: Duration::from_millis(100),
    };
    let mut stderr = Vec::new();
    assert!(!ensure_cuda_with(&env, None, &mut stderr));
    let text = String::from_utf8(stderr).unwrap();
    assert_eq!(text, "CUDA runtime download failed, falling back to CPU: network unreachable\n");
}

#[test]
fn test_ensure_cuda_sha_mismatch_falls_back_to_cpu() {
    let dir = tempfile::tempdir().unwrap();
    let (raw, _) = make_fake_wheel("cublas", "bin", "cublas64_12.dll");
    let wheels = [CudaWheel { subdir: "cublas", url: "u", sha256: "00", size: raw.len() as u64 }];
    let env = CudaEnv {
        platform: "win32",
        wheels: &wheels,
        cache_dir: dir.path().to_path_buf(),
        opener: &BytesOpener(raw),
        needs_download: &|| true,
        usable: &|| true,
        refresh: &|| {},
        interval: Duration::ZERO,
    };
    let mut stderr = Vec::new();
    assert!(!ensure_cuda_with(&env, None, &mut stderr));
    assert!(String::from_utf8(stderr).unwrap().contains("SHA-256 mismatch for cublas CUDA wheel"));
    assert!(!PathBuf::from(dir.path()).join("nvidia").exists());
}

#[test]
fn test_ensure_cuda_without_download_reports_usability() {
    let dir = tempfile::tempdir().unwrap();
    let env = CudaEnv {
        platform: "win32",
        wheels: &[],
        cache_dir: dir.path().to_path_buf(),
        opener: &FailingOpener,
        needs_download: &|| false,
        usable: &|| true,
        refresh: &|| panic!("no refresh without a download"),
        interval: Duration::ZERO,
    };
    assert!(ensure_cuda_with(&env, None, &mut Vec::new()));
}

#[test]
#[serial]
fn test_needs_cuda_download_is_false_in_strict_offline_mode() {
    // SAFETY: serial test.
    unsafe { std::env::set_var(se_core::offline::OFFLINE_ENV_VAR, "1") };
    let got = needs_cuda_download_with("win32", &|| true, &|| false);
    unsafe { std::env::remove_var(se_core::offline::OFFLINE_ENV_VAR) };
    assert!(!got);
    assert!(needs_cuda_download_with("win32", &|| true, &|| false));
    assert!(!needs_cuda_download_with("darwin", &|| true, &|| false));
}

#[test]
fn test_device_resolve_cpu_stays_cpu() {
    assert_eq!(se_models::cuda::Device::Cpu.resolve(), se_models::cuda::Device::Cpu);
}
