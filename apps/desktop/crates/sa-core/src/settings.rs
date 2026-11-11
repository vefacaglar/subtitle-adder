//! Typed key/value settings (win-core §1, UNIFIED.md §5.5, §3.17).
//!
//! Implementations:
//! * [`InMemorySettingsStore`] (tests, and the base of the file store): complete.
//! * [`JsonFileSettingsStore`] (Windows `%LOCALAPPDATA%\SubtitleAdder\settings.json`, C#-compatible).
//! * `UserDefaultsSettingsStore` (macOS `CFPreferences` domain `app.subtitleadder.mac`) lives in `sa-platform`
//!   because it needs Core Foundation.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use parking_lot::{Condvar, Mutex};
use serde_json::Value;

use crate::subscription::{Subscribers, Subscription};

/// The settings contract shared by every backend.
///
/// Reads of a wrong JSON type are "missing" (`None`); a missing bool is not `false` (callers apply their default).
/// Setters are no-ops when the stored value is deep-equal; otherwise they persist and notify subscribers with the key
/// on the calling thread, outside the internal lock. `set_double(NaN/±Inf)` behaves like `remove`.
pub trait SettingsStore: Send + Sync {
    fn get_string(&self, key: &str) -> Option<String>;
    /// `Some` only for JSON `true`/`false`.
    fn get_bool(&self, key: &str) -> Option<bool>;
    /// JSON integers and integral doubles within the `i32` range (C# `int?`; larger values read as missing).
    fn get_int(&self, key: &str) -> Option<i64>;
    /// Any JSON number (ints accepted).
    fn get_double(&self, key: &str) -> Option<f64>;
    /// macOS `AppleLanguages`; `None` elsewhere.
    fn get_string_array(&self, key: &str) -> Option<Vec<String>> {
        let _ = key;
        None
    }
    fn set_string(&self, key: &str, value: &str);
    fn set_bool(&self, key: &str, value: bool);
    fn set_int(&self, key: &str, value: i64);
    fn set_double(&self, key: &str, value: f64);
    /// macOS `AppleLanguages` (`None` removes). Windows: unsupported (no-op).
    fn set_string_array(&self, key: &str, value: Option<&[String]>) {
        let _ = (key, value);
    }
    fn remove(&self, key: &str);
    /// Local changes (macOS: also external changes from another process). Handlers run on the writing thread, which
    /// may be a worker holding a lock: the file-association worker writes `fileAssociationsDisabled` while the
    /// registration gate is held, so a handler must never call the `FileAssociations` methods.
    fn subscribe(&self, handler: Box<dyn Fn(&str) + Send + Sync>) -> Subscription;
    /// Writes pending changes synchronously (exit path).
    fn flush(&self);
}

/// Typed reads over a JSON value (shared by every JSON-backed store).
mod read {
    use serde_json::Value;

    pub fn string(v: Option<&Value>) -> Option<String> {
        v.and_then(Value::as_str).map(str::to_owned)
    }
    pub fn bool(v: Option<&Value>) -> Option<bool> {
        v.and_then(Value::as_bool)
    }
    /// C# `GetInt` (an `int?`): JSON integers and integral doubles, but only within the `i32` range; anything
    /// larger reads as missing so the caller's default applies (C# fixture verified).
    pub fn int(v: Option<&Value>) -> Option<i64> {
        let n = v?.as_number()?;
        let range = f64::from(i32::MIN)..=f64::from(i32::MAX);
        if let Some(i) = n.as_i64() {
            return i32::try_from(i).is_ok().then_some(i);
        }
        let f = n.as_f64()?;
        (f.fract() == 0.0 && range.contains(&f)).then_some(f as i64)
    }
    pub fn double(v: Option<&Value>) -> Option<f64> {
        v.and_then(Value::as_f64)
    }
    pub fn string_array(v: Option<&Value>) -> Option<Vec<String>> {
        v?.as_array()?.iter().map(|v| v.as_str().map(str::to_owned)).collect()
    }

