//! Video platform hub (UNIFIED.md §3.9, §4; mac-viewmodels §12 pipeline on both platforms, Windows feed semantics
//! through the store; win-viewmodels §20).
//!
//! Rule 6: logged out every tab shows the sign-in card, the search field is disabled, no feed/index query runs, no
//! cards, lists, counts, discovered paths or pictures exist in memory; logout/switch forgets them synchronously
//! before the store answers; results built under an older session generation are dropped.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sa_core::cancel::CancellationToken;
use sa_core::l;
use sa_core::l10n::{LanguageState, UiLanguage};
use sa_core::secrets::SecretString;
use sa_core::subscription::Subscription;
use sa_profiles::{FeedQuery, FingerprintMode, ProfileChange, ProfileCounts, ProfileError, Ticks, UserVideoList, VideoFeedFilter, VideoUserStats};

use crate::lanes::{Lane, StoreLanes};
use crate::policy::{HubDateFormat, QuickListNaming};
use crate::services::AppServices;
use crate::uiref::UiKey;

/// Indexed paths merged into Keşfet.
pub const DISCOVERED_LIMIT: usize = 500;
/// Watch progress cadence.
pub const PROGRESS_INTERVAL: Duration = Duration::from_secs(5);
/// macOS quit wait.
pub const QUIT_TIMEOUT: Duration = Duration::from_secs(2);

/// A feed card (plain value; win-viewmodels §20.3 / mac-viewmodels §12.11).
#[derive(Debug, Clone, PartialEq)]
pub struct VideoCard {
    pub path: PathBuf,
    pub title: String,
    pub file_name: String,
    pub extension_label: String,
    pub folder: String,
    pub view_count: i64,
    pub is_liked: bool,
    pub rating: i64,
    pub progress_ratio: f64,
    pub duration_seconds: f64,
    pub duration_text: String,
    pub last_watched: Option<Ticks>,
    pub last_watched_text: Option<String>,
    pub list_ids: Vec<String>,
    pub list_names: Vec<String>,
    /// Upper-cased sidecar extension (`SRT`).
    pub sidecar_badge: Option<String>,
    /// Decoded lazily on the thumbnails lane.
    pub has_thumbnail: bool,
}

impl VideoCard {
    pub fn has_progress(&self) -> bool {
        self.progress_ratio > 0.01
    }
    pub fn has_watched(&self) -> bool {
        self.view_count > 0 || self.last_watched.is_some()
    }
    /// `%lld görüntüleme` / `Henüz izlenmedi`.
    pub fn views_text(&self) -> String {
        if self.view_count > 0 { l!("%lld görüntüleme", self.view_count) } else { l!("Henüz izlenmedi") }
    }
    /// macOS watch badges.
    pub fn watch_badges(&self) -> Vec<String> {
        if !self.has_watched() {
            return vec![l!("Henüz izlenmedi")];
        }
        let mut v = Vec::new();
        if self.view_count > 0 {
            v.push(l!("%lld görüntüleme", self.view_count));
        }
        if let Some(t) = &self.last_watched_text {
            v.push(t.clone());
        }
        v
    }
    /// Windows `SubtitleStatusText`.
    pub fn subtitle_status_text(&self) -> String {
        match &self.sidecar_badge {
            None => l!("Altyazı yok"),
            Some(b) if b.is_empty() => l!("Altyazılı"),
            Some(b) => format!("{} ({b})", l!("Altyazılı")),
        }
    }
    /// macOS chip `Altyazılı (%@)` / `Altyazı yok`.
    pub fn subtitle_chip_text(&self) -> String {
        match &self.sidecar_badge {
            None => l!("Altyazı yok"),
            Some(b) => l!("Altyazılı (%@)", b.as_str()),
        }
    }
    pub fn like_button_text(&self) -> String {
        if self.is_liked { l!("Favorilerde") } else { l!("Favorilere ekle") }
    }
    pub fn rating_badge_text(&self) -> String {
        if self.rating > 0 { format!("★ {}/5", self.rating) } else { String::new() }
    }
}

/// Engagement bar state for the open video (logged in + video open only).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlayerEngagement {
    pub visible: bool,
    pub view_count: i64,
    pub is_liked: bool,
    pub rating: i64,
    pub list_ids: Vec<String>,
}

impl PlayerEngagement {
    pub fn views_text(&self) -> String {
        l!("%lld görüntüleme", self.view_count)
    }
    pub fn like_text(&self) -> String {
        if self.is_liked { l!("Beğenildi") } else { l!("Beğen") }
    }
}

/// Hub changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HubChange {
    Session,
    Feed,
    Lists,
    Counts,
    Engagement,
    Thumbnail,
    Busy,
    Error,
}

/// Playback reports from the player (watch progress, privacy-gated).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PlaybackReport {
    Opened { duration: f64 },
    Progress { position: f64, duration: f64 },
    Paused { position: f64 },
    Leaving { position: f64 },
    Seeked { position: f64 },
}

// ----- text helpers

const TR_MONTHS: [&str; 12] = ["Oca", "Şub", "Mar", "Nis", "May", "Haz", "Tem", "Ağu", "Eyl", "Eki", "Kas", "Ara"];
const EN_MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// Local date text of a UTC tick value in the app UI language.
pub fn date_text(t: Ticks, format: HubDateFormat, language: UiLanguage) -> String {
    use chrono::{Datelike, TimeZone, Timelike};
    let secs = (t.0 - Ticks::UNIX_EPOCH).div_euclid(10_000_000);
    let Some(dt) = chrono::Local.timestamp_opt(secs, 0).single() else { return String::new() };
    let (h12, pm) = {
        let h = dt.hour();
        (if h % 12 == 0 { 12 } else { h % 12 }, h >= 12)
    };
    let tr = language == UiLanguage::Tr;
    match format {
        HubDateFormat::DotNetGeneral => {
            if tr {
                format!("{:02}.{:02}.{} {:02}:{:02}", dt.day(), dt.month(), dt.year(), dt.hour(), dt.minute())
            } else {
                format!("{}/{}/{} {}:{:02} {}", dt.month(), dt.day(), dt.year(), h12, dt.minute(), if pm { "PM" } else { "AM" })
            }
        }
        HubDateFormat::AppleAbbreviated => {
            let m0 = dt.month0() as usize;
            if tr {
                format!("{} {} {} {:02}:{:02}", dt.day(), TR_MONTHS[m0], dt.year(), dt.hour(), dt.minute())
            } else {
                // ICU (macOS 13+) separates the day period with a narrow no-break space.
                format!("{} {}, {} at {}:{:02}\u{202F}{}", EN_MONTHS[m0], dt.day(), dt.year(), h12, dt.minute(), if pm { "PM" } else { "AM" })
            }
        }
    }
}

/// Windows `FormatTime` (floor, `mm:ss` / `hh:mm:ss`) or macOS `m:ss` / `h:mm:ss` (rounded).
pub fn duration_text(seconds: f64, format: HubDateFormat) -> String {
    if !seconds.is_finite() || seconds <= 0.0 {
        return String::new();
    }
    match format {
        HubDateFormat::DotNetGeneral => {
            let total = seconds.floor() as i64;
            let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
            if seconds >= 3600.0 { format!("{h:02}:{m:02}:{s:02}") } else { format!("{m:02}:{s:02}") }
        }
        HubDateFormat::AppleAbbreviated => {
            let total = seconds.round() as i64;
            let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
            if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
        }
    }
}

