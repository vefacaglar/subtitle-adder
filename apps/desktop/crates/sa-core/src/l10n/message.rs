//! `LocalizedMessage`: a message localized at display time, compared by id (win-core §4.9, win-viewmodels §2.1,
//! mac-viewmodels §2.3).

use std::fmt;
use std::sync::Arc;

use crate::engine_types::EngineError;

/// A status/error text that follows a language switch. Equality and hash use [`id`](Self::id) only.
#[derive(Clone)]
pub struct LocalizedMessage {
    id: String,
    build: Arc<dyn Fn() -> String + Send + Sync>,
}

impl LocalizedMessage {
    /// A message with an explicit id (`"stage:…"`, `"offline:…"`, `"notice:…"`, `"saved:…"`, `"done:…"`).
    pub fn new(id: impl Into<String>, build: impl Fn() -> String + Send + Sync + 'static) -> Self {
        Self { id: id.into(), build: Arc::new(build) }
    }

    /// id `"key:" + k`, text `L(k)` in the language current at display time.
    pub fn key(k: &str) -> Self {
        let key = k.to_owned();
        Self::new(format!("key:{k}"), move || super::l(&key))
    }

    /// id `"verbatim:" + t`, text `t` (engine messages).
    pub fn verbatim(t: impl Into<String>) -> Self {
        let text: String = t.into();
        Self::new(format!("verbatim:{text}"), move || text.clone())
    }

    /// `NotFound` → id `"error:notFound"`, re-localized at each display; any other error → id
    /// `"error:<kind>:<message>"` with the message captured now.
    pub fn error(err: &EngineError) -> Self {
        match err {
            EngineError::NotFound => Self::new("error:notFound", || EngineError::NotFound.to_string()),
            other => {
                let text = other.to_string();
                Self::new(format!("error:{}:{text}", other.kind_name()), move || text.clone())
            }
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// The text in the current UI language.
    pub fn text(&self) -> String {
        (self.build)()
    }
}

impl PartialEq for LocalizedMessage {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for LocalizedMessage {}

impl std::hash::Hash for LocalizedMessage {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl fmt::Display for LocalizedMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text())
    }
}

impl fmt::Debug for LocalizedMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("LocalizedMessage").field(&self.id).finish()
    }
}
