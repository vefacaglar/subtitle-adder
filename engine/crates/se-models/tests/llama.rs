//! Tests for `llama.rs` (no Python counterpart; spec models.md §10 lists what to cover).

use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serial_test::serial;
use sha2::{Digest, Sha256};

use se_models::http::Opener;
use se_models::llama::*;

struct BytesOpener(Vec<u8>);
impl Opener for BytesOpener {
    fn open(&self, _url: &str) -> Result<Box<dyn Read>, String> {
        Ok(Box::new(Cursor::new(self.0.clone())))
    }
}

fn reset() {
    se_core::offline::reset_for_tests();
    // SAFETY: serial tests.
    unsafe { std::env::remove_var(se_core::i18n::ENV_VAR) };
    se_core::i18n::set_language(None);
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn zip_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buf);
        for (name, data) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
    }
    buf.into_inner()
}

fn tar_gz_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(enc);
    for (name, data) in entries {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o755);
        h.set_cksum();
        tar.append_data(&mut h, name, *data).unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap()
}

fn archive(name: &'static str, raw: &[u8]) -> LlamaArchive {
    LlamaArchive { filename: name, url: "https://example.invalid/a", sha256: leak(hex::encode(Sha256::digest(raw))), size: raw.len() as u64 }
}

#[test]
#[serial]
fn test_llama_cache_dir_respects_env_override() {
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: serial test.
    unsafe { std::env::set_var(LLAMA_DIR_ENV_VAR, dir.path()) };
    let got = llama_cache_dir();
    unsafe { std::env::set_var(LLAMA_DIR_ENV_VAR, "   ") };
    let blank = llama_cache_dir();
    unsafe { std::env::remove_var(LLAMA_DIR_ENV_VAR) };
    assert_eq!(got, dir.path());
    assert!(blank.ends_with(Path::new("llama")));
}

#[test]
fn test_server_candidates_search_order() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    assert_eq!(llama_server_path_in(&root.join("missing")), None);
    let exe = server_binary_name();
    let c = server_candidates(root);
    assert_eq!(
        c,
        vec![
            root.join(exe),
            root.join("bin").join(exe),
            root.join("llama-b12032").join(exe),
            root.join("llama-b12032").join("bin").join(exe),
            root.join("build").join("bin").join(exe),
        ]
    );
    std::fs::create_dir_all(root.join("build/bin")).unwrap();
    std::fs::write(root.join("build/bin").join(exe), b"").unwrap();
    assert_eq!(llama_server_path_in(root), Some(root.join("build").join("bin").join(exe)));
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join("bin").join(exe), b"").unwrap();
    assert_eq!(llama_server_path_in(root), Some(root.join("bin").join(exe)));
}

#[test]
#[serial]
fn test_offline_mode_refuses_the_runtime_download() {
    reset();
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: serial test.
    unsafe { std::env::set_var(se_core::offline::OFFLINE_ENV_VAR, "1") };
    let raw = zip_with(&[]);
    let e = ensure_llama_with(&BytesOpener(raw), dir.path(), "win32", Some(&WINDOWS_ARCHIVE), None, Duration::ZERO).unwrap_err();
    unsafe { std::env::set_var(LLAMA_DIR_ENV_VAR, dir.path()) };
    let r = require_llama_download();
    unsafe { std::env::remove_var(LLAMA_DIR_ENV_VAR) };
    reset();
    assert!(e.message.starts_with("LLM çalışma zamanı (llama.cpp) indirilmemiş."), "{e}");
    assert!(e.is(se_core::ErrorKind::Model));
    let r = r.unwrap_err();
    assert!(r.is(se_core::ErrorKind::Value));
    assert_eq!(r.message, e.message);
}

#[test]
#[serial]
fn test_sha_mismatch_is_llama_unavailable() {
    reset();
    let dir = tempfile::tempdir().unwrap();
    let raw = zip_with(&[("llama-server.exe", b"x")]);
    let mut a = archive("llama-test.zip", &raw);
    a.sha256 = "deadbeef";
    let e = ensure_llama_with(&BytesOpener(raw), dir.path(), "win32", Some(&a), None, Duration::ZERO).unwrap_err();
    assert_eq!(e.message, "Yerel LLM çalışma zamanı (llama.cpp) başlatılamadı: SHA-256 mismatch for llama-test.zip");
    assert!(!dir.path().join(".llama-test.zip.tmp").exists());
}

#[test]
#[serial]
fn test_unsupported_platform_message() {
    reset();
    let dir = tempfile::tempdir().unwrap();
    let e = ensure_llama_with(&BytesOpener(vec![]), dir.path(), "freebsd", None, None, Duration::ZERO).unwrap_err();
    assert_eq!(e.message, "Yerel LLM çalışma zamanı (llama.cpp) başlatılamadı: unsupported platform freebsd");
}

