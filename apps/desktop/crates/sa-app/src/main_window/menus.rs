//! Drop-down and context menus of the main window, rebuilt on every opening (win-ui-main §6.2.1, §6.5.6, §6.6;
//! mac-ui §6.4, §6.5, §7.6, §10.1). Pure builders over plain data; the controller converts them to
//! `ui::MenuEntry` and dispatches the chosen id ([`MenuAction::parse`]).

use sa_core::engine_types::EngineModel;
use sa_core::models::{ModelLabel, SubtitleLanguage};
use sa_viewmodels::media_tracks::{TrackChoice, TrackItem, TrackMenu};

/// One menu row (mirrors `ui::MenuEntry`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Entry {
    pub id: String,
    pub title: String,
    pub glyph: String,
    pub symbol: String,
    /// RGB tint (speaker colors), `None` = text color.
    pub tint: Option<(u8, u8, u8)>,
    pub checkable: bool,
    pub checked: bool,
    pub radio: bool,
    pub enabled: bool,
    pub separator: bool,
    pub destructive: bool,
    pub hint: String,
    pub level: i32,
    pub submenu: bool,
    /// A section caption (not clickable).
    pub header: bool,
}

impl Entry {
    pub fn item(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self { id: id.into(), title: title.into(), enabled: true, ..Default::default() }
    }
    pub fn separator() -> Self {
        Self { separator: true, ..Default::default() }
    }
    pub fn header(title: impl Into<String>) -> Self {
        Self { title: title.into(), header: true, ..Default::default() }
    }
    fn radio(mut self, checked: bool) -> Self {
        self.checkable = true;
        self.radio = true;
        self.checked = checked;
        self
    }
    fn check(mut self, checked: bool) -> Self {
        self.checkable = true;
        self.checked = checked;
        self
    }
    fn enabled(mut self, e: bool) -> Self {
        self.enabled = e;
        self
    }
    fn glyph(mut self, g: char) -> Self {
        self.glyph = g.to_string();
        self
    }
    fn symbol(mut self, s: &str) -> Self {
        self.symbol = s.to_owned();
        self
    }
    fn level(mut self, l: i32) -> Self {
        self.level = l;
        self
    }
}

/// What a chosen menu id asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum MenuAction {
    Model(String),
    ToggleDiarize,
    SpeakerCount(i64),
    /// (row index, speaker)
    SetSpeaker(usize, Option<i64>),
    NewSpeaker(usize),
    RowRetranslate(usize),
    RowSplit(usize),
    RowMerge(usize),
    RowDelete(usize),
    AllPlaylistLanguages(String),
    /// (queue row index, language code)
    QueueRowLanguage(usize, String),
    Display(i32),
    QuickLocation(usize),
    ExplorerPlay(usize),
    ExplorerAddToPlaylist(usize),
    ExplorerReveal(usize),
    ExplorerOpenFolder(usize),
    /// macOS toolbar overflow item (the toolbar's item index).
    Overflow(usize),
    /// Player track menu: an audio track or an embedded subtitle track.
    Track(TrackChoice),
}

