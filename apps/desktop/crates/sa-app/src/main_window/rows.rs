//! Keyed row models (win-ui-main §9.3, §9.5): the VM replaces its lists as a whole; the views update a
//! `VecModel` in place (remove gone rows, move/insert by key, update changed fields) so a focused text field keeps
//! its caret and the list keeps its scroll position. Never a full reset.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use slint::{Model, VecModel};

/// What [`sync_keyed`] did (for tests and diagnostics).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SyncStats {
    pub removed: usize,
    pub inserted: usize,
    pub moved: usize,
    pub updated: usize,
}

/// Makes `model` equal `rows` (same order), keyed by `key`. Rows whose key is unchanged and whose data is equal are
/// not touched.
pub fn sync_keyed<T, K, F>(model: &VecModel<T>, rows: Vec<T>, key: F) -> SyncStats
where
    T: Clone + PartialEq + 'static,
    K: Eq + Hash + Clone,
    F: Fn(&T) -> K,
{
    let mut stats = SyncStats::default();
    let wanted: HashSet<K> = rows.iter().map(&key).collect();
    // 1. Remove rows whose key is gone (from the back, so indices stay valid).
    let mut i = model.row_count();
    while i > 0 {
        i -= 1;
        let k = key(&model.row_data(i).expect("row"));
        if !wanted.contains(&k) {
            model.remove(i);
            stats.removed += 1;
        }
    }
    // 2. Walk the wanted order: keep / move / insert, then update the data.
    for (target, row) in rows.into_iter().enumerate() {
        let k = key(&row);
        let current = (target..model.row_count()).find(|&j| model.row_data(j).is_some_and(|r| key(&r) == k));
        match current {
            Some(j) if j == target => {
                if model.row_data(j).as_ref() != Some(&row) {
                    model.set_row_data(j, row);
                    stats.updated += 1;
                }
            }
            Some(j) => {
                model.remove(j);
                model.insert(target, row);
                stats.moved += 1;
            }
            None => {
                model.insert(target, row);
                stats.inserted += 1;
            }
        }
    }
    stats
}

/// Rebuilds the item list only when the titles/enabled flags differ (`fill` rule, win-ui-main §9.5). Returns
/// whether the list was rebuilt.
pub fn fill_if_changed<T: Clone + PartialEq + 'static>(model: &VecModel<T>, items: Vec<T>, force: bool) -> bool {
    let same = !force && model.row_count() == items.len() && items.iter().enumerate().all(|(i, it)| model.row_data(i).as_ref() == Some(it));
    if same {
        return false;
    }
    model.set_vec(items);
    true
}

/// Index of `key` in a list (`-1` = no match, an empty combo).
pub fn index_of<T: PartialEq>(items: &[T], key: &T) -> i32 {
    items.iter().position(|x| x == key).map(|i| i as i32).unwrap_or(-1)
}

/// The row order of a keyed list as keys (tests).
pub fn keys_of<T: Clone + 'static, K>(model: &VecModel<T>, key: impl Fn(&T) -> K) -> Vec<K> {
    (0..model.row_count()).filter_map(|i| model.row_data(i)).map(|r| key(&r)).collect()
}

/// Selection change by the user (`row-pressed`): plain click selects one, toggle (Ctrl/⌘) flips one, range (Shift)
/// extends from the anchor. Returns the new set and the line that becomes primary (`last added`, or `None`).
pub fn press_selection<K: Eq + Hash + Clone>(
    order: &[K],
    selected: &HashSet<K>,
    anchor: Option<&K>,
    index: usize,
    toggle: bool,
    range: bool,
) -> (HashSet<K>, Option<K>) {
    let Some(k) = order.get(index).cloned() else { return (selected.clone(), None) };
    if range {
        let a = anchor.and_then(|a| order.iter().position(|x| x == a)).unwrap_or(index);
        let (lo, hi) = if a <= index { (a, index) } else { (index, a) };
        let mut set: HashSet<K> = if toggle { selected.clone() } else { HashSet::new() };
        set.extend(order[lo..=hi].iter().cloned());
        return (set, Some(k));
    }
    if toggle {
        let mut set = selected.clone();
        if set.contains(&k) {
            set.remove(&k);
            let primary = order.iter().rev().find(|x| set.contains(*x)).cloned();
            return (set, primary);
        }
        set.insert(k.clone());
        return (set, Some(k));
    }
    (HashSet::from([k.clone()]), Some(k))
}

