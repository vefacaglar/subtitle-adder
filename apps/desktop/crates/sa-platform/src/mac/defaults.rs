//! `UserDefaultsSettingsStore` (UNIFIED.md §2, §5.5; mac-viewmodels §1): `CFPreferences*` on the Swift app's domain
//! (`app.subtitleadder.mac` packaged, `SubtitleAdderApp` unbundled) with the same keys and plist types; never writes
//! the plist file directly (cfprefsd caches it). Setters are no-ops when the value is unchanged and are pushed to
//! cfprefsd immediately. External changes (another process, `defaults write`) are found by a 1 s poll of the domain
//! while anyone is subscribed, and reported per key like local changes.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Arc, Weak};
use std::time::Duration;

use core_foundation::array::CFArray;
use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::data::CFData;
use core_foundation::date::CFDate;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::{CFNumber, CFNumberIsFloatType};
use core_foundation::string::{CFString, CFStringRef};
use parking_lot::Mutex;
use sa_core::settings::SettingsStore;
use sa_core::subscription::{Subscribers, Subscription};

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFPreferencesAnyApplication: CFStringRef;
    static kCFPreferencesCurrentUser: CFStringRef;
    static kCFPreferencesAnyHost: CFStringRef;
    fn CFPreferencesCopyAppValue(key: CFStringRef, application: CFStringRef) -> CFTypeRef;
    fn CFPreferencesSetAppValue(key: CFStringRef, value: CFTypeRef, application: CFStringRef);
    fn CFPreferencesAppSynchronize(application: CFStringRef) -> u8;
    fn CFPreferencesCopyValue(key: CFStringRef, application: CFStringRef, user: CFStringRef, host: CFStringRef) -> CFTypeRef;
    fn CFPreferencesCopyMultiple(keys: CFTypeRef, application: CFStringRef, user: CFStringRef, host: CFStringRef) -> CFTypeRef;
}

/// A copied preference value.
pub struct PrefValue(CFType);

impl PrefValue {
    fn from_create(r: CFTypeRef) -> Option<PrefValue> {
        (!r.is_null()).then(|| PrefValue(unsafe { CFType::wrap_under_create_rule(r) }))
    }
    pub fn as_string(&self) -> Option<String> {
        self.0.downcast::<CFString>().map(|s| s.to_string())
    }
    /// `object(forKey:) as? Bool`: a boolean, or a number that is exactly 0 or 1.
    pub fn as_bool(&self) -> Option<bool> {
        if let Some(b) = self.0.downcast::<CFBoolean>() {
            return Some(b.into());
        }
        match self.0.downcast::<CFNumber>().and_then(|n| n.to_f64()) {
            Some(v) if v == 0.0 => Some(false),
            Some(v) if v == 1.0 => Some(true),
            _ => None,
        }
    }
    /// Integral numbers (booleans count as 0/1, like `integer(forKey:)`).
    pub fn as_int(&self) -> Option<i64> {
        if let Some(b) = self.0.downcast::<CFBoolean>() {
            return Some(bool::from(b) as i64);
        }
        let n = self.0.downcast::<CFNumber>()?;
        if is_float(&n) {
            let v = n.to_f64()?;
            (v.is_finite() && v.fract() == 0.0 && v.abs() < 9.0e15).then_some(v as i64)
        } else {
            n.to_i64()
        }
    }
    pub fn as_double(&self) -> Option<f64> {
        if self.0.downcast::<CFBoolean>().is_some() {
            return None;
        }
        self.0.downcast::<CFNumber>()?.to_f64()
    }
    pub fn as_string_array(&self) -> Option<Vec<String>> {
        array_values(&self.0)?.iter().map(|v| v.downcast::<CFString>().map(|s| s.to_string())).collect()
    }
    /// A stable text form for change detection (no pointers).
    pub fn canonical(&self) -> String {
        canonical(&self.0)
    }
}

fn is_float(n: &CFNumber) -> bool {
    unsafe { CFNumberIsFloatType(n.as_concrete_TypeRef()) != 0 }
}

/// The elements of a CFArray value (retained).
fn array_values(v: &CFType) -> Option<Vec<CFType>> {
    let a = v.downcast::<CFArray>()?;
    Some(a.iter().map(|p| unsafe { CFType::wrap_under_get_rule(*p) }).collect())
}

