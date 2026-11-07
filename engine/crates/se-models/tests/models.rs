//! Port of `tests/test_models.py` (catalog, cache size, deletion and download progress; no network).

mod common;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::*;
use serial_test::serial;

use se_core::Result;
use se_models::catalog::{ALLOW_PATTERNS, Backend, KATALOG, MADLAD_FILES, ModelInfo, ModelKind, TRANSLATION_FILES, VAD_FILE, VAD_REPO};
use se_models::delete::{DeleteHooks, delete_model_with};
#[cfg(unix)]
use se_models::delete::sweep_shared_blobs;
use se_models::hub::Hub;
use se_models::{cached_disk_mb, delete_model, find};

fn by_id(id: &str) -> &'static ModelInfo {
    find(id).unwrap()
}

fn map(entries: &[(&str, i64)]) -> BTreeMap<String, i64> {
    entries.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

#[test]
fn test_catalog_order_and_fields() {
    let ids: Vec<&str> = KATALOG.iter().map(|m| m.id).collect();
    assert_eq!(
        ids,
        [
            "base", "gigaam-v3", "small", "parakeet-v3", "canary-1b-v2", "medium", "large-v3-turbo", "large-v3", "nllb-1.3b",
            "qwen3.5-4b", "madlad-3b", "gemma-4-e4b", "qwen3.5-9b", "gemma-4-12b", "speaker-diarization",
        ]
    );
    assert!(KATALOG.iter().all(|m| !m.repo.is_empty()));
    assert!(KATALOG.iter().all(|m| m.size_mb > 0));
    assert!(KATALOG.iter().all(|m| (1..=5).contains(&m.speed) && (1..=5).contains(&m.accuracy)));
    for kind in [ModelKind::Transcription, ModelKind::Translation] {
        let sizes: Vec<i64> = KATALOG.iter().filter(|m| m.kind == kind).map(|m| m.size_mb).collect();
        let mut sorted = sizes.clone();
        sorted.sort();
        assert_eq!(sizes, sorted);
    }
}

#[test]
fn test_catalog_backends_and_languages() {
    let whisper = ["base", "small", "medium", "large-v3-turbo", "large-v3"];
    let onnx = ["parakeet-v3", "canary-1b-v2", "gigaam-v3"];
    assert!(whisper.iter().all(|m| by_id(m).backend == Backend::Whisper && by_id(m).languages.is_none()));
    assert!(onnx.iter().all(|m| by_id(m).backend == Backend::Onnx));
    assert!(whisper.iter().chain(onnx.iter()).all(|m| by_id(m).kind == ModelKind::Transcription));
    for id in ["parakeet-v3", "canary-1b-v2"] {
        let langs = by_id(id).languages.unwrap();
        assert!(["pt", "es", "ru"].iter().all(|l| langs.contains(l)));
    }
    assert_eq!(by_id("gigaam-v3").languages, Some(&["ru"][..]));
    assert!(onnx.iter().all(|m| by_id(m).onnx_model.is_some() && by_id(m).quantization.is_some() && !by_id(m).files.is_empty()));
}

#[test]
fn test_translation_catalog_kind_languages_and_licenses() {
    for id in ["madlad-3b", "nllb-1.3b"] {
        let info = by_id(id);
        assert_eq!(info.kind, ModelKind::Translation);
        assert_eq!(info.backend, Backend::Ctranslate2);
        assert!(["tr", "pt", "es", "ru", "en"].iter().all(|l| info.languages.unwrap().contains(l)));
        assert!(TRANSLATION_FILES.iter().all(|f| info.files.contains(f)));
    }
    for id in ["qwen3.5-4b", "gemma-4-e4b", "qwen3.5-9b", "gemma-4-12b"] {
        let info = by_id(id);
        assert_eq!(info.kind, ModelKind::Translation);
        assert_eq!(info.backend, Backend::Llama);
        assert!(["tr", "pt", "es", "ru", "en"].iter().all(|l| info.languages.unwrap().contains(l)));
        assert!(info.files.len() == 1 && info.files[0].ends_with(".gguf"));
        assert!(info.owns_files_only);
        assert_eq!(info.license, "Apache-2.0");
    }
    assert_eq!(by_id("madlad-3b").files, MADLAD_FILES);
    assert!(by_id("madlad-3b").files.contains(&"spiece.model"));
    assert_eq!(by_id("madlad-3b").license, "Apache-2.0");
    assert_eq!(by_id("nllb-1.3b").license, "CC-BY-NC-4.0");
}

#[test]
fn test_catalog_licenses_are_explicit() {
    for id in ["base", "small", "medium", "large-v3", "large-v3-turbo", "gigaam-v3"] {
        assert_eq!(by_id(id).license, "MIT");
    }
    assert_eq!(by_id("parakeet-v3").license, "CC-BY-4.0");
    assert_eq!(by_id("canary-1b-v2").license, "CC-BY-4.0");
    assert!(KATALOG.iter().all(|m| !m.license.is_empty()));
}

#[test]
fn test_catalog_repos_match_faster_whisper() {
    // faster_whisper.utils._MODELS (hard-coded: the Rust engine has no faster-whisper package).
    let expected = [
        ("base", "Systran/faster-whisper-base"),
        ("small", "Systran/faster-whisper-small"),
        ("medium", "Systran/faster-whisper-medium"),
        ("large-v3", "Systran/faster-whisper-large-v3"),
        ("large-v3-turbo", "mobiuslabsgmbh/faster-whisper-large-v3-turbo"),
    ];
    let whisper: Vec<(&str, &str)> = KATALOG.iter().filter(|m| m.backend == Backend::Whisper).map(|m| (m.id, m.repo)).collect();
    assert_eq!(whisper.len(), expected.len());
    for (id, repo) in expected {
        assert!(whisper.contains(&(id, repo)), "{id}");
    }
}

#[test]
fn test_catalog_repos_match_onnx_asr() {
    let expected = [
        ("gigaam-v3-e2e-rnnt", "istupakov/gigaam-v3-onnx"),
        ("nemo-parakeet-tdt-0.6b-v3", "istupakov/parakeet-tdt-0.6b-v3-onnx"),
        ("nemo-canary-1b-v2", "istupakov/canary-1b-v2-onnx"),
    ];
    let onnx: Vec<(&str, &str)> =
        KATALOG.iter().filter(|m| m.backend == Backend::Onnx).map(|m| (m.onnx_model.unwrap(), m.repo)).collect();
    assert_eq!(onnx.len(), 3);
    for pair in expected {
        assert!(onnx.contains(&pair));
    }
}

#[test]
#[serial]
fn test_support_error_explains_unsupported_language_in_turkish() {
    reset_env();
    assert_eq!(se_models::support_error(by_id("gigaam-v3"), "tr"), "GigaAM yalnız Rusça destekler.");
    let m = se_models::support_error(by_id("parakeet-v3"), "tr");
    assert!(m.starts_with("Parakeet tr dilini desteklemiyor") && m.contains("pt"));
    let m = se_models::support_error(by_id("madlad-3b"), "ja");
    assert!(m.starts_with("MADLAD ja dilini desteklemiyor") && m.contains("es"));
}

#[test]
fn test_is_downloaded_looks_at_local_cache_only() {
    use se_models::cache::is_downloaded_by;
    let fake = |repo: &str, _name: &str| repo == "Systran/faster-whisper-small";
    assert!(is_downloaded_by("small", &fake));
    assert!(!is_downloaded_by("base", &fake));
    let needs = |id: &str| find(id).is_some() && !is_downloaded_by(id, &fake);
    assert!(needs("base"));
    assert!(!needs("small"));
    let never = |_: &str, _: &str| -> bool { panic!("outside the catalog: no cache lookup") };
    assert!(!is_downloaded_by("turbo", &never));
    assert!(!needs("turbo"));
}

#[test]
#[serial]
fn test_is_downloaded_reads_the_hf_hub_cache_env() {
    reset_env();
    let dir = tempfile::tempdir().unwrap();
    cached(dir.path(), "Systran/faster-whisper-small", &[("model.bin", 10)]);
    // SAFETY: serial test.
    unsafe { std::env::set_var("HF_HUB_CACHE", dir.path()) };
    let result = (se_models::is_downloaded("small"), se_models::is_downloaded("base"), se_models::needs_download("base"));
    let snapshot = se_models::model_dir("small");
    unsafe { std::env::remove_var("HF_HUB_CACHE") };
    assert_eq!(result, (true, false, true));
    assert_eq!(snapshot.unwrap(), repo_dir(dir.path(), "Systran/faster-whisper-small").join("snapshots").join(REV1));
}

/// A fake Hub: records the calls, reports `steps` (cumulative) per download and returns `/cache/<repo>`.
struct FakeHub {
    total: u64,
    steps: Vec<u64>,
    calls: RefCell<Vec<(String, Vec<String>)>>,
}

impl FakeHub {
    fn new(total: u64, steps: &[u64]) -> Self {
        FakeHub { total, steps: steps.to_vec(), calls: RefCell::new(Vec::new()) }
    }
}

impl Hub for FakeHub {
    fn repo_bytes(&self, _repo: &str, _patterns: &[&str], _fallback: u64) -> u64 {
        self.total
    }
    fn snapshot_download(&self, repo: &str, patterns: &[&str], on_bytes: &mut dyn FnMut(u64)) -> Result<PathBuf> {
        self.calls.borrow_mut().push((repo.to_string(), patterns.iter().map(|s| s.to_string()).collect()));
        let mut n = 0;
        for s in &self.steps {
            n += s;
            on_bytes(n);
        }
        Ok(PathBuf::from(format!("/cache/{repo}")))
    }
}

struct ForbiddenHub;
impl Hub for ForbiddenHub {
    fn repo_bytes(&self, _: &str, _: &[&str], _: u64) -> u64 {
        panic!("must not hit the network")
    }
    fn snapshot_download(&self, _: &str, _: &[&str], _: &mut dyn FnMut(u64)) -> Result<PathBuf> {
        panic!("must not download")
    }
}

fn patterns(p: &[&str]) -> Vec<String> {
    p.iter().map(|s| s.to_string()).collect()
}

#[test]
fn test_ensure_model_reports_monotonic_byte_progress() {
    let empty = tempfile::tempdir().unwrap();
    let hub = FakeHub::new(1000, &[100, 100, 200, 100]);
    let mut fractions = Vec::new();
    let mut cb = |f: f64| fractions.push(f);
    let path = se_models::download::ensure_model_with(&hub, empty.path(), "small", Some(&mut cb), Duration::ZERO).unwrap();
    assert_eq!(path, Some(PathBuf::from("/cache/Systran/faster-whisper-small")));
    assert_eq!(*hub.calls.borrow(), vec![("Systran/faster-whisper-small".to_string(), patterns(ALLOW_PATTERNS))]);
    let mut sorted = fractions.clone();
    sorted.sort_by(f64::total_cmp);
    assert_eq!(fractions, sorted);
    assert!(fractions.iter().all(|f| (0.0..=1.0).contains(f)));
    assert_eq!(*fractions.last().unwrap(), 1.0);
    assert!(fractions[0] < 1.0);
    assert_eq!(fractions, vec![0.1, 0.2, 0.4, 0.5, 1.0]);
}

#[test]
fn test_progress_is_throttled_to_the_interval() {
    let empty = tempfile::tempdir().unwrap();
    let hub = FakeHub::new(1000, &[100, 100, 200, 100]);
    let mut fractions = Vec::new();
    let mut cb = |f: f64| fractions.push(f);
    se_models::download::ensure_model_with(&hub, empty.path(), "small", Some(&mut cb), Duration::from_secs(60)).unwrap();
    assert_eq!(fractions, vec![0.1, 1.0]); // the first report always passes, then only the final 1.0
}

#[test]
fn test_ensure_model_returns_immediately_when_cached() {
    let dir = tempfile::tempdir().unwrap();
    cached(dir.path(), "Systran/faster-whisper-base", &[("model.bin", 10)]);
    let mut cb = |_: f64| panic!("no progress may be reported");
    assert_eq!(se_models::download::ensure_model_with(&ForbiddenHub, dir.path(), "base", Some(&mut cb), Duration::ZERO).unwrap(), None);
}

#[test]
fn test_ensure_model_leaves_unknown_models_to_faster_whisper() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(se_models::download::ensure_model_with(&ForbiddenHub, dir.path(), "turbo", None, Duration::ZERO).unwrap(), None);
}

