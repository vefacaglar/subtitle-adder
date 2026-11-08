//! View-model values → bridge rows for the hub (pure; no window needed): cards, list pills, filters, the empty state
//! symbol. Every text is produced in the current UI language, so a re-map after a language switch relabels.

use std::path::Path;

use slint::{Image, ModelRc, SharedString, VecModel};

use sa_core::l;
use sa_profiles::{UserVideoList, VideoFeedFilter};
use sa_viewmodels::hub::VideoCard;

use crate::ui::{HubCardData, HubListData};

/// Which face the hub shows (texts and actions differ, the bridge is the same).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HubFlavor {
    Windows,
    Mac,
}

impl HubFlavor {
    /// The flavor of the compiled UI tree.
    pub fn native() -> Self {
        if crate::UI_TREE == "mac" { HubFlavor::Mac } else { HubFlavor::Windows }
    }

    /// Registered-name chips on the auth card (Windows 5, macOS 4) and in the flyout/popover (4).
    pub fn card_chip_count(self) -> usize {
        match self {
            HubFlavor::Windows => 5,
            HubFlavor::Mac => 4,
        }
    }
}

/// Audio-only extensions (macOS shows `waveform` instead of `film`).
pub const AUDIO_EXTENSIONS: &[&str] = &["mp3", "wav", "m4a", "aac", "flac", "ogg", "opus", "wma", "aiff", "aif", "caf"];

pub fn is_audio(path: &Path) -> bool {
    path.extension().map(|e| e.to_string_lossy().to_lowercase()).is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.as_str()))
}

/// Bridge filter index: 0 Keşfet, 1 İzleme geçmişi, 2 Favorilerim, 3 Puan verdiklerim, 4 En çok izlenenler, 5 list.
pub fn filter_index(filter: &VideoFeedFilter) -> i32 {
    match filter {
        VideoFeedFilter::All => 0,
        VideoFeedFilter::History => 1,
        VideoFeedFilter::Liked => 2,
        VideoFeedFilter::TopRated => 3,
        VideoFeedFilter::MostWatched => 4,
        VideoFeedFilter::CustomList(_) => 5,
    }
}

pub fn filter_from_index(index: i32) -> Option<VideoFeedFilter> {
    Some(match index {
        0 => VideoFeedFilter::All,
        1 => VideoFeedFilter::History,
        2 => VideoFeedFilter::Liked,
        3 => VideoFeedFilter::TopRated,
        4 => VideoFeedFilter::MostWatched,
        _ => return None,
    })
}

/// The id of the list on show (`CustomList`), if any.
pub fn selected_list_id(filter: &VideoFeedFilter) -> Option<&str> {
    match filter {
        VideoFeedFilter::CustomList(Some(id)) => Some(id.as_str()),
        _ => None,
    }
}

/// macOS empty-state symbol per filter (mac-ui §9.2); a search shows `magnifyingglass`.
pub fn empty_symbol(filter: &VideoFeedFilter, searching: bool) -> &'static str {
    if searching {
        return "magnifyingglass";
    }
    match filter {
        VideoFeedFilter::History => "clock",
        VideoFeedFilter::Liked => "heart",
        VideoFeedFilter::MostWatched => "eye",
        VideoFeedFilter::TopRated => "star",
        VideoFeedFilter::CustomList(_) => "list.bullet",
        VideoFeedFilter::All => "film.stack",
    }
}

/// Windows `lastWatchedText`: `Son izleme: …` or `Henüz izlenmedi`.
fn windows_last_watched(card: &VideoCard) -> String {
    card.last_watched_text.clone().unwrap_or_else(|| l!("Henüz izlenmedi"))
}

