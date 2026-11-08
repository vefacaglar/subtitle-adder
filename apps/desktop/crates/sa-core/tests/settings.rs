//! Settings store and AtomicFileStore tests: ports of AppLanguageAndSettingsTests (JsonFileStore*,
//! MultiInstanceConcurrentStores… settings part) and the win-core §12 recommended tests.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sa_core::Subscription;
use sa_core::atomic_file::{decode_with_bom, read_all_text_shared, write_all_text_atomic};
use sa_core::settings::{InMemorySettingsStore, JsonFileSettingsStore, SettingsStore, SettingsSyncHook};

fn settings_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("nested").join("settings.json")
}

fn wait_for(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    f()
}

fn tmp_files(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|it| it.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.ends_with(".tmp")).collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------- JsonFileSettingsStore

#[test]
fn json_file_store_saves_off_thread_after_a_debounce() {
    let dir = tempfile::tempdir().unwrap();
    let path = settings_path(&dir);
    let store = JsonFileSettingsStore::open(path.clone(), Duration::from_millis(50), None);
    for tick in 0..20 {
        store.set_double("overlayOpacity", f64::from(tick) / 20.0);
    }
    assert_eq!(store.get_double("overlayOpacity"), Some(0.95)); // memory is updated at once
    assert!(!path.exists(), "nothing is written before the debounce elapsed");
    assert!(wait_for(Duration::from_secs(5), || path.exists()));
    std::thread::sleep(Duration::from_millis(100));
    let fresh = JsonFileSettingsStore::open(path.clone(), JsonFileSettingsStore::DEFAULT_SAVE_DELAY, None);
    assert_eq!(fresh.get_double("overlayOpacity"), Some(0.95));
}

#[test]
fn json_file_store_round_trips_and_survives_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let path = settings_path(&dir);
    let store = JsonFileSettingsStore::open(path.clone(), JsonFileSettingsStore::DEFAULT_SAVE_DELAY, None);
    store.set_string("selectedModel", "small");
    store.set_bool("diarizeSpeakers", false);
    store.set_int("speakerCount", 3);
    store.set_double("overlayFontSize", 0.07);
    store.flush();
    let reloaded = JsonFileSettingsStore::open(path.clone(), JsonFileSettingsStore::DEFAULT_SAVE_DELAY, None);
    assert_eq!(reloaded.get_string("selectedModel").as_deref(), Some("small"));
    assert_eq!(reloaded.get_bool("diarizeSpeakers"), Some(false));
    assert_eq!(reloaded.get_int("speakerCount"), Some(3));
    assert_eq!(reloaded.get_double("overlayFontSize"), Some(0.07));
    assert_eq!(reloaded.get_string("speakerCount"), None); // wrong type reads as missing
    let raw: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(raw["selectedModel"], "small");

    std::fs::write(&path, "{ not json").unwrap();
    let corrupt = JsonFileSettingsStore::open(path.clone(), JsonFileSettingsStore::DEFAULT_SAVE_DELAY, None);
    assert_eq!(corrupt.get_string("selectedModel"), None);
    corrupt.set_string("selectedModel", "base");
    corrupt.dispose();
    let after = JsonFileSettingsStore::open(path, JsonFileSettingsStore::DEFAULT_SAVE_DELAY, None);
    assert_eq!(after.get_string("selectedModel").as_deref(), Some("base"));
}

#[test]
fn json_file_store_keeps_unknown_keys_and_reads_a_csharp_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    // A file as System.Text.Json writes it: UTF-8 BOM-less, CRLF, \u escapes, unknown keys and types.
    let csharp = "{\r\n  \"explorerLastFolder\": \"C:\\\\Users\\\\\\u00D6zcan\",\r\n  \"overlayFontSize\": 1E-05,\r\n  \"speakerCount\": 3.0,\r\n  \"futureObject\": {\r\n    \"a\": [1, 2]\r\n  },\r\n  \"appTheme\": \"dark\"\r\n}";
    std::fs::write(&path, csharp).unwrap();
    let store = JsonFileSettingsStore::open(path.clone(), Duration::from_millis(5), None);
    assert_eq!(store.get_string("explorerLastFolder").as_deref(), Some("C:\\Users\\Özcan"));
    assert_eq!(store.get_double("overlayFontSize"), Some(1e-5));
    assert_eq!(store.get_int("speakerCount"), Some(3));
    assert_eq!(store.get_double("speakerCount"), Some(3.0));
    store.set_string("appTheme", "light");
    store.flush();
    let raw: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(raw["futureObject"], serde_json::json!({ "a": [1, 2] }));
    assert_eq!(raw["appTheme"], "light");
    let keys: Vec<&String> = raw.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["explorerLastFolder", "overlayFontSize", "speakerCount", "futureObject", "appTheme"]);
    let bytes = std::fs::read(&path).unwrap();
    assert_ne!(&bytes[..3], &[0xEF, 0xBB, 0xBF], "UTF-8 without BOM");
}