#[test]
fn test_translation_download_state_requires_all_files() {
    use se_models::cache::is_downloaded_by;
    let info = by_id("madlad-3b");
    let present = RefCell::new(info.files.iter().map(|n| (info.repo.to_string(), n.to_string())).collect::<Vec<_>>());
    let lookup = |repo: &str, name: &str| present.borrow().contains(&(repo.to_string(), name.to_string()));
    assert!(is_downloaded_by("madlad-3b", &lookup));
    present.borrow_mut().retain(|(_, n)| n != "tokenizer.json");
    assert!(!is_downloaded_by("madlad-3b", &lookup));
}

#[test]
fn test_translation_download_state_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let info = by_id("madlad-3b");
    let files: Vec<(&str, usize)> = info.files.iter().map(|n| (*n, 3)).collect();
    let repo = cached(dir.path(), info.repo, &files);
    assert!(se_models::cache::is_downloaded_in(dir.path(), "madlad-3b"));
    fs::remove_file(repo.join("snapshots").join(REV1).join("tokenizer.json")).unwrap();
    assert!(!se_models::cache::is_downloaded_in(dir.path(), "madlad-3b"));
}

#[test]
fn test_ensure_translation_model_downloads_only_its_files() {
    let empty = tempfile::tempdir().unwrap();
    let info = by_id("nllb-1.3b");
    let hub = FakeHub::new(100, &[100]);
    let mut fractions = Vec::new();
    let mut cb = |f: f64| fractions.push(f);
    let path = se_models::download::ensure_model_with(&hub, empty.path(), "nllb-1.3b", Some(&mut cb), Duration::ZERO).unwrap();
    assert_eq!(path, Some(PathBuf::from(format!("/cache/{}", info.repo))));
    assert_eq!(*hub.calls.borrow(), vec![(info.repo.to_string(), patterns(info.files))]);
    assert_eq!(*fractions.last().unwrap(), 1.0);
    assert!(fractions.windows(2).all(|w| w[0] <= w[1]));
}