    /// C# `JsonNode.DeepEquals`: numbers compare by value (`3` = `3.0` = `3e0`, so re-setting a value read from the
    /// file as an int to the same double is a no-op), objects ignore key order.
    pub fn same_json(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Number(x), Value::Number(y)) => match (x.as_i64(), y.as_i64()) {
                (Some(i), Some(j)) => i == j,
                _ => match (x.as_u64(), y.as_u64()) {
                    (Some(i), Some(j)) => i == j,
                    _ => x.as_f64() == y.as_f64(),
                },
            },
            (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same_json(p, q)),
            (Value::Object(x), Value::Object(y)) => {
                x.len() == y.len() && x.iter().all(|(k, p)| y.get(k).is_some_and(|q| same_json(p, q)))
            }
            _ => a == b,
        }
    }
}

/// Map-only settings store (no persistence). Thread-safe.
#[derive(Default)]
pub struct InMemorySettingsStore {
    values: Mutex<IndexMap<String, Value>>,
    changed: Subscribers<String>,
}

impl InMemorySettingsStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// A store pre-filled from a JSON object (tests).
    pub fn from_json(object: &str) -> Arc<Self> {
        let map = serde_json::from_str::<IndexMap<String, Value>>(object).unwrap_or_default();
        Arc::new(Self { values: Mutex::new(map), changed: Subscribers::new() })
    }

    /// A deep copy of the value map.
    pub fn snapshot(&self) -> IndexMap<String, Value> {
        self.values.lock().clone()
    }

    /// Raw JSON value of a key.
    pub fn raw(&self, key: &str) -> Option<Value> {
        self.values.lock().get(key).cloned()
    }

    fn put(&self, key: &str, value: Value) {
        {
            let mut g = self.values.lock();
            if g.get(key).is_some_and(|old| read::same_json(old, &value)) {
                return;
            }
            g.insert(key.to_owned(), value);
        }
        self.changed.notify(&key.to_owned());
    }
}

impl SettingsStore for InMemorySettingsStore {
    fn get_string(&self, key: &str) -> Option<String> {
        read::string(self.values.lock().get(key))
    }
    fn get_bool(&self, key: &str) -> Option<bool> {
        read::bool(self.values.lock().get(key))
    }
    fn get_int(&self, key: &str) -> Option<i64> {
        read::int(self.values.lock().get(key))
    }
    fn get_double(&self, key: &str) -> Option<f64> {
        read::double(self.values.lock().get(key))
    }
    fn get_string_array(&self, key: &str) -> Option<Vec<String>> {
        read::string_array(self.values.lock().get(key))
    }
    fn set_string(&self, key: &str, value: &str) {
        self.put(key, Value::String(value.to_owned()));
    }
    fn set_bool(&self, key: &str, value: bool) {
        self.put(key, Value::Bool(value));
    }
    fn set_int(&self, key: &str, value: i64) {
        self.put(key, Value::from(value));
    }
    fn set_double(&self, key: &str, value: f64) {
        match serde_json::Number::from_f64(value) {
            Some(n) => self.put(key, Value::Number(n)),
            None => self.remove(key),
        }
    }
    fn set_string_array(&self, key: &str, value: Option<&[String]>) {
        match value {
            Some(v) => self.put(key, Value::Array(v.iter().cloned().map(Value::String).collect())),
            None => self.remove(key),
        }
    }
    fn remove(&self, key: &str) {
        if self.values.lock().shift_remove(key).is_some() {
            self.changed.notify(&key.to_owned());
        }
    }
    fn subscribe(&self, handler: Box<dyn Fn(&str) + Send + Sync>) -> Subscription {
        self.changed.subscribe(move |k: &String| handler(k))
    }
    fn flush(&self) {}
}

