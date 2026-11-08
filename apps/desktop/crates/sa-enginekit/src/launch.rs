//! Process-wide launch settings (offline mode, active external LLM) applied to every engine launch
//! (mac-enginekit §6, win-enginekit §9, UNIFIED.md §3.7: external LLM variables reach `translate` only).

use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;

/// The active external LLM (URL as typed, model, API key in memory only — persisted encrypted by `sa-providers`).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ExternalLlm {
    pub url: String,
    pub model: String,
    pub api_key: String,
}

impl std::fmt::Debug for ExternalLlm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalLlm")
            .field("url", &self.url)
            .field("model", &self.model)
            .field("api_key", &if self.api_key.is_empty() { "" } else { "[redacted]" })
            .finish()
    }
}

impl ExternalLlm {
    pub fn normalized_url(&self) -> Option<String> {
        crate::external_llm::ExternalLlmServer::normalize_base_url(&self.url)
    }
    pub fn is_on_local_network(&self) -> bool {
        self.normalized_url()
            .and_then(|u| crate::external_llm::ExternalLlmServer::host(&u))
            .is_some_and(|h| crate::external_llm::ExternalLlmServer::is_local_network_host(&h))
    }
}

/// Launch settings (defaults: offline false, no external LLM; the app sets offline from `offlineMode`, default true).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EngineLaunchSettings {
    pub offline: bool,
    pub external_llm: Option<ExternalLlm>,
}

impl EngineLaunchSettings {
    /// Always sets `SUBTITLE_ENGINE_OFFLINE` (`1`/`0`). Only for translate with a usable URL: sets
    /// `SUBTITLE_ENGINE_EXTERNAL_LLM_URL` (normalized), `..._MODEL` (trimmed, even empty), `..._API_KEY` (trimmed;
    /// removed when empty). Otherwise removes all three, inherited ones included.
    pub fn apply(&self, env: &mut Vec<(String, String)>, for_translation: bool) {
        set_env(env, OFFLINE_VAR, if self.offline { "1" } else { "0" });
        let active =
            if for_translation { self.external_llm.as_ref().and_then(|l| l.normalized_url().map(|u| (l, u))) } else { None };
        match active {
            Some((llm, url)) => {
                set_env(env, EXTERNAL_LLM_URL_VAR, &url);
                set_env(env, EXTERNAL_LLM_MODEL_VAR, llm.model.trim());
                let key = llm.api_key.trim();
                if key.is_empty() {
                    remove_env(env, EXTERNAL_LLM_API_KEY_VAR);
                } else {
                    set_env(env, EXTERNAL_LLM_API_KEY_VAR, key);
                }
            }
            None => {
                remove_env(env, EXTERNAL_LLM_URL_VAR);
                remove_env(env, EXTERNAL_LLM_MODEL_VAR);
                remove_env(env, EXTERNAL_LLM_API_KEY_VAR);
            }
        }
    }
}

pub const LANG_VAR: &str = "SUBTITLE_ENGINE_LANG";
pub const OFFLINE_VAR: &str = "SUBTITLE_ENGINE_OFFLINE";
pub const EXTERNAL_LLM_URL_VAR: &str = "SUBTITLE_ENGINE_EXTERNAL_LLM_URL";
pub const EXTERNAL_LLM_MODEL_VAR: &str = "SUBTITLE_ENGINE_EXTERNAL_LLM_MODEL";
pub const EXTERNAL_LLM_API_KEY_VAR: &str = "SUBTITLE_ENGINE_EXTERNAL_LLM_API_KEY";

/// Environment names compare case-insensitively on Windows, exactly elsewhere.
pub fn env_name_eq(a: &str, b: &str) -> bool {
    if cfg!(windows) { a.eq_ignore_ascii_case(b) } else { a == b }
}

/// The value of `name` (exact match first, then a case-insensitive one on Windows).
pub fn get_env<'a>(env: &'a [(String, String)], name: &str) -> Option<&'a str> {
    env.iter().find(|(k, _)| k == name).or_else(|| env.iter().find(|(k, _)| env_name_eq(k, name))).map(|(_, v)| v.as_str())
}

/// Sets `name`, replacing every existing spelling of it.
pub fn set_env(env: &mut Vec<(String, String)>, name: &str, value: &str) {
    remove_env(env, name);
    env.push((name.to_owned(), value.to_owned()));
}

/// Removes every spelling of `name`.
pub fn remove_env(env: &mut Vec<(String, String)>, name: &str) {
    env.retain(|(k, _)| !env_name_eq(k, name));
}

/// Process-wide `Mutex<EngineLaunchSettings>` (no I/O). Updated when the offline switch or the active provider
/// changes; read at every launch and by the diagnostic log's secret list.
pub struct EngineLaunchState {
    inner: Mutex<EngineLaunchSettings>,
    /// The last external-LLM address set (kept while the provider is disabled, like the C# `ExternalLlmState.Url`).
    last_url: Mutex<String>,
}

