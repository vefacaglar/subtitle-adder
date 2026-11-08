//! `PlatformPolicy` (UNIFIED.md §6): one shared view-model core, documented platform behaviors. Tests run both
//! values; `app-core` code never uses `cfg` for these.

use sa_core::l10n::CatalogChain;
use sa_core::subtitle_import::ImportFlavor;

/// Single jobs in the list tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SingleJobsAsListRows {
    /// Windows: every single-video job is a playlist row.
    PlaylistRows,
    /// macOS: `recent_jobs` rows + "Sıradakileri temizle".
    RecentJobs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlaylistAutosave {
    /// Windows: always next to the video.
    Always,
    /// macOS: only when `autoSaveSubtitle`.
    IfEnabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuickListNaming {
    /// Windows `Listem {count+1}`, duplicate → silent.
    CountPlusOne,
    /// macOS `NameSequence` skipping taken names.
    NameSequence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RememberMeStorage {
    /// Windows: wrapped in `profiles.json` (C#-compatible).
    HeaderWrap,
    /// macOS: `profile-remember-keys.json` through the encrypted secret store.
    SecretStore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExplorerCompare {
    /// Windows OrdinalIgnoreCase contains.
    OrdinalIgnoreCase,
    /// macOS case + diacritic + width fold, `ı` → `i`.
    Fold,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingsPage {
    General,
    Translation,
    Appearance,
    Models,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HubDateFormat {
    /// .NET `g` in the **app** UI language (tr `dd.MM.yyyy HH:mm`, en `M/d/yyyy h:mm tt`).
    DotNetGeneral,
    /// Abbreviated date + shortened time in the UI language (macOS).
    AppleAbbreviated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ByteSizeFormat {
    /// `FormatByteSize` (MiB/GiB, UI-language separator).
    WindowsBinary,
    /// `ByteCountFormatter` (decimal, system locale).
    AppleDecimal,
}

/// Every knob of UNIFIED.md §6.
#[derive(Debug, Clone, PartialEq)]
pub struct PlatformPolicy {
    pub catalog_chain: CatalogChain,
    pub delete_wav_on_result: bool,
    pub evict_sessions: bool,
    pub confirm_open_when_losing_work: bool,
    pub playlist_autosave: PlaylistAutosave,
    /// `None` = always ≤1 extraction / ≤1 ASR / ≤1 translation; `Some(bytes)` = local translation overlaps ASR only
    /// with at least that much RAM (or an external provider).
    pub playlist_overlap_requires_memory: Option<u64>,
    pub single_jobs_as_list_rows: SingleJobsAsListRows,
    pub selected_retry_undo_group: bool,
    pub import_json_review_entries: bool,
    pub split_merge_timing_need_single_selection: bool,
    pub ui_font_live: bool,
    pub hub_card_page_size: Option<usize>,
    pub hub_quick_list_naming: QuickListNaming,
    pub remember_me_storage: RememberMeStorage,
    pub explorer_compare: ExplorerCompare,
    pub new_provider_async_default_all: bool,
    pub settings_manage_providers_page: SettingsPage,
    pub hub_date_format: HubDateFormat,
    pub byte_size_format: ByteSizeFormat,
    /// Windows OrdinalIgnoreCase path keys; macOS standardized (case-sensitive) keys.
    pub case_insensitive_paths: bool,
    /// macOS: every playlist item gets its own `<work>/playlist/<hash12>` folder; Windows uses `<work>` itself.
    pub playlist_item_work_dir: bool,
    /// macOS cloud title `"<name>: Otomatik"` without a model; Windows `"<name>"`.
    pub provider_title_auto_model: bool,
    /// Windows finished summary always names a language (`dil: ?`); macOS `%lld satır` without one.
    pub summary_language_placeholder: bool,
    /// Subtitle import rules (macOS: JSON ids renumbered, strict UTF-16, listing-based companion video).
    pub import_flavor: ImportFlavor,
}

impl PlatformPolicy {
    pub fn windows() -> Self {
        Self {
            catalog_chain: CatalogChain::Windows,
            delete_wav_on_result: false,
            evict_sessions: false,
            confirm_open_when_losing_work: false,
            playlist_autosave: PlaylistAutosave::Always,
            playlist_overlap_requires_memory: None,
            single_jobs_as_list_rows: SingleJobsAsListRows::PlaylistRows,
            selected_retry_undo_group: false,
            import_json_review_entries: false,
            split_merge_timing_need_single_selection: false,
            ui_font_live: false,
            hub_card_page_size: Some(40),
            hub_quick_list_naming: QuickListNaming::CountPlusOne,
            remember_me_storage: RememberMeStorage::HeaderWrap,
            explorer_compare: ExplorerCompare::OrdinalIgnoreCase,
            new_provider_async_default_all: true,
            settings_manage_providers_page: SettingsPage::General,
            hub_date_format: HubDateFormat::DotNetGeneral,
            byte_size_format: ByteSizeFormat::WindowsBinary,
            case_insensitive_paths: true,
            playlist_item_work_dir: false,
            provider_title_auto_model: false,
            summary_language_placeholder: true,
            import_flavor: ImportFlavor::Windows,
        }
    }

    pub fn mac() -> Self {
        Self {
            catalog_chain: CatalogChain::Mac,
            delete_wav_on_result: true,
            evict_sessions: true,
            confirm_open_when_losing_work: true,
            playlist_autosave: PlaylistAutosave::IfEnabled,
            playlist_overlap_requires_memory: Some(24 * 1024 * 1024 * 1024),
            single_jobs_as_list_rows: SingleJobsAsListRows::RecentJobs,
            selected_retry_undo_group: true,
            import_json_review_entries: true,
            split_merge_timing_need_single_selection: true,
            ui_font_live: true,
            hub_card_page_size: None,
            hub_quick_list_naming: QuickListNaming::NameSequence,
            remember_me_storage: RememberMeStorage::SecretStore,
            explorer_compare: ExplorerCompare::Fold,
            new_provider_async_default_all: false,
            settings_manage_providers_page: SettingsPage::Translation,
            hub_date_format: HubDateFormat::AppleAbbreviated,
            byte_size_format: ByteSizeFormat::AppleDecimal,
            case_insensitive_paths: false,
            playlist_item_work_dir: true,
            provider_title_auto_model: true,
            summary_language_placeholder: false,
            import_flavor: ImportFlavor::Mac,
        }
    }

    /// The policy of the platform this binary was built for.
    pub fn native() -> Self {
        if cfg!(windows) { Self::windows() } else { Self::mac() }
    }

    /// Session / playlist key of a path (Windows case-insensitive, macOS lexical standardization).
    pub fn path_key(&self, path: &std::path::Path) -> String {
        let standardized = standardize(path);
        if self.case_insensitive_paths { standardized.to_lowercase() } else { standardized }
    }

    /// Path equality under this platform's rule.
    pub fn same_path(&self, a: &std::path::Path, b: &std::path::Path) -> bool {
        self.path_key(a) == self.path_key(b)
    }

    /// Whether a new provider from `preset_id` translates in the background by default.
    pub fn new_provider_async_default(&self, preset_id: &str) -> bool {
        self.new_provider_async_default_all || preset_id.eq_ignore_ascii_case("lan")
    }
}

/// Lexical standardization (`.`/`..` removed, no symlink resolution; Swift `standardizedFileURL`).
pub fn standardize(path: &std::path::Path) -> String {
    use std::path::Component;
    let mut out: Vec<Component> = Vec::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.last(), Some(Component::Normal(_))) {
                    out.pop();
                } else if !matches!(out.last(), Some(Component::RootDir | Component::Prefix(_))) {
                    out.push(c);
                }
            }
            other => out.push(other),
        }
    }
    let p: std::path::PathBuf = out.iter().collect();
    p.to_string_lossy().into_owned()
}