fn canonical(v: &CFType) -> String {
    if let Some(s) = v.downcast::<CFString>() {
        return format!("s:{s}");
    }
    if let Some(b) = v.downcast::<CFBoolean>() {
        return format!("b:{}", bool::from(b));
    }
    if let Some(n) = v.downcast::<CFNumber>() {
        return if is_float(&n) { format!("d:{:?}", n.to_f64()) } else { format!("i:{:?}", n.to_i64()) };
    }
    if let Some(a) = array_values(v) {
        let parts: Vec<String> = a.iter().map(canonical).collect();
        return format!("a:[{}]", parts.join(","));
    }
    if let Some(d) = v.downcast::<CFDictionary>() {
        let (keys, values) = d.get_keys_and_values();
        let mut parts: Vec<String> = keys
            .iter()
            .zip(values.iter())
            .map(|(k, v)| unsafe {
                format!("{}={}", canonical(&CFType::wrap_under_get_rule(*k)), canonical(&CFType::wrap_under_get_rule(*v)))
            })
            .collect();
        parts.sort();
        return format!("m:{{{}}}", parts.join(","));
    }
    if let Some(d) = v.downcast::<CFData>() {
        let bytes = d.bytes();
        let hash = bytes.iter().fold(0xcbf29ce484222325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3));
        return format!("x:{}:{hash:016x}", bytes.len());
    }
    if let Some(d) = v.downcast::<CFDate>() {
        return format!("t:{}", d.abs_time());
    }
    format!("?:{}", v.type_of())
}

/// `CFPreferencesCopyAppValue(key, domain)` (app domain, then the global domain, like `UserDefaults.standard`).
pub fn copy_app_value(domain: &str, key: &str) -> Option<PrefValue> {
    let k = CFString::new(key);
    let d = CFString::new(domain);
    PrefValue::from_create(unsafe { CFPreferencesCopyAppValue(k.as_concrete_TypeRef(), d.as_concrete_TypeRef()) })
}

/// A value of the global domain (`kCFPreferencesAnyApplication`), e.g. `AppleActionOnDoubleClick`.
pub fn copy_any_app_value(key: &str) -> Option<PrefValue> {
    let k = CFString::new(key);
    PrefValue::from_create(unsafe { CFPreferencesCopyAppValue(k.as_concrete_TypeRef(), kCFPreferencesAnyApplication) })
}

/// The system-wide preferred languages: the global domain's `AppleLanguages` for the current user (so the app's own
/// override does not hide the real system setting); empty when unset.
pub fn global_apple_languages() -> Vec<String> {
    let k = CFString::new("AppleLanguages");
    let v = PrefValue::from_create(unsafe {
        CFPreferencesCopyValue(k.as_concrete_TypeRef(), kCFPreferencesAnyApplication, kCFPreferencesCurrentUser, kCFPreferencesAnyHost)
    });
    v.and_then(|v| v.as_string_array()).unwrap_or_default()
}

fn set_raw(domain: &str, key: &str, value: Option<CFType>) {
    let k = CFString::new(key);
    let d = CFString::new(domain);
    unsafe {
        let v = value.as_ref().map(|v| v.as_CFTypeRef()).unwrap_or(std::ptr::null());
        CFPreferencesSetAppValue(k.as_concrete_TypeRef(), v, d.as_concrete_TypeRef());
        CFPreferencesAppSynchronize(d.as_concrete_TypeRef());
    }
}

/// Per-app `AppleLanguages` (`None` removes it).
pub fn set_app_string_array(domain: &str, key: &str, value: Option<&[String]>) {
    let v = value.map(|items| {
        let strings: Vec<CFString> = items.iter().map(|s| CFString::new(s)).collect();
        CFArray::from_CFTypes(&strings).as_CFType()
    });
    set_raw(domain, key, v);
}

/// Every key of the domain (current user, any host) in canonical form.
fn snapshot(domain: &str) -> HashMap<String, String> {
    let d = CFString::new(domain);
    let mut out = HashMap::new();
    unsafe {
        CFPreferencesAppSynchronize(d.as_concrete_TypeRef());
        let dict = CFPreferencesCopyMultiple(std::ptr::null(), d.as_concrete_TypeRef(), kCFPreferencesCurrentUser, kCFPreferencesAnyHost);
        if dict.is_null() {
            return out;
        }
        let dict: CFDictionary = CFDictionary::wrap_under_create_rule(dict as _);
        let (keys, values) = dict.get_keys_and_values();
        for (k, v) in keys.iter().zip(values.iter()) {
            let k = CFType::wrap_under_get_rule(*k as *const c_void);
            if let Some(name) = k.downcast::<CFString>() {
                out.insert(name.to_string(), canonical(&CFType::wrap_under_get_rule(*v as *const c_void)));
            }
        }
    }
    out
}

