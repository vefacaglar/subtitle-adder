//! Demo content for look checks (`sa-shot --scene <name>`): fills the main-window bridges with representative data
//! without any view model, engine or player. Uses only the generated UI types, Slint and the localization lookups,
//! so it also runs in stand-alone previews of the UI trees.

use slint::{Color, ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};

use crate::ui::{AppBridge, ExplorerBridge, ExplorerRowData, MainBridge, MainWindow, MenuEntry, PickerItem, PickerModelRow, QueueRowData, SentenceRowData, Workspace};

fn l(key: &str) -> SharedString {
    sa_core::l10n::l(key).into()
}

fn picker(items: &[(&str, bool)]) -> ModelRc<PickerItem> {
    ModelRc::new(VecModel::from(items.iter().map(|(t, e)| PickerItem { title: (*t).into(), enabled: *e }).collect::<Vec<_>>()))
}

/// A synthetic "video frame": dark gradient with a horizon, so the letterboxing and the overlay are visible.
fn demo_frame(w: u32, h: u32) -> Image {
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(w, h);
    for (i, p) in buf.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = ((i as u32 % w) as f32 / w as f32, (i as u32 / w) as f32 / h as f32);
        let sky = y < 0.62;
        let (r, g, b) = if sky {
            (40.0 + 120.0 * y + 30.0 * x, 70.0 + 110.0 * y, 120.0 + 80.0 * y)
        } else {
            (35.0 + 30.0 * x, 60.0 + 40.0 * (1.0 - y), 40.0)
        };
        *p = Rgba8Pixel { r: r.min(255.0) as u8, g: g.min(255.0) as u8, b: b.min(255.0) as u8, a: 255 };
    }
    Image::from_rgba8(buf)
}

#[allow(clippy::too_many_arguments)]
fn row(n: i32, t: &str, text: &str, speaker: Option<i32>, tr: Option<&str>, active: bool, selected: bool, mac: bool) -> SentenceRowData {
    let speaker_text = match speaker {
        Some(s) if mac => sa_core::l10n::l_args("Konuşmacı %lld", &[sa_core::l10n::Arg::Int(s as i64 + 1)]),
        Some(s) => sa_core::l10n::l_args("K%lld", &[sa_core::l10n::Arg::Int(s as i64 + 1)]),
        None if mac => sa_core::l10n::l("Konuşmacı yok"),
        None => "—".to_owned(),
    };
    let colors = [Color::from_rgb_u8(0, 122, 255), Color::from_rgb_u8(255, 149, 0), Color::from_rgb_u8(40, 205, 65)];
    SentenceRowData {
        uid: format!("00000000-0000-0000-0000-{n:012}").into(),
        number: n.to_string().into(),
        time_text: t.into(),
        text: text.into(),
        has_speaker: speaker.is_some(),
        speaker_text: speaker_text.into(),
        speaker_color: speaker.map(|s| colors[s as usize % 3]).unwrap_or(Color::from_argb_u8(0, 0, 0, 0)),
        has_translation: tr.is_some(),
        translation_text: tr.unwrap_or("").into(),
        is_stale: n == 4,
        needs_review: n == 6,
        is_active: active,
        is_selected: selected,
        accessible_label: format!("{n}. {text}").into(),
    }
}