/// `"{folderName} ({dir})"`, the dir alone for a root, `""` when blank.
pub fn folder_line(dir: &str) -> String {
    let trimmed = if dir.len() > 1 { dir.trim_end_matches(['/', '\\']) } else { dir };
    if trimmed.is_empty() {
        return String::new();
    }
    let name = Path::new(trimmed).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if name.is_empty() || name == trimmed { trimmed.to_owned() } else { format!("{name} ({trimmed})") }
}

/// The sidecar badge (`SRT`) of a video: exact sidecar, else `{stem}.*{ext}`.
pub fn sidecar_badge(video: &Path, listing: &mut HashMap<PathBuf, Vec<String>>) -> Option<String> {
    if let Some(p) = sa_core::subtitle_import::find_sidecar_subtitle(video) {
        return p.extension().map(|e| e.to_string_lossy().to_uppercase());
    }
    let dir = video.parent()?.to_path_buf();
    let stem = video.file_stem()?.to_string_lossy().into_owned();
    let names = listing.entry(dir.clone()).or_insert_with(|| {
        std::fs::read_dir(&dir)
            .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default()
    });
    for ext in sa_core::subtitle_import::EXTENSIONS {
        if let Some(n) = names.iter().find(|n| {
            let lower = n.to_lowercase();
            lower.starts_with(&format!("{}.", stem.to_lowercase())) && lower.ends_with(ext)
        }) {
            return Path::new(n).extension().map(|e| e.to_string_lossy().to_uppercase());
        }
    }
    None
}

/// macOS `NameSequence(preferred, base, existing)`: the preferred name if free, else `"<base> <n>"`, ≤ 500 tries.
pub struct NameSequence {
    taken: HashSet<String>,
    counter: usize,
    queue: Vec<String>,
    base: String,
    tries: usize,
}

impl NameSequence {
    pub fn new(preferred: Option<&str>, base: &str, existing: &[String]) -> Self {
        let taken: HashSet<String> = existing.iter().map(|e| crate::explorer::fold(e.trim())).collect();
        let queue = preferred.filter(|p| !taken.contains(&crate::explorer::fold(p.trim()))).map(|p| vec![p.to_owned()]).unwrap_or_default();
        Self { counter: existing.len() + 1, taken, queue, base: base.to_owned(), tries: 0 }
    }
}

impl Iterator for NameSequence {
    type Item = String;
    fn next(&mut self) -> Option<String> {
        if !self.queue.is_empty() {
            return Some(self.queue.remove(0));
        }
        while self.tries < 500 {
            self.tries += 1;
            let candidate = format!("{} {}", self.base, self.counter);
            self.counter += 1;
            if !self.taken.contains(&crate::explorer::fold(&candidate)) {
                return Some(candidate);
            }
        }
        None
    }
}

/// What a feed build produced.
#[derive(Debug, Clone, Default)]
struct Snapshot {
    generation: u64,
    logged_in: bool,
    username: Option<String>,
    registered: Vec<String>,
    last_username: Option<String>,
    remember_me: bool,
    counts: ProfileCounts,
    lists: Vec<UserVideoList>,
    engagement: Option<(PathBuf, VideoUserStats, Vec<String>)>,
    cards: Option<Vec<VideoCard>>,
    thumbnails: Vec<(String, Vec<u8>)>,
}

#[derive(Debug, Clone, Copy, Default)]
struct PendingEdit {
    liked: Option<bool>,
    rating: Option<i64>,
    outstanding: i64,
}

struct State {
    logged_in: bool,
    username: Option<String>,
    registered: Vec<String>,
    last_username: Option<String>,
    remember_me: bool,
    counts: ProfileCounts,
    auth_error: Option<ProfileError>,
    action_error: Option<ProfileError>,
    is_authenticating: bool,
    filter: VideoFeedFilter,
    search: String,
    cards: Vec<VideoCard>,
    lists: Vec<UserVideoList>,
    has_loaded_feed: bool,
    refreshing: bool,
    refresh_pending: bool,
    ticket: u64,
    applied_generation: Option<u64>,
    engagement: PlayerEngagement,
    engagement_loaded: bool,
    player_video: Option<PathBuf>,
    /// The store session under which playback of `player_video` may be recorded: the one that was open when the
    /// video was opened (Swift `recordingGeneration`, C# `_activeVideoSessionToken` + `_activeVideoRecordsProgress`).
    /// `None` when nobody was logged in then, or after that session ended: progress of a video never lands in the
    /// history of a profile that logged in (or took over) while it played on.
    recording_generation: Option<u64>,
    /// The session of a logout under way: a snapshot of it is not shown again, even before the store has ended it
    /// (Swift `closingGeneration`; the refresh queued behind the logout shows the result).
    closing_generation: Option<u64>,
    restored: bool,
    pending_open: Option<PathBuf>,
    pending_edits: HashMap<String, PendingEdit>,
    thumbnails: HashMap<String, Vec<u8>>,
    thumbnail_failures: HashSet<String>,
    thumbnail_requested: HashSet<String>,
    page_limit: usize,
    hub_active: bool,
    last_duration: f64,
    last_position: Option<f64>,
    last_progress_write: Option<Instant>,
    started: bool,
}

struct Inner {
    svc: AppServices,
    lanes: Arc<StoreLanes>,
    key: Cell<Option<UiKey<Inner>>>,
    st: RefCell<State>,
    handlers: RefCell<Vec<Rc<dyn Fn(HubChange)>>>,
    subs: RefCell<Vec<Subscription>>,
}

fn lower_key(p: &Path) -> String {
    p.to_string_lossy().to_lowercase()
}

impl Inner {
    fn key(&self) -> UiKey<Inner> {
        self.key.get().expect("registered")
    }

    fn notify(&self, changes: &[HubChange]) {
        let hs: Vec<Rc<dyn Fn(HubChange)>> = self.handlers.borrow().clone();
        for c in changes {
            for h in &hs {
                h(*c);
            }
        }
    }

    /// Rule 6: forgets everything shown for a profile, synchronously.
    fn forget_profile_state(&self) {
        let mut st = self.st.borrow_mut();
        st.logged_in = false;
        st.username = None;
        st.counts = ProfileCounts::default();
        st.lists.clear();
        st.cards.clear();
        st.has_loaded_feed = false;
        st.engagement = PlayerEngagement::default();
        st.engagement_loaded = false;
        st.pending_edits.clear();
        st.thumbnails.clear();
        st.thumbnail_failures.clear();
        st.thumbnail_requested.clear();
        st.pending_open = None;
        st.applied_generation = None;
        st.recording_generation = None;
        // The closing profile's search is not run again over the logged-out feed (rule 6: nothing kept in memory).
        st.search.clear();
    }