#[test]
#[serial]
fn test_zip_archive_is_detected_by_content_and_extracted() {
    reset();
    let dir = tempfile::tempdir().unwrap();
    let exe = server_binary_name();
    let raw = zip_with(&[(exe, b"server"), ("ggml.dll", b"lib"), ("../escape.txt", b"no")]);
    // The name says tar.gz: detection is by content, like zipfile.is_zipfile.
    let a = archive("llama-test.tar.gz", &raw);
    let mut progress = Vec::new();
    let mut cb = |f: f64| progress.push(f);
    let server = ensure_llama_with(&BytesOpener(raw), dir.path(), "win32", Some(&a), Some(&mut cb), Duration::ZERO).unwrap();
    assert_eq!(server, dir.path().join(exe));
    assert_eq!(std::fs::read(dir.path().join("ggml.dll")).unwrap(), b"lib");
    assert!(dir.path().join("escape.txt").exists(), "zipfile strips `..` components");
    assert!(!dir.path().parent().unwrap().join("escape.txt").exists());
    assert_eq!(*progress.last().unwrap(), 1.0);
    // Already installed: returned without any download.
    let again = ensure_llama_with(&BytesOpener(vec![]), dir.path(), "win32", Some(&a), None, Duration::ZERO).unwrap();
    assert_eq!(again, server);
}

#[test]
#[serial]
fn test_tar_gz_archive_with_tagged_folder() {
    reset();
    let dir = tempfile::tempdir().unwrap();
    let exe = server_binary_name();
    let member = format!("llama-b12032/{exe}");
    let raw = tar_gz_with(&[(&member, b"server")]);
    let a = archive("llama-test.tar.gz", &raw);
    let server = ensure_llama_with(&BytesOpener(raw), dir.path(), "linux", Some(&a), None, Duration::ZERO).unwrap();
    assert_eq!(server, dir.path().join("llama-b12032").join(exe));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(std::fs::metadata(&server).unwrap().permissions().mode() & 0o111, 0);
    }
}

#[test]
#[serial]
fn test_archive_without_server_binary() {
    reset();
    let dir = tempfile::tempdir().unwrap();
    let raw = zip_with(&[("README.md", b"hi")]);
    let a = archive("llama-test.zip", &raw);
    let e = ensure_llama_with(&BytesOpener(raw), dir.path(), "win32", Some(&a), None, Duration::ZERO).unwrap_err();
    assert_eq!(e.message, "Yerel LLM çalışma zamanı (llama.cpp) başlatılamadı: llama-server binary not found in archive");
}

#[test]
fn test_tar_data_filter_rejects_escapes() {
    let dir = tempfile::tempdir().unwrap();
    let raw = {
        let enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_size(0);
        h.set_cksum();
        tar.append_link(&mut h, "evil", "../../outside").unwrap();
        tar.into_inner().unwrap().finish().unwrap()
    };
    let path = dir.path().join("a.tar.gz");
    std::fs::write(&path, raw).unwrap();
    assert!(extract_archive(&path, &dir.path().join("root")).is_err());
}

#[test]
fn test_start_server_command_line() {
    let args: Vec<String> =
        server_args(Path::new("model.gguf"), 4321, 99).into_iter().map(|a| a.to_string_lossy().to_string()).collect();
    assert_eq!(
        args,
        [
            "-m", "model.gguf", "--host", "127.0.0.1", "--port", "4321", "-ngl", "99", "-fa", "on", "-np", "1", "-c", "4096",
            "--reasoning-budget", "0"
        ]
    );
    assert!(free_local_port().unwrap() > 0);
}

#[test]
fn test_health_wait_loop() {
    // Ready after a failed probe.
    let mut calls = 0;
    let r = wait_until_healthy(Duration::from_secs(5), &mut || None, &mut || {
        calls += 1;
        if calls < 2 { Health::Failed } else { Health::Ready }
    });
    assert_eq!(r, Ok(()));
    // The process exits before it is healthy.
    let r = wait_until_healthy(Duration::from_secs(5), &mut || Some(Some(3)), &mut || Health::Failed);
    assert_eq!(r, Err(WaitError::Exited(Some(3))));
    // Deadline.
    let r = wait_until_healthy(Duration::from_millis(50), &mut || None, &mut || Health::Failed);
    assert_eq!(r, Err(WaitError::TimedOut));
}

#[test]
fn test_probe_health_against_a_local_server() {
    use std::net::TcpListener;
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        for (i, stream) in listener.incoming().take(2).enumerate() {
            let mut s = stream.unwrap();
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let reply: &[u8] = if i == 0 {
                b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            } else {
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
            };
            s.write_all(reply).unwrap();
        }
    });
    assert_eq!(probe_health(port), Health::Failed);
    assert_eq!(probe_health(port), Health::Ready);
    handle.join().unwrap();
    assert_eq!(probe_health(free_local_port().unwrap()), Health::Failed);
}

#[test]
#[serial]
fn test_early_exit_reports_stderr_tail_or_exit_code() {
    reset();
    // The test binary itself rejects llama-server's arguments and exits non-zero right away.
    let exe = std::env::current_exe().unwrap();
    let e = LlamaServer::launch(&exe, Path::new("model.gguf"), 0, true).err().expect("must fail");
    assert!(e.message.starts_with("Yerel LLM çalışma zamanı (llama.cpp) başlatılamadı: "), "{e}");
    assert!(e.message.len() > "Yerel LLM çalışma zamanı (llama.cpp) başlatılamadı: ".len());
}

#[test]
#[serial]
fn test_spawn_failure_is_llama_unavailable() {
    reset();
    let missing = PathBuf::from("Z:/definitely/missing/llama-server.exe");
    let e = LlamaServer::launch(&missing, Path::new("m.gguf"), 99, false).err().expect("must fail");
    assert!(e.message.starts_with("Yerel LLM çalışma zamanı (llama.cpp) başlatılamadı: "), "{e}");
}