/// The open translation-model picker: `picker-local` (on-device models of the fallback catalog), `picker-provider`
/// (a cloud provider's models after a refresh, external glyph on the button), `picker-empty` (a search without hits).
fn fill_picker(w: &MainWindow, scene: &str, mac: bool) {
    let main = w.global::<MainBridge>();
    let provider = scene == "picker-provider";
    let providers = [sa_core::l10n::l("Yerel modeller (Cihaz üzerinde)"), "OpenRouter".to_owned(), "LM Studio (Ofis)".to_owned()];
    main.set_picker_providers(picker(&providers.iter().map(|t| (t.as_str(), true)).collect::<Vec<_>>()));
    main.set_picker_provider_index(if provider { 1 } else { 0 });
    let rows: Vec<PickerModelRow> = if scene == "picker-empty" {
        Vec::new()
    } else if provider {
        ["deepseek/deepseek-v4-flash", "google/gemma-4-12b-it", "meta-llama/llama-4-scout", "openai/gpt-5-mini", "qwen/qwen3.5-9b"]
            .iter()
            .map(|m| PickerModelRow { id: (*m).into(), title: (*m).into(), current: *m == "google/gemma-4-12b-it", enabled: true, highlighted: *m == "google/gemma-4-12b-it" })
            .collect()
    } else {
        sa_core::models::fallback_models()
            .iter()
            .filter(|m| m.is_translation())
            .map(|m| {
                let title = if mac { sa_core::models::ModelLabel::translation_menu_title(m) } else { sa_core::models::ModelLabel::menu_title(m) };
                PickerModelRow { id: m.id.clone().into(), title: title.into(), current: m.id == "madlad-3b", enabled: true, highlighted: m.id == "madlad-3b" }
            })
            .collect()
    };
    main.set_picker_rows(ModelRc::new(VecModel::from(rows)));
    main.set_picker_empty_text(if scene == "picker-empty" { l("Aramayla eşleşen model yok.") } else { SharedString::new() });
    main.set_picker_status(if provider { "Bağlantı başarılı: 5 model bulundu.".into() } else { SharedString::new() });
    main.set_picker_show_refresh(!mac || provider);
    main.set_picker_can_refresh(provider);
    if provider {
        main.set_translation_model_title(if mac { "OpenRouter: google/gemma-4-12b-it".into() } else { "OpenRouter: google/gemma-4-12b-it".into() });
        main.set_translation_model_kind("cloud".into());
    }
    // Opens once the window is laid out (the flyout's position follows the button).
    let weak = w.as_weak();
    slint::Timer::single_shot(std::time::Duration::ZERO, move || {
        if let Some(w) = weak.upgrade() {
            let main = w.global::<MainBridge>();
            main.set_picker_open_epoch(main.get_picker_open_epoch() + 1);
        }
    });
}

