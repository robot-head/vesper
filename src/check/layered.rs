//! A span-keyed map that copies cheaply.
//!
//! Literal typing ([`super::literal_typing`]) copies the whole checker for
//! each function it checks again, and the expression types and call
//! substitutions of every function checked before it were most of that copy.
//! [`Layered`] keeps those earlier entries behind an [`Rc`], shared by every
//! copy, and only the current function's entries are copied. It reads and
//! writes like the `BTreeMap` it stands for.
use std::{collections::BTreeMap, rc::Rc};

type Key = (usize, usize);

#[derive(Clone, Debug)]
pub(super) struct Layered<V> {
    /// The entries as of the last [`Self::freeze`], shared by copies.
    below: Rc<BTreeMap<Key, V>>,
    /// Changes since then: `None` removes the entry below.
    above: BTreeMap<Key, Option<V>>,
}

impl<V> Default for Layered<V> {
    fn default() -> Self {
        Self {
            below: Rc::default(),
            above: BTreeMap::new(),
        }
    }
}

impl<V: Clone> Layered<V> {
    pub(super) fn get(&self, k: &Key) -> Option<&V> {
        match self.above.get(k) {
            Some(v) => v.as_ref(),
            None => self.below.get(k),
        }
    }

    pub(super) fn insert(&mut self, k: Key, v: V) {
        self.above.insert(k, Some(v));
    }

    /// Inserts `v` unless `k` already has an entry.
    pub(super) fn insert_if_absent(&mut self, k: Key, v: V) {
        if self.get(&k).is_none() {
            self.insert(k, v);
        }
    }

    /// Removes the entries whose span lies within `start..=end`.
    pub(super) fn remove_within(&mut self, start: usize, end: usize) {
        let range = (start, 0)..=(end, usize::MAX);
        let inside: Vec<Key> = self
            .below
            .range(range.clone())
            .map(|(k, _)| *k)
            .chain(self.above.range(range).map(|(k, _)| *k))
            .filter(|k| k.1 <= end)
            .collect();
        for k in inside {
            self.above.insert(k, None);
        }
    }

    /// Moves the changes below, where later copies share them. Cheap when
    /// no copy is alive, which is the case between functions.
    pub(super) fn freeze(&mut self) {
        if self.above.is_empty() {
            return;
        }
        let below = Rc::make_mut(&mut self.below);
        for (k, v) in std::mem::take(&mut self.above) {
            match v {
                Some(v) => {
                    below.insert(k, v);
                }
                None => {
                    below.remove(&k);
                }
            }
        }
    }

    pub(super) fn into_map(mut self) -> BTreeMap<Key, V> {
        self.freeze();
        Rc::try_unwrap(self.below).unwrap_or_else(|shared| (*shared).clone())
    }
}

#[cfg(test)]
mod tests {
    use super::Layered;
    use std::collections::BTreeMap;

    #[test]
    fn reads_and_writes_like_a_map() {
        let mut l = Layered::default();
        let mut m = BTreeMap::new();
        let ops: [(usize, usize, i32); 6] = [
            (1, 3, 1),
            (2, 2, 2),
            (5, 9, 3),
            (1, 3, 4),
            (4, 4, 5),
            (9, 9, 6),
        ];
        for (i, (a, b, v)) in ops.into_iter().enumerate() {
            l.insert((a, b), v);
            m.insert((a, b), v);
            if i == 2 {
                l.freeze();
            }
        }
        let copy = l.clone();
        l.remove_within(2, 5);
        m.retain(|k, _| !(2 <= k.0 && k.1 <= 5));
        l.insert_if_absent((4, 4), 7);
        m.entry((4, 4)).or_insert(7);
        l.insert_if_absent((1, 3), 8);
        m.entry((1, 3)).or_insert(8);
        for a in 0..10 {
            for b in 0..10 {
                assert_eq!(l.get(&(a, b)), m.get(&(a, b)), "({a}, {b})");
            }
        }
        assert_eq!(copy.get(&(2, 2)), Some(&2));
        assert_eq!(l.into_map(), m);
    }
}