impl MenuAction {
    pub fn parse(id: &str) -> Option<Self> {
        let (head, rest) = id.split_once(':').unwrap_or((id, ""));
        let idx = |s: &str| s.parse::<usize>().ok();
        Some(match head {
            "model" => MenuAction::Model(rest.to_owned()),
            "diarize" => MenuAction::ToggleDiarize,
            "speakers" => MenuAction::SpeakerCount(rest.parse().ok()?),
            "spk" => {
                let (row, s) = rest.split_once(':')?;
                MenuAction::SetSpeaker(idx(row)?, if s == "none" { None } else { Some(s.parse().ok()?) })
            }
            "spk-new" => MenuAction::NewSpeaker(idx(rest)?),
            "row-retranslate" => MenuAction::RowRetranslate(idx(rest)?),
            "row-split" => MenuAction::RowSplit(idx(rest)?),
            "row-merge" => MenuAction::RowMerge(idx(rest)?),
            "row-delete" => MenuAction::RowDelete(idx(rest)?),
            "lang-all" => MenuAction::AllPlaylistLanguages(rest.to_owned()),
            "row-lang" => {
                let (row, code) = rest.split_once(':')?;
                MenuAction::QueueRowLanguage(idx(row)?, code.to_owned())
            }
            "display" => MenuAction::Display(rest.parse().ok()?),
            "quick" => MenuAction::QuickLocation(idx(rest)?),
            "ex-play" => MenuAction::ExplorerPlay(idx(rest)?),
            "ex-add" => MenuAction::ExplorerAddToPlaylist(idx(rest)?),
            "ex-reveal" => MenuAction::ExplorerReveal(idx(rest)?),
            "ex-open" => MenuAction::ExplorerOpenFolder(idx(rest)?),
            "ovf" => MenuAction::Overflow(idx(rest)?),
            "audio" | "subtitle" => MenuAction::Track(TrackChoice::parse(id)?),
            _ => return None,
        })
    }
}

/// Toolbar model menu: Whisper models (`detects_language`) first, a separator only when both groups exist, then the
/// others; the current one is checked (radio on Windows, check mark on macOS).
pub fn model_menu(models: &[EngineModel], current: &str, mac: bool) -> Vec<Entry> {
    let (whisper, other): (Vec<&EngineModel>, Vec<&EngineModel>) = models.iter().partition(|m| m.detects_language());
    let row = |m: &EngineModel| {
        let e = Entry::item(format!("model:{}", m.id), ModelLabel::menu_title(m));
        if mac { e.check(m.id == current) } else { e.radio(m.id == current) }
    };
    let mut out: Vec<Entry> = whisper.iter().map(|m| row(m)).collect();
    if !whisper.is_empty() && !other.is_empty() {
        out.push(Entry::separator());
    }
    out.extend(other.iter().map(|m| row(m)));
    out
}

/// `SpeakerCountTitle(n)`: `0` → `Otomatik`, else the number.
pub fn speaker_count_title(n: i64) -> String {
    if n == 0 { sa_core::l("Otomatik") } else { n.to_string() }
}

/// Toolbar speakers menu: `Konuşmacıları ayır` toggle + `Konuşmacı sayısı` submenu (enabled only when diarizing).
pub fn speakers_menu(diarize: bool, count: i64) -> Vec<Entry> {
    let mut out = vec![
        Entry::item("diarize", sa_core::l("Konuşmacıları ayır")).check(diarize),
        Entry { submenu: true, ..Entry::item("speaker-count", sa_core::l("Konuşmacı sayısı")).enabled(diarize) },
    ];
    for n in [0, 2, 3, 4, 5, 6] {
        out.push(Entry::item(format!("speakers:{n}"), speaker_count_title(n)).radio(count == n).enabled(diarize).level(1));
    }
    out
}

/// `L("Konuşmacı %lld", s + 1)`.
pub fn speaker_title(s: i64) -> String {
    sa_core::l!("Konuşmacı %lld", s + 1)
}

/// Windows compact speaker button text: `L("K%lld", n+1)` (`S1` in English), `—` without a speaker.
pub fn compact_speaker_title(s: Option<i64>) -> String {
    match s {
        Some(s) => sa_core::l!("K%lld", s + 1),
        None => "—".to_owned(),
    }
}