#[test]
fn json_file_store_non_object_root_and_bom_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(&path, "[1, 2]").unwrap();
    assert!(JsonFileSettingsStore::open(path.clone(), Duration::from_millis(5), None).snapshot().is_empty());
    let mut bom = vec![0xEF, 0xBB, 0xBF];
    bom.extend_from_slice(b"{\"selectedModel\":\"medium\"}");
    std::fs::write(&path, bom).unwrap();
    let store = JsonFileSettingsStore::open(path.clone(), Duration::from_millis(5), None);
    assert_eq!(store.get_string("selectedModel").as_deref(), Some("medium"));
    let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain("{\"a\":true}".encode_utf16().flat_map(u16::to_le_bytes)).collect();
    std::fs::write(&path, utf16).unwrap();
    assert_eq!(JsonFileSettingsStore::open(path, Duration::from_millis(5), None).get_bool("a"), Some(true));
}

#[test]
fn json_file_store_empty_store_writes_braces_and_equal_values_do_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let store = JsonFileSettingsStore::open(path.clone(), Duration::from_secs(60), None);
    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    let e = events.clone();
    let _sub = store.subscribe(Box::new(move |k: &str| e.lock().unwrap().push(k.to_owned())));
    store.set_string("x", "1");
    store.set_string("x", "1"); // equal: no event
    store.remove("missing"); // absent: no event
    store.set_double("x", f64::NAN); // NaN removes
    assert_eq!(store.get_string("x"), None);
    assert_eq!(*events.lock().unwrap(), ["x", "x"]);
    store.flush();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}");
    assert!(tmp_files(dir.path()).is_empty());
}

#[test]
fn json_file_store_paths_instance_id_and_drop_flushes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    {
        let store = JsonFileSettingsStore::open(path.clone(), Duration::from_secs(60), None);
        assert!(store.path().is_absolute());
        assert_eq!(store.instance_id().len(), 32);
        assert!(store.instance_id().chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        store.set_bool("autoPlayVideo", false);
    }
    let reopened = JsonFileSettingsStore::open(path, Duration::from_secs(60), None);
    assert_eq!(reopened.get_bool("autoPlayVideo"), Some(false));
    assert_eq!(JsonFileSettingsStore::default_path(dir.path()), std::path::absolute(dir.path().join("settings.json")).unwrap());
}

#[derive(Default)]
struct FakeHook {
    published: Mutex<Vec<(String, PathBuf)>>,
    reload: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    subscribed: AtomicUsize,
}

impl SettingsSyncHook for FakeHook {
    fn published(&self, instance_id: &str, path: &Path) {
        self.published.lock().unwrap().push((instance_id.to_owned(), path.to_path_buf()));
    }
    fn on_remote_change(&self, reload: Box<dyn Fn() + Send + Sync>) -> Subscription {
        self.subscribed.fetch_add(1, Ordering::SeqCst);
        *self.reload.lock().unwrap() = Some(reload);
        Subscription::empty()
    }
}