    fn schedule_refresh(self: &Rc<Self>) {
        let start = {
            let mut st = self.st.borrow_mut();
            st.refresh_pending = true;
            st.ticket += 1;
            !st.refreshing
        };
        if start {
            self.st.borrow_mut().refreshing = true;
            self.notify(&[HubChange::Busy]);
            self.run_refresh();
        }
    }

    fn run_refresh(self: &Rc<Self>) {
        let (filter, search, player_video, wants_feed) = {
            let mut st = self.st.borrow_mut();
            st.refresh_pending = false;
            (st.filter.clone(), st.search.trim().to_owned(), st.player_video.clone(), st.hub_active || true)
        };
        let store = self.svc.profiles.clone();
        let index = self.svc.media_index.clone();
        let shell = self.svc.shell.clone();
        let date_format = self.svc.policy.hub_date_format;
        let key = self.key();
        let ui = self.svc.ui.clone();
        let ticket = self.st.borrow().ticket;
        self.lanes.run(
            Lane::Feed,
            move || build_snapshot(store.as_ref(), index.as_ref(), shell.as_ref(), filter, search, player_video, wants_feed, date_format),
            move |snap| {
                if let Some(inner) = key.get() {
                    inner.apply_snapshot(snap, ticket);
                }
                let _ = ui;
            },
        );
    }

    fn apply_snapshot(self: &Rc<Self>, snap: Snapshot, _ticket: u64) {
        let current_gen = self.svc.profiles.session_generation();
        let closing = snap.logged_in && self.st.borrow().closing_generation == Some(snap.generation);
        if closing {
            // A profile that is being logged out never comes back on screen.
        } else if snap.generation != current_gen {
            // Built under a login that is over: rebuild.
            self.st.borrow_mut().refresh_pending = true;
        } else {
            let mut changes = vec![HubChange::Session, HubChange::Counts, HubChange::Lists, HubChange::Engagement, HubChange::Feed];
            let gen_changed = self.st.borrow().applied_generation.is_some_and(|g| g != snap.generation);
            if gen_changed || !snap.logged_in {
                let mut st = self.st.borrow_mut();
                st.thumbnails.clear();
                st.thumbnail_failures.clear();
                st.thumbnail_requested.clear();
            }
            if !snap.logged_in {
                self.forget_profile_state();
            }
            {
                let mut st = self.st.borrow_mut();
                st.applied_generation = Some(snap.generation);
                st.logged_in = snap.logged_in;
                st.username = snap.username.clone();
                st.registered = snap.registered.clone();
                st.last_username = snap.last_username.clone();
                if !st.restored || !snap.logged_in || snap.remember_me {
                    st.remember_me = snap.remember_me;
                }
                st.counts = snap.counts;
                st.lists = snap.lists.clone();
                if let Some((video, stats, list_ids)) = snap.engagement.clone() {
                    if st.player_video.as_ref().is_some_and(|v| lower_key(v) == lower_key(&video)) {
                        st.engagement = PlayerEngagement {
                            visible: snap.logged_in,
                            view_count: stats.view_count,
                            is_liked: stats.is_liked,
                            rating: stats.rating.clamp(0, 5),
                            list_ids,
                        };
                        st.engagement_loaded = true;
                    }
                } else if !snap.logged_in || st.player_video.is_none() {
                    st.engagement = PlayerEngagement::default();
                }
                if snap.logged_in {
                    for (k, b) in snap.thumbnails {
                        st.thumbnails.entry(k).or_insert(b);
                    }
                }
                if let Some(mut cards) = snap.cards {
                    for c in &mut cards {
                        let k = lower_key(&c.path);
                        if let Some(e) = st.pending_edits.get(&k) {
                            if let Some(l) = e.liked {
                                c.is_liked = l;
                            }
                            if let Some(r) = e.rating {
                                c.rating = r;
                            }
                        }
                        c.has_thumbnail = st.thumbnails.contains_key(&k);
                    }
                    if st.cards != cards {
                        st.cards = cards;
                    }
                    st.has_loaded_feed = true;
                } else if !snap.logged_in {
                    st.cards.clear();
                }
                // A list filter whose list is gone falls back to Keşfet.
                if let VideoFeedFilter::CustomList(Some(id)) = &st.filter
                    && !st.lists.iter().any(|l| l.id.eq_ignore_ascii_case(id))
                {
                    st.filter = VideoFeedFilter::All;
                }
                if st.logged_in != snap.logged_in {
                    st.refresh_pending = true;
                }
            }
            changes.push(HubChange::Busy);
            self.notify(&changes);
            self.request_thumbnails();
        }
        let again = self.st.borrow().refresh_pending;
        if again {
            self.run_refresh();
        } else {
            self.st.borrow_mut().refreshing = false;
            self.notify(&[HubChange::Busy]);
        }
    }

    /// Thumbnails for visible cards (logged in only; never generated, read or written logged out).
    fn request_thumbnails(self: &Rc<Self>) {
        let (logged_in, todo, generation) = {
            let mut st = self.st.borrow_mut();
            if !st.logged_in {
                return;
            }
            let limit = self.svc.policy.hub_card_page_size.map(|_| st.page_limit).unwrap_or(usize::MAX);
            let paths: Vec<PathBuf> = st
                .cards
                .iter()
                .take(limit)
                .map(|c| c.path.clone())
                .filter(|p| {
                    let k = lower_key(p);
                    !st.thumbnails.contains_key(&k) && !st.thumbnail_failures.contains(&k) && !st.thumbnail_requested.contains(&k)
                })
                .collect();
            for p in &paths {
                st.thumbnail_requested.insert(lower_key(p));
            }
            (st.logged_in, paths, st.applied_generation.unwrap_or(0))
        };
        if !logged_in {
            return;
        }
        for path in todo {
            let store = self.svc.profiles.clone();
            let generator = self.svc.thumbnails.clone();
            let key = self.key();
            let p = path.clone();
            self.lanes.run(
                Lane::Thumbnails,
                move || {
                    if !store.is_logged_in() || store.session_generation() != generation {
                        return None;
                    }
                    if let Some(b) = store.thumbnail(&p).filter(|b| !b.is_empty()) {
                        return Some(b);
                    }
                    let bytes = generator.generate(&p, &CancellationToken::new())?;
                    // Compared again under the store's lock: a login switch while the picture was generated stores
                    // nothing in the new profile.
                    let _ = store.set_thumbnail_in_session(generation, &p, &bytes);
                    Some(bytes)
                },
                move |bytes: Option<Vec<u8>>| {
                    let Some(inner) = key.get() else { return };
                    let ok = inner.svc.profiles.session_generation() == generation && inner.st.borrow().logged_in;
                    if !ok {
                        return;
                    }
                    let k = lower_key(&path);
                    {
                        let mut st = inner.st.borrow_mut();
                        st.thumbnail_requested.remove(&k);
                        match bytes {
                            Some(b) => {
                                st.thumbnails.insert(k.clone(), b);
                                for c in st.cards.iter_mut().filter(|c| lower_key(&c.path) == k) {
                                    c.has_thumbnail = true;
                                }
                            }
                            None => {
                                st.thumbnail_failures.insert(k);
                            }
                        }
                    }
                    inner.notify(&[HubChange::Thumbnail]);
                },
            );
        }
    }