/// Speaker choices for a line (`AddSpeakerItems`): one item per speaker, separator, `Yeni konuşmacı`,
/// `Konuşmacıyı kaldır` (enabled iff the line has a speaker).
pub fn speaker_items(row: usize, speakers: &[i64], line_speaker: Option<i64>, dark: bool, mac: bool) -> Vec<Entry> {
    let mut out = Vec::new();
    for &s in speakers {
        let c = sa_core::appearance::SpeakerPalette::color(s, dark);
        let mut e = Entry::item(format!("spk:{row}:{s}"), speaker_title(s));
        e = if mac { e.check(line_speaker == Some(s)).symbol("circle.fill") } else { e.radio(line_speaker == Some(s)).glyph('\u{EA3B}') };
        e.tint = Some((c.r, c.g, c.b));
        out.push(e);
    }
    if !speakers.is_empty() {
        out.push(Entry::separator());
    }
    out.push(Entry::item(format!("spk-new:{row}"), sa_core::l("Yeni konuşmacı")).glyph('\u{E8FA}').symbol("person.badge.plus"));
    out.push(Entry::item(format!("spk:{row}:none"), sa_core::l("Konuşmacıyı kaldır")).enabled(line_speaker.is_some()));
    out
}

/// What the row context menu needs to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RowMenuState {
    pub can_retranslate: bool,
    pub can_split: bool,
    pub can_merge: bool,
    pub can_delete: bool,
    pub shows_speakers: bool,
    pub is_working: bool,
}

/// Row context menu: Windows (§6.5.6) puts the speaker items before Böl/Birleştir/Sil; macOS (§7.6) after them, with a
/// destructive `Sil`.
pub fn row_menu(row: usize, st: RowMenuState, speakers: Vec<Entry>, mac: bool) -> Vec<Entry> {
    let retranslate = Entry::item(format!("row-retranslate:{row}"), sa_core::l("Seçilenleri yeniden çevir"))
        .glyph('\u{F2B7}')
        .symbol("arrow.triangle.2.circlepath")
        .enabled(if mac { !st.is_working && st.can_retranslate } else { st.can_retranslate });
    let split = Entry::item(format!("row-split:{row}"), sa_core::l("Böl")).glyph('\u{E8C6}').symbol("scissors").enabled(st.can_split);
    let merge = Entry::item(format!("row-merge:{row}"), sa_core::l("Sonrakiyle birleştir")).glyph('\u{E71B}').symbol("arrow.triangle.merge").enabled(st.can_merge);
    let mut delete = Entry::item(format!("row-delete:{row}"), sa_core::l("Sil")).glyph('\u{E74D}').symbol("trash");
    let mut out = vec![retranslate, Entry::separator()];
    if mac {
        delete.destructive = true;
        delete.enabled = st.can_delete;
        out.extend([split, merge, delete]);
        if st.shows_speakers {
            out.push(Entry::separator());
            out.extend(speakers);
        }
    } else {
        delete.hint = "Delete".to_owned();
        if st.shows_speakers {
            out.extend(speakers);
            out.push(Entry::separator());
        }
        out.extend([split, merge, delete]);
    }
    out
}

/// Playlist bulk language menu (plain items) and per-row language menu (radio).
pub fn language_menu(id_prefix: &str, current: Option<&str>) -> Vec<Entry> {
    SubtitleLanguage::ALL
        .iter()
        .map(|lang| {
            let e = Entry::item(format!("{id_prefix}{}", lang.code), lang.title());
            match current {
                Some(c) => e.radio(c == lang.code),
                None => e,
            }
        })
        .collect()
}

/// macOS compact display picker (`Orijinal`, translation title, `İkisi`).
pub fn display_menu(current: i32, translation_title: &str, has_translations: bool) -> Vec<Entry> {
    [sa_core::l("Orijinal"), translation_title.to_owned(), sa_core::l("İkisi")]
        .into_iter()
        .enumerate()
        .map(|(i, t)| Entry::item(format!("display:{i}"), t).check(current == i as i32).enabled(i == 0 || has_translations))
        .collect()
}