#[test]
fn test_onnx_download_state_checks_catalog_files_and_vad() {
    use se_models::cache::is_downloaded_by;
    let info = by_id("gigaam-v3");
    let mut entries: Vec<(String, String)> = info.files.iter().map(|n| (info.repo.to_string(), n.to_string())).collect();
    entries.push((VAD_REPO.to_string(), VAD_FILE.to_string()));
    let present = RefCell::new(entries);
    let lookup = |repo: &str, name: &str| present.borrow().contains(&(repo.to_string(), name.to_string()));
    assert!(is_downloaded_by("gigaam-v3", &lookup));
    let last = info.files[info.files.len() - 1];
    present.borrow_mut().retain(|(_, n)| n != last);
    assert!(!is_downloaded_by("gigaam-v3", &lookup));
}

#[test]
fn test_ensure_model_onnx_downloads_model_files_and_shared_vad() {
    let empty = tempfile::tempdir().unwrap();
    let info = by_id("parakeet-v3");
    let hub = FakeHub::new(100, &[100]);
    let mut fractions = Vec::new();
    let mut cb = |f: f64| fractions.push(f);
    let path = se_models::download::ensure_model_with(&hub, empty.path(), "parakeet-v3", Some(&mut cb), Duration::ZERO).unwrap();
    assert_eq!(path, Some(PathBuf::from(format!("/cache/{}", info.repo))));
    assert_eq!(
        *hub.calls.borrow(),
        vec![(info.repo.to_string(), patterns(info.files)), (VAD_REPO.to_string(), vec![VAD_FILE.to_string()])]
    );
    assert!(fractions.windows(2).all(|w| w[0] <= w[1]));
    assert_eq!(*fractions.last().unwrap(), 1.0);
}

#[test]
fn test_ensure_model_onnx_only_vad_missing_returns_none() {
    let dir = tempfile::tempdir().unwrap();
    let info = by_id("gigaam-v3");
    let files: Vec<(&str, usize)> = info.files.iter().map(|n| (*n, 1)).collect();
    cached(dir.path(), info.repo, &files);
    let hub = FakeHub::new(100, &[100]);
    let path = se_models::download::ensure_model_with(&hub, dir.path(), "gigaam-v3", None, Duration::ZERO).unwrap();
    assert_eq!(path, None);
    assert_eq!(*hub.calls.borrow(), vec![(VAD_REPO.to_string(), vec![VAD_FILE.to_string()])]);
}

