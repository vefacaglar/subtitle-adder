//! The model-catalog parts of `tests/test_diarize.py` (two-repo diarization model: owned files, disk usage,
//! deletion, partial deletes, fallback sizes).

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use serial_test::serial;

use se_models::catalog::ModelInfo;
use se_models::delete::{DeleteFailure, DeleteHooks, delete_model_with};
use se_models::{cached_disk_mb, delete_model, find};

const MODEL_ID: &str = "speaker-diarization";

fn info() -> &'static ModelInfo {
    find(MODEL_ID).unwrap()
}

fn emb() -> (&'static str, &'static str) {
    (info().extra[0].0, info().extra[0].1[0])
}

fn make_two_repo_cache(cache: &Path) {
    cached(cache, info().repo, &[(info().files[0], MIB)]);
    cached(cache, emb().0, &[(emb().1, 2 * MIB)]);
}

fn no_repo_folders(cache: &Path) -> bool {
    !fs::read_dir(cache).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with("models--"))
}

#[test]
fn test_diarization_model_delete_removes_both_repos() {
    let dir = tempfile::tempdir().unwrap();
    make_two_repo_cache(dir.path());
    assert_eq!(delete_model(MODEL_ID, Some(dir.path())).unwrap(), 3);
    assert!(no_repo_folders(dir.path()));
}

#[test]
fn test_diarization_model_disk_usage_sums_both_repos() {
    let seg_key = format!("{}#{}", info().repo, info().files[0]);
    let emb_key = format!("{}#{}", emb().0, emb().1);
    let yes = |_: &str| true;
    let sizes: BTreeMap<String, i64> = [(seg_key.clone(), 1), (emb_key, 28), (emb().0.to_string(), 5000)].into_iter().collect();
    assert_eq!(se_models::cache::disk_mb_by(MODEL_ID, Some(&sizes), &yes), Some(29));
    let one: BTreeMap<String, i64> = [(seg_key, 1)].into_iter().collect();
    assert_eq!(se_models::cache::disk_mb_by(MODEL_ID, Some(&one), &yes), None);
}

#[test]
fn test_diarization_model_is_downloaded_needs_both_repos() {
    use se_models::cache::is_downloaded_by;
    let main_only = |repo: &str, _: &str| repo == info().repo;
    assert!(!is_downloaded_by(MODEL_ID, &main_only));
    assert!(is_downloaded_by(MODEL_ID, &|_: &str, _: &str| true));
}

/// `owned_cache(tmp_path, shared, layout)`: `None` when the requested layout needs unavailable symlinks.
fn owned_cache(cache: &Path, shared: bool, layout: Layout) -> Option<(PathBuf, PathBuf, Option<PathBuf>)> {
    let (emb_repo, emb_file) = emb();
    if shared {
        let (seg, store) = make_shared_repo(cache, info().repo, &[(info().files[0], "seg", MIB), ("model.onnx", "seg-fp32", 4 * MIB)], REV1)?;
        let (e, _) = make_shared_repo(cache, emb_repo, &[(emb_file, "emb", 2 * MIB), ("other-model.onnx", "other", 5 * MIB)], REV1)?;
        return Some((seg, e, Some(store)));
    }
    let seg = make_cached_repo(cache, info().repo, &[(info().files[0], MIB), ("model.onnx", 4 * MIB)], REV1, layout)?;
    let e = make_cached_repo(cache, emb_repo, &[(emb_file, 2 * MIB), ("other-model.onnx", 5 * MIB)], REV1, layout)?;
    Some((seg, e, None))
}

#[test]
fn test_disk_usage_counts_only_owned_files() {
    let dir = tempfile::tempdir().unwrap();
    owned_cache(dir.path(), false, Layout::Auto).unwrap();
    let sizes = cached_disk_mb(Some(dir.path()));
    assert_eq!(sizes[emb().0], 7);
    assert_eq!(sizes[&format!("{}#{}", emb().0, emb().1)], 2);
    assert_eq!(sizes[&format!("{}#{}", info().repo, info().files[0])], 1);
}