/// One card row. `thumbnail` is the decoded picture (or `None` while it loads / for none).
pub fn card_row(card: &VideoCard, flavor: HubFlavor, thumbnail: Option<&Image>) -> HubCardData {
    let views = card.views_text();
    let names: Vec<SharedString> = card.list_names.iter().map(|n| SharedString::from(n.as_str())).collect();
    let directory = card.path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let accessible = match flavor {
        HubFlavor::Windows => card.title.clone(),
        HubFlavor::Mac => card.file_name.clone(),
    };
    HubCardData {
        path: card.path.to_string_lossy().as_ref().into(),
        title: card.title.as_str().into(),
        file_name: card.file_name.as_str().into(),
        folder: card.folder.as_str().into(),
        directory: directory.into(),
        views_text: views.as_str().into(),
        view_count: card.view_count.to_string().into(),
        watch_line: format!("{views} · {}", windows_last_watched(card)).into(),
        watch_badges: card.watch_badges().join(" · ").into(),
        duration_text: card.duration_text.as_str().into(),
        has_progress: card.has_progress(),
        progress: card.progress_ratio.clamp(0.0, 1.0) as f32,
        has_subtitle: card.sidecar_badge.is_some(),
        subtitle_badge: card.sidecar_badge.clone().unwrap_or_default().into(),
        subtitle_status: card.subtitle_status_text().into(),
        subtitle_chip: card.subtitle_chip_text().into(),
        is_liked: card.is_liked,
        like_text: card.like_button_text().into(),
        rating: card.rating.clamp(0, 5) as i32,
        rating_badge: card.rating_badge_text().into(),
        list_names: ModelRc::new(VecModel::from(names)),
        is_audio: is_audio(&card.path),
        has_thumbnail: thumbnail.is_some(),
        thumbnail: thumbnail.cloned().unwrap_or_default(),
        accessible_label: accessible.into(),
    }
}

/// `"{name} ({count})"` pills; `selected` = the list on show (case-insensitive id).
pub fn list_rows(lists: &[UserVideoList], filter: &VideoFeedFilter) -> Vec<HubListData> {
    let selected = selected_list_id(filter);
    lists
        .iter()
        .map(|list| HubListData {
            id: list.id.as_str().into(),
            name: list.name.as_str().into(),
            title: format!("{} ({})", list.name, list.video_paths.len()).into(),
            selected: selected.is_some_and(|s| s.eq_ignore_ascii_case(&list.id)),
        })
        .collect()
}

/// Case-insensitive path key (the hub's caches and in-place updates).
pub fn path_key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

/// Whether two rows show the same card for an in-place update (same path, case-insensitive).
pub fn same_card(a: &HubCardData, b: &HubCardData) -> bool {
    a.path.to_lowercase() == b.path.to_lowercase()
}

/// Field-wise equality (generated structs with images/models have no `PartialEq` that compares contents).
pub fn row_equal(a: &HubCardData, b: &HubCardData) -> bool {
    use slint::Model;
    let names = |m: &ModelRc<SharedString>| m.iter().collect::<Vec<_>>();
    a.path == b.path
        && a.title == b.title
        && a.file_name == b.file_name
        && a.folder == b.folder
        && a.directory == b.directory
        && a.views_text == b.views_text
        && a.view_count == b.view_count
        && a.watch_line == b.watch_line
        && a.watch_badges == b.watch_badges
        && a.duration_text == b.duration_text
        && a.has_progress == b.has_progress
        && a.progress == b.progress
        && a.has_subtitle == b.has_subtitle
        && a.subtitle_badge == b.subtitle_badge
        && a.subtitle_status == b.subtitle_status
        && a.subtitle_chip == b.subtitle_chip
        && a.is_liked == b.is_liked
        && a.like_text == b.like_text
        && a.rating == b.rating
        && a.rating_badge == b.rating_badge
        && names(&a.list_names) == names(&b.list_names)
        && a.is_audio == b.is_audio
        && a.has_thumbnail == b.has_thumbnail
        && a.thumbnail == b.thumbnail
        && a.accessible_label == b.accessible_label
}

/// Applies `rows` to `model` without flicker (win-ui-hub-settings §5.4): same sequence of paths → only changed rows
/// are replaced; otherwise the whole list is swapped. Returns whether the sequence changed.
pub fn apply_rows(model: &VecModel<HubCardData>, rows: Vec<HubCardData>) -> bool {
    use slint::Model;
    let same_sequence = model.row_count() == rows.len()
        && rows.iter().enumerate().all(|(i, r)| model.row_data(i).is_some_and(|old| same_card(&old, r)));
    if same_sequence {
        for (i, r) in rows.into_iter().enumerate() {
            if model.row_data(i).is_some_and(|old| !row_equal(&old, &r)) {
                model.set_row_data(i, r);
            }
        }
        false
    } else {
        model.set_vec(rows);
        true
    }
}