#[test]
fn test_cached_disk_mb_reads_real_repo_sizes() {
    let dir = tempfile::tempdir().unwrap();
    cached(dir.path(), "Systran/faster-whisper-base", &[("model.bin", 2 * MIB + 600_000), ("config.json", 1000)]);
    cached(dir.path(), VAD_REPO, &[(VAD_FILE, 2_327_554)]);
    assert_eq!(cached_disk_mb(Some(dir.path())), map(&[("Systran/faster-whisper-base", 3), (VAD_REPO, 2)]));
}

#[test]
fn test_disk_mb_is_null_for_models_not_downloaded() {
    let dir = tempfile::tempdir().unwrap();
    cached(dir.path(), "Systran/faster-whisper-base", &[("model.bin", 2 * MIB)]);
    let sizes = cached_disk_mb(Some(dir.path()));
    let downloaded = |id: &str| id == "base";
    assert_eq!(se_models::cache::disk_mb_by("base", Some(&sizes), &downloaded), Some(2));
    assert_eq!(se_models::cache::disk_mb_by("small", Some(&sizes), &downloaded), None);
    assert_eq!(se_models::cache::disk_mb_by("turbo", Some(&sizes), &downloaded), None);
}

#[test]
fn test_cached_disk_mb_is_empty_without_cache_dir() {
    let dir = tempfile::tempdir().unwrap();
    assert!(cached_disk_mb(Some(&dir.path().join("yok"))).is_empty());
}

#[test]
fn test_delete_model_removes_all_revisions_and_keeps_other_repos() {
    let dir = tempfile::tempdir().unwrap();
    let c = dir.path();
    let target = cached(c, "istupakov/gigaam-v3-onnx", &[("encoder.onnx", 400_000)]);
    make_cached_repo(c, "istupakov/gigaam-v3-onnx", &[("encoder.onnx", 600_000)], REV2, Layout::Auto).unwrap();
    let other = cached(c, "Systran/faster-whisper-small", &[("model.bin", 900_000)]);
    let vad = cached(c, VAD_REPO, &[(VAD_FILE, 2_327_554)]);

    assert_eq!(delete_model("gigaam-v3", Some(c)).unwrap(), 1);

    assert!(!target.exists());
    assert!(other.is_dir() && vad.is_dir());
    assert_eq!(cached_disk_mb(Some(c)), map(&[("Systran/faster-whisper-small", 1), (VAD_REPO, 2)]));
}

#[test]
fn test_delete_model_cleans_up_repo_without_revisions() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("models--Systran--faster-whisper-base");
    fs::create_dir_all(repo.join("snapshots")).unwrap();
    fs::create_dir_all(repo.join("blobs")).unwrap();
    fs::write(repo.join("blobs").join("partial.incomplete"), vec![b'x'; 1000]).unwrap();
    assert_eq!(delete_model("base", Some(dir.path())).unwrap(), 0);
    assert!(!repo.exists());
}

#[test]
#[serial]
fn test_delete_model_errors_are_turkish() {
    reset_env();
    let dir = tempfile::tempdir().unwrap();
    let e = delete_model("turbo", Some(dir.path())).unwrap_err();
    assert!(e.message.contains("Bilinmeyen model: turbo"), "{e}");
    assert!(e.is(se_core::ErrorKind::Model));
    let e = delete_model("small", Some(dir.path())).unwrap_err();
    assert!(e.message.contains("indirilmemiş"), "{e}");
    let e = delete_model("small", Some(&dir.path().join("yok"))).unwrap_err();
    assert_eq!(e.message, "small modeli indirilmemiş; silinecek dosya yok.");
}

#[test]
fn test_delete_model_removes_translation_repo() {
    let dir = tempfile::tempdir().unwrap();
    let target = cached(dir.path(), "OpenNMT/nllb-200-distilled-1.3B-ct2-int8", &[("model.bin", 1_000_000)]);
    let other = cached(dir.path(), "Systran/faster-whisper-small", &[("model.bin", 900_000)]);
    assert_eq!(delete_model("nllb-1.3b", Some(dir.path())).unwrap(), 1);
    assert!(!target.exists() && other.is_dir());
}

#[test]
fn test_translation_model_dir_returns_cached_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let repo = cached(dir.path(), "Nextcloud-AI/madlad400-3b-mt-ct2-int8", &[("model.bin", 1000)]);
    let snapshot = repo.join("snapshots").join(REV1);
    assert_eq!(se_models::cache::model_dir_in(dir.path(), "madlad-3b").unwrap(), snapshot);
}

#[test]
#[serial]
fn test_model_dir_and_file_errors() {
    reset_env();
    let dir = tempfile::tempdir().unwrap();
    let e = se_models::cache::model_dir_in(dir.path(), "madlad-3b").unwrap_err();
    assert_eq!(e.message, "madlad-3b modeli indirilmemiş; önce indirilmesi gerekiyor.");
    let e = se_models::cache::model_file_in(dir.path(), "nope").unwrap_err();
    assert!(e.message.starts_with("Bilinmeyen model: nope."));
    cached(dir.path(), "unsloth/Qwen3.5-4B-GGUF", &[("Qwen3.5-4B-Q4_K_M.gguf", 10)]);
    let f = se_models::cache::model_file_in(dir.path(), "qwen3.5-4b").unwrap();
    assert!(f.ends_with(Path::new("snapshots").join(REV1).join("Qwen3.5-4B-Q4_K_M.gguf")));
}

