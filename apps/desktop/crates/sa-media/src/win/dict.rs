//! A map that enumerates in the order of a .NET `Dictionary<string, T>` with `StringComparer.OrdinalIgnoreCase`.
//!
//! The C# `MediaLibraryIndex.Search` sorts `_filesByPath.Values` with stable LINQ sorts, so files that tie on every
//! sort key (the same name in two folders with the same modification time, e.g. a copied video) keep the dictionary's
//! enumeration order. That order is not insertion order: `Remove` puts the freed entry slot on a LIFO free list and
//! the next `Add` reuses it (.NET Core 3.0+ `Dictionary<TKey, TValue>`), while assigning to an existing key keeps its
//! slot and its original key spelling. This type reproduces exactly that, so tie order matches the C# app.

use std::collections::HashMap;

use crate::text::CiKey;

pub(crate) struct DotNetDict<V> {
    slots: Vec<Option<(CiKey, V)>>,
    index: HashMap<CiKey, usize>,
    /// Freed slots, most recently freed last (the next insert takes it).
    free: Vec<usize>,
}

impl<V> Default for DotNetDict<V> {
    fn default() -> Self {
        Self { slots: Vec::new(), index: HashMap::new(), free: Vec::new() }
    }
}

impl<V> DotNetDict<V> {
    pub(crate) fn len(&self) -> usize {
        self.index.len()
    }

    pub(crate) fn get(&self, key: &CiKey) -> Option<&V> {
        self.index.get(key).and_then(|&i| self.slots[i].as_ref()).map(|(_, v)| v)
    }

    pub(crate) fn contains_key(&self, key: &CiKey) -> bool {
        self.index.contains_key(key)
    }

    /// The indexer setter: an existing key keeps its slot and spelling and only gets the new value; a new key takes
    /// the most recently freed slot, else a new one at the end.
    pub(crate) fn insert(&mut self, key: CiKey, value: V) {
        if let Some(&i) = self.index.get(&key) {
            if let Some((_, slot)) = self.slots[i].as_mut() {
                *slot = value;
            }
            return;
        }
        let i = match self.free.pop() {
            Some(i) => i,
            None => {
                self.slots.push(None);
                self.slots.len() - 1
            }
        };
        self.index.insert(key.clone(), i);
        self.slots[i] = Some((key, value));
    }

    pub(crate) fn remove(&mut self, key: &CiKey) -> Option<V> {
        let i = self.index.remove(key)?;
        let (_, v) = self.slots[i].take()?;
        self.free.push(i);
        Some(v)
    }

    /// Keys in enumeration order (original spelling).
    pub(crate) fn keys(&self) -> impl Iterator<Item = &CiKey> {
        self.slots.iter().filter_map(|s| s.as_ref().map(|(k, _)| k))
    }

    /// Values in enumeration order.
    pub(crate) fn values(&self) -> impl Iterator<Item = &V> {
        self.slots.iter().filter_map(|s| s.as_ref().map(|(_, v)| v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(d: &DotNetDict<i32>) -> Vec<i32> {
        d.values().copied().collect()
    }

    #[test]
    fn freed_slots_are_reused_last_in_first_out() {
        let mut d = DotNetDict::default();
        for (i, k) in ["a", "b", "c", "d"].iter().enumerate() {
            d.insert(CiKey::new(k), i as i32);
        }
        assert_eq!(d.remove(&CiKey::new("B")), Some(1));
        assert_eq!(d.remove(&CiKey::new("d")), Some(3));
        d.insert(CiKey::new("e"), 4); // takes d's slot (freed last)
        d.insert(CiKey::new("f"), 5); // takes b's slot
        d.insert(CiKey::new("g"), 6); // appended
        assert_eq!(order(&d), [0, 5, 2, 4, 6]);
        d.insert(CiKey::new("A"), 9);
        assert_eq!(order(&d), [9, 5, 2, 4, 6]);
        assert_eq!(d.keys().next().unwrap().as_str(), "a", "an existing key keeps its spelling");
        assert_eq!(d.len(), 5);
        assert!(d.contains_key(&CiKey::new("G")));
        assert_eq!(d.get(&CiKey::new("F")), Some(&5));
    }
}
