//! Demo content for look checks of the hub area (`sa-shot --hub <scene>`): fills `HubBridge` with representative
//! data without a view model or a profile store. Texts go through the catalogs, so `--lang en` shows English.
//!
//! Scenes: `logged-out` (auth card), `feed` (logged in, cards, lists), `empty` (logged in, History empty),
//! `flyout-out` / `flyout-in` (profile flyout / popover open), `engagement` (the player's engagement bar).

use std::path::PathBuf;

use slint::{ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};

use sa_core::l;
use sa_profiles::{Ticks, UserVideoList, VideoFeedFilter};
use sa_viewmodels::hub::VideoCard;

use super::cards::{self, HubFlavor};
use crate::ui::{HubBridge, MainWindow};

/// A synthetic 16:9 frame (sky / horizon) standing in for a video thumbnail.
fn picture(seed: u32) -> Image {
    let (w, h) = (384u32, 216u32);
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(w, h);
    let hue = (seed * 67 % 255) as f32;
    for (i, p) in buf.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = ((i as u32 % w) as f32 / w as f32, (i as u32 / w) as f32 / h as f32);
        let sky = y < 0.58 + 0.08 * (x * 6.0 + seed as f32).sin();
        let (r, g, b) = if sky {
            (40.0 + 0.4 * hue + 90.0 * y, 80.0 + 100.0 * y, 150.0 + 60.0 * y - 0.2 * hue)
        } else {
            (30.0 + 40.0 * x, 70.0 + 0.3 * hue * (1.0 - y), 45.0)
        };
        *p = Rgba8Pixel { r: r.clamp(0.0, 255.0) as u8, g: g.clamp(0.0, 255.0) as u8, b: b.clamp(0.0, 255.0) as u8, a: 255 };
    }
    Image::from_rgba8(buf)
}

fn card(name: &str, dir: &str, views: i64, liked: bool, rating: i64, progress: f64, seconds: f64, badge: Option<&str>, lists: &[&str], watched: bool) -> VideoCard {
    let path = PathBuf::from(dir).join(name);
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let format = match HubFlavor::native() {
        HubFlavor::Windows => sa_viewmodels::policy::HubDateFormat::DotNetGeneral,
        HubFlavor::Mac => sa_viewmodels::policy::HubDateFormat::AppleAbbreviated,
    };
    let when = Ticks(Ticks::UNIX_EPOCH + 1_793_000_000 * 10_000_000);
    VideoCard {
        title: stem,
        file_name: name.to_owned(),
        extension_label: path.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default(),
        folder: sa_viewmodels::hub::folder_line(dir),
        view_count: views,
        is_liked: liked,
        rating,
        progress_ratio: progress,
        duration_seconds: seconds,
        duration_text: sa_viewmodels::hub::duration_text(seconds, format),
        last_watched: watched.then_some(when),
        last_watched_text: watched.then(|| {
            l!("Son izleme: %@", sa_viewmodels::hub::date_text(when, format, sa_core::l10n::LanguageState::shared().language()).as_str())
        }),
        list_ids: lists.iter().map(|l| l.to_lowercase()).collect(),
        list_names: lists.iter().map(|l| (*l).to_owned()).collect(),
        sidecar_badge: badge.map(str::to_owned),
        has_thumbnail: true,
        path,
    }
}

fn list(id: &str, name: &str, n: usize) -> UserVideoList {
    UserVideoList { id: id.into(), name: name.into(), video_paths: (0..n).map(|i| format!("v{i}")).collect(), created_at: Ticks(0), updated_at: Ticks(0) }
}

fn strings(v: &[&str]) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(v.iter().map(|s| SharedString::from(*s)).collect::<Vec<_>>()))
}

/// Opens the profile flyout/popover once the window is laid out (its position follows the button).
fn open_popover_later(w: &MainWindow) {
    let weak = w.as_weak();
    slint::Timer::single_shot(std::time::Duration::ZERO, move || {
        if let Some(w) = weak.upgrade() {
            let hub = w.global::<HubBridge>();
            hub.set_popover_open_epoch(hub.get_popover_open_epoch() + 1);
        }
    });
}

/// Fills the hub bridge for `scene`.
pub fn fill(w: &MainWindow, scene: &str) {
    let flavor = HubFlavor::native();
    let hub = w.global::<HubBridge>();
    let names = ["alice", "bora", "Ceyda", "deniz", "Emre"];
    hub.set_card_usernames(strings(&names[..flavor.card_chip_count()]));
    hub.set_flyout_usernames(strings(&names[..4]));
    hub.set_last_username("Ceyda".into());
    let logged_in = !matches!(scene, "logged-out" | "flyout-out");
    hub.set_is_logged_in(logged_in);
    hub.set_shows_auth_card(!logged_in);
    hub.set_search_enabled(logged_in);
    if !logged_in {
        hub.set_profile_title(l!("Profil / Giriş").into());
        hub.set_summary_text(l!("İzleme geçmişi için giriş yapın").into());
        if scene == "flyout-out" {
            open_popover_later(w);
        }
        return;
    }
    hub.set_current_username("Ceyda".into());
    hub.set_profile_title("Ceyda".into());
    hub.set_summary_text(l!("%lld izlenen · %lld beğenilen", 12i64, 4i64).into());
    hub.set_watched_count(12);
    hub.set_liked_count(4);
    hub.set_rated_count(3);
    let lists = vec![list("later", &l!("Daha Sonra İzle"), 3), list("docs", "Belgeseller", 5)];
    let filter = if scene == "empty" { VideoFeedFilter::History } else { VideoFeedFilter::CustomList(Some("docs".into())) };
    hub.set_filter(if scene == "feed" { 0 } else { cards::filter_index(&filter) });
    let filter_for_lists = if scene == "feed" { VideoFeedFilter::All } else { filter.clone() };
    hub.set_lists(ModelRc::new(VecModel::from(cards::list_rows(&lists, &filter_for_lists))));
    match scene {
        "empty" => {
            hub.set_empty_title(l!("Henüz izleme geçmişi yok").into());
            hub.set_empty_hint(
                l!("Bir video açıp izlediğinizde, favorilere eklediğinizde veya puanladığınızda burada YouTube tarzı kartlar halinde listelenir.").into(),
            );
            hub.set_empty_symbol(cards::empty_symbol(&VideoFeedFilter::History, false).into());
        }
        "engagement" | "flyout-in" => {
            hub.set_engagement_visible(true);
            hub.set_engagement_views(7);
            hub.set_engagement_liked(true);
            hub.set_engagement_rating(4);
            if scene == "flyout-in" {
                open_popover_later(w);
            }
        }
        _ => {
            let feed = [
                card("Sezon_2_Bolum_05.mp4", r"D:\Diziler\Sezon 2", 7, true, 5, 0.42, 2712.0, Some("SRT"), &["Daha Sonra İzle"], true),
                card("Kapadokya belgeseli.mkv", r"D:\Belgeseller", 3, false, 4, 0.0, 3725.0, Some("VTT"), &["Belgeseller"], true),
                card("Konferans kaydı.mov", r"C:\Users\ceyda\Videos", 1, true, 0, 0.88, 1260.0, None, &[], true),
                card("Yeni video.mp4", r"C:\Users\ceyda\Downloads", 0, false, 0, 0.0, 95.0, None, &[], false),
            ];
            let rows: Vec<_> = feed
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let img = (i < 3).then(|| picture(i as u32 + 1));
                    cards::card_row(c, flavor, img.as_ref())
                })
                .collect();
            hub.set_cards(ModelRc::new(VecModel::from(rows)));
            hub.set_can_clear_history(false);
        }
    }
}