impl EngineLaunchState {
    pub fn new(initial: EngineLaunchSettings) -> Arc<Self> {
        let last_url = initial.external_llm.as_ref().map(|l| l.url.clone()).unwrap_or_default();
        Arc::new(Self { inner: Mutex::new(initial), last_url: Mutex::new(last_url) })
    }
    /// Singleton; initially `offline = true` (strict offline is the app default).
    pub fn shared() -> &'static Arc<EngineLaunchState> {
        static S: OnceLock<Arc<EngineLaunchState>> = OnceLock::new();
        S.get_or_init(|| EngineLaunchState::new(EngineLaunchSettings { offline: true, external_llm: None }))
    }
    pub fn get(&self) -> EngineLaunchSettings {
        self.inner.lock().clone()
    }
    /// Replaces the settings. A different external-LLM address than the last one set forgets the cached `.local` LAN
    /// address (C# `ExternalLlmState.Url` setter).
    pub fn set(&self, v: EngineLaunchSettings) {
        let mut inner = self.inner.lock();
        self.note_url(v.external_llm.as_ref());
        *inner = v;
    }
    pub fn set_offline(&self, offline: bool) {
        self.inner.lock().offline = offline;
    }
    /// Replaces the active external LLM (see [`set`](Self::set) for the cached LAN address).
    pub fn set_external_llm(&self, llm: Option<ExternalLlm>) {
        let mut inner = self.inner.lock();
        self.note_url(llm.as_ref());
        inner.external_llm = llm;
    }

    /// A different address forgets the cached `.local` LAN address. `None` (no active provider) counts as the empty
    /// address, as the C# `TranslationProviderManager` disables the state by setting `Url = ""`.
    fn note_url(&self, llm: Option<&ExternalLlm>) {
        let url = llm.map_or("", |l| l.url.as_str());
        let mut last = self.last_url.lock();
        if *last != url {
            crate::external_llm::clear_cached_lan_ip();
            *last = url.to_owned();
        }
    }
    /// Keys to redact in diagnostics.
    pub fn secrets(&self) -> Vec<String> {
        self.inner.lock().external_llm.as_ref().map(|l| l.api_key.trim().to_owned()).filter(|k| !k.is_empty()).into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(settings: &EngineLaunchSettings, base: &[(&str, &str)], for_translation: bool) -> Vec<(String, String)> {
        let mut e: Vec<(String, String)> = base.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
        settings.apply(&mut e, for_translation);
        e
    }

    fn llm(url: &str, model: &str, key: &str) -> Option<ExternalLlm> {
        Some(ExternalLlm { url: url.into(), model: model.into(), api_key: key.into() })
    }

    /// Serializes the tests that change the process-wide cached LAN address.
    static LAN_CACHE: Mutex<()> = Mutex::new(());

    const NAMES: [&str; 3] = [EXTERNAL_LLM_URL_VAR, EXTERNAL_LLM_MODEL_VAR, EXTERNAL_LLM_API_KEY_VAR];

    #[test]
    fn only_translate_gets_the_external_server_and_its_key() {
        let settings =
            EngineLaunchSettings { offline: true, external_llm: llm("https://api.openai.com/v1", "gpt-4o-mini", "sk-secret") };
        let inherited: Vec<(&str, &str)> = NAMES.iter().map(|n| (*n, "inherited")).collect();
        let other = env(&settings, &inherited, false);
        for name in NAMES {
            assert_eq!(get_env(&other, name), None, "{name}");
        }
        assert_eq!(get_env(&other, OFFLINE_VAR), Some("1"));
        let translating = env(&settings, &[], true);
        assert_eq!(get_env(&translating, EXTERNAL_LLM_API_KEY_VAR), Some("sk-secret"));
        assert_eq!(get_env(&translating, EXTERNAL_LLM_URL_VAR), Some("https://api.openai.com/v1"));
        assert_eq!(get_env(&translating, EXTERNAL_LLM_MODEL_VAR), Some("gpt-4o-mini"));
    }

    #[test]
    fn offline_flag_is_always_set() {
        let on = EngineLaunchSettings { offline: true, external_llm: None };
        let off = EngineLaunchSettings { offline: false, external_llm: None };
        assert_eq!(get_env(&env(&on, &[], true), OFFLINE_VAR), Some("1"));
        assert_eq!(get_env(&env(&off, &[], true), OFFLINE_VAR), Some("0"));
        assert_eq!(get_env(&env(&off, &[(OFFLINE_VAR, "1")], false), OFFLINE_VAR), Some("0"));
    }

    #[test]
    fn external_llm_is_normalized_and_model_trimmed() {
        let s = EngineLaunchSettings {
            offline: false,
            external_llm: llm(" 192.168.1.50:1234/v1/chat/completions/ ", "  gemma4:e4b ", ""),
        };
        let e = env(&s, &[], true);
        assert_eq!(get_env(&e, EXTERNAL_LLM_URL_VAR), Some("http://192.168.1.50:1234"));
        assert_eq!(get_env(&e, EXTERNAL_LLM_MODEL_VAR), Some("gemma4:e4b"));
    }

    #[test]
    fn empty_model_is_still_set() {
        let s = EngineLaunchSettings { offline: false, external_llm: llm("http://mac.local:11434", "", "") };
        let e = env(&s, &[], true);
        assert_eq!(get_env(&e, EXTERNAL_LLM_URL_VAR), Some("http://mac.local:11434"));
        assert_eq!(get_env(&e, EXTERNAL_LLM_MODEL_VAR), Some(""));
    }

    #[test]
    fn inherited_external_variables_are_removed_when_inactive() {
        let inherited = [
            (EXTERNAL_LLM_URL_VAR, "http://old:1"),
            (EXTERNAL_LLM_MODEL_VAR, "old"),
            (EXTERNAL_LLM_API_KEY_VAR, "sk-old"),
            ("PATH", "/bin"),
        ];
        let blank = EngineLaunchSettings { offline: false, external_llm: llm("   ", "", "sk-new") };
        for s in [EngineLaunchSettings::default(), blank] {
            let e = env(&s, &inherited, true);
            for name in NAMES {
                assert_eq!(get_env(&e, name), None);
            }
            assert_eq!(get_env(&e, "PATH"), Some("/bin"));
        }
    }

    #[test]
    fn api_key_is_trimmed_and_a_blank_one_removes_an_inherited_one() {
        let s =
            EngineLaunchSettings { offline: false, external_llm: llm("https://api.openai.com/v1", "gpt-4o-mini", "  sk-secret\n") };
        assert_eq!(get_env(&env(&s, &[], true), EXTERNAL_LLM_API_KEY_VAR), Some("sk-secret"));
        for key in ["", "   \n"] {
            let s = EngineLaunchSettings { offline: false, external_llm: llm("10.0.0.2:11434", "", key) };
            let e = env(&s, &[(EXTERNAL_LLM_API_KEY_VAR, "sk-old")], true);
            assert_eq!(get_env(&e, EXTERNAL_LLM_URL_VAR), Some("http://10.0.0.2:11434"));
            assert_eq!(get_env(&e, EXTERNAL_LLM_API_KEY_VAR), None);
        }
        assert_eq!(ExternalLlm { url: "x".into(), ..Default::default() }.api_key, "");
    }

    #[cfg(windows)]
    #[test]
    fn windows_variable_names_are_case_insensitive() {
        let s = EngineLaunchSettings { offline: true, external_llm: None };
        let e = env(&s, &[("subtitle_engine_external_llm_api_key", "sk-old"), ("subtitle_engine_offline", "0")], false);
        assert_eq!(e.iter().filter(|(k, _)| k.eq_ignore_ascii_case(OFFLINE_VAR)).count(), 1);
        assert_eq!(get_env(&e, OFFLINE_VAR), Some("1"));
        assert_eq!(get_env(&e, EXTERNAL_LLM_API_KEY_VAR), None);
    }

    #[test]
    fn the_debug_form_never_shows_the_key() {
        let l = ExternalLlm { url: "u".into(), model: "m".into(), api_key: "sk-secret".into() };
        assert!(!format!("{l:?}").contains("sk-secret"));
    }

    #[test]
    fn secrets_list_the_trimmed_active_key() {
        let _cache = LAN_CACHE.lock();
        let state = EngineLaunchState::new(EngineLaunchSettings { offline: true, external_llm: llm("u", "m", " sk-1 ") });
        assert_eq!(state.secrets(), vec!["sk-1".to_owned()]);
        state.set_external_llm(None);
        assert!(state.secrets().is_empty());
    }

    #[test]
    fn another_address_or_disabling_forgets_the_cached_lan_ip() {
        let _cache = LAN_CACHE.lock();
        use crate::external_llm::{cached_lan_ip, set_cached_lan_ip};
        let state = EngineLaunchState::new(EngineLaunchSettings { offline: true, external_llm: llm("mac.local:1234", "", "") });
        set_cached_lan_ip("192.168.1.5");
        state.set_external_llm(llm("mac.local:1234", "other-model", ""));
        state.set_offline(false);
        assert_eq!(cached_lan_ip().as_deref(), Some("192.168.1.5"));
        state.set(EngineLaunchSettings { offline: true, external_llm: llm("mac.local:1234", "", "sk") });
        assert_eq!(cached_lan_ip().as_deref(), Some("192.168.1.5"));
        state.set_external_llm(llm("studio.local:1234", "", ""));
        assert_eq!(cached_lan_ip(), None);
        // C# `TranslationProviderManager` disables with `Url = ""`, which forgets the address too.
        set_cached_lan_ip("192.168.1.6");
        state.set_external_llm(None);
        assert_eq!(cached_lan_ip(), None);
        set_cached_lan_ip("192.168.1.7");
        state.set_external_llm(None);
        assert_eq!(cached_lan_ip().as_deref(), Some("192.168.1.7"));
    }

    #[test]
    fn the_shared_state_starts_offline() {
        // Strict offline mode is the app default (`offlineMode` = true).
        assert!(EngineLaunchState::new(EngineLaunchSettings { offline: true, external_llm: None }).get().offline);
    }
}
