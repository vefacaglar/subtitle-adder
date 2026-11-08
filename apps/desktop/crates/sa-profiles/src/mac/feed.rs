//! Library search (`VideoUserStats.matches`, mac-librarykit §10.3) and the feed builder (§10.2).

use std::cmp::Ordering;
use std::collections::HashMap;

use sa_core::folding::fold;

use super::paths::{ci, localized_ci_cmp, swift_cmp, trim_ws_nl};
use super::vault::{VaultContents, VideoProbe, placeholder};
use crate::{Ticks, VideoFeedFilter, VideoUserStats};

/// A blank query matches everything; otherwise the whole folded query must occur in the file name, folder or path,
/// or else every token (split on space, tab, `_`, `-`, `.`) must occur in one of them.
pub fn matches(video: &VideoUserStats, query: Option<&str>) -> bool {
    let trimmed = trim_ws_nl(query.unwrap_or(""));
    if trimmed.is_empty() {
        return true;
    }
    let haystacks = [fold(&video.file_name), fold(&video.directory_path), fold(&video.video_path)];
    let whole = fold(trimmed);
    if haystacks.iter().any(|h| h.contains(&whole)) {
        return true;
    }
    let tokens: Vec<String> =
        trimmed.split([' ', '\t', '_', '-', '.']).filter(|t| !t.is_empty()).map(fold).collect();
    if tokens.is_empty() {
        return true;
    }
    tokens.iter().all(|t| haystacks.iter().any(|h| h.contains(t.as_str())))
}

pub fn narrowed(items: Vec<VideoUserStats>, search: Option<&str>) -> Vec<VideoUserStats> {
    match search {
        Some(s) if !trim_ws_nl(s).is_empty() => items.into_iter().filter(|v| matches(v, Some(s))).collect(),
        _ => items,
    }
}

/// File name ignoring case, then lowercased path (order never depends on map iteration).
pub fn by_name(a: &VideoUserStats, b: &VideoUserStats) -> Ordering {
    localized_ci_cmp(&a.file_name, &b.file_name)
        .then_with(|| swift_cmp(&a.video_path.to_lowercase(), &b.video_path.to_lowercase()))
}

fn desc<T: Ord>(a: T, b: T) -> Ordering {
    b.cmp(&a)
}

fn engaged(v: &VideoUserStats) -> bool {
    v.last_watched_at.is_some() || v.is_liked || v.rating > 0
}

/// §10.2: every known video by path (records, then probes, then list entries), filtered and ordered.
pub fn items(contents: &VaultContents, filter: &VideoFeedFilter, probes: &[VideoProbe], now: Ticks) -> Vec<VideoUserStats> {
    let mut merged: HashMap<String, VideoUserStats> = HashMap::new();
    for (k, video) in contents.videos() {
        if !video.video_path.is_empty()
            && let Some(stats) = contents.stats_for_key(k)
        {
            merged.insert(ci(&video.video_path), stats);
        }
    }
    for probe in probes {
        merged
            .entry(probe.path_key.clone())
            .or_insert_with(|| placeholder(&probe.path, now, probe.fingerprint.clone()));
    }
    for list in &contents.custom_lists {
        for path in &list.video_paths {
            merged.entry(ci(path)).or_insert_with(|| placeholder(path, now, None));
        }
    }

    let mut values: Vec<VideoUserStats>;
    match filter {
        VideoFeedFilter::CustomList(id) => {
            let Some(index) = contents.list_index(id.as_deref().unwrap_or("")) else { return Vec::new() };
            return contents.custom_lists[index].video_paths.iter().filter_map(|p| merged.get(&ci(p)).cloned()).collect();
        }
        VideoFeedFilter::History => {
            values = merged.into_values().filter(VideoUserStats::has_watched).collect();
            values.sort_by(|a, b| {
                desc(a.last_watched_at.unwrap_or(a.updated_at), b.last_watched_at.unwrap_or(b.updated_at))
                    .then_with(|| desc(a.view_count, b.view_count))
                    .then_with(|| by_name(a, b))
            });
        }
        VideoFeedFilter::Liked => {
            values = merged.into_values().filter(|v| v.is_liked).collect();
            values.sort_by(|a, b| {
                desc(a.updated_at, b.updated_at).then_with(|| desc(a.view_count, b.view_count)).then_with(|| by_name(a, b))
            });
        }
        VideoFeedFilter::MostWatched => {
            values = merged.into_values().filter(|v| v.view_count > 0).collect();
            values.sort_by(|a, b| {
                desc(a.view_count, b.view_count)
                    .then_with(|| desc(a.last_watched_at.unwrap_or(a.updated_at), b.last_watched_at.unwrap_or(b.updated_at)))
                    .then_with(|| by_name(a, b))
            });
        }
        VideoFeedFilter::TopRated => {
            values = merged.into_values().filter(|v| v.rating > 0).collect();
            values.sort_by(|a, b| {
                desc(a.rating, b.rating)
                    .then_with(|| desc(a.view_count, b.view_count))
                    .then_with(|| desc(a.updated_at, b.updated_at))
                    .then_with(|| by_name(a, b))
            });
        }
        VideoFeedFilter::All => {
            values = merged.into_values().collect();
            values.sort_by(|a, b| {
                desc(engaged(a), engaged(b))
                    .then_with(|| desc(a.last_watched_at, b.last_watched_at))
                    .then_with(|| desc(a.view_count, b.view_count))
                    .then_with(|| by_name(a, b))
            });
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(path: &str, name: &str, dir: &str) -> VideoUserStats {
        VideoUserStats { file_name: name.into(), directory_path: dir.into(), ..placeholder(path, Ticks::MIN, None) }
    }

    #[test]
    fn matches_search_whole_string_then_every_token() {
        let video = stats("/Videolar/Dizi Arşivi/Sezon_2_Bolum_05.mkv", "Sezon_2_Bolum_05.mkv", "/Videolar/Dizi Arşivi");
        assert!(matches(&video, None));
        assert!(matches(&video, Some("")));
        assert!(matches(&video, Some("bolum_05")));
        assert!(matches(&video, Some("sezon 2 bolum")));
        assert!(matches(&video, Some("bolum-05.mkv sezon")));
        assert!(matches(&video, Some("dizi arsivi")));
        assert!(matches(&video, Some("ARŞİVİ")));
        assert!(matches(&video, Some("videolar sezon")));
        assert!(!matches(&video, Some("sezon 3")));
        assert!(!matches(&video, Some("bolum 06")));
        assert!(matches(&video, Some("...")));
    }

    #[test]
    fn search_folds_the_turkish_dotted_and_dotless_i_alike() {
        let video = stats("/Videolar/KISA FİLM.mp4", "KISA FİLM.mp4", "/Videolar");
        for q in ["kısa", "kisa", "KISA film", "kısa fılm", "fİlm"] {
            assert!(matches(&video, Some(q)), "{q}");
        }
        let lower = stats("/v/kısa film.mp4", "kısa film.mp4", "/v");
        assert!(matches(&lower, Some("KISA")));
        assert!(matches(&lower, Some("Kısa Fİlm")));
    }
}
