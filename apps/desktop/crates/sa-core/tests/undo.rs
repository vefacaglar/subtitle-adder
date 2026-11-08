//! UndoStack tests: ports of UndoStackTests plus the win-core §7 rules (groups, inverse naming, titles).

use std::sync::{Arc, Mutex};

use sa_core::l10n::{LanguageState, UiLanguage};
use sa_core::undo::{UndoStack, mac_redo_menu_title, mac_undo_menu_title, redo_title, undo_title};

static SHARED_LANGUAGE: Mutex<()> = Mutex::new(());

#[test]
fn groups_undo_together_in_reverse_order() {
    let mut stack: UndoStack<Vec<String>> = UndoStack::new();
    let mut log = Vec::new();
    stack.begin_group();
    stack.register("a", Box::new(|log: &mut Vec<String>, _| log.push("undo a".into())));
    stack.register("b", Box::new(|log: &mut Vec<String>, _| log.push("undo b".into())));
    stack.end_group();
    assert_eq!(stack.undo_action_key(), Some("b"));
    stack.undo(&mut log);
    assert_eq!(log, ["undo b", "undo a"]);
    assert!(!stack.can_undo());
    assert!(!stack.can_redo()); // nothing registered while undoing
}

/// The self-registering setter of the C# test: `set(v)` records how to go back to the old value.
fn set(value: &mut i32, stack: &mut UndoStack<i32>, v: i32) {
    let old = *value;
    *value = v;
    stack.register("set", Box::new(move |value: &mut i32, stack: &mut UndoStack<i32>| set(value, stack, old)));
}

#[test]
fn registrations_while_undoing_fill_redo() {
    let mut stack = UndoStack::new();
    let mut value = 0;
    set(&mut value, &mut stack, 1);
    set(&mut value, &mut stack, 2);
    stack.undo(&mut value);
    assert_eq!(value, 1);
    stack.redo(&mut value);
    assert_eq!(value, 2);
    stack.undo(&mut value);
    stack.undo(&mut value);
    assert_eq!(value, 0);
    assert!(!stack.can_undo());
    stack.redo(&mut value);
    assert_eq!(value, 1);
    stack.clear();
    assert!(!stack.can_redo());
}

#[test]
fn a_new_registration_clears_redo_and_nested_groups_push_once() {
    let mut stack = UndoStack::new();
    let mut value = 0;
    set(&mut value, &mut stack, 1);
    stack.undo(&mut value);
    assert!(stack.can_redo());
    stack.begin_group();
    stack.begin_group();
    set(&mut value, &mut stack, 5);
    stack.end_group();
    assert!(!stack.can_undo(), "the inner close does not push");
    set(&mut value, &mut stack, 6);
    stack.end_group();
    assert!(stack.can_undo());
    assert!(!stack.can_redo(), "an ordinary push clears redo");
    stack.end_group(); // extra close is ignored
    stack.undo(&mut value);
    assert_eq!(value, 0);
    assert!(!stack.can_undo());
}

#[test]
fn inverse_group_keeps_the_original_name() {
    let mut stack: UndoStack<i32> = UndoStack::new();
    stack.register(
        "Satırı Sil",
        Box::new(|_v: &mut i32, stack: &mut UndoStack<i32>| stack.register("Metni Değiştir", Box::new(|_v: &mut i32, _| {}))),
    );
    let mut v = 0;
    stack.undo(&mut v);
    assert_eq!(stack.redo_action_key(), Some("Satırı Sil"));
    stack.redo(&mut v);
    assert!(!stack.can_undo(), "the redo action registered nothing");
    assert!(!stack.can_redo());
}

#[test]
fn changed_fires_on_push_run_and_clear_but_not_inside_a_group() {
    let mut stack: UndoStack<i32> = UndoStack::new();
    let count = Arc::new(Mutex::new(0));
    let c = count.clone();
    let _sub = stack.subscribe(move |_| *c.lock().unwrap() += 1);
    stack.with_group(|s| {
        s.register("a", Box::new(|_: &mut i32, _| {}));
        s.register("b", Box::new(|_: &mut i32, _| {}));
    });
    assert_eq!(*count.lock().unwrap(), 1);
    let mut v = 0;
    stack.undo(&mut v);
    assert_eq!(*count.lock().unwrap(), 2, "fires even when the inverse is empty");
    stack.undo(&mut v); // empty stack: no event
    assert_eq!(*count.lock().unwrap(), 2);
    stack.clear();
    assert_eq!(*count.lock().unwrap(), 3);
}

#[test]
fn undo_inside_an_action_is_ignored() {
    let mut stack: UndoStack<i32> = UndoStack::new();
    stack.register("x", Box::new(|v: &mut i32, _| *v += 1));
    stack.register(
        "y",
        Box::new(|v: &mut i32, stack: &mut UndoStack<i32>| {
            *v += 10;
            stack.undo(v); // re-entrant run is a no-op
            assert!(stack.is_undoing());
        }),
    );
    let mut v = 0;
    stack.undo(&mut v);
    assert_eq!(v, 10);
    assert!(!stack.is_undoing());
    assert_eq!(stack.undo_action_key(), Some("x"));
}

#[test]
fn undo_titles_are_localized() {
    let _g = SHARED_LANGUAGE.lock().unwrap_or_else(|e| e.into_inner());
    let saved = LanguageState::shared().language();
    LanguageState::shared().set_language(UiLanguage::En);
    assert_eq!(undo_title(None), "Undo");
    assert_eq!(redo_title(None), "Redo");
    let t = undo_title(Some("Satırı Sil"));
    assert!(t.starts_with("Undo ") && t != "Undo Satırı Sil", "{t}");
    LanguageState::shared().set_language(UiLanguage::Tr);
    assert_eq!(undo_title(Some("Satırı Sil")), "Satırı Sil İşlemini Geri Al");
    assert_eq!(redo_title(Some("Satırı Sil")), "Satırı Sil İşlemini Yinele");
    assert_eq!(mac_undo_menu_title(Some("Satırı Sil")), "Geri Al: Satırı Sil");
    assert_eq!(mac_redo_menu_title(None), "Yinele");
    LanguageState::shared().set_language(saved);
}