/// Arrow keys in the list: moves the primary line (Shift extends from the anchor).
pub fn arrow_selection<K: Eq + Hash + Clone>(order: &[K], primary: Option<&K>, anchor: Option<&K>, down: bool, extend: bool) -> Option<(HashSet<K>, K)> {
    if order.is_empty() {
        return None;
    }
    let cur = primary.and_then(|p| order.iter().position(|x| x == p));
    let next = match cur {
        None => {
            if down { 0 } else { order.len() - 1 }
        }
        Some(i) if down => (i + 1).min(order.len() - 1),
        Some(i) => i.saturating_sub(1),
    };
    let k = order[next].clone();
    if extend {
        let a = anchor.and_then(|a| order.iter().position(|x| x == a)).or(cur).unwrap_or(next);
        let (lo, hi) = if a <= next { (a, next) } else { (next, a) };
        return Some((order[lo..=hi].iter().cloned().collect(), k));
    }
    Some((HashSet::from([k.clone()]), k))
}

/// Groups keys by position for quick lookups.
pub fn positions<K: Eq + Hash + Clone>(order: &[K]) -> HashMap<K, usize> {
    order.iter().enumerate().map(|(i, k)| (k.clone(), i)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    struct Row {
        id: u32,
        text: &'static str,
    }

    fn r(id: u32, text: &'static str) -> Row {
        Row { id, text }
    }

    #[test]
    fn keyed_sync_keeps_unchanged_rows_and_moves_inserts_removes() {
        let model = VecModel::from(vec![r(1, "a"), r(2, "b"), r(3, "c")]);
        let s = sync_keyed(&model, vec![r(1, "a"), r(2, "b"), r(3, "c")], |x| x.id);
        assert_eq!(s, SyncStats::default());
        let s = sync_keyed(&model, vec![r(3, "c"), r(1, "A"), r(4, "d")], |x| x.id);
        assert_eq!(keys_of(&model, |x| x.id), vec![3, 1, 4]);
        assert_eq!(model.row_data(1).unwrap().text, "A");
        assert_eq!(s.removed, 1);
        assert_eq!(s.inserted, 1);
        assert!(s.moved >= 1);
        let s = sync_keyed(&model, vec![], |x| x.id);
        assert_eq!(s.removed, 3);
        assert_eq!(model.row_count(), 0);
    }

    #[test]
    fn editing_one_line_updates_only_that_row() {
        let model = VecModel::from((0..50).map(|i| Row { id: i, text: "x" }).collect::<Vec<_>>());
        let mut rows: Vec<Row> = (0..50).map(|i| Row { id: i, text: "x" }).collect();
        rows[17].text = "edited";
        let s = sync_keyed(&model, rows, |x| x.id);
        assert_eq!(s, SyncStats { updated: 1, ..Default::default() });
    }

    #[test]
    fn fill_rebuilds_only_when_items_differ() {
        let model = VecModel::from(vec!["a".to_owned(), "b".to_owned()]);
        assert!(!fill_if_changed(&model, vec!["a".into(), "b".into()], false));
        assert!(fill_if_changed(&model, vec!["a".into(), "b".into()], true));
        assert!(fill_if_changed(&model, vec!["a".into(), "c".into()], false));
        assert_eq!(model.row_data(1).unwrap(), "c");
        assert_eq!(index_of(&["x", "y"], &"y"), 1);
        assert_eq!(index_of(&["x", "y"], &"z"), -1);
    }

    #[test]
    fn press_selection_handles_click_toggle_and_range() {
        let order = vec![10, 20, 30, 40];
        let none = HashSet::new();
        let (s, p) = press_selection(&order, &none, None, 1, false, false);
        assert_eq!((s, p), (HashSet::from([20]), Some(20)));
        let (s, p) = press_selection(&order, &HashSet::from([20]), Some(&20), 3, true, false);
        assert_eq!((s, p), (HashSet::from([20, 40]), Some(40)));
        let (s, p) = press_selection(&order, &HashSet::from([20, 40]), Some(&40), 3, true, false);
        assert_eq!((s, p), (HashSet::from([20]), Some(20)));
        let (s, p) = press_selection(&order, &HashSet::from([20]), Some(&20), 3, false, true);
        assert_eq!((s, p), (HashSet::from([20, 30, 40]), Some(40)));
        let (s, _) = press_selection(&order, &HashSet::from([10]), Some(&40), 2, false, true);
        assert_eq!(s, HashSet::from([30, 40]));
    }

    #[test]
    fn arrows_move_and_extend() {
        let order = vec![1, 2, 3];
        assert_eq!(arrow_selection(&order, None, None, true, false), Some((HashSet::from([1]), 1)));
        assert_eq!(arrow_selection(&order, Some(&3), None, true, false), Some((HashSet::from([3]), 3)));
        assert_eq!(arrow_selection(&order, Some(&2), None, false, false), Some((HashSet::from([1]), 1)));
        assert_eq!(arrow_selection(&order, Some(&1), Some(&1), true, true), Some((HashSet::from([1, 2]), 2)));
        assert_eq!(arrow_selection::<i32>(&[], None, None, true, false), None);
    }
}