/// Explorer file row menu: Windows `Oynat` / `Çalma listesine ekle` / — / `Dosya konumunu aç`; macOS `Oynat` /
/// `Çalma listesine ekle` / — / `Finder'da göster`. Folder rows: `Dosya Gezgini'nde aç` / `Finder'da aç`.
pub fn explorer_row_menu(row: usize, is_folder: bool, mac: bool) -> Vec<Entry> {
    if is_folder {
        let title = if mac { sa_core::l("Finder'da aç") } else { sa_core::l("Dosya Gezgini'nde aç") };
        return vec![Entry::item(format!("ex-open:{row}"), title).glyph('\u{E8DA}').symbol("folder")];
    }
    vec![
        Entry::item(format!("ex-play:{row}"), sa_core::l("Oynat")).glyph('\u{E768}').symbol("play.fill"),
        Entry::item(format!("ex-add:{row}"), sa_core::l("Çalma listesine ekle")).glyph('\u{E90B}').symbol("text.badge.plus"),
        Entry::separator(),
        if mac {
            Entry::item(format!("ex-reveal:{row}"), sa_core::l("Finder'da göster")).symbol("magnifyingglass")
        } else {
            Entry::item(format!("ex-reveal:{row}"), sa_core::l("Dosya konumunu aç")).glyph('\u{E8DA}')
        },
    ]
}