/// Cross-instance notification hook of the file store (Windows: the `settings` broker topic, wired by `sa-app`
/// through `sa-sync`; win-core §1.4).
pub trait SettingsSyncHook: Send + Sync {
    /// Called after every successful write (publish `SettingsUpdated` with this store's instance id and path).
    fn published(&self, instance_id: &str, path: &Path);
    /// Registers the reload callback invoked for a remote `settings` event that passed the sender/scope filter
    /// (used only when [`on_remote_event`](Self::on_remote_event) is not implemented; the hook must then filter
    /// itself).
    fn on_remote_change(&self, reload: Box<dyn Fn() + Send + Sync>) -> Subscription;
    /// Preferred: registers a handler for **every** `settings` broker event with its `senderInstanceId` and
    /// `scopePath`; the store applies the C# filter itself (ignore its own instance id, ignore another path, compared
    /// OrdinalIgnoreCase) and reloads. `None` (the default) falls back to [`on_remote_change`](Self::on_remote_change).
    fn on_remote_event(&self, handler: Box<dyn Fn(&str, &str) + Send + Sync>) -> Option<Subscription> {
        let _ = handler;
        None
    }
}

/// Map + dirty flag, guarded together (C# `SyncRoot`).
#[derive(Default)]
struct FileState {
    values: IndexMap<String, Value>,
    dirty: bool,
}

/// Trailing-edge debounce timer of the file store: one background thread waiting for the latest deadline.
#[derive(Default)]
struct DebounceTimer {
    state: Mutex<TimerState>,
    wake: Condvar,
}

#[derive(Default)]
struct TimerState {
    deadline: Option<Instant>,
    disposed: bool,
}

impl DebounceTimer {
    /// Re-arms the timer (non-blocking). `false` once disposed: the change stays dirty for a later flush.
    fn arm(&self, delay: Duration) -> bool {
        let mut g = self.state.lock();
        if g.disposed {
            return false;
        }
        g.deadline = Some(Instant::now() + delay);
        drop(g);
        self.wake.notify_one();
        true
    }

    fn dispose(&self) {
        self.state.lock().disposed = true;
        self.wake.notify_one();
    }

    /// Thread body: waits for a due deadline and calls `fire`; returns when disposed or `fire` returns `false`.
    fn run(&self, fire: impl Fn() -> bool) {
        loop {
            {
                let mut g = self.state.lock();
                loop {
                    if g.disposed {
                        return;
                    }
                    match g.deadline {
                        None => {
                            self.wake.wait(&mut g);
                        }
                        Some(d) => {
                            let now = Instant::now();
                            if now >= d {
                                g.deadline = None;
                                break;
                            }
                            let _ = self.wake.wait_for(&mut g, d - now);
                        }
                    }
                }
            }
            if !fire() {
                return;
            }
        }
    }
}

/// Windows `settings.json` store (win-core §1.2–1.4): flat JSON object, unknown keys preserved verbatim, memory
/// updated immediately, trailing-edge debounced (300 ms) atomic write off the UI thread, remote reload replaces the
/// map silently (no `changed` events).
pub struct JsonFileSettingsStore {
    path: PathBuf,
    instance_id: String,
    save_delay: Duration,
    state: Mutex<FileState>,
    /// One writer at a time (timer vs. explicit flush).
    file_lock: Mutex<()>,
    timer: Arc<DebounceTimer>,
    changed: Subscribers<String>,
    sync: Option<Arc<dyn SettingsSyncHook>>,
    remote: Mutex<Option<Subscription>>,
    disposed: std::sync::atomic::AtomicBool,
}

impl JsonFileSettingsStore {
    /// Default debounce.
    pub const DEFAULT_SAVE_DELAY: Duration = Duration::from_millis(300);