#[test]
#[serial]
fn test_delete_model_refuses_catalog_entries_sharing_a_repo() {
    reset_env();
    let dir = tempfile::tempdir().unwrap();
    let repo = cached(dir.path(), "Systran/faster-whisper-base", &[("model.bin", 1000)]);
    let mut catalog: Vec<ModelInfo> = KATALOG.to_vec();
    catalog.push(ModelInfo { id: "base-kopya", size_mb: 1, speed: 1, accuracy: 1, ..*by_id("base") });
    let hooks = DeleteHooks { catalog: &catalog, ..DeleteHooks::default() };
    let e = delete_model_with("base", dir.path(), &hooks).unwrap_err();
    assert!(e.message.contains("paylaşıyor"), "{e}");
    assert!(repo.join("blobs").is_dir());
}

#[test]
fn test_disk_mb_counts_shared_store_targets_once() {
    let dir = tempfile::tempdir().unwrap();
    let files = [("model.bin", "a", 3 * MIB), ("alias.bin", "a", 3 * MIB), ("config.json", "b", 1000)];
    if make_shared_repo(dir.path(), "Systran/faster-whisper-base", &files, REV1).is_none() {
        return;
    }
    make_shared_repo(dir.path(), "Systran/faster-whisper-base", &files, REV2).unwrap();
    assert_eq!(cached_disk_mb(Some(dir.path())), map(&[("Systran/faster-whisper-base", 3)]));
}

#[test]
fn test_delete_model_sweeps_shared_blobs_but_keeps_ones_other_repos_use() {
    let dir = tempfile::tempdir().unwrap();
    let c = dir.path();
    let Some((repo, store)) = make_shared_repo(
        c,
        "Systran/faster-whisper-base",
        &[("model.bin", "only-base", 3 * MIB), ("tokenizer.json", "common", 2 * MIB)],
        REV1,
    ) else {
        return;
    };
    let (other, _) = make_shared_repo(
        c,
        "Systran/faster-whisper-small",
        &[("model.bin", "only-small", 2 * MIB), ("tokenizer.json", "common", 2 * MIB)],
        REV1,
    )
    .unwrap();
    let (vad, _) = make_shared_repo(c, VAD_REPO, &[(VAD_FILE, "vad", MIB)], REV1).unwrap();

    assert_eq!(delete_model("base", Some(c)).unwrap(), 3);

    assert!(!repo.exists());
    let only_base = payload(&store, "only-base");
    assert!(!only_base.exists());
    assert!(!only_base.with_file_name(format!("{}.refs", sha("only-base"))).exists());
    assert!(only_base.with_file_name(format!("{}.lock", sha("only-base"))).exists());
    assert!(payload(&store, "common").is_file() && payload(&store, "only-small").is_file() && payload(&store, "vad").is_file());
    assert!(other.is_dir() && vad.is_dir());
    assert_eq!(fs::read(other.join("snapshots").join(REV1).join("tokenizer.json")).unwrap(), vec![b'x'; 2 * MIB]);
}

#[test]
fn test_delete_model_frees_shared_blob_when_last_user_is_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let c = dir.path();
    if make_shared_repo(c, "Systran/faster-whisper-base", &[("model.bin", "common", 2 * MIB)], REV1).is_none() {
        return;
    }
    make_shared_repo(c, "Systran/faster-whisper-small", &[("model.bin", "common", 2 * MIB)], REV1).unwrap();
    assert_eq!(delete_model("base", Some(c)).unwrap(), 0);
    assert_eq!(delete_model("small", Some(c)).unwrap(), 2);
    let left: Vec<PathBuf> = files_below(&c.join("blobs"))
        .into_iter()
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            p.is_file() && !name.contains('.') && !name.starts_with('.')
        })
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn test_delete_model_leaks_shared_blob_without_store_marker() {
    let dir = tempfile::tempdir().unwrap();
    let Some((repo, store)) = make_shared_repo(dir.path(), "Systran/faster-whisper-base", &[("model.bin", "a", MIB)], REV1) else {
        return;
    };
    fs::remove_file(store.join(".huggingface-shared-blobs")).unwrap();
    assert_eq!(delete_model("base", Some(dir.path())).unwrap(), 0);
    assert!(!repo.exists());
    assert!(files_below(&store).iter().any(|p| p.is_file() && p.file_name().unwrap().len() == 64));
}

#[test]
fn test_delete_model_legacy_layout_counts_repo_blobs() {
    let dir = tempfile::tempdir().unwrap();
    let repo = cached(dir.path(), "Systran/faster-whisper-base", &[("model.bin", 3 * MIB)]);
    assert_eq!(delete_model("base", Some(dir.path())).unwrap(), 3);
    assert!(!repo.exists());
}

/// `sweep_mode` fixture: HF's locked sweep and the engine's fallback.
fn sweep_modes() -> [bool; 2] {
    [true, false]
}

fn delete_with_sweep(id: &str, cache: &Path, hf_sweep: bool) -> Result<i64> {
    delete_model_with(id, cache, &DeleteHooks { hf_sweep, ..DeleteHooks::default() })
}

#[test]
fn test_sweep_both_paths_delete_unshared() {
    for hf in sweep_modes() {
        let dir = tempfile::tempdir().unwrap();
        let Some((_, store)) = make_shared_repo(dir.path(), "Systran/faster-whisper-base", &[("model.bin", "a", 2 * MIB)], REV1) else {
            return;
        };
        assert_eq!(delete_with_sweep("base", dir.path(), hf).unwrap(), 2);
        assert!(!payload(&store, "a").exists());
    }
}