pub struct UserDefaultsSettingsStore {
    domain: String,
    changed: Subscribers<String>,
    /// Keys written by this store with their canonical value, so the poll does not report them again.
    own_writes: Mutex<HashMap<String, Option<String>>>,
    poller: Mutex<bool>,
    weak: Weak<UserDefaultsSettingsStore>,
}

impl UserDefaultsSettingsStore {
    pub fn new(domain: &str) -> Arc<Self> {
        Arc::new_cyclic(|weak| Self {
            domain: domain.to_owned(),
            changed: Subscribers::new(),
            own_writes: Mutex::new(HashMap::new()),
            poller: Mutex::new(false),
            weak: weak.clone(),
        })
    }

    /// The store of this process' domain ([`super::MacShell::defaults_domain`]).
    pub fn standard() -> Arc<Self> {
        Self::new(super::MacShell::defaults_domain())
    }

    pub fn domain(&self) -> &str {
        &self.domain
    }

    fn value(&self, key: &str) -> Option<PrefValue> {
        copy_app_value(&self.domain, key)
    }

    fn put(&self, key: &str, value: Option<CFType>) {
        let new = value.as_ref().map(canonical);
        let old = self.value(key).map(|v| v.canonical());
        if old == new {
            return;
        }
        set_raw(&self.domain, key, value);
        self.own_writes.lock().insert(key.to_owned(), new);
        self.changed.notify(&key.to_owned());
    }

    fn start_poller(&self) {
        let mut started = self.poller.lock();
        if *started {
            return;
        }
        *started = true;
        let weak = self.weak.clone();
        let domain = self.domain.clone();
        let _ = std::thread::Builder::new().name("sa-defaults-watch".into()).spawn(move || {
            let mut last = snapshot(&domain);
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let Some(store) = weak.upgrade() else { return };
                let now = snapshot(&domain);
                let mut keys: Vec<&String> = now.keys().chain(last.keys()).collect();
                keys.sort();
                keys.dedup();
                let mut changed = Vec::new();
                {
                    let mut own = store.own_writes.lock();
                    for k in keys {
                        let (a, b) = (last.get(k), now.get(k));
                        if a == b {
                            continue;
                        }
                        if let Some(written) = own.get(k)
                            && written.as_ref() == b
                        {
                            own.remove(k);
                            continue;
                        }
                        changed.push(k.clone());
                    }
                }
                for k in changed {
                    store.changed.notify(&k);
                }
                last = now;
            }
        });
    }
}

impl SettingsStore for UserDefaultsSettingsStore {
    fn get_string(&self, key: &str) -> Option<String> {
        self.value(key)?.as_string()
    }
    fn get_bool(&self, key: &str) -> Option<bool> {
        self.value(key)?.as_bool()
    }
    fn get_int(&self, key: &str) -> Option<i64> {
        self.value(key)?.as_int()
    }
    fn get_double(&self, key: &str) -> Option<f64> {
        self.value(key)?.as_double()
    }
    fn get_string_array(&self, key: &str) -> Option<Vec<String>> {
        self.value(key)?.as_string_array()
    }
    fn set_string(&self, key: &str, value: &str) {
        self.put(key, Some(CFString::new(value).as_CFType()));
    }
    fn set_bool(&self, key: &str, value: bool) {
        let b = if value { CFBoolean::true_value() } else { CFBoolean::false_value() };
        self.put(key, Some(b.as_CFType()));
    }
    fn set_int(&self, key: &str, value: i64) {
        self.put(key, Some(CFNumber::from(value).as_CFType()));
    }
    fn set_double(&self, key: &str, value: f64) {
        if value.is_nan() {
            self.remove(key);
        } else {
            self.put(key, Some(CFNumber::from(value).as_CFType()));
        }
    }
    fn set_string_array(&self, key: &str, value: Option<&[String]>) {
        let v = value.map(|items| {
            let strings: Vec<CFString> = items.iter().map(|s| CFString::new(s)).collect();
            CFArray::from_CFTypes(&strings).as_CFType()
        });
        self.put(key, v);
    }
    fn remove(&self, key: &str) {
        self.put(key, None);
    }
    fn subscribe(&self, handler: Box<dyn Fn(&str) + Send + Sync>) -> Subscription {
        self.start_poller();
        self.changed.subscribe(move |k: &String| handler(k))
    }
    fn flush(&self) {
        let d = CFString::new(&self.domain);
        unsafe {
            CFPreferencesAppSynchronize(d.as_concrete_TypeRef());
        }
    }
}