    /// Opens (or starts empty on a missing/invalid file) the store at `path`. `instance_id` is a new GUID "N".
    pub fn open(path: PathBuf, save_delay: Duration, sync: Option<Arc<dyn SettingsSyncHook>>) -> Arc<Self> {
        let path = std::path::absolute(&path).unwrap_or(path);
        let values = Self::load(&path);
        let timer = Arc::new(DebounceTimer::default());
        let store = Arc::new(Self {
            path,
            instance_id: crate::text::guid_n(),
            save_delay,
            state: Mutex::new(FileState { values, dirty: false }),
            file_lock: Mutex::new(()),
            timer: timer.clone(),
            changed: Subscribers::new(),
            sync: sync.clone(),
            remote: Mutex::new(None),
            disposed: std::sync::atomic::AtomicBool::new(false),
        });

        let weak: Weak<Self> = Arc::downgrade(&store);
        let spawned = std::thread::Builder::new().name("settings-save".into()).spawn(move || {
            timer.run(|| match weak.upgrade() {
                Some(store) => {
                    store.flush();
                    true
                }
                None => false,
            });
        });
        if spawned.is_err() {
            // No timer thread: changes stay dirty until an explicit flush/dispose.
            store.timer.dispose();
        }

        if let Some(hook) = sync {
            let weak = Arc::downgrade(&store);
            let filtered = hook.on_remote_event(Box::new(move |sender: &str, scope: &str| {
                if let Some(store) = weak.upgrade()
                    && store.accepts_remote(sender, scope)
                {
                    store.reload();
                }
            }));
            let sub = filtered.unwrap_or_else(|| {
                let weak = Arc::downgrade(&store);
                hook.on_remote_change(Box::new(move || {
                    if let Some(store) = weak.upgrade() {
                        store.reload();
                    }
                }))
            });
            *store.remote.lock() = Some(sub);
        }
        store
    }

    /// `%LOCALAPPDATA%\SubtitleAdder\settings.json`, made absolute.
    pub fn default_path(app_data_dir: &Path) -> PathBuf {
        let p = app_data_dir.join("settings.json");
        std::path::absolute(&p).unwrap_or(p)
    }

    /// The absolute settings path (broker scope).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 32 lowercase hex chars.
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Unsubscribe from the broker, stop the timer, flush. Idempotent; the store keeps working in memory and a
    /// later [`flush`](SettingsStore::flush) still writes.
    pub fn dispose(&self) {
        if self.disposed.swap(true, std::sync::atomic::Ordering::AcqRel) {
            self.flush();
            return;
        }
        drop(self.remote.lock().take());
        self.timer.dispose();
        self.flush();
    }

    /// The C# broker filter (win-core §1.4): an event from another instance (`sender_instance_id` differs,
    /// ordinal) about this file (`scope_path` equal OrdinalIgnoreCase).
    pub fn accepts_remote(&self, sender_instance_id: &str, scope_path: &str) -> bool {
        sender_instance_id != self.instance_id
            && crate::text::eq_ordinal_ignore_case(scope_path, &self.path.as_os_str().to_string_lossy())
    }

    /// Remote change (win-core §1.4): re-reads the file and **replaces** the in-memory map. No `changed` events,
    /// `dirty` untouched.
    pub fn reload(&self) {
        let reloaded = Self::load(&self.path);
        self.state.lock().values = reloaded;
    }

    /// `key` as a bool as it is in the file right now (another instance's flushed write included), without touching
    /// the in-memory map; `None` when absent, not a bool, or the file is missing or unreadable.
    pub fn persisted_bool(&self, key: &str) -> Option<bool> {
        read::bool(Self::load(&self.path).get(key))
    }

    /// A deep copy of the value map (tests, diagnostics).
    pub fn snapshot(&self) -> IndexMap<String, Value> {
        self.state.lock().values.clone()
    }