#[test]
fn test_link_pointing_outside_cache_is_not_deleted() {
    for hf in sweep_modes() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache");
        fs::create_dir_all(&cache).unwrap();
        let outside = dir.path().join("disari.bin");
        fs::write(&outside, vec![b'x'; MIB]).unwrap();
        let Some((repo, _)) = make_shared_repo(&cache, "Systran/faster-whisper-base", &[("model.bin", "a", MIB)], REV1) else {
            return;
        };
        se_models::fsx::symlink_file(&outside, &repo.join("blobs").join("abs")).unwrap();
        let rel = se_models::fsx::relpath(&outside, &repo.join("blobs")).unwrap();
        se_models::fsx::symlink_file(&rel, &repo.join("blobs").join("rel")).unwrap();
        let _ = delete_with_sweep("base", &cache, hf);
        assert_eq!(fs::read(&outside).unwrap(), vec![b'x'; MIB]);
    }
}

#[test]
fn test_reference_from_dataset_folder_keeps_blob() {
    for hf in sweep_modes() {
        let dir = tempfile::tempdir().unwrap();
        let c = dir.path();
        let Some((_, store)) = make_shared_repo(c, "Systran/faster-whisper-base", &[("model.bin", "a", MIB)], REV1) else {
            return;
        };
        let d = sha("a");
        let ds = c.join("datasets--a--b").join("blobs");
        fs::create_dir_all(&ds).unwrap();
        let rel = se_models::fsx::relpath(&payload(&store, "a"), &ds).unwrap();
        se_models::fsx::symlink_file(&rel, &ds.join(&d)).unwrap();
        let refs_path = store.join(&d[..2]).join(format!("{d}.refs"));
        let mut refs = fs::read_to_string(&refs_path).unwrap();
        refs.push_str(&format!("datasets--a--b/blobs/{d}\n"));
        fs::write(&refs_path, refs).unwrap();
        assert_eq!(delete_with_sweep("base", c, hf).unwrap(), 0);
        assert!(payload(&store, "a").is_file());
    }
}

#[cfg(unix)]
#[test]
fn test_unreadable_other_repo_blocks_deletion() {
    use std::os::unix::fs::PermissionsExt;
    for hf in sweep_modes() {
        let dir = tempfile::tempdir().unwrap();
        let c = dir.path();
        let Some((_, store)) = make_shared_repo(c, "Systran/faster-whisper-base", &[("model.bin", "a", MIB)], REV1) else {
            return;
        };
        let (other, _) = make_shared_repo(c, "Systran/faster-whisper-small", &[("model.bin", "b", MIB)], REV1).unwrap();
        let blobs = other.join("blobs");
        fs::set_permissions(&blobs, fs::Permissions::from_mode(0)).unwrap();
        if fs::read_dir(&blobs).is_ok() {
            fs::set_permissions(&blobs, fs::Permissions::from_mode(0o755)).unwrap();
            eprintln!("skipped: permissions not enforced (root)");
            return;
        }
        let _ = delete_with_sweep("base", c, hf);
        let root = se_core::paths::resolve(c);
        assert_eq!(sweep_shared_blobs(&root, &[payload(&root.join("blobs"), "a")], hf), 0);
        fs::set_permissions(&blobs, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(payload(&store, "a").is_file());
    }
}

#[test]
fn test_marker_content_mismatch_deletes_nothing() {
    for hf in sweep_modes() {
        for content in ["2\n", "1", ""] {
            let dir = tempfile::tempdir().unwrap();
            let Some((_, store)) = make_shared_repo(dir.path(), "Systran/faster-whisper-base", &[("model.bin", "a", MIB)], REV1) else {
                return;
            };
            fs::write(store.join(".huggingface-shared-blobs"), content).unwrap();
            assert_eq!(delete_with_sweep("base", dir.path(), hf).unwrap(), 0);
            assert!(payload(&store, "a").is_file());
        }
    }
}

#[test]
fn test_marker_symlink_deletes_nothing() {
    for hf in sweep_modes() {
        let dir = tempfile::tempdir().unwrap();
        let Some((_, store)) = make_shared_repo(dir.path(), "Systran/faster-whisper-base", &[("model.bin", "a", MIB)], REV1) else {
            return;
        };
        let real = dir.path().join("gercek");
        fs::write(&real, "1\n").unwrap();
        fs::remove_file(store.join(".huggingface-shared-blobs")).unwrap();
        se_models::fsx::symlink_file(&real, &store.join(".huggingface-shared-blobs")).unwrap();
        assert_eq!(delete_with_sweep("base", dir.path(), hf).unwrap(), 0);
        assert!(payload(&store, "a").is_file());
    }
}

#[test]
fn test_regular_repo_blob_with_shared_store_present() {
    for hf in sweep_modes() {
        let dir = tempfile::tempdir().unwrap();
        let Some((repo, store)) = make_shared_repo(dir.path(), "Systran/faster-whisper-base", &[("model.bin", "a", 2 * MIB)], REV1) else {
            return;
        };
        fs::write(repo.join("blobs").join("f".repeat(40)), vec![b'x'; 3 * MIB]).unwrap();
        assert_eq!(delete_with_sweep("base", dir.path(), hf).unwrap(), 5);
        assert!(!repo.exists() && !payload(&store, "a").exists());
    }
}

#[test]
#[serial]
fn test_delete_failure_raises_turkish_error() {
    reset_env();
    let dir = tempfile::tempdir().unwrap();
    // Python runs this with the shared layout (skipped without symlinks); the copy layout exercises the same
    // `delete_incomplete` path everywhere.
    let shared = make_shared_repo(dir.path(), "Systran/faster-whisper-base", &[("model.bin", "a", MIB)], REV1);
    if shared.is_none() {
        cached(dir.path(), "Systran/faster-whisper-base", &[("model.bin", MIB)]);
    }
    let noop = |_: &Path, _: &Path, _: &str| {};
    let hooks = DeleteHooks { remove_leftovers: &noop, ..DeleteHooks::default() };
    let e = delete_model_with("base", dir.path(), &hooks).unwrap_err();
    assert!(e.message.contains("tamamen silinemedi"), "{e}");
    assert!(e.message.starts_with("base modeli kısmen silindi (0 MB boşaldı); kalan: Model dosyaları tamamen silinemedi: "));
    if let Some((_, store)) = shared {
        assert!(payload(&store, "a").is_file());
    }
}

#[test]
fn test_copy_layout_disk_usage_and_detection() {
    for layout in [Layout::Copy, Layout::CopyDup] {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_cached_repo(dir.path(), "Systran/faster-whisper-base", &[("model.bin", 3 * MIB), ("config.json", 1000)], REV1, layout)
            .unwrap();
        assert!(!se_models::fsx::walk_all(&repo).iter().any(|p| se_models::fsx::is_symlink(p)));
        assert_eq!(cached_disk_mb(Some(dir.path())), map(&[("Systran/faster-whisper-base", 3)]));
        assert!(se_models::cache::is_downloaded_in(dir.path(), "base"));
    }
}

#[test]
fn test_copy_layout_delete_counts_snapshot_files() {
    for (layout, expected) in [(Layout::Copy, 5), (Layout::CopyDup, 10)] {
        let dir = tempfile::tempdir().unwrap();
        let c = dir.path();
        let repo = make_cached_repo(c, "Systran/faster-whisper-base", &[("model.bin", 3 * MIB)], REV1, layout).unwrap();
        make_cached_repo(c, "Systran/faster-whisper-base", &[("model.bin", 2 * MIB)], REV2, layout).unwrap();
        let other = make_cached_repo(c, "Systran/faster-whisper-small", &[("model.bin", MIB)], REV1, layout).unwrap();
        assert_eq!(delete_model("base", Some(c)).unwrap(), expected);
        assert!(!repo.exists());
        assert!(other.is_dir());
    }
}

#[test]
fn test_copy_layout_ignores_shared_store_and_keeps_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("blobs");
    fs::create_dir_all(store.join("ab")).unwrap();
    fs::write(store.join(".huggingface-shared-blobs"), "1\n").unwrap();
    let p = store.join("ab").join(format!("ab{}", "0".repeat(62)));
    fs::write(&p, vec![b'x'; MIB]).unwrap();
    make_cached_repo(dir.path(), "Systran/faster-whisper-base", &[("model.bin", 2 * MIB)], REV1, Layout::Copy).unwrap();
    assert_eq!(delete_model("base", Some(dir.path())).unwrap(), 2);
    assert!(p.is_file());
}

