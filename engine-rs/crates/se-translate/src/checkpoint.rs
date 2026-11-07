//! Translation checkpoints `<temp>/SubtitleAdder/translations/chunk-<hash>.json` (LLM/external only;
//! spec translate-llm.md §11). The hash input uses Python `repr`/`str` texts (int vs float start/end).
//!
//! The cache keeps insertion order like a Python dict (loaded order, then new ids appended), which is the key
//! order of the saved JSON object.

use std::path::{Path, PathBuf};

use se_core::{EngineError, Map, Result, Sentence, Value, json, pyfmt};
use sha2::{Digest, Sha256};

use crate::external::{external_llm_model, external_llm_url};

/// An insertion-ordered `dict[int, str]`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cache {
    entries: Vec<(i64, String)>,
}

impl Cache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, id: i64) -> Option<&str> {
        self.entries.iter().find(|(k, _)| *k == id).map(|(_, v)| v.as_str())
    }

    pub fn contains(&self, id: i64) -> bool {
        self.get(id).is_some()
    }

    /// `cache[id] = text` (an existing key keeps its position).
    pub fn insert(&mut self, id: i64, text: String) {
        match self.entries.iter_mut().find(|(k, _)| *k == id) {
            Some(e) => e.1 = text,
            None => self.entries.push((id, text)),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = (i64, &str)> {
        self.entries.iter().map(|(k, v)| (*k, v.as_str()))
    }

    /// `json.dumps(cache, ensure_ascii=False)`.
    pub fn to_json(&self) -> String {
        let mut map = Map::new();
        for (k, v) in &self.entries {
            map.insert(k.to_string(), v.as_str());
        }
        json::dumps(&Value::Object(map), false)
    }
}

/// The digest input and resulting file name of `_temp_checkpoint_path` (no I/O).
pub fn checkpoint_file_name(
    model_id: &str,
    target: &str,
    source: Option<&str>,
    sentences: &[Sentence],
    system_prompt: &str,
) -> String {
    let mut digest = Sha256::new();
    let ext_url = external_llm_url().unwrap_or_default();
    let ext_model = external_llm_model().unwrap_or_default();
    digest.update(
        format!("quality-v6|{model_id}|{ext_url}|{ext_model}|{}|{target}|{system_prompt}\n", source.unwrap_or(""))
            .as_bytes(),
    );
    for s in sentences {
        let speaker = s.speaker.map_or_else(|| "None".to_string(), |v| v.to_string());
        digest.update(
            format!("{}:{}:{}:{}:{}\n", s.id, s.start.py_str(), s.end.py_str(), speaker, pyfmt::strip(&s.text))
                .as_bytes(),
        );
    }
    let hex = hex::encode(digest.finalize());
    format!("chunk-{}.json", &hex[..20])
}

/// The checkpoint folder `<tempfile.gettempdir()>/SubtitleAdder/translations`.
pub fn checkpoint_dir() -> PathBuf {
    se_core::fsutil::gettempdir().join(se_core::fsutil::APP_TEMP_DIR_NAME).join("translations")
}

/// `_temp_checkpoint_path(model_id, target, source, sentences, system_prompt)`: creates the folder (an
/// `OSError` there aborts `translate()`, as in Python) and returns the path.
pub fn checkpoint_path(
    model_id: &str,
    target: &str,
    source: Option<&str>,
    sentences: &[Sentence],
    system_prompt: &str,
) -> Result<PathBuf> {
    let folder = checkpoint_dir();
    std::fs::create_dir_all(&folder).map_err(|e| EngineError::win(&e, Some(&folder)))?;
    Ok(folder.join(checkpoint_file_name(model_id, target, source, sentences, system_prompt)))
}

/// Python `int(k)` for a JSON object key: surrounding whitespace, a sign, `_` between digits, Unicode digits.
fn py_int_key(k: &str) -> Option<i64> {
    let s = pyfmt::strip(k);
    let (neg, digits) = match s.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let mut v: i64 = 0;
    for c in digits.chars() {
        if c == '_' {
            continue;
        }
        let d = crate::pyunicode::decimal_value(c)? as i64;
        v = v.checked_mul(10)?.checked_add(d)?;
    }
    Some(if neg { -v } else { v })
}

/// `_load_temp_checkpoint(path)`: sentence id → translated text (missing/corrupt → empty).
pub fn load(path: &Path) -> Cache {
    (|| -> Option<Cache> {
        if !path.is_file() {
            return Some(Cache::new());
        }
        let bytes = std::fs::read(path).ok()?;
        let text = json::decode_utf8_strict(&bytes).ok()?;
        let raw = json::parse(&pyfmt::universal_newlines(text)).ok()?;
        let obj = raw.as_object()?;
        let mut cache = Cache::new();
        for (k, v) in obj.iter() {
            let id = py_int_key(k)?;
            let value = v.py_str();
            if !pyfmt::strip(&value).is_empty() {
                cache.insert(id, value);
            }
        }
        Some(cache)
    })()
    .unwrap_or_default()
}

fn with_json_tmp_suffix(path: &Path) -> PathBuf {
    // Path.with_suffix(".json.tmp"): replaces the last suffix.
    path.with_extension("json.tmp")
}

/// `_save_temp_checkpoint(path, cache)` (`<path>.json.tmp` + replace, errors ignored).
pub fn save(path: &Path, cache: &Cache) {
    let tmp = with_json_tmp_suffix(path);
    if std::fs::write(&tmp, cache.to_json().as_bytes()).is_ok() {
        let _ = se_core::fsutil::replace(&tmp, path);
    }
}

/// `_delete_temp_checkpoint(path)` (errors ignored).
pub fn delete(path: Option<&Path>) {
    if let Some(path) = path {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(with_json_tmp_suffix(path));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_keys_like_python() {
        assert_eq!(py_int_key(" 10 "), Some(10));
        assert_eq!(py_int_key("1_0"), Some(10));
        assert_eq!(py_int_key("-3"), Some(-3));
        assert_eq!(py_int_key("٣"), Some(3));
        assert_eq!(py_int_key("x"), None);
        assert_eq!(py_int_key("_1"), None);
    }

    #[test]
    fn cache_json_keeps_order() {
        let mut c = Cache::new();
        c.insert(10, "Müdür Hanım geldi.".into());
        c.insert(2, "b".into());
        c.insert(10, "x".into());
        assert_eq!(c.to_json(), r#"{"10": "x", "2": "b"}"#);
    }
}