    /// Load rules of win-core §1.2: missing/unreadable/invalid/non-object → empty map.
    fn load(path: &Path) -> IndexMap<String, Value> {
        let Some(text) = crate::atomic_file::read_all_text_shared(path, crate::atomic_file::DEFAULT_ATTEMPTS) else {
            return IndexMap::new();
        };
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(map)) => map.into_iter().collect(),
            _ => IndexMap::new(),
        }
    }

    fn read<T>(&self, key: &str, f: impl FnOnce(Option<&Value>) -> T) -> T {
        f(self.state.lock().values.get(key))
    }

    fn put(&self, key: &str, value: Value) {
        {
            let mut g = self.state.lock();
            if g.values.get(key).is_some_and(|old| read::same_json(old, &value)) {
                return;
            }
            g.values.insert(key.to_owned(), value);
            self.persist(&mut g);
        }
        self.changed.notify(&key.to_owned());
    }

    /// Under the map lock after each effective change; never blocks.
    fn persist(&self, state: &mut FileState) {
        state.dirty = true;
        let _ = self.timer.arm(self.save_delay);
    }
}

impl Drop for JsonFileSettingsStore {
    fn drop(&mut self) {
        self.timer.dispose();
        drop(self.remote.get_mut().take());
        // Pending changes are written like on dispose (the C# store relied on an explicit Dispose at exit).
        self.flush();
    }
}

impl SettingsStore for JsonFileSettingsStore {
    fn get_string(&self, key: &str) -> Option<String> {
        self.read(key, read::string)
    }
    fn get_bool(&self, key: &str) -> Option<bool> {
        self.read(key, read::bool)
    }
    fn get_int(&self, key: &str) -> Option<i64> {
        self.read(key, read::int)
    }
    fn get_double(&self, key: &str) -> Option<f64> {
        self.read(key, read::double)
    }
    fn get_string_array(&self, key: &str) -> Option<Vec<String>> {
        self.read(key, read::string_array)
    }
    fn set_string(&self, key: &str, value: &str) {
        self.put(key, Value::String(value.to_owned()));
    }
    fn set_bool(&self, key: &str, value: bool) {
        self.put(key, Value::Bool(value));
    }
    fn set_int(&self, key: &str, value: i64) {
        self.put(key, Value::from(value));
    }
    fn set_double(&self, key: &str, value: f64) {
        match serde_json::Number::from_f64(value) {
            Some(n) => self.put(key, Value::Number(n)),
            None => self.remove(key),
        }
    }
    // `set_string_array`: unsupported on Windows (trait default no-op).
    fn remove(&self, key: &str) {
        {
            let mut g = self.state.lock();
            if g.values.shift_remove(key).is_none() {
                return;
            }
            self.persist(&mut g);
        }
        self.changed.notify(&key.to_owned());
    }
    fn subscribe(&self, handler: Box<dyn Fn(&str) + Send + Sync>) -> Subscription {
        self.changed.subscribe(move |k: &String| handler(k))
    }
    /// win-core §1.3 `flush()`: file lock → (map lock: dirty? clear, snapshot) → atomic write → publish on success.
    /// A failed write is not retried (dirty was cleared), as in the C# app.
    fn flush(&self) {
        let wrote = {
            let _file = self.file_lock.lock();
            let json = {
                let mut g = self.state.lock();
                if !g.dirty {
                    return;
                }
                g.dirty = false;
                let object: serde_json::Map<String, Value> = g.values.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                serde_json::to_string_pretty(&Value::Object(object)).unwrap_or_else(|_| "{}".into())
            };
            crate::atomic_file::write_all_text_atomic(&self.path, &json, crate::atomic_file::DEFAULT_ATTEMPTS)
        };
        if wrote && let Some(hook) = &self.sync {
            hook.published(&self.instance_id, &self.path);
        }
    }
}

