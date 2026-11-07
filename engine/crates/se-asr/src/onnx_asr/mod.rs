//! The parts of onnx-asr 0.12 the engine uses (spec transcribe.md §6): model resolution from the HF cache,
//! NumPy preprocessors, Silero VAD, Parakeet TDT, Canary AED and GigaAM v3 RNN-T greedy decoding.

pub mod models;
pub mod preproc;
pub mod vad;

use std::path::{Path, PathBuf};

use se_core::{EngineError, ErrorKind, Result};

/// `onnx_asr.resolver.model_repos` (the entries the engine can reach).
pub fn model_repo(name: &str) -> Option<&'static str> {
    Some(match name {
        "gigaam-v3-e2e-rnnt" => "istupakov/gigaam-v3-onnx",
        "nemo-parakeet-tdt-0.6b-v3" => "istupakov/parakeet-tdt-0.6b-v3-onnx",
        "nemo-canary-1b-v2" => "istupakov/canary-1b-v2-onnx",
        "silero" => "istupakov/silero-vad-onnx",
        _ => return None,
    })
}

/// Glob match of one path component (`*`, `?`; no character classes needed here).
pub fn glob_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    fn rec(p: &[char], n: &[char]) -> bool {
        match p.first() {
            None => n.is_empty(),
            Some('*') => (0..=n.len()).any(|i| rec(&p[1..], &n[i..])),
            Some('?') => !n.is_empty() && rec(&p[1..], &n[1..]),
            Some(c) => n.first() == Some(c) && rec(&p[1..], &n[1..]),
        }
    }
    rec(&p, &n)
}

/// `Resolver._resolve_model_files.find(filename)`.
pub fn find_file(dir: &Path, pattern: &str) -> Result<PathBuf> {
    let dir_text = se_core::paths::py_path_str(dir);
    let list = |pat: &str| -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| glob_match(pat, &e.file_name().to_string_lossy()))
                    .map(|e| e.path())
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        out
    };
    let files = list(pattern);
    if files.len() > 1 {
        return Err(EngineError::new(
            ErrorKind::Os,
            format!("Found more than 1 file '{pattern}' in path '{dir_text}'."),
        ));
    }
    let not_found = || EngineError::new(ErrorKind::Model, format!("File '{pattern}' not found in path '{dir_text}'."));
    if files.is_empty() {
        if let Some(stem) = pattern.strip_suffix(".onnx") {
            let ort = list(&format!("{stem}.ort"));
            if ort.len() == 1 && ort[0].is_file() {
                return Ok(ort[0].clone());
            }
        }
        return Err(not_found());
    }
    if !files[0].is_file() {
        return Err(not_found());
    }
    Ok(files[0].clone())
}

/// The cached snapshot folder of an onnx-asr model (`snapshot_download(..., local_files_only=True)`).
pub fn model_dir(name: &str) -> Result<PathBuf> {
    let repo = model_repo(name)
        .ok_or_else(|| EngineError::value(format!("Model '{name}' not supported!")))?;
    match crate::hf::cached_snapshot(repo) {
        Some(dir) => Ok(dir),
        None => Err(EngineError::new(
            ErrorKind::Model,
            if se_core::offline::hub_offline() { crate::hf::OFFLINE_MESSAGE } else { crate::hf::LOCAL_ONLY_MESSAGE },
        )),
    }
}

/// `vocab.txt`: `token id` per line, `▁` replaced by a space.
pub fn read_vocab(path: &Path) -> Result<Vec<(i64, String)>> {
    let text = std::fs::read_to_string(path).map_err(|e| EngineError::os(&e, Some(path)))?;
    let mut out = Vec::new();
    for line in text.split_inclusive('\n') {
        let line = line.trim_matches('\n');
        let mut parts = line.split(' ');
        let (Some(token), Some(id), None) = (parts.next(), parts.next(), parts.next()) else {
            return Err(EngineError::value(format!("bad vocab line {line:?}")));
        };
        let id: i64 = id.trim().parse().map_err(|_| EngineError::value(format!("bad vocab id {id:?}")))?;
        out.push((id, token.replace('\u{2581}', " ")));
    }
    Ok(out)
}

/// `config.json` of a model folder (empty when absent).
pub fn read_config(dir: &Path) -> Result<se_core::json::Map> {
    let path = dir.join("config.json");
    if !path.exists() {
        return Ok(se_core::json::Map::new());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| EngineError::os(&e, Some(&path)))?;
    let v = se_core::json::parse(&text)?;
    Ok(v.as_object().cloned().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_question_mark_matches_one_char() {
        assert!(glob_match("encoder-model?int8.onnx", "encoder-model.int8.onnx"));
        assert!(!glob_match("encoder-model?int8.onnx", "encoder-model.onnx"));
        assert!(glob_match("v?_vocab.txt", "v3_vocab.txt"));
    }

    #[test]
    fn missing_file_message() {
        let dir = tempfile::tempdir().unwrap();
        let err = find_file(dir.path(), "vocab.txt").unwrap_err();
        assert!(err.message.starts_with("File 'vocab.txt' not found in path '"), "{}", err.message);
        std::fs::write(dir.path().join("a.ort"), b"x").unwrap();
        assert_eq!(find_file(dir.path(), "a.onnx").unwrap(), dir.path().join("a.ort"));
    }
}
