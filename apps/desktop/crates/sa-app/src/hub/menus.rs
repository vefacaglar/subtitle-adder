//! Hub menus (pure): the add-to-list menu shared by the engagement bar and every card (win-ui-hub-settings §4.1,
//! mac-ui §7.5 `AddToListMenu`), the macOS card and list-pill context menus. Activation ids are resolved by the
//! presenter against the target captured when the menu opened.

use slint::{Color, SharedString};

use sa_core::l;
use sa_profiles::UserVideoList;

use crate::ui::MenuEntry;

/// Activation ids.
pub const ADD_NEW: &str = "add-new";
pub const ADD_LIST_PREFIX: &str = "add-list:";
pub const CARD_PLAY: &str = "card-play";
pub const CARD_REVEAL: &str = "card-reveal";
pub const LIST_RENAME: &str = "list-rename";
pub const LIST_DELETE: &str = "list-delete";

fn entry(id: &str, title: impl Into<SharedString>) -> MenuEntry {
    MenuEntry {
        id: id.into(),
        title: title.into(),
        glyph: SharedString::new(),
        symbol: SharedString::new(),
        tint: Color::from_argb_u8(0, 0, 0, 0),
        checkable: false,
        checked: false,
        radio: false,
        enabled: true,
        separator: false,
        destructive: false,
        hint: SharedString::new(),
        level: 0,
        submenu: false,
    }
}

fn separator() -> MenuEntry {
    MenuEntry { separator: true, enabled: false, ..entry("", "") }
}

/// The add-to-list menu for one video. `logged_in && has_path` else a single disabled
/// `Listeye eklemek için giriş yapın`. Lists in profile order as check items `"{name} ({count})"`, a separator,
/// then the quick-create item (glyph E710 / symbol `plus`).
pub fn add_to_list(lists: &[UserVideoList], logged_in: bool, has_path: bool, is_in: impl Fn(&str) -> bool) -> Vec<MenuEntry> {
    if !logged_in || !has_path {
        return vec![MenuEntry { enabled: false, ..entry("", l!("Listeye eklemek için giriş yapın")) }];
    }
    let mut out: Vec<MenuEntry> = lists
        .iter()
        .map(|list| MenuEntry {
            checkable: true,
            checked: is_in(&list.id),
            ..entry(&format!("{ADD_LIST_PREFIX}{}", list.id), format!("{} ({})", list.name, list.video_paths.len()))
        })
        .collect();
    if !lists.is_empty() {
        out.push(separator());
    }
    let title = if lists.is_empty() { l!("+ \"Daha Sonra İzle\" listesi oluştur ve ekle") } else { l!("+ Yeni liste oluştur ve ekle") };
    out.push(MenuEntry { glyph: "\u{E710}".into(), symbol: "plus".into(), ..entry(ADD_NEW, title) });
    out
}

/// macOS card context menu: `Oynat` (play.fill), `Finder'da göster` (folder).
pub fn card_context() -> Vec<MenuEntry> {
    vec![
        MenuEntry { symbol: "play.fill".into(), glyph: "\u{E768}".into(), ..entry(CARD_PLAY, l!("Oynat")) },
        MenuEntry { symbol: "folder".into(), glyph: "\u{E8B7}".into(), ..entry(CARD_REVEAL, l!("Finder'da göster")) },
    ]
}

/// macOS list-pill context menu: `Yeniden adlandır` (pencil), `Listeyi sil` (trash, destructive).
pub fn list_context() -> Vec<MenuEntry> {
    vec![
        MenuEntry { symbol: "pencil".into(), ..entry(LIST_RENAME, l!("Yeniden adlandır")) },
        MenuEntry { symbol: "trash".into(), destructive: true, ..entry(LIST_DELETE, l!("Listeyi sil")) },
    ]
}