#[test]
fn json_file_store_publishes_after_writes_and_reloads_silently() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let hook = Arc::new(FakeHook::default());
    let store = JsonFileSettingsStore::open(path.clone(), Duration::from_secs(60), Some(hook.clone()));
    assert_eq!(hook.subscribed.load(Ordering::SeqCst), 1);
    store.set_string("selectedModel", "small");
    store.flush();
    {
        let published = hook.published.lock().unwrap();
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].0, store.instance_id());
        assert_eq!(published[0].1, store.path());
    }
    store.flush(); // not dirty: no write, no publish
    assert_eq!(hook.published.lock().unwrap().len(), 1);

    // Another instance wrote the file; the broker reload replaces memory without change events.
    let events = Arc::new(AtomicUsize::new(0));
    let e = events.clone();
    let _sub = store.subscribe(Box::new(move |_k: &str| {
        e.fetch_add(1, Ordering::SeqCst);
    }));
    std::fs::write(&path, r#"{"selectedModel":"base","appTheme":"dark"}"#).unwrap();
    (hook.reload.lock().unwrap().as_ref().unwrap())();
    assert_eq!(store.get_string("selectedModel").as_deref(), Some("base"));
    assert_eq!(store.get_string("appTheme").as_deref(), Some("dark"));
    assert_eq!(events.load(Ordering::SeqCst), 0);
}

#[test]
fn multi_instance_concurrent_stores_never_throw_or_corrupt() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let a = JsonFileSettingsStore::open(path.clone(), Duration::from_millis(5), None);
    let b = JsonFileSettingsStore::open(path.clone(), Duration::from_millis(5), None);
    let mut handles = Vec::new();
    for t in 0..12 {
        let store = if t % 2 == 0 { a.clone() } else { b.clone() };
        let p = path.clone();
        handles.push(std::thread::spawn(move || {
            for i in 0..40 {
                store.set_int(&format!("k{t}"), i);
                store.set_double("shared", i as f64 / 7.0);
                if i % 5 == 0 {
                    store.flush();
                }
                let _ = read_all_text_shared(&p, 5);
            }
        }));
    }
    for h in handles {
        h.join().expect("no panic");
    }
    a.dispose();
    b.dispose();
    let text = read_all_text_shared(&path, 5).expect("readable");
    let parsed: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
    assert!(parsed.is_object());
    assert!(tmp_files(dir.path()).is_empty());
}

// ---------------------------------------------------------------- InMemorySettingsStore

#[test]
fn in_memory_store_typed_reads() {
    let store = InMemorySettingsStore::from_json(r#"{"s":"x","b":true,"i":3,"f":2.0,"g":2.5,"t":"true","a":["tr"]}"#);
    assert_eq!(store.get_string("s").as_deref(), Some("x"));
    assert_eq!(store.get_bool("b"), Some(true));
    assert_eq!(store.get_bool("t"), None); // string "true" is not a bool
    assert_eq!(store.get_int("i"), Some(3));
    assert_eq!(store.get_int("f"), Some(2));
    assert_eq!(store.get_int("g"), None);
    assert_eq!(store.get_double("i"), Some(3.0));
    assert_eq!(store.get_string_array("a"), Some(vec!["tr".to_owned()]));
    store.set_double("g", f64::INFINITY);
    assert_eq!(store.raw("g"), None);
}

// ---------------------------------------------------------------- AtomicFileStore

#[test]
fn atomic_write_replaces_and_never_leaves_temp_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a").join("b.json");
    assert!(write_all_text_atomic(&path, "one", 5));
    assert!(write_all_text_atomic(&path, "two ü", 5));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "two ü");
    assert!(tmp_files(path.parent().unwrap()).is_empty());
    assert!(!write_all_text_atomic(Path::new("   "), "x", 5));
    assert_eq!(read_all_text_shared(Path::new(""), 5), None);
    assert_eq!(read_all_text_shared(&dir.path().join("missing.json"), 5), None);
    assert_eq!(read_all_text_shared(&dir.path().join("nodir").join("x.json"), 5), None);
    assert_eq!(read_all_text_shared(dir.path(), 5), None, "a directory is not a file");
}

#[test]
fn atomic_write_to_a_directory_target_fails_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("occupied");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("child"), "x").unwrap();
    assert!(!write_all_text_atomic(&target, "x", 2));
    assert!(tmp_files(dir.path()).is_empty());
}