    fn perform(self: &Rc<Self>, work: impl FnOnce(&dyn sa_profiles::ProfileStore) -> Result<(), ProfileError> + Send + 'static) {
        let store = self.svc.profiles.clone();
        let key = self.key();
        self.lanes.run(
            Lane::Actions,
            move || work(store.as_ref()),
            move |r: Result<(), ProfileError>| {
                if let Some(inner) = key.get() {
                    if let Err(e) = r {
                        inner.st.borrow_mut().action_error = Some(e);
                        inner.notify(&[HubChange::Error]);
                    }
                    inner.schedule_refresh();
                }
            },
        );
    }

    fn report(self: &Rc<Self>, position: f64, flush: bool) {
        let (ok, video, duration) = {
            let st = self.st.borrow();
            let generation = st.recording_generation.filter(|g| st.logged_in && *g == self.svc.profiles.session_generation());
            (generation, st.player_video.clone(), st.last_duration)
        };
        let Some(video) = video else { return };
        let Some(generation) = ok else { return };
        if !position.is_finite() || !duration.is_finite() || duration <= 0.0 {
            return;
        }
        let store = self.svc.profiles.clone();
        self.lanes.spawn(Lane::Actions, move || {
            // The generation is compared again under the store's lock, in the same critical section as the write: a
            // login switch since the UI thread captured it records nothing in the new profile.
            if store.session_generation() != generation {
                return;
            }
            let _ = store.update_playback_progress_in_session(generation, &video, position, duration);
            if flush {
                let _ = store.flush_playback_progress();
            }
        });
        self.st.borrow_mut().last_progress_write = Some(Instant::now());
    }

    /// Counts a view for the session `generation` the open was recorded under (compared under the store's lock).
    fn count_open(self: &Rc<Self>, video: PathBuf, generation: u64) {
        let store = self.svc.profiles.clone();
        let key = self.key();
        self.lanes.run(
            Lane::Actions,
            move || {
                let _ = store.record_watch_in_session(generation, &video, 0.0, 0.0);
            },
            move |()| {
                if let Some(inner) = key.get() {
                    inner.schedule_refresh();
                }
            },
        );
    }

    fn authenticate(
        self: &Rc<Self>,
        work: impl FnOnce(&dyn sa_profiles::ProfileStore) -> Result<(), ProfileError> + Send + 'static,
    ) {
        {
            let mut st = self.st.borrow_mut();
            if st.is_authenticating {
                return;
            }
            st.is_authenticating = true;
            st.auth_error = None;
        }
        self.notify(&[HubChange::Busy, HubChange::Error]);
        let store = self.svc.profiles.clone();
        let key = self.key();
        self.lanes.run(
            Lane::Actions,
            move || work(store.as_ref()),
            move |r: Result<(), ProfileError>| {
                let Some(inner) = key.get() else { return };
                {
                    let mut st = inner.st.borrow_mut();
                    st.is_authenticating = false;
                    match r {
                        Err(e) => st.auth_error = Some(e),
                        Ok(()) => {
                            if matches!(st.filter, VideoFeedFilter::CustomList(_)) {
                                st.filter = VideoFeedFilter::All;
                            }
                            st.page_limit = inner.svc.policy.hub_card_page_size.unwrap_or(usize::MAX);
                        }
                    }
                }
                inner.notify(&[HubChange::Busy, HubChange::Error, HubChange::Session]);
                inner.schedule_refresh();
            },
        );
    }
}