/// Every settings key (UNIFIED.md §3.17). Owners are in other crates; defaults are in [`defaults`].
pub mod keys {
    pub const UI_LANGUAGE: &str = "uiLanguage";
    pub const APP_THEME: &str = "appTheme";
    pub const APP_FONT_FAMILY: &str = "appFontFamily";
    pub const OVERLAY_FONT_FAMILY: &str = "overlayFontFamily";
    pub const OVERLAY_FONT_SIZE: &str = "overlayFontSize";
    pub const OVERLAY_TEXT_COLOR: &str = "overlayTextColor";
    pub const OVERLAY_BACKGROUND_STYLE: &str = "overlayBackgroundStyle";
    pub const OVERLAY_BACKGROUND_OPACITY: &str = "overlayBackgroundOpacity";
    pub const SELECTED_MODEL: &str = "selectedModel";
    pub const SELECTED_LANGUAGE: &str = "selectedLanguage";
    pub const TRANSLATION_MODEL: &str = "translationModel";
    pub const TRANSLATION_TARGET: &str = "translationTarget";
    pub const SUBTITLE_DISPLAY_MODE: &str = "subtitleDisplayMode";
    pub const DIARIZE_SPEAKERS: &str = "diarizeSpeakers";
    pub const SPEAKER_COUNT: &str = "speakerCount";
    pub const OFFLINE_MODE: &str = "offlineMode";
    pub const AUTO_SAVE_SUBTITLE: &str = "autoSaveSubtitle";
    pub const AUTO_PLAY_VIDEO: &str = "autoPlayVideo";
    pub const SIDE_BY_SIDE_LAYOUT: &str = "sideBySideLayout";
    pub const HIDE_SUBTITLE_PANEL: &str = "hideSubtitlePanel";
    pub const SHOW_FILE_EXPLORER: &str = "showFileExplorer";
    /// Removed at startup on both platforms and never written (UNIFIED.md §3.10).
    pub const EXPLORER_LAST_FOLDER: &str = "explorerLastFolder";
    // Windows only
    pub const VIDEO_CACHE_MAX_SIZE_GB: &str = "videoCacheMaxSizeGb";
    pub const EXTERNAL_LLM_ENABLED: &str = "externalLlmEnabled";
    pub const EXTERNAL_LLM_URL: &str = "externalLlmUrl";
    pub const EXTERNAL_LLM_MODEL: &str = "externalLlmModel";
    pub const EXTERNAL_LLM_ASYNC: &str = "externalLlmAsync";
    pub const FILE_ASSOCIATIONS_DISABLED: &str = "fileAssociationsDisabled";
    pub const MAIN_WINDOW_WIDTH: &str = "mainWindowWidth";
    pub const MAIN_WINDOW_HEIGHT: &str = "mainWindowHeight";
    pub const SETTINGS_WINDOW_WIDTH: &str = "settingsWindowWidth";
    pub const SETTINGS_WINDOW_HEIGHT: &str = "settingsWindowHeight";
    // macOS only
    pub const SETTINGS_TAB: &str = "settingsTab";
    pub const APPLE_LANGUAGES: &str = "AppleLanguages";
    pub const TRANSLATION_PROVIDERS_MIGRATED: &str = "translationProvidersMigrated";
    /// New in the Rust app: `"{x,y,w,h}"` in points.
    pub const MAIN_WINDOW_FRAME: &str = "mainWindowFrame";
}

/// Shared defaults (UNIFIED.md §3.17).
pub mod defaults {
    pub const SELECTED_MODEL: &str = "large-v3";
    pub const SELECTED_LANGUAGE: &str = "";
    pub const TRANSLATION_TARGET: &str = "en";
    pub const TRANSLATION_MODEL: &str = "madlad-3b";
    pub const SUBTITLE_DISPLAY_MODE: &str = "original";
    pub const OFFLINE_MODE: bool = true;
    pub const AUTO_SAVE_SUBTITLE: bool = true;
    pub const DIARIZE_SPEAKERS: bool = true;
    pub const SPEAKER_COUNT: i64 = 0;
    pub const AUTO_PLAY_VIDEO: bool = true;
    pub const SIDE_BY_SIDE_LAYOUT: bool = false;
    pub const HIDE_SUBTITLE_PANEL: bool = false;
    pub const SHOW_FILE_EXPLORER: bool = true;
    pub const VIDEO_CACHE_MAX_SIZE_GB: i64 = 20;
}