/// Player track menu (`sa_viewmodels::media_tracks::TrackMenu`): an `Ses` section with one entry per audio track (the
/// playing one marked: radio on Windows, check mark on macOS), then, when the file has any, a `Videodaki altyazılar`
/// section ("Off" first while the list holds an unedited embedded track, the embedded track in the list checked; image
/// tracks disabled with their hint).
pub fn track_menu(menu: &TrackMenu, mac: bool) -> Vec<Entry> {
    let row = |i: &TrackItem, radio: bool| {
        let mut e = Entry::item(i.id.clone(), i.label.clone()).enabled(i.enabled);
        e = if radio && !mac { e.radio(i.active) } else { e.check(i.active) };
        e.hint = i.hint.clone();
        e
    };
    let mut out = Vec::new();
    if !menu.audio.is_empty() {
        out.push(Entry::header(sa_core::l("Ses")));
        out.extend(menu.audio.iter().map(|i| row(i, true)));
    }
    if !menu.subtitles.is_empty() {
        if !out.is_empty() {
            out.push(Entry::separator());
        }
        out.push(Entry::header(sa_core::l("Videodaki altyazılar")));
        out.extend(menu.subtitles.iter().map(|i| row(i, false)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_menu_has_an_audio_and_a_subtitle_section() {
        let item = |id: &str, active: bool, enabled: bool| TrackItem { id: id.into(), label: id.into(), active, enabled, hint: if enabled { String::new() } else { "x".into() } };
        let menu = TrackMenu { audio: vec![item("audio:1", false, true), item("audio:2", true, true)], subtitles: vec![item("subtitle:3", false, false)] };
        let win = track_menu(&menu, false);
        let ids: Vec<&str> = win.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["", "audio:1", "audio:2", "", "", "subtitle:3"]);
        assert!(win[0].header && !win[0].enabled);
        assert!(win[2].checked && win[2].radio && !win[1].checked);
        assert!(win[3].separator && win[4].header);
        assert!(!win[5].enabled && win[5].hint == "x" && win[5].checkable && !win[5].radio);
        let mac = track_menu(&menu, true);
        assert!(mac[2].checked && !mac[2].radio, "macOS marks with a check");
        let only_audio = track_menu(&TrackMenu { audio: menu.audio.clone(), subtitles: vec![] }, false);
        assert_eq!(only_audio.len(), 3, "no subtitle section without subtitle tracks");
        assert_eq!(MenuAction::parse("audio:2"), Some(MenuAction::Track(TrackChoice::Audio(2))));
        assert_eq!(MenuAction::parse("subtitle:3"), Some(MenuAction::Track(TrackChoice::Subtitle(3))));
        assert_eq!(MenuAction::parse("subtitle:off"), Some(MenuAction::Track(TrackChoice::SubtitleOff)));
    }

    fn model(id: &str, backend: &str, downloaded: bool) -> EngineModel {
        let mut m = EngineModel::new(id, 100, 3, 3, downloaded);
        m.backend = backend.into();
        m.kind = "transcription".into();
        m
    }

    #[test]
    fn model_menu_groups_whisper_first_with_a_separator_only_between_groups() {
        let ms = vec![model("parakeet-v3", "onnx", false), model("large-v3", "whisper", true), model("base", "whisper", false)];
        let menu = model_menu(&ms, "large-v3", false);
        let ids: Vec<&str> = menu.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["model:large-v3", "model:base", "", "model:parakeet-v3"]);
        assert!(menu[0].checked && menu[0].radio);
        assert!(menu[2].separator);
        let only_whisper = model_menu(&ms[1..], "base", true);
        assert!(only_whisper.iter().all(|e| !e.separator));
        assert!(only_whisper[1].checked && !only_whisper[1].radio);
    }

    #[test]
    fn speakers_menu_disables_the_count_without_diarization() {
        let m = speakers_menu(false, 3);
        assert!(m[0].checkable && !m[0].checked);
        assert!(!m[1].enabled && m[1].submenu);
        assert_eq!(m.iter().filter(|e| e.level == 1).count(), 6);
        assert!(m.iter().any(|e| e.id == "speakers:3" && e.checked));
        assert!(speakers_menu(true, 0).iter().filter(|e| e.level == 1).all(|e| e.enabled));
    }

    #[test]
    fn speaker_items_and_row_menu_order_per_platform() {
        let spk = speaker_items(4, &[0, 1], Some(1), false, false);
        assert_eq!(spk.len(), 5);
        assert!(spk[1].checked && spk[1].tint.is_some());
        assert!(spk[2].separator);
        assert!(spk[4].enabled);
        assert!(!speaker_items(4, &[], None, false, false).last().unwrap().enabled);
        assert_eq!(speaker_items(4, &[], None, false, false).len(), 2);

        let st = RowMenuState { can_retranslate: true, can_split: false, can_merge: true, can_delete: true, shows_speakers: true, is_working: false };
        let win = row_menu(4, st, spk.clone(), false);
        assert_eq!(win[0].id, "row-retranslate:4");
        assert!(win[1].separator);
        assert_eq!(win[2].id, "spk:4:0");
        assert_eq!(win.last().unwrap().id, "row-delete:4");
        assert_eq!(win.last().unwrap().hint, "Delete");
        let mac = row_menu(4, st, spk, true);
        assert_eq!(mac[2].id, "row-split:4");
        assert!(mac[4].destructive);
        assert!(mac[5].separator);
        let busy = row_menu(4, RowMenuState { is_working: true, ..st }, vec![], true);
        assert!(!busy[0].enabled);
    }

    #[test]
    fn menu_ids_parse_back() {
        assert_eq!(MenuAction::parse("model:large-v3"), Some(MenuAction::Model("large-v3".into())));
        assert_eq!(MenuAction::parse("spk:3:none"), Some(MenuAction::SetSpeaker(3, None)));
        assert_eq!(MenuAction::parse("spk:3:2"), Some(MenuAction::SetSpeaker(3, Some(2))));
        assert_eq!(MenuAction::parse("row-lang:1:tr"), Some(MenuAction::QueueRowLanguage(1, "tr".into())));
        assert_eq!(MenuAction::parse("lang-all:"), Some(MenuAction::AllPlaylistLanguages(String::new())));
        assert_eq!(MenuAction::parse("speakers:0"), Some(MenuAction::SpeakerCount(0)));
        assert_eq!(MenuAction::parse("nope"), None);
        assert_eq!(MenuAction::parse("row-split:x"), None);
    }

    #[test]
    fn language_and_display_menus() {
        let all = language_menu("lang-all:", None);
        assert_eq!(all.len(), SubtitleLanguage::ALL.len());
        assert!(all.iter().all(|e| !e.checkable));
        let row = language_menu("row-lang:2:", Some("tr"));
        assert_eq!(row.iter().filter(|e| e.checked).count(), 1);
        let d = display_menu(0, "Çeviri", false);
        assert!(d[0].enabled && !d[1].enabled && !d[2].enabled && d[0].checked);
    }
}