/// Fills the window for `scene`: `empty`, `editor`, `editor-side`, `editor-player`, `subtitles-only`, `working`,
/// `error`, `playlist`, `playlist-empty`, `platform`, `full-screen`, `theater`, `picker-local`, `picker-provider`,
/// `picker-empty`.
pub fn fill(w: &MainWindow, scene: &str, mac: bool) {
    let app = w.global::<AppBridge>();
    let main = w.global::<MainBridge>();
    let ex = w.global::<ExplorerBridge>();

    let langs: Vec<(&str, bool)> = vec![("Otomatik", true), ("Türkçe", true), ("İngilizce", true), ("Almanca", true)];
    let titles: Vec<String> = langs.iter().map(|(t, _)| sa_core::l10n::l(t)).collect();
    main.set_language_items(picker(&titles.iter().map(|t| (t.as_str(), true)).collect::<Vec<_>>()));
    main.set_language_index(1);
    main.set_model_title("large-v3".into());
    main.set_diarize(true);
    let targets = [sa_core::l10n::l("İngilizce"), sa_core::l10n::l("Türkçe"), sa_core::l10n::l("Almanca")];
    main.set_target_items(picker(&[(targets[0].as_str(), true), (targets[1].as_str(), false), (targets[2].as_str(), true)]));
    main.set_target_index(0);
    main.set_target_help(l("Çeviri hedef dili; kaynak dille aynı olan seçilemez."));
    main.set_translation_model_title("MADLAD-400 3B".into());
    main.set_translation_model_help("MADLAD-400 3B".into());
    main.set_can_generate(true);
    main.set_can_generate_and_translate(true);
    main.set_can_translate(true);
    main.set_auto_save(true);
    main.set_status_kind(0);
    main.set_status_text(l("Hazır"));
    main.set_status_fraction(-1.0);
    main.set_format_index(0);
    main.set_export_language_index(0);
    main.set_translation_title(if mac { sa_core::l10n::l_args("Çeviri (%@)", &[sa_core::l10n::Arg::Str(targets[0].clone())]).into() } else { targets[0].clone().into() });
    main.set_layout_mode(2);
    main.set_playlist_count(0);
    main.set_menu_entries(ModelRc::new(VecModel::from(Vec::<MenuEntry>::new())));
    main.set_undo_title(sa_core::l10n::l_args("%@ İşlemini Geri Al", &[sa_core::l10n::Arg::Str(sa_core::l10n::l("Metni Değiştir"))]).into());
    main.set_redo_title(sa_core::l10n::l("Yinele").into());
    main.set_empty_title(l("Henüz altyazı yok"));
    main.set_empty_detail(l("Araç çubuğundan “Altyazı üret”e bas."));

    // Explorer
    ex.set_current_folder(if mac { l("Filmler") } else { l("Videolar") });
    ex.set_current_path(if mac { "/Users/demo/Movies".into() } else { "C:\\Users\\demo\\Videos".into() });
    ex.set_can_go_up(true);
    ex.set_can_open_folder(true);
    let size = |s: &str| SharedString::from(s);
    let er = |name: &str, folder: bool, detail: &str, badge: &str, active: bool| ExplorerRowData {
        path: name.into(),
        name: name.into(),
        is_folder: folder,
        is_audio: name.ends_with(".mp3"),
        detail: detail.into(),
        badge: badge.into(),
        is_active: active,
        accessible_label: name.into(),
    };
    ex.set_rows(ModelRc::new(VecModel::from(vec![
        er("Konferans 2026", true, "", "", false),
        er("Röportajlar", true, "", "", false),
        er("dialog-en.mp4", false, &size("2,4 MB"), "SRT", true),
        er("Sunum kaydı.mkv", false, &size("812,6 MB"), "", false),
        er("Podcast bölüm 12.mp3", false, &size("54,1 MB"), "VTT", false),
        er("demo.mp4", false, &size("410 KB"), "", false),
    ])));
    let status = if mac {
        sa_core::l10n::l_args("%lld medya · indekste %lld dosya", &[sa_core::l10n::Arg::Int(4), sa_core::l10n::Arg::Int(128)])
    } else {
        sa_core::l10n::l_args("%lld video · SSD indeksinde %lld dosya", &[sa_core::l10n::Arg::Int(4), sa_core::l10n::Arg::Int(128)])
    };
    ex.set_status_text(status.into());
    ex.set_empty_text(l("Bu klasörde medya dosyası bulunamadı"));

    let with_video = !matches!(scene, "empty" | "playlist-empty" | "subtitles-only" | "platform");
    let with_lines = !matches!(scene, "empty" | "playlist-empty" | "platform" | "editor-player");
    app.set_window_title(if with_video { "dialog-en.mp4".into() } else { "Subtitle Adder".into() });
    app.set_title_file(if with_video { "— dialog-en.mp4".into() } else { "".into() });
    main.set_has_video(with_video);
    main.set_has_sentences(with_lines);
    if with_video {
        main.set_video_frame(demo_frame(640, 360));
        main.set_video_aspect(16.0 / 9.0);
        main.set_overlay_text("- Hello, how are you doing today?".into());
        main.set_elapsed_text(if mac { "0:07".into() } else { "00:07".into() });
        main.set_total_text(if mac { "0:38".into() } else { "00:38".into() });
        main.set_remaining_text("-0:31".into());
        main.set_seek_ratio(0.18);
        main.set_buffered_ratio(0.55);
        main.set_seek_interactive(true);
        main.set_is_playing(false);
        main.set_hud_visible(true);
        main.set_picker_visible(true);
        main.set_volume(0.8);
        main.set_can_save_subtitle(true);
        main.set_can_export(true);
    }
    if with_lines {
        let tr = scene != "subtitles-only";
        let rows = vec![
            row(1, "00:00.00 → 00:03.42", "Hello, how are you doing today?", Some(0), tr.then_some("Merhaba, bugün nasılsın?"), true, false, mac),
            row(2, "00:03.42 → 00:06.10", "I wanted to ask you about the project.", Some(0), tr.then_some("Sana proje hakkında sormak istedim."), false, false, mac),
            row(3, "00:06.80 → 00:10.95", "I am doing well, thank you. The project is going fine.", Some(1), tr.then_some("İyiyim, teşekkürler. Proje iyi gidiyor."), false, true, mac),
            row(4, "00:10.95 → 00:13.20", "We finished the first part yesterday.", Some(1), tr.then_some("İlk bölümü dün bitirdik."), false, false, mac),
            row(5, "00:13.90 → 00:17.30", "That is great news. When do you think the second part will be ready?", Some(0), tr.then_some("Bu harika bir haber. Sence ikinci bölüm ne zaman hazır olur?"), false, false, mac),
            row(6, "00:17.30 → 00:20.85", "I think we will need about two more weeks.", Some(1), tr.then_some("I think we will need about two more weeks."), false, false, mac),
            row(7, "00:21.40 → 00:24.00", "There are still a few problems to solve.", Some(1), None, false, false, mac),
        ];
        main.set_rows(ModelRc::new(VecModel::from(rows)));
        main.set_shows_speakers(true);
        main.set_selected_start_text("00:06.80".into());
        main.set_selected_end_text("00:10.95".into());
        main.set_timing_enabled(true);
        main.set_can_split(true);
        main.set_can_merge(true);
        main.set_can_delete(true);
        main.set_can_retranslate(true);
        main.set_can_undo(true);
        main.set_has_translations(tr);
        main.set_display_mode(if tr { 1 } else { 0 });
        main.set_export_language_index(if tr { 1 } else { 0 });
        if tr {
            main.set_overlay_text("- Merhaba, bugün nasılsın?".into());
        }
    }
    match scene {
        "editor-side" => {
            main.set_side_by_side(true);
            main.set_layout_mode(1);
        }
        "editor-player" => {
            main.set_editor_hidden(true);
            main.set_layout_mode(0);
        }
        "full-screen" => {
            app.set_is_full_screen(true);
            main.set_layout_mode(0);
            main.set_hud_visible(true);
        }
        "theater" => {
            app.set_is_theater_mode(true);
        }
        "working" => {
            main.set_is_working(true);
            main.set_can_cancel(true);
            main.set_status_kind(1);
            main.set_status_fraction(0.42);
            main.set_status_text(sa_core::l10n::l_args("Konuşma metne çevriliyor… %lld%%", &[sa_core::l10n::Arg::Int(42)]).into());
            main.set_badge_text(sa_core::l10n::l_args("Çevriliyor… %lld%%", &[sa_core::l10n::Arg::Int(45)]).into());
        }
        "error" => {
            main.set_error_open(true);
            main.set_error_text(sa_core::l10n::l_args("Video oynatılamadı: %@", &[sa_core::l10n::Arg::Str("MF_E_UNSUPPORTED_FORMAT (0xC00D36C4)".into())]).into());
            main.set_go_online_visible(true);
            main.set_status_kind(3);
            main.set_status_text(sa_core::l10n::l_args("%@ indirilmemiş. Çevrimdışı mod açıkken model indirilemez.", &[sa_core::l10n::Arg::Str("En doğru".into())]).into());
            main.set_retry_visible(true);
            main.set_retry_title(l("Çevrimdışı modu kapat ve indir"));
            main.set_model_notice(l("Seçili model bu dil için listede yoktu; en doğru uygun model seçildi."));
            main.set_playback_notice(sa_core::l10n::l_args("Video oynatılamadı: %@", &[sa_core::l10n::Arg::Str("format".into())]).into());
        }
        "playlist" | "playlist-empty" => {
            main.set_workspace(Workspace::Playlist);
            if scene == "playlist" {
                let q = |n: &str, f: &str, st: i32, msg: &str, p: f32| QueueRowData {
                    id: n.into(),
                    file_name: n.into(),
                    folder_path: f.into(),
                    source_language_title: l("Otomatik"),
                    status_text: msg.into(),
                    state: st,
                    audio_ready: st == 0,
                    indeterminate: false,
                    progress: p,
                    can_edit: st == 0,
                    can_stop: st == 1,
                    is_playlist_item: true,
                    accessible_label: n.into(),
                };
                main.set_queue_rows(ModelRc::new(VecModel::from(vec![
                    q("Konferans açılış.mp4", "C:\\Users\\demo\\Videos\\Konferans 2026", 2, &sa_core::l10n::l("Tamamlandı"), 1.0),
                    q("Panel tartışması.mkv", "C:\\Users\\demo\\Videos\\Konferans 2026", 1, &sa_core::l10n::l_args("Konuşma metne çevriliyor… %lld%%", &[sa_core::l10n::Arg::Int(63)]), 0.63),
                    q("Kapanış.mp4", "C:\\Users\\demo\\Videos\\Konferans 2026", 0, &sa_core::l10n::l("Sırada"), 0.0),
                ])));
                main.set_has_playlist_items(true);
                main.set_has_list_rows(true);
                main.set_playlist_count(3);
                main.set_can_start_playlist(true);
                main.set_playlist_summary(sa_core::l10n::l_args("%lld / %lld tamamlandı", &[sa_core::l10n::Arg::Int(1), sa_core::l10n::Arg::Int(3)]).into());
            } else {
                main.set_playlist_summary(l("Listede henüz video yok."));
            }
        }
        "platform" => main.set_workspace(Workspace::Platform),
        "picker-local" | "picker-provider" | "picker-empty" => fill_picker(w, scene, mac),
        "menu" => {
            main.set_menu_entries(ModelRc::new(VecModel::from(vec![
                MenuEntry { id: "a".into(), title: "large-v3 · hazır".into(), checkable: true, checked: true, radio: true, enabled: true, ..Default::default() },
                MenuEntry { separator: true, ..Default::default() },
                MenuEntry { id: "b".into(), title: "Parakeet v3 · ilk kullanımda ~640 MB indirilir".into(), checkable: true, radio: true, enabled: true, ..Default::default() },
            ])));
        }
        _ => {}
    }
}