#[test]
fn decode_with_bom_detects_every_bom() {
    assert_eq!(decode_with_bom(b"plain"), "plain");
    assert_eq!(decode_with_bom(&[0xEF, 0xBB, 0xBF, b'a']), "a");
    assert_eq!(decode_with_bom(&[0xFF, 0xFE, b'a', 0]), "a");
    assert_eq!(decode_with_bom(&[0xFE, 0xFF, 0, b'a']), "a");
    assert_eq!(decode_with_bom(&[0xFF, 0xFE, 0, 0, b'a', 0, 0, 0]), "a");
    assert_eq!(decode_with_bom(&[0, 0, 0xFE, 0xFF, 0, 0, 0, b'a']), "a");
    assert_eq!(decode_with_bom(&[b'a', 0xFF, b'b']), "a\u{FFFD}b");
}

// ---------------------------------------------------------------- parity fixes (C# fixture verified)

#[derive(Default)]
struct EventHook {
    handler: Mutex<Option<Box<dyn Fn(&str, &str) + Send + Sync>>>,
    fallback_used: AtomicUsize,
}

impl SettingsSyncHook for EventHook {
    fn published(&self, _instance_id: &str, _path: &Path) {}
    fn on_remote_change(&self, _reload: Box<dyn Fn() + Send + Sync>) -> Subscription {
        self.fallback_used.fetch_add(1, Ordering::SeqCst);
        Subscription::empty()
    }
    fn on_remote_event(&self, handler: Box<dyn Fn(&str, &str) + Send + Sync>) -> Option<Subscription> {
        *self.handler.lock().unwrap() = Some(handler);
        Some(Subscription::empty())
    }
}

#[test]
fn json_file_store_filters_broker_events_like_csharp() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let hook = Arc::new(EventHook::default());
    let store = JsonFileSettingsStore::open(path.clone(), Duration::from_secs(60), Some(hook.clone()));
    assert_eq!(hook.fallback_used.load(Ordering::SeqCst), 0, "the filtered registration wins");
    std::fs::write(&path, r#"{"appTheme":"dark"}"#).unwrap();
    let fire = |sender: &str, scope: &str| (hook.handler.lock().unwrap().as_ref().unwrap())(sender, scope);
    let own_path = store.path().to_string_lossy().into_owned();

    fire(store.instance_id(), &own_path); // own event: ignored
    assert_eq!(store.get_string("appTheme"), None);
    fire("0123456789abcdef0123456789abcdef", &format!("{own_path}.other")); // other file: ignored
    assert_eq!(store.get_string("appTheme"), None);
    fire("0123456789abcdef0123456789abcdef", &own_path.to_uppercase()); // same file, other case: reloads
    assert_eq!(store.get_string("appTheme").as_deref(), Some("dark"));
    assert!(store.accepts_remote("x", &own_path.to_lowercase()));
    assert!(!store.accepts_remote(store.instance_id(), &own_path));
}

#[test]
fn ints_outside_i32_read_as_missing_and_numbers_compare_by_value() {
    let store = InMemorySettingsStore::from_json(r#"{"big":3000000000,"max":2147483647,"min":-2147483648,"f":3.0,"i":3}"#);
    assert_eq!(store.get_int("big"), None);
    assert_eq!(store.get_double("big"), Some(3e9));
    assert_eq!(store.get_int("max"), Some(i64::from(i32::MAX)));
    assert_eq!(store.get_int("min"), Some(i64::from(i32::MIN)));

    // C# `JsonNode.DeepEquals`: setting 3.0 over a stored 3 (or 3 over 3.0) is no change: no event, no write.
    let events = Arc::new(AtomicUsize::new(0));
    let e = events.clone();
    let _sub = store.subscribe(Box::new(move |_k: &str| {
        e.fetch_add(1, Ordering::SeqCst);
    }));
    store.set_double("i", 3.0);
    store.set_int("f", 3);
    assert_eq!(events.load(Ordering::SeqCst), 0);
    store.set_double("i", 3.5);
    assert_eq!(events.load(Ordering::SeqCst), 1);

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(&path, r#"{"overlayFontSize":1}"#).unwrap();
    let file = JsonFileSettingsStore::open(path.clone(), Duration::from_millis(10), None);
    file.set_double("overlayFontSize", 1.0);
    file.flush();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"overlayFontSize":1}"#, "an equal value never rewrites the file");
}