/// Engagement of a video the profile has no record of: no views, not liked, no stars.
fn unrecorded_stats(path: &Path) -> VideoUserStats {
    VideoUserStats {
        content_fingerprint: None,
        video_path: path.to_string_lossy().into_owned(),
        file_name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        directory_path: path.parent().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        view_count: 0,
        is_liked: false,
        rating: 0,
        last_position_seconds: 0.0,
        duration_seconds: 0.0,
        last_watched_at: None,
        updated_at: Ticks::MIN,
        thumbnail: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn build_snapshot(
    store: &dyn sa_profiles::ProfileStore,
    index: &dyn sa_media::MediaIndex,
    shell: &dyn sa_core::platform::PlatformShell,
    filter: VideoFeedFilter,
    search: String,
    player_video: Option<PathBuf>,
    wants_feed: bool,
    date_format: HubDateFormat,
) -> Snapshot {
    let generation = store.session_generation();
    let logged_in = store.is_logged_in();
    let mut snap = Snapshot {
        generation,
        logged_in,
        username: store.current_username(),
        registered: store.registered_usernames(),
        last_username: store.last_username(),
        remember_me: store.remember_me(),
        ..Snapshot::default()
    };
    if !logged_in {
        // Rule 6: no feed, no search, no file opened.
        return snap;
    }
    snap.counts = store.totals();
    if let Some(v) = &player_video {
        // The video open in the player always gets the bar while a profile is logged in, even one the profile has
        // no record of yet: a login while a video is open adopts that video for likes, ratings and lists (C#
        // `AdoptOpenVideoForNewSession` via `OpenVideoProvider`, `stats?.ViewCount ?? 0`; Swift `EngagementSnapshot`
        // with `stats?.viewCount ?? 0`). Whether its playback is recorded is `recording_generation`'s business.
        let stats = store.stats(v, FingerprintMode::Compute).unwrap_or_else(|| unrecorded_stats(v));
        let ids = store.lists_containing(v);
        snap.engagement = Some((v.clone(), stats, ids));
    }
    snap.lists = store.lists();
    if !wants_feed {
        return snap;
    }
    let mut discovered: Vec<PathBuf> = Vec::new();
    if filter == VideoFeedFilter::All {
        let mut seen: HashSet<String> = HashSet::new();
        let mut files = index.all_files(DISCOVERED_LIMIT);
        if !search.is_empty() {
            files.extend(index.search(&search, DISCOVERED_LIMIT));
        }
        for f in files {
            if seen.insert(f.full_path.to_lowercase()) {
                discovered.push(PathBuf::from(f.full_path));
            }
        }
    }
    let query = FeedQuery {
        filter,
        search: (!search.is_empty()).then_some(search),
        discovered,
        mode: FingerprintMode::Compute,
    };
    let items = store.feed(&query, &|| true);
    let _ = store.persist_pending_migrations();
    let list_names: HashMap<String, Vec<(String, String)>> = {
        let mut m: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for l in &snap.lists {
            for p in &l.video_paths {
                m.entry(p.to_lowercase()).or_default().push((l.id.clone(), l.name.clone()));
            }
        }
        m
    };
    let language = LanguageState::shared().language();
    let mut listings: HashMap<PathBuf, Vec<String>> = HashMap::new();
    let cards = items
        .into_iter()
        .filter(|s| shell.storage_available(Path::new(&s.video_path)))
        .map(|s| {
            let path = PathBuf::from(&s.video_path);
            if let Some(t) = s.thumbnail.clone().filter(|b| !b.is_empty()) {
                snap.thumbnails.push((lower_key(&path), t));
            }
            let lists = list_names.get(&s.video_path.to_lowercase()).cloned().unwrap_or_default();
            let file_name = if s.file_name.is_empty() {
                path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
            } else {
                s.file_name.clone()
            };
            let stem = Path::new(&file_name).file_stem().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default();
            VideoCard {
                title: if stem.is_empty() { file_name.clone() } else { stem },
                extension_label: path.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default(),
                file_name,
                folder: folder_line(&s.directory_path),
                view_count: s.view_count,
                is_liked: s.is_liked,
                rating: s.rating.clamp(0, 5),
                progress_ratio: s.progress_ratio(),
                duration_seconds: s.duration_seconds,
                duration_text: duration_text(s.duration_seconds, date_format),
                last_watched: s.last_watched_at,
                last_watched_text: s.last_watched_at.map(|t| l!("Son izleme: %@", date_text(t, date_format, language))),
                list_ids: lists.iter().map(|x| x.0.clone()).collect(),
                list_names: lists.into_iter().map(|x| x.1).collect(),
                sidecar_badge: sidecar_badge(&path, &mut listings),
                has_thumbnail: false,
                path,
            }
        })
        .collect();
    snap.cards = Some(cards);
    snap
}

/// The hub model.
pub struct VideoPlatformModel {
    inner: Rc<Inner>,
}

impl Drop for VideoPlatformModel {
    fn drop(&mut self) {
        if let Some(k) = self.inner.key.get() {
            k.unregister();
        }
    }
}

impl VideoPlatformModel {
    pub fn new(services: AppServices) -> Self {
        let lanes = StoreLanes::new(services.ui.clone());
        Self::with_lanes(services, lanes)
    }

    /// Shares lanes with other models (one feed/actions/thumbnails executor per app).
    pub fn with_lanes(services: AppServices, lanes: Arc<StoreLanes>) -> Self {
        let page = services.policy.hub_card_page_size.unwrap_or(usize::MAX);
        let inner = Rc::new(Inner {
            svc: services,
            lanes,
            key: Cell::new(None),
            st: RefCell::new(State {
                logged_in: false,
                username: None,
                registered: Vec::new(),
                last_username: None,
                remember_me: false,
                counts: ProfileCounts::default(),
                auth_error: None,
                action_error: None,
                is_authenticating: false,
                filter: VideoFeedFilter::All,
                search: String::new(),
                cards: Vec::new(),
                lists: Vec::new(),
                has_loaded_feed: false,
                refreshing: false,
                refresh_pending: false,
                ticket: 0,
                applied_generation: None,
                engagement: PlayerEngagement::default(),
                engagement_loaded: false,
                player_video: None,
                recording_generation: None,
                closing_generation: None,
                restored: false,
                pending_open: None,
                pending_edits: HashMap::new(),
                thumbnails: HashMap::new(),
                thumbnail_failures: HashSet::new(),
                thumbnail_requested: HashSet::new(),
                page_limit: page,
                hub_active: false,
                last_duration: 0.0,
                last_position: None,
                last_progress_write: None,
                started: false,
            }),
            handlers: RefCell::new(Vec::new()),
            subs: RefCell::new(Vec::new()),
        });
        let key = UiKey::register(&inner);
        inner.key.set(Some(key));
        VideoPlatformModel { inner }
    }

    pub fn on_change(&self, handler: Box<dyn Fn(HubChange)>) {
        self.inner.handlers.borrow_mut().push(Rc::from(handler));
    }

    /// Background start: store first read + remembered restore, then the first snapshot.
    pub fn start(&self) {
        if std::mem::replace(&mut self.inner.st.borrow_mut().started, true) {
            return;
        }
        let key = self.inner.key();
        let ui = self.inner.svc.ui.clone();
        {
            let ui2 = ui.clone();
            let sub = self.inner.svc.profiles.subscribe_changes(Box::new(move |c| {
                if c == ProfileChange::External {
                    key.post(&ui2, |inner| inner.schedule_refresh());
                }
            }));
            self.inner.subs.borrow_mut().push(sub);
        }
        {
            let ui2 = ui.clone();
            let sub = self.inner.svc.media_index.subscribe_changes(Box::new(move || {
                key.post(&ui2, |inner| {
                    let (active, all, q) = {
                        let st = inner.st.borrow();
                        (st.hub_active, st.filter == VideoFeedFilter::All, !st.search.trim().is_empty())
                    };
                    if active && st_logged_in(inner) && (all || q) {
                        inner.schedule_refresh();
                    }
                });
            }));
            self.inner.subs.borrow_mut().push(sub);
        }
        let store = self.inner.svc.profiles.clone();
        self.inner.lanes.run(
            Lane::Actions,
            move || store.start(),
            move |()| {
                let Some(inner) = key.get() else { return };
                inner.st.borrow_mut().restored = true;
                inner.schedule_refresh();
                let pending = inner.st.borrow_mut().pending_open.take();
                if let Some(p) = pending {
                    let same = inner.st.borrow().player_video.as_ref().is_some_and(|v| lower_key(v) == lower_key(&p));
                    if same && inner.svc.profiles.is_logged_in() {
                        let generation = inner.svc.profiles.session_generation();
                        inner.st.borrow_mut().recording_generation = Some(generation);
                        inner.count_open(p, generation);
                    }
                }
            },
        );
    }

    /// The hub tab became visible / hidden.
    pub fn set_hub_active(&self, active: bool) {
        let was = std::mem::replace(&mut self.inner.st.borrow_mut().hub_active, active);
        if active && !was {
            self.inner.schedule_refresh();
        }
    }

    // ----- session

    pub fn is_logged_in(&self) -> bool {
        self.inner.st.borrow().logged_in
    }
    pub fn current_username(&self) -> Option<String> {
        self.inner.st.borrow().username.clone()
    }
    /// Pills (4 on macOS, 5 chips on Windows auth card).
    pub fn registered_usernames(&self) -> Vec<String> {
        self.inner.st.borrow().registered.clone()
    }
    pub fn last_username(&self) -> Option<String> {
        self.inner.st.borrow().last_username.clone()
    }
    /// "Girişi hatırla" checkbox state (default off; true only while a remembered session exists).
    pub fn remember_me(&self) -> bool {
        self.inner.st.borrow().remember_me
    }
    pub fn set_remember_me(&self, v: bool) {
        self.inner.st.borrow_mut().remember_me = v;
        self.inner.notify(&[HubChange::Session]);
    }
    pub fn is_authenticating(&self) -> bool {
        self.inner.st.borrow().is_authenticating
    }
    /// `Profil / Giriş` or the user name.
    pub fn profile_title(&self) -> String {
        self.current_username().filter(|_| self.is_logged_in()).unwrap_or_else(|| l!("Profil / Giriş"))
    }
    pub fn summary_text(&self) -> String {
        let st = self.inner.st.borrow();
        if st.logged_in {
            l!("%lld izlenen · %lld beğenilen", st.counts.watched, st.counts.liked)
        } else {
            l!("İzleme geçmişi için giriş yapın")
        }
    }
    pub fn login(&self, username: &str, password: SecretString, remember: bool) {
        self.inner.st.borrow_mut().remember_me = remember;
        let u = username.trim().to_owned();
        self.inner.authenticate(move |s| s.login(&u, &password, remember));
    }
    pub fn register(&self, username: &str, password: SecretString, remember: bool) {
        self.inner.st.borrow_mut().remember_me = remember;
        let u = username.trim().to_owned();
        self.inner.authenticate(move |s| s.register(&u, &password, remember));
    }
    /// Forgets UI state synchronously, then logs the store out (logout always wins).
    pub fn logout(&self) {
        let last = {
            let st = self.inner.st.borrow();
            st.last_position
        };
        if let Some(p) = last {
            self.inner.report(p, false);
        }
        {
            let mut st = self.inner.st.borrow_mut();
            st.recording_generation = None;
            st.closing_generation = Some(self.inner.svc.profiles.session_generation());
            st.remember_me = false;
            st.auth_error = None;
            if matches!(st.filter, VideoFeedFilter::CustomList(_)) {
                st.filter = VideoFeedFilter::All;
            }
        }
        self.inner.forget_profile_state();
        self.inner.notify(&[HubChange::Session, HubChange::Feed, HubChange::Lists, HubChange::Counts, HubChange::Engagement, HubChange::Thumbnail]);
        self.inner.perform(|s| {
            let _ = s.flush_playback_progress();
            s.logout()
        });
    }
    /// macOS only UI.
    pub fn delete_profile(&self, username: &str, password: SecretString) {
        let u = username.trim().to_owned();
        self.inner.forget_profile_state();
        self.inner.notify(&[HubChange::Session, HubChange::Feed]);
        self.inner.authenticate(move |s| s.delete_profile(&u, &password));
    }
    /// The auth form error.
    pub fn last_error(&self) -> Option<ProfileError> {
        self.inner.st.borrow().auth_error.clone()
    }
    pub fn clear_auth_error(&self) {
        self.inner.st.borrow_mut().auth_error = None;
        self.inner.notify(&[HubChange::Error]);
    }
    /// macOS alert `İşlem tamamlanamadı`.
    pub fn action_error(&self) -> Option<ProfileError> {
        self.inner.st.borrow().action_error.clone()
    }
    pub fn clear_action_error(&self) {
        self.inner.st.borrow_mut().action_error = None;
        self.inner.notify(&[HubChange::Error]);
    }

    // ----- feed

    /// Always true while logged out.
    pub fn shows_auth_card(&self) -> bool {
        !self.is_logged_in()
    }
    pub fn filter(&self) -> VideoFeedFilter {
        self.inner.st.borrow().filter.clone()
    }
    pub fn set_filter(&self, filter: VideoFeedFilter) {
        let changed = {
            let mut st = self.inner.st.borrow_mut();
            let changed = st.filter != filter;
            st.filter = filter;
            if changed {
                st.cards.clear();
                st.page_limit = self.inner.svc.policy.hub_card_page_size.unwrap_or(usize::MAX);
            }
            changed
        };
        if changed {
            self.inner.notify(&[HubChange::Feed]);
            self.inner.schedule_refresh();
        }
    }
    /// Disabled while logged out.
    pub fn is_search_enabled(&self) -> bool {
        self.is_logged_in()
    }
    pub fn search_query(&self) -> String {
        self.inner.st.borrow().search.clone()
    }
    /// Applies a search (Enter / "Ara"); ignored while logged out.
    pub fn search(&self, query: &str) {
        if !self.is_logged_in() {
            return;
        }
        let q = query.trim().to_owned();
        let changed = {
            let mut st = self.inner.st.borrow_mut();
            let changed = st.search != q;
            st.search = q;
            if changed {
                st.page_limit = self.inner.svc.policy.hub_card_page_size.unwrap_or(usize::MAX);
            }
            changed
        };
        if changed {
            self.inner.schedule_refresh();
        }
    }
    /// All cards (macOS) or the visible page (Windows paging).
    pub fn cards(&self) -> Vec<VideoCard> {
        let st = self.inner.st.borrow();
        if !st.logged_in {
            return Vec::new();
        }
        st.cards.iter().take(st.page_limit).cloned().collect()
    }
    pub fn total_cards(&self) -> usize {
        self.inner.st.borrow().cards.len()
    }
    pub fn has_loaded_feed(&self) -> bool {
        self.inner.st.borrow().has_loaded_feed
    }
    pub fn is_refreshing(&self) -> bool {
        self.inner.st.borrow().refreshing
    }
    /// Windows paging (40 per page).
    pub fn load_more(&self) {
        let Some(step) = self.inner.svc.policy.hub_card_page_size else { return };
        let grew = {
            let mut st = self.inner.st.borrow_mut();
            if st.page_limit < st.cards.len() {
                st.page_limit += step;
                true
            } else {
                false
            }
        };
        if grew {
            self.inner.notify(&[HubChange::Feed]);
            self.inner.request_thumbnails();
        }
    }
    /// `(title, hint)` of the empty feed.
    pub fn empty_state(&self) -> (String, String) {
        let st = self.inner.st.borrow();
        let search_hint = l!("Farklı bir anahtar kelime deneyin veya klasör gezgininden yeni bir video klasörü açın.");
        if !st.search.trim().is_empty() {
            return (l!("\"%@\" için eşleşen video bulunamadı", st.search.trim()), search_hint);
        }
        // Without a profile there is no history at all; the hint must not suggest that one is being kept.
        if !st.logged_in && !self.inner.svc.policy.hub_logged_out_empty_per_filter {
            return (
                l!("Gösterilecek video bulunamadı"),
                l!("Profil açık değilken izleme geçmişi tutulmaz. Klasör gezgininden bir video açabilir veya geçmişinizi görmek için giriş yapabilirsiniz."),
            );
        }
        let general = if st.logged_in {
            l!("Bir video açıp izlediğinizde, favorilere eklediğinizde veya puanladığınızda burada YouTube tarzı kartlar halinde listelenir.")
        } else {
            l!("Klasör gezgininden bir video klasörü açın. Giriş yaptığınızda izleme geçmişiniz de burada görünür.")
        };
        match &st.filter {
            VideoFeedFilter::History => (l!("Henüz izleme geçmişi yok"), general),
            VideoFeedFilter::Liked => (l!("Henüz favorilere eklenen video yok"), general),
            VideoFeedFilter::MostWatched => (l!("Henüz izlenen video yok"), general),
            VideoFeedFilter::TopRated => (l!("Henüz puanlanan video yok"), general),
            VideoFeedFilter::CustomList(id) => {
                let name = id
                    .as_ref()
                    .and_then(|id| st.lists.iter().find(|l| l.id.eq_ignore_ascii_case(id)))
                    .map(|l| l.name.clone())
                    .unwrap_or_else(|| l!("Liste"));
                (
                    l!("\"%@\" listesinde henüz video yok", name),
                    l!("Herhangi bir video kartındaki veya oynatıcıdaki \"+ Listeye ekle\" düğmesiyle bu listeye video ekleyebilirsiniz."),
                )
            }
            VideoFeedFilter::All => (l!("Gösterilecek video bulunamadı"), general),
        }
    }
    pub fn empty_state_text(&self) -> String {
        self.empty_state().0
    }
    pub fn counts(&self) -> ProfileCounts {
        self.inner.st.borrow().counts
    }
    /// Decoded picture bytes (JPEG/PNG/BMP) of a card; `None` logged out.
    pub fn thumbnail(&self, path: &Path) -> Option<Vec<u8>> {
        let st = self.inner.st.borrow();
        if !st.logged_in {
            return None;
        }
        st.thumbnails.get(&lower_key(path)).cloned()
    }
    pub fn refresh(&self) {
        self.inner.schedule_refresh();
    }
    /// Lets queued store work finish (tests, quit): drains the lanes.
    pub fn settle(&self, timeout: Duration) -> bool {
        self.inner.lanes.drain(Lane::Actions, timeout) && self.inner.lanes.drain(Lane::Feed, timeout)
    }

    // ----- lists

    pub fn lists(&self) -> Vec<UserVideoList> {
        let st = self.inner.st.borrow();
        if st.logged_in { st.lists.clone() } else { Vec::new() }
    }
    /// A named list, or the quick-create name (`Daha Sonra İzle`, `Listem N`, policy naming) for `None`.
    pub fn create_list(&self, name: Option<&str>) {
        if !self.is_logged_in() {
            return;
        }
        let names = self.quick_names(name);
        if names.is_empty() {
            return;
        }
        let silent = self.inner.svc.policy.hub_quick_list_naming == QuickListNaming::CountPlusOne;
        let store = self.inner.svc.profiles.clone();
        let key = self.inner.key();
        self.inner.lanes.run(
            Lane::Actions,
            move || {
                for n in names {
                    match store.create_list(&n) {
                        Ok(l) => return Ok(l),
                        Err(ProfileError::DuplicateListName) => continue,
                        Err(e) => return Err(e),
                    }
                }
                Ok(None)
            },
            move |r: Result<Option<UserVideoList>, ProfileError>| {
                let Some(inner) = key.get() else { return };
                match r {
                    Ok(Some(l)) => inner.st.borrow_mut().filter = VideoFeedFilter::CustomList(Some(l.id)),
                    Ok(None) => {}
                    Err(e) if !silent => {
                        inner.st.borrow_mut().action_error = Some(e);
                        inner.notify(&[HubChange::Error]);
                    }
                    Err(_) => {}
                }
                inner.notify(&[HubChange::Lists, HubChange::Feed]);
                inner.schedule_refresh();
            },
        );
    }

    fn quick_names(&self, name: Option<&str>) -> Vec<String> {
        if let Some(n) = name {
            let t = n.trim();
            return if t.is_empty() { Vec::new() } else { vec![t.to_owned()] };
        }
        let lists = self.lists();
        let existing: Vec<String> = lists.iter().map(|l| l.name.clone()).collect();
        let preferred = lists.is_empty().then(|| l!("Daha Sonra İzle"));
        match self.inner.svc.policy.hub_quick_list_naming {
            QuickListNaming::CountPlusOne => {
                vec![preferred.unwrap_or_else(|| format!("{} {}", l!("Listem"), lists.len() + 1))]
            }
            QuickListNaming::NameSequence => NameSequence::new(preferred.as_deref(), &l!("Listem"), &existing).take(500).collect(),
        }
    }

    /// "+ Yeni liste oluştur ve ekle".
    pub fn create_list_and_add(&self, path: &Path) {
        if !self.is_logged_in() {
            return;
        }
        let names = self.quick_names(None);
        let store = self.inner.svc.profiles.clone();
        let p = path.to_path_buf();
        self.inner.perform(move |s| {
            let _ = &store;
            for n in names {
                match s.create_list(&n) {
                    Ok(Some(l)) => {
                        s.set_in_list(&l.id, &p, true);
                        return Ok(());
                    }
                    Ok(None) => return Ok(()),
                    Err(ProfileError::DuplicateListName) => continue,
                    Err(e) => return Err(e),
                }
            }
            Ok(())
        });
    }
    pub fn rename_list(&self, id: &str, name: &str) {
        if !self.is_logged_in() {
            return;
        }
        let (id, name) = (id.to_owned(), name.trim().to_owned());
        self.inner.perform(move |s| match s.rename_list(&id, &name) {
            Ok(true) => Ok(()),
            Ok(false) => Err(ProfileError::ListNotFound),
            Err(e) => Err(e),
        });
    }
    pub fn delete_list(&self, id: &str) {
        if !self.is_logged_in() {
            return;
        }
        {
            let mut st = self.inner.st.borrow_mut();
            if let VideoFeedFilter::CustomList(Some(cur)) = &st.filter
                && cur.eq_ignore_ascii_case(id)
            {
                st.filter = VideoFeedFilter::All;
            }
        }
        let id = id.to_owned();
        self.inner.perform(move |s| {
            s.delete_list(&id);
            Ok(())
        });
    }
    pub fn toggle_in_list(&self, id: &str, path: &Path) {
        if !self.is_logged_in() {
            return;
        }
        let is_in = self.is_in_list(id, path);
        let (id, p) = (id.to_owned(), path.to_path_buf());
        self.inner.perform(move |s| {
            s.set_in_list(&id, &p, !is_in);
            Ok(())
        });
    }
    pub fn is_in_list(&self, id: &str, path: &Path) -> bool {
        let st = self.inner.st.borrow();
        if !st.logged_in {
            return false;
        }
        let k = lower_key(path);
        if st.engagement_loaded && st.player_video.as_ref().is_some_and(|v| lower_key(v) == k) {
            return st.engagement.list_ids.iter().any(|i| i.eq_ignore_ascii_case(id));
        }
        st.cards.iter().find(|c| lower_key(&c.path) == k).is_some_and(|c| c.list_ids.iter().any(|i| i.eq_ignore_ascii_case(id)))
            || st.lists.iter().any(|l| l.id.eq_ignore_ascii_case(id) && l.video_paths.iter().any(|p| p.to_lowercase() == k))
    }

    // ----- engagement / player

    /// The video open in the player (the engagement bar's add-to-list target); `None` logged out.
    pub fn player_video(&self) -> Option<PathBuf> {
        let st = self.inner.st.borrow();
        if st.logged_in { st.player_video.clone() } else { None }
    }

    pub fn engagement(&self) -> PlayerEngagement {
        let st = self.inner.st.borrow();
        let mut e = st.engagement.clone();
        e.visible = st.logged_in && st.player_video.is_some();
        e
    }
    fn edit_card(&self, path: &Path, f: impl Fn(&mut PendingEdit, &mut VideoCard, &mut PlayerEngagement, bool)) -> bool {
        let mut st = self.inner.st.borrow_mut();
        if !st.logged_in {
            return false;
        }
        let k = lower_key(path);
        let is_open = st.player_video.as_ref().is_some_and(|v| lower_key(v) == k);
        let mut edit = st.pending_edits.get(&k).copied().unwrap_or_default();
        let mut dummy = VideoCard {
            path: PathBuf::new(),
            title: String::new(),
            file_name: String::new(),
            extension_label: String::new(),
            folder: String::new(),
            view_count: 0,
            is_liked: false,
            rating: 0,
            progress_ratio: 0.0,
            duration_seconds: 0.0,
            duration_text: String::new(),
            last_watched: None,
            last_watched_text: None,
            list_ids: Vec::new(),
            list_names: Vec::new(),
            sidecar_badge: None,
            has_thumbnail: false,
        };
        let mut eng = st.engagement.clone();
        let idx = st.cards.iter().position(|c| lower_key(&c.path) == k);
        match idx {
            Some(i) => {
                let mut card = st.cards[i].clone();
                f(&mut edit, &mut card, &mut eng, is_open);
                st.cards[i] = card;
            }
            None => f(&mut edit, &mut dummy, &mut eng, is_open),
        }
        if is_open {
            st.engagement = eng;
        }
        edit.outstanding += 1;
        st.pending_edits.insert(k, edit);
        true
    }
    fn finish_edit(&self, path: PathBuf, work: impl FnOnce(&dyn sa_profiles::ProfileStore) + Send + 'static) {
        let store = self.inner.svc.profiles.clone();
        let key = self.inner.key();
        let k = lower_key(&path);
        self.inner.lanes.run(
            Lane::Actions,
            move || work(store.as_ref()),
            move |()| {
                let Some(inner) = key.get() else { return };
                {
                    let mut st = inner.st.borrow_mut();
                    if let Some(e) = st.pending_edits.get_mut(&k) {
                        e.outstanding -= 1;
                        if e.outstanding <= 0 {
                            st.pending_edits.remove(&k);
                        }
                    }
                }
                inner.schedule_refresh();
            },
        );
    }
    /// Like/unlike a video (the open one when `path` is `None`).
    pub fn set_liked_for(&self, path: &Path, liked: bool) {
        if !self.edit_card(path, |e, c, eng, open| {
            e.liked = Some(liked);
            c.is_liked = liked;
            if open {
                eng.is_liked = liked;
            }
        }) {
            return;
        }
        self.inner.notify(&[HubChange::Feed, HubChange::Engagement]);
        let p = path.to_path_buf();
        self.finish_edit(path.to_path_buf(), move |s| {
            let _ = s.set_liked(&p, liked);
        });
    }
    pub fn set_rating_for(&self, path: &Path, stars: u8) {
        let stars = stars.min(5);
        if !self.edit_card(path, |e, c, eng, open| {
            e.rating = Some(i64::from(stars));
            c.rating = i64::from(stars);
            if open {
                eng.rating = i64::from(stars);
            }
        }) {
            return;
        }
        self.inner.notify(&[HubChange::Feed, HubChange::Engagement]);
        let p = path.to_path_buf();
        self.finish_edit(path.to_path_buf(), move |s| {
            let _ = s.set_rating(&p, stars);
        });
    }
    pub fn toggle_like_for(&self, path: &Path) {
        let liked = {
            let st = self.inner.st.borrow();
            let k = lower_key(path);
            if st.player_video.as_ref().is_some_and(|v| lower_key(v) == k) {
                st.engagement.is_liked
            } else {
                st.cards.iter().find(|c| lower_key(&c.path) == k).is_some_and(|c| c.is_liked)
            }
        };
        self.set_liked_for(path, !liked);
    }
    pub fn set_liked(&self, liked: bool) {
        let v = self.inner.st.borrow().player_video.clone();
        if let Some(v) = v {
            self.set_liked_for(&v, liked);
        }
    }
    pub fn set_rating(&self, stars: u8) {
        let v = self.inner.st.borrow().player_video.clone();
        if let Some(v) = v {
            self.set_rating_for(&v, stars);
        }
    }
    /// Resets view counts / last watched; keeps likes, ratings and lists.
    pub fn clear_watch_history(&self) {
        if !self.is_logged_in() {
            return;
        }
        self.inner.perform(|s| {
            s.clear_watch_history();
            Ok(())
        });
    }
    /// The player opened/closed a video (counts as a view when logged in and `counts_as_open`).
    pub fn player_video_changed(&self, path: Option<&Path>, counts_as_open: bool) {
        let same = {
            let st = self.inner.st.borrow();
            match (&st.player_video, path) {
                (None, None) => true,
                (Some(a), Some(b)) => lower_key(a) == lower_key(b),
                _ => false,
            }
        };
        let store_logged_in = self.inner.svc.profiles.is_logged_in();
        let generation = self.inner.svc.profiles.session_generation();
        if same {
            // The same video opened again under a login that began after it was opened: the new session records it.
            let reopen = {
                let st = self.inner.st.borrow();
                counts_as_open && st.restored && st.logged_in && store_logged_in && st.recording_generation != Some(generation)
            };
            if reopen && let Some(p) = path {
                self.inner.st.borrow_mut().recording_generation = Some(generation);
                self.inner.count_open(p.to_path_buf(), generation);
            }
            return;
        }
        {
            let mut st = self.inner.st.borrow_mut();
            st.player_video = path.map(Path::to_path_buf);
            st.pending_open = None;
            // Recorded only for the session that is open now; nobody logged in means nothing is recorded for it.
            st.recording_generation = (st.restored && st.logged_in && store_logged_in).then_some(generation);
            st.engagement = PlayerEngagement::default();
            st.engagement_loaded = false;
            st.last_position = None;
            st.last_duration = 0.0;
            st.last_progress_write = None;
        }
        self.inner.notify(&[HubChange::Engagement]);
        let Some(p) = path else { return };
        let (restored, logged_in) = {
            let st = self.inner.st.borrow();
            (st.restored, st.logged_in)
        };
        if !counts_as_open {
            self.inner.schedule_refresh();
            return;
        }
        if !restored {
            self.inner.st.borrow_mut().pending_open = Some(p.to_path_buf());
            self.inner.schedule_refresh();
            return;
        }
        if !logged_in || !self.inner.svc.profiles.is_logged_in() {
            // Rule 6: nothing recorded logged out.
            self.inner.schedule_refresh();
            return;
        }
        self.inner.count_open(p.to_path_buf(), generation);
    }
    /// 5 s cadence, flush on pause/leave/seek; nothing recorded logged out.
    pub fn report_playback(&self, report: PlaybackReport) {
        match report {
            PlaybackReport::Opened { duration } => {
                if duration.is_finite() && duration > 0.0 {
                    self.inner.st.borrow_mut().last_duration = duration;
                }
            }
            PlaybackReport::Progress { position, duration } => {
                let due = {
                    let mut st = self.inner.st.borrow_mut();
                    if duration.is_finite() && duration > 0.0 {
                        st.last_duration = duration;
                    }
                    st.last_position = Some(position);
                    st.last_progress_write.is_none_or(|t| t.elapsed() >= PROGRESS_INTERVAL)
                };
                if due {
                    self.inner.report(position, false);
                }
            }
            PlaybackReport::Paused { position } | PlaybackReport::Leaving { position } | PlaybackReport::Seeked { position } => {
                self.inner.st.borrow_mut().last_position = Some(position);
                self.inner.report(position, true);
            }
        }
    }
    /// ≤ 2 s (macOS quit): final progress write and flush.
    pub fn prepare_for_quit(&self) -> bool {
        let last = self.inner.st.borrow().last_position;
        if let Some(p) = last {
            self.inner.report(p, true);
        }
        self.inner.lanes.drain(Lane::Actions, QUIT_TIMEOUT)
    }
}

fn st_logged_in(inner: &Inner) -> bool {
    inner.st.borrow().logged_in
}