// --- scanner edge cases (spec §8) -------------------------------------------------------------------

#[test]
fn test_ref_with_trailing_newline_hides_the_repo() {
    let dir = tempfile::tempdir().unwrap();
    let repo = cached(dir.path(), "Systran/faster-whisper-base", &[("model.bin", MIB)]);
    fs::write(repo.join("refs").join("main"), format!("{REV1}\n")).unwrap();
    assert!(cached_disk_mb(Some(dir.path())).is_empty());
    assert!(!se_models::cache::is_downloaded_in(dir.path(), "base"));
}

#[test]
fn test_no_exist_marker_counts_as_not_cached() {
    let dir = tempfile::tempdir().unwrap();
    let repo = cached(dir.path(), "Systran/faster-whisper-base", &[("model.bin", 10)]);
    assert!(se_models::cache::is_downloaded_in(dir.path(), "base"));
    let marker = repo.join(".no_exist").join(REV1).join("model.bin");
    fs::create_dir_all(marker.parent().unwrap()).unwrap();
    fs::write(&marker, b"").unwrap();
    assert!(!se_models::cache::is_downloaded_in(dir.path(), "base"));
}

#[test]
fn test_missing_refs_means_not_downloaded_but_still_measured() {
    let dir = tempfile::tempdir().unwrap();
    let repo = cached(dir.path(), "Systran/faster-whisper-base", &[("model.bin", 2 * MIB)]);
    fs::remove_file(repo.join("refs").join("main")).unwrap();
    assert!(!se_models::cache::is_downloaded_in(dir.path(), "base"));
    assert_eq!(cached_disk_mb(Some(dir.path())), map(&[("Systran/faster-whisper-base", 2)]));
}

#[test]
fn test_mb_rounds_half_to_even() {
    use se_models::cache::mb;
    assert_eq!(mb(5 * MIB as u64 / 2), 2);
    assert_eq!(mb(7 * MIB as u64 / 2), 4);
    assert_eq!(mb(1000), 0);
}

#[test]
fn test_reads_python_written_cache_when_present() {
    // The real cache (if any) must be readable without panicking; nothing is modified.
    let real = se_models::cache::hub_cache_dir();
    if real.is_dir() {
        let _ = cached_disk_mb(Some(&real));
    }
}