#[test]
fn test_delete_only_removes_owned_files_and_keeps_foreign_ones() {
    for shared in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let Some((seg, e, store)) = owned_cache(dir.path(), shared, Layout::Auto) else { continue };
        assert_eq!(delete_model(MODEL_ID, Some(dir.path())).unwrap(), 3);
        assert_eq!(snapshot_names(&seg), vec!["model.onnx"]);
        assert_eq!(snapshot_names(&e), vec!["other-model.onnx"]);
        assert_eq!(fs::read(e.join("snapshots").join(REV1).join("other-model.onnx")).unwrap(), vec![b'x'; 5 * MIB]);
        if let Some(store) = store {
            let left = files_below(&store).into_iter().filter(|p| p.is_file() && p.file_name().unwrap().len() == 64).count();
            assert_eq!(left, 2);
        }
    }
}

#[test]
fn test_delete_owned_files_in_copy_layout() {
    let dir = tempfile::tempdir().unwrap();
    let (seg, e, _) = owned_cache(dir.path(), false, Layout::Copy).unwrap();
    assert_eq!(cached_disk_mb(Some(dir.path()))[&format!("{}#{}", info().repo, info().files[0])], 1);
    assert_eq!(delete_model(MODEL_ID, Some(dir.path())).unwrap(), 3);
    assert_eq!(snapshot_names(&seg), vec!["model.onnx"]);
    assert_eq!(snapshot_names(&e), vec!["other-model.onnx"]);
    assert_eq!(fs::read(e.join("snapshots").join(REV1).join("other-model.onnx")).unwrap(), vec![b'x'; 5 * MIB]);
}

#[test]
fn test_delete_removes_repo_folder_when_nothing_else_is_left_in_copy_layout() {
    let dir = tempfile::tempdir().unwrap();
    make_cached_repo(dir.path(), info().repo, &[(info().files[0], MIB)], REV1, Layout::Copy).unwrap();
    make_cached_repo(dir.path(), emb().0, &[(emb().1, 2 * MIB)], REV1, Layout::Copy).unwrap();
    assert_eq!(delete_model(MODEL_ID, Some(dir.path())).unwrap(), 3);
    assert!(no_repo_folders(dir.path()));
}

#[test]
fn test_delete_removes_repo_folder_when_nothing_else_is_left() {
    let dir = tempfile::tempdir().unwrap();
    make_two_repo_cache(dir.path());
    assert_eq!(delete_model(MODEL_ID, Some(dir.path())).unwrap(), 3);
    assert!(no_repo_folders(dir.path()));
}

#[test]
#[serial]
fn test_partial_delete_reports_what_is_left_and_still_frees_the_rest() {
    reset_env();
    let dir = tempfile::tempdir().unwrap();
    let (seg, e, _) = owned_cache(dir.path(), false, Layout::Auto).unwrap();
    let seg_repo = info().repo;
    let flaky = move |repo_id: &str| (repo_id == seg_repo).then(|| Err(DeleteFailure::Os("izin yok".to_string())));
    let hooks = DeleteHooks { delete_override: Some(&flaky), ..DeleteHooks::default() };
    let err = delete_model_with(MODEL_ID, dir.path(), &hooks).unwrap_err();
    assert!(err.message.contains("kısmen silindi (2 MB boşaldı); kalan: "), "{err}");
    assert!(err.message.ends_with("izin yok)"), "{err}");
    assert!(!e.join("snapshots").join(REV1).join(emb().1).exists());
    assert!(seg.join("snapshots").join(REV1).join(info().files[0]).exists());
}

#[test]
fn test_fallback_download_sizes_use_real_part_sizes() {
    let sizes: Vec<u64> = (0..2).map(|i| se_models::download::fallback_bytes(info(), i)).collect();
    assert_eq!(sizes, vec![1_540_506, 28_281_164]);
}