/// Live download (network): the silero VAD into a temp cache, then the same layout checks as the Python
/// cache. Run with `cargo test -p se-models -- --ignored live`.
#[test]
#[ignore]
#[serial]
fn live_download_vad_into_temp_cache() {
    reset_env();
    let dir = tempfile::tempdir().unwrap();
    let hub = se_models::hub::HfHub { cache_dir: dir.path().to_path_buf(), endpoint: se_models::cache::hf_endpoint() };
    let mut seen = Vec::new();
    let mut on_bytes = |n: u64| seen.push(n);
    let folder = hub.snapshot_download(VAD_REPO, &[VAD_FILE], &mut on_bytes).unwrap();
    assert!(folder.join(VAD_FILE).is_file());
    assert!(se_models::cache::try_to_load_from_cache_in(dir.path(), VAD_REPO, VAD_FILE).is_some());
    assert!(dir.path().join("CACHEDIR.TAG").is_file());
    let repo = repo_dir(dir.path(), VAD_REPO);
    let refs = fs::read_to_string(repo.join("refs").join("main")).unwrap();
    assert_eq!(refs.len(), 40);
    assert_eq!(folder, repo.join("snapshots").join(&refs));
    assert!(!seen.is_empty());
    let sizes = cached_disk_mb(Some(dir.path()));
    assert_eq!(sizes.get(VAD_REPO), Some(&2));
    assert!(hub.repo_bytes(VAD_REPO, &[VAD_FILE], 1) > 1_000_000);
    // A second call downloads nothing.
    let mut again = Vec::new();
    hub.snapshot_download(VAD_REPO, &[VAD_FILE], &mut |n| again.push(n)).unwrap();
    assert!(again.is_empty());
    eprintln!("{:?}", files_below(&repo));
}

/// Live: downloads gigaam-v3's model files + VAD with `ensure_model` into `$SE_MODELS_KEEP_DIR` (kept, for
/// cross-checking with the Python engine). Skipped when the variable is unset.
#[test]
#[ignore]
#[serial]
fn live_ensure_model_keep() {
    reset_env();
    let Ok(dir) = std::env::var("SE_MODELS_KEEP_DIR") else { return };
    let dir = PathBuf::from(dir);
    let hub = se_models::hub::HfHub { cache_dir: dir.clone(), endpoint: se_models::cache::hf_endpoint() };
    let mut fractions = Vec::new();
    let mut cb = |f: f64| fractions.push(f);
    let path = se_models::download::ensure_model_with(&hub, &dir, "speaker-diarization", Some(&mut cb), Duration::from_millis(100)).unwrap();
    eprintln!("{path:?} {fractions:?}");
    assert!(se_models::cache::is_downloaded_in(&dir, "speaker-diarization"));
    assert_eq!(*fractions.last().unwrap(), 1.0);
}

/// Read-only report of the real cache (`HF_HUB_CACHE` or the default), for comparing with the Python engine.
#[test]
#[ignore]
fn parity_real_cache_report() {
    let sizes = cached_disk_mb(None);
    for (k, v) in &sizes {
        println!("SIZE {k} {v}");
    }
    for m in KATALOG {
        println!("MODEL {} {} {:?}", m.id, se_models::is_downloaded(m.id), se_models::disk_mb(m.id, Some(&sizes)));
    }
}

/// Deletes speaker-diarization from `$SE_MODELS_KEEP_DIR` (e.g. a cache the Python engine downloaded).
#[test]
#[ignore]
#[serial]
fn live_delete_keep() {
    let Ok(dir) = std::env::var("SE_MODELS_KEEP_DIR") else { return };
    let dir = PathBuf::from(dir);
    assert!(se_models::cache::is_downloaded_in(&dir, "speaker-diarization"));
    println!("FREED {}", delete_model("speaker-diarization", Some(&dir)).unwrap());
    assert!(!se_models::cache::is_downloaded_in(&dir, "speaker-diarization"));
}

#[test]
#[serial]
fn test_go_offline_blocks_hub_requests() {
    reset_env();
    let dir = tempfile::tempdir().unwrap();
    se_core::offline::go_offline();
    let hub = se_models::hub::HfHub { cache_dir: dir.path().to_path_buf(), endpoint: "http://127.0.0.1:9".to_string() };
    let err = hub.snapshot_download(VAD_REPO, &[VAD_FILE], &mut |_| {}).unwrap_err();
    let fallback = hub.repo_bytes(VAD_REPO, &[VAD_FILE], 77);
    let ensure = se_models::download::ensure_model_with(&hub, dir.path(), "base", None, Duration::ZERO).unwrap_err();
    reset_env();
    assert!(err.message.contains("set 'HF_HUB_OFFLINE=0'"), "{err}");
    assert_eq!(fallback, 77);
    assert_eq!(ensure.message, err.message);
    assert!(!dir.path().join(format!("models--{}", VAD_REPO.replace('/', "--"))).exists());
}

#[test]
#[serial]
fn test_strict_offline_message_is_turkish_by_default() {
    reset_env();
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: serial test.
    unsafe {
        std::env::set_var("HF_HUB_CACHE", dir.path());
        std::env::set_var(se_core::offline::OFFLINE_ENV_VAR, "1");
    }
    let missing = se_models::require_download("small");
    let unknown = se_models::require_download("turbo");
    unsafe { std::env::remove_var("HF_HUB_CACHE") };
    reset_env();
    let e = missing.unwrap_err();
    assert!(e.is(se_core::ErrorKind::Value));
    assert!(e.message.contains("Çevrimdışı mod"), "{e}");
    assert_eq!(unknown.unwrap(), false);
}
