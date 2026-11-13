//! Independent scalar ownership/probing model through the public flat API.
#![forbid(unsafe_code)]
use std::{
    cell::{Cell, RefCell},
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    iter::FusedIterator,
    panic::{AssertUnwindSafe, catch_unwind},
    rc::Rc,
};
use vesper_alloc::{FlatConfig, FlatError, HandleError, Region, RegionConfig, RegionFlatTable};

#[derive(Default)]
struct Calls {
    hashes: Cell<usize>,
    equalities: Cell<usize>,
    panic_hash: Cell<bool>,
    panic_eq: Cell<bool>,
    panic_hash_id: Cell<Option<u32>>,
}
#[derive(Default)]
struct Ledger {
    created: RefCell<Vec<(u8, u64)>>,
    dropped: RefCell<Vec<(u8, u64)>>,
    next: Cell<u64>,
    panic_next_value: Cell<bool>,
}
impl Ledger {
    fn serial(&self, kind: u8) -> u64 {
        let serial = self.next.get();
        self.next.set(serial + 1);
        self.created.borrow_mut().push((kind, serial));
        serial
    }
    fn assert_exactly_once(&self) {
        let mut created = self.created.borrow().clone();
        let mut dropped = self.dropped.borrow().clone();
        created.sort_unstable();
        dropped.sort_unstable();
        assert_eq!(created, dropped);
    }
}
// Equality includes the hash input, so *all* representable equal keys hash
// equally. The serial is observable ownership identity, never key identity.
// Query keys have no ledger serial; stored/incoming keys do.
struct Key {
    id: u32,
    seed: u64,
    serial: Option<u64>,
    calls: Rc<Calls>,
    ledger: Rc<Ledger>,
}
impl Hash for Key {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.calls.hashes.set(self.calls.hashes.get() + 1);
        assert!(
            !self.calls.panic_hash.get() && self.calls.panic_hash_id.get() != Some(self.id),
            "injected Hash panic"
        );
        self.seed.hash(state);
    }
}
impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        self.calls.equalities.set(self.calls.equalities.get() + 1);
        assert!(
            !self.calls.panic_eq.get() && !other.calls.panic_eq.get(),
            "injected Eq panic"
        );
        self.id == other.id && self.seed == other.seed
    }
}
impl Eq for Key {}
impl Drop for Key {
    fn drop(&mut self) {
        if let Some(serial) = self.serial {
            self.ledger.dropped.borrow_mut().push((b'k', serial));
        }
    }
}
struct Payload {
    serial: u64,
    data: i64,
    ledger: Rc<Ledger>,
}
impl Drop for Payload {
    fn drop(&mut self) {
        self.ledger.dropped.borrow_mut().push((b'v', self.serial));
        assert!(
            !self.ledger.panic_next_value.replace(false),
            "single injected payload Drop panic"
        );
    }
}
fn query(id: u32, seed: u64, calls: &Rc<Calls>, ledger: &Rc<Ledger>) -> Key {
    Key {
        id,
        seed,
        serial: None,
        calls: calls.clone(),
        ledger: ledger.clone(),
    }
}
fn owned(id: u32, seed: u64, data: i64, calls: &Rc<Calls>, ledger: &Rc<Ledger>) -> (Key, Payload) {
    let key = Key {
        id,
        seed,
        serial: Some(ledger.serial(b'k')),
        calls: calls.clone(),
        ledger: ledger.clone(),
    };
    let value = Payload {
        serial: ledger.serial(b'v'),
        data,
        ledger: ledger.clone(),
    };
    (key, value)
}
fn hash(seed: u64) -> u64 {
    let mut hasher = DefaultHasher::new();
    seed.hash(&mut hasher);
    hasher.finish()
}
fn region() -> Region {
    Region::new(RegionConfig::bounded(8 * 1024 * 1024)).unwrap()
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Model {
    id: u32,
    seed: u64,
    key_serial: u64,
    value_serial: u64,
    data: i64,
}
fn row(key: &Key, value: &Payload) -> Model {
    Model {
        id: key.id,
        seed: key.seed,
        key_serial: key.serial.unwrap(),
        value_serial: value.serial,
        data: value.data,
    }
}
fn actual(r: &Region, table: &RegionFlatTable<Key, Payload>) -> Vec<Model> {
    let mut rows = r
        .flat_iter(table)
        .unwrap()
        .map(|(k, v)| row(k, v))
        .collect::<Vec<_>>();
    rows.sort_unstable();
    rows
}
fn assert_model(r: &Region, table: &RegionFlatTable<Key, Payload>, model: &[Model]) {
    let mut expected = model.to_vec();
    expected.sort_unstable();
    assert_eq!(r.flat_len(table).unwrap(), expected.len());
    assert_eq!(r.flat_is_empty(table).unwrap(), expected.is_empty());
    assert_eq!(actual(r, table), expected);
    assert_eq!(
        r.flat_capacity(table).unwrap(),
        14 * r.flat_group_count(table).unwrap()
    );
}
fn controls(r: &Region, table: &RegionFlatTable<Key, Payload>) -> Vec<[u8; 16]> {
    (0..r.flat_group_count(table).unwrap())
        .map(|group| *r.flat_control_group(table, group).unwrap().unwrap())
        .collect()
}
fn model_insert(model: &mut Vec<Model>, incoming: Model) -> Option<Model> {
    if let Some(slot) = model
        .iter_mut()
        .find(|row| row.id == incoming.id && row.seed == incoming.seed)
    {
        Some(std::mem::replace(slot, incoming))
    } else {
        model.push(incoming);
        None
    }
}

#[test]
fn scalar_oracle_matches_affine_operations_replacement_and_repeated_growth() {
    let calls = Rc::new(Calls::default());
    let ledger = Rc::new(Ledger::default());
    let mut r = region();
    let mut table = r
        .flat_new::<Key, Payload>(FlatConfig { max_groups: 64 })
        .unwrap();
    let mut model = vec![];
    for id in 0..300_u32 {
        let (key, value) = owned(id, u64::from(id), i64::from(id) * 3, &calls, &ledger);
        let incoming = row(&key, &value);
        assert!(r.flat_insert(&mut table, key, value).unwrap().is_none());
        model.push(incoming);
    }
    assert!(r.flat_group_count(&table).unwrap() >= 32);
    assert_model(&r, &table, &model);
    let mut random = 0x61d8_75ac_c9e2_034f_u64;
    for step in 0..2400_u32 {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        let id = (random % 360) as u32;
        let seed = u64::from(id);
        match (random >> 32) % 5 {
            0 | 1 => {
                let (key, value) = owned(id, seed, i64::from(step) - 17, &calls, &ledger);
                let incoming = row(&key, &value);
                let expected = model_insert(&mut model, incoming);
                let old = r.flat_insert(&mut table, key, value).unwrap();
                assert_eq!(old.as_ref().map(|(k, v)| row(k, v)), expected);
                drop(old);
            }
            2 => {
                let q = query(id, seed, &calls, &ledger);
                let expected = model
                    .iter()
                    .position(|row| row.id == id && row.seed == seed)
                    .map(|at| model.swap_remove(at));
                let old = r.flat_remove(&table, &q).unwrap();
                assert_eq!(old.as_ref().map(|(k, v)| row(k, v)), expected);
                drop(old);
            }
            3 => {
                let q = query(id, seed, &calls, &ledger);
                let expected = model
                    .iter_mut()
                    .find(|row| row.id == id && row.seed == seed);
                let found = r.flat_get_mut(&table, &q).unwrap();
                match (expected, found) {
                    (Some(a), Some(b)) => {
                        a.data ^= 23;
                        b.data ^= 23;
                    }
                    (None, None) => (),
                    _ => panic!("mutable lookup disagrees"),
                }
            }
            _ => {
                let q = query(id, seed, &calls, &ledger);
                assert_eq!(
                    r.flat_get(&table, &q).unwrap().map(|v| (v.serial, v.data)),
                    model
                        .iter()
                        .find(|row| row.id == id && row.seed == seed)
                        .map(|v| (v.value_serial, v.data))
                );
                r.flat_reserve(&mut table, (random as usize) % 64).unwrap();
            }
        }
        if step % 37 == 0 {
            assert_model(&r, &table, &model);
        }
    }
    assert_model(&r, &table, &model);
    assert!(calls.hashes.get() > 2000);
    r.flat_release(table).unwrap();
    ledger.assert_exactly_once();
}

#[test]
fn equal_but_observably_distinct_keys_replace_without_dropping_or_growing() {
    let calls = Rc::new(Calls::default());
    let ledger = Rc::new(Ledger::default());
    let mut r = region();
    let mut table = r
        .flat_new::<Key, Payload>(FlatConfig { max_groups: 1 })
        .unwrap();
    for id in 0..14 {
        let (k, v) = owned(id, 0, i64::from(id), &calls, &ledger);
        r.flat_insert(&mut table, k, v).unwrap();
    }
    let q = query(3, 0, &calls, &ledger);
    let previous = r
        .flat_iter(&table)
        .unwrap()
        .find(|(k, _)| k.id == 3)
        .map(|(k, v)| row(k, v))
        .unwrap();
    let (key, value) = owned(3, 0, 999, &calls, &ledger);
    let incoming = row(&key, &value);
    let before = r.stats();
    let dropped = ledger.dropped.borrow().len();
    let old = r.flat_insert(&mut table, key, value).unwrap().unwrap();
    assert_eq!(row(&old.0, &old.1), previous);
    assert_eq!(ledger.dropped.borrow().len(), dropped);
    assert_eq!(r.stats(), before);
    assert_eq!(r.flat_len(&table).unwrap(), 14);
    assert_eq!(
        r.flat_get(&table, &q).unwrap().unwrap().serial,
        incoming.value_serial
    );
    assert_eq!(
        r.flat_iter(&table)
            .unwrap()
            .find(|(k, _)| k.id == 3)
            .map(|(k, v)| row(k, v)),
        Some(incoming)
    );
    drop(old);
    assert_eq!(ledger.dropped.borrow().len(), dropped + 2);
    let (key, value) = owned(99, 0, 11, &calls, &ledger);
    let rejected = row(&key, &value);
    let before_rows = actual(&r, &table);
    let before = r.stats();
    let error = match r.flat_insert(&mut table, key, value) {
        Err(error) => error,
        Ok(_) => panic!("new key exceeded configured live capacity"),
    };
    assert!(matches!(
        error.error,
        FlatError::CapacityLimit {
            requested: 15,
            max: 14
        }
    ));
    assert_eq!(row(&error.value.0, &error.value.1), rejected);
    assert_eq!(r.stats(), before);
    assert_eq!(actual(&r, &table), before_rows);
    drop(error);
    r.flat_release(table).unwrap();
    ledger.assert_exactly_once();
}

#[test]
fn same_h2_different_full_hash_avoids_eq_and_controls_follow_group_lane_iteration() {
    let calls = Rc::new(Calls::default());
    let ledger = Rc::new(Ledger::default());
    let mut r = region();
    let mut table = r
        .flat_with_capacity::<Key, Payload>(56, FlatConfig { max_groups: 4 })
        .unwrap();
    let first = (0..100000_u64).find(|&seed| hash(seed) & 3 == 3).unwrap();
    let tag = hash(first) >> 57;
    let mut seeds = vec![];
    for seed in first..1_000_000 {
        let full = hash(seed);
        if full & 3 == 3 && full >> 57 == tag && !seeds.iter().any(|&s| hash(s) == full) {
            seeds.push(seed);
            if seeds.len() == 25 {
                break;
            }
        }
    }
    assert_eq!(seeds.len(), 25);
    for (id, &seed) in seeds[..24].iter().enumerate() {
        let (k, v) = owned(id as u32, seed, id as i64, &calls, &ledger);
        r.flat_insert(&mut table, k, v).unwrap();
    }
    assert_eq!(
        calls.equalities.get(),
        0,
        "distinct full hashes must skip Eq even with equal h2"
    );
    let q = query(999, seeds[24], &calls, &ledger);
    assert!(r.flat_get(&table, &q).unwrap().is_none());
    assert_eq!(calls.equalities.get(), 0);
    assert_eq!(
        r.flat_control_group(&table, 3)
            .unwrap()
            .unwrap()
            .iter()
            .filter(|&&x| x < 128)
            .count(),
        16,
        "global threshold permits a full starting group"
    );
    assert_eq!(
        r.flat_control_group(&table, 0)
            .unwrap()
            .unwrap()
            .iter()
            .filter(|&&x| x < 128)
            .count(),
        8,
        "probe must wrap from final to first group"
    );
    let tags = controls(&r, &table)
        .into_iter()
        .flatten()
        .filter(|&tag| tag < 128)
        .collect::<Vec<_>>();
    let iteration = r
        .flat_iter(&table)
        .unwrap()
        .map(|(key, _)| (hash(key.seed) >> 57) as u8)
        .collect::<Vec<_>>();
    assert_eq!(tags, iteration);
    r.flat_release(table).unwrap();
    ledger.assert_exactly_once();
}

#[test]
fn full_collisions_survive_early_tombstones_and_replacement_searches_past_deleted_slots() {
    let calls = Rc::new(Calls::default());
    let ledger = Rc::new(Ledger::default());
    let mut r = region();
    let mut table = r
        .flat_with_capacity::<Key, Payload>(56, FlatConfig { max_groups: 4 })
        .unwrap();
    let mut model = vec![];
    for id in 0..50 {
        let (k, v) = owned(id, 0, i64::from(id), &calls, &ledger);
        model.push(row(&k, &v));
        assert!(r.flat_insert(&mut table, k, v).unwrap().is_none());
    }
    assert!(calls.equalities.get() > 500);
    for id in (0..32).step_by(2) {
        let q = query(id, 0, &calls, &ledger);
        let old = r.flat_remove(&table, &q).unwrap().unwrap();
        let at = model.iter().position(|v| v.id == id).unwrap();
        assert_eq!(row(&old.0, &old.1), model.swap_remove(at));
        drop(old);
    }
    for expected in &model {
        let q = query(expected.id, 0, &calls, &ledger);
        assert_eq!(
            r.flat_get(&table, &q).unwrap().unwrap().serial,
            expected.value_serial
        );
    }
    let (k, v) = owned(49, 0, 999, &calls, &ledger);
    let incoming = row(&k, &v);
    let previous = model_insert(&mut model, incoming);
    let old = r.flat_insert(&mut table, k, v).unwrap().unwrap();
    assert_eq!(Some(row(&old.0, &old.1)), previous);
    drop(old);
    assert_model(&r, &table, &model);
    for id in 100..112 {
        let (k, v) = owned(id, 0, i64::from(id), &calls, &ledger);
        model.push(row(&k, &v));
        assert!(r.flat_insert(&mut table, k, v).unwrap().is_none());
    }
    assert_model(&r, &table, &model);
    r.flat_release(table).unwrap();
    ledger.assert_exactly_once();
}

#[test]
fn all_deleted_controls_terminate_missing_search_and_reuse_without_empty_sentinel() {
    let calls = Rc::new(Calls::default());
    let ledger = Rc::new(Ledger::default());
    let mut r = region();
    let mut table = r
        .flat_with_capacity::<Key, Payload>(28, FlatConfig { max_groups: 2 })
        .unwrap();
    let empty = r.flat_control_group(&table, 0).unwrap().unwrap()[0];
    let first_seed = 0;
    let start = hash(first_seed) & 1;
    let other_seed = (1..10000).find(|&s| hash(s) & 1 != start).unwrap();
    for id in 0..28 {
        let (k, v) = owned(id, first_seed, i64::from(id), &calls, &ledger);
        r.flat_insert(&mut table, k, v).unwrap();
    }
    for id in 0..16 {
        let q = query(id, first_seed, &calls, &ledger);
        drop(r.flat_remove(&table, &q).unwrap().unwrap());
    }
    for id in 28..32 {
        let (k, v) = owned(id, other_seed, i64::from(id), &calls, &ledger);
        r.flat_insert(&mut table, k, v).unwrap();
    }
    for id in 16..32 {
        let seed = if id < 28 { first_seed } else { other_seed };
        let q = query(id, seed, &calls, &ledger);
        drop(r.flat_remove(&table, &q).unwrap().unwrap());
    }
    assert_eq!(r.flat_len(&table).unwrap(), 0);
    let saturated = controls(&r, &table);
    assert!(
        saturated
            .iter()
            .flatten()
            .all(|&tag| tag >= 128 && tag != empty),
        "every lane must now be DELETED, not EMPTY"
    );
    let q = query(999, first_seed, &calls, &ledger);
    assert!(r.flat_get(&table, &q).unwrap().is_none());
    assert!(r.flat_remove(&table, &q).unwrap().is_none());
    let (k, v) = owned(999, first_seed, 123, &calls, &ledger);
    r.flat_insert(&mut table, k, v).unwrap();
    assert_eq!(r.flat_get(&table, &q).unwrap().unwrap().data, 123);
    assert_eq!(r.flat_group_count(&table).unwrap(), 2);
    r.flat_release(table).unwrap();
    ledger.assert_exactly_once();
}

#[test]
fn reserve_rehash_and_iteration_do_not_invoke_user_hash_eq_or_drop_payloads() {
    let calls = Rc::new(Calls::default());
    let ledger = Rc::new(Ledger::default());
    let mut r = region();
    let mut table = r
        .flat_new::<Key, Payload>(FlatConfig { max_groups: 64 })
        .unwrap();
    for id in 0..14 {
        let (k, v) = owned(id, u64::from(id), i64::from(id), &calls, &ledger);
        r.flat_insert(&mut table, k, v).unwrap();
    }
    let expected = actual(&r, &table);
    let counts = (calls.hashes.get(), calls.equalities.get());
    calls.panic_hash.set(true);
    calls.panic_eq.set(true);
    for additional in [20, 70, 200] {
        r.flat_reserve(&mut table, additional).unwrap();
        assert_eq!(actual(&r, &table), expected);
    }
    assert_eq!((calls.hashes.get(), calls.equalities.get()), counts);
    assert!(ledger.dropped.borrow().is_empty());
    calls.panic_hash.set(false);
    calls.panic_eq.set(false);
    for expected in expected {
        let q = query(expected.id, expected.seed, &calls, &ledger);
        assert_eq!(r.flat_get(&table, &q).unwrap().unwrap().data, expected.data);
    }
    r.flat_release(table).unwrap();
    ledger.assert_exactly_once();
}

#[test]
fn growing_insert_hashes_only_the_incoming_key_and_compares_only_during_search() {
    let calls = Rc::new(Calls::default());
    let ledger = Rc::new(Ledger::default());
    let mut r = region();
    let mut table = r
        .flat_new::<Key, Payload>(FlatConfig { max_groups: 4 })
        .unwrap();
    for id in 0..14 {
        let (key, value) = owned(id, 0, i64::from(id), &calls, &ledger);
        r.flat_insert(&mut table, key, value).unwrap();
    }
    let before = actual(&r, &table);
    let hashes = calls.hashes.get();
    let equalities = calls.equalities.get();
    let (key, value) = owned(100, 0, 100, &calls, &ledger);
    let incoming = row(&key, &value);
    assert!(r.flat_insert(&mut table, key, value).unwrap().is_none());
    assert_eq!(r.flat_group_count(&table).unwrap(), 2);
    assert_eq!(calls.hashes.get() - hashes, 1);
    // Every prior full-hash collision must be compared before the search can
    // conclude a new key is absent. Any rehash comparison would add to these14.
    assert_eq!(calls.equalities.get() - equalities, 14);
    assert!(ledger.dropped.borrow().is_empty());
    let mut expected = before;
    expected.push(incoming);
    assert_model(&r, &table, &expected);
    r.flat_release(table).unwrap();
    ledger.assert_exactly_once();
}

#[test]
fn hash_and_eq_panics_precede_all_entry_control_owner_and_region_mutation() {
    let calls = Rc::new(Calls::default());
    let ledger = Rc::new(Ledger::default());
    let mut r = region();
    let mut table = r
        .flat_new::<Key, Payload>(FlatConfig { max_groups: 8 })
        .unwrap();
    for id in 0..8 {
        let (k, v) = owned(id, 0, i64::from(id), &calls, &ledger);
        r.flat_insert(&mut table, k, v).unwrap();
    }
    let expected = actual(&r, &table);
    let before_controls = controls(&r, &table);
    let stats = r.stats();
    let owner = table.region();
    for hash_panics in [true, false] {
        for operation in 0..5 {
            calls.panic_hash.set(hash_panics);
            calls.panic_eq.set(!hash_panics);
            let q = query(3, 0, &calls, &ledger);
            let other = query(4, 0, &calls, &ledger);
            let result = catch_unwind(AssertUnwindSafe(|| match operation {
                0 => {
                    let _ = r.flat_get(&table, &q);
                }
                1 => {
                    let _ = r.flat_get_mut(&table, &q);
                }
                2 => {
                    let _ = r.flat_remove(&table, &q);
                }
                3 => {
                    let (k, v) = owned(99, 0, 999, &calls, &ledger);
                    let _ = r.flat_insert(&mut table, k, v);
                }
                _ => {
                    let _ = r.flat_get_disjoint_mut(&table, [&q, &other]);
                }
            }));
            assert!(result.is_err(), "operation{operation} hash={hash_panics}");
            assert_eq!(table.region(), owner);
            assert_eq!(r.stats(), stats);
            assert_eq!(controls(&r, &table), before_controls);
            assert_eq!(actual(&r, &table), expected);
        }
    }
    calls.panic_hash.set(false);
    calls.panic_eq.set(false);
    r.flat_release(table).unwrap();
    ledger.assert_exactly_once();
}

#[test]
fn invalid_owner_validation_precedes_hash_and_eq_callbacks() {
    let calls = Rc::new(Calls::default());
    let ledger = Rc::new(Ledger::default());
    let mut r = region();
    let mut other = region();
    let mut table = r
        .flat_new::<Key, Payload>(FlatConfig { max_groups: 4 })
        .unwrap();
    let (k, v) = owned(1, 0, 10, &calls, &ledger);
    r.flat_insert(&mut table, k, v).unwrap();
    let q = query(1, 0, &calls, &ledger);
    calls.panic_hash.set(true);
    calls.panic_eq.set(true);
    let counts = (calls.hashes.get(), calls.equalities.get());
    assert!(matches!(
        other.flat_get(&table, &q),
        Err(HandleError::CrossRegion { .. })
    ));
    assert!(matches!(
        other.flat_get_mut(&table, &q),
        Err(HandleError::CrossRegion { .. })
    ));
    assert!(matches!(
        other.flat_remove(&table, &q),
        Err(HandleError::CrossRegion { .. })
    ));
    assert!(matches!(
        other.flat_get_disjoint_mut(&table, [&q, &q]),
        Err(HandleError::CrossRegion { .. })
    ));
    assert!(other.flat_reserve(&mut table, 0).is_err());
    assert_eq!((calls.hashes.get(), calls.equalities.get()), counts);
    r.reset().unwrap();
    assert!(matches!(r.flat_get(&table, &q), Err(HandleError::Stale)));
    assert!(matches!(r.flat_remove(&table, &q), Err(HandleError::Stale)));
    assert!(matches!(
        r.flat_get_disjoint_mut(&table, [&q, &q]),
        Err(HandleError::Stale)
    ));
    assert_eq!((calls.hashes.get(), calls.equalities.get()), counts);
    ledger.assert_exactly_once();
}

#[test]
fn disjoint_projection_uses_occupied_slot_identity_and_resolves_both_queries_first() {
    let calls = Rc::new(Calls::default());
    let ledger = Rc::new(Ledger::default());
    let mut r = region();
    let mut table = r
        .flat_new::<Key, Payload>(FlatConfig { max_groups: 4 })
        .unwrap();
    for id in [1, 2] {
        let (k, v) = owned(id, 0, i64::from(id) * 10, &calls, &ledger);
        r.flat_insert(&mut table, k, v).unwrap();
    }
    let a = query(1, 0, &calls, &ledger);
    let same = query(1, 0, &calls, &ledger);
    let b = query(2, 0, &calls, &ledger);
    let missing = query(9, 0, &calls, &ledger);
    let also_missing = query(9, 0, &calls, &ledger);
    assert!(matches!(
        r.flat_get_disjoint_mut(&table, [&a, &same]),
        Err(HandleError::Aliased)
    ));
    let [second, first] = r.flat_get_disjoint_mut(&table, [&b, &a]).unwrap();
    second.unwrap().data += 7;
    first.unwrap().data += 3;
    let [absent, present] = r.flat_get_disjoint_mut(&table, [&missing, &a]).unwrap();
    assert!(absent.is_none());
    assert_eq!(present.unwrap().data, 13);
    let [present, absent] = r.flat_get_disjoint_mut(&table, [&b, &missing]).unwrap();
    assert_eq!(present.unwrap().data, 27);
    assert!(absent.is_none());
    let [x, y] = r
        .flat_get_disjoint_mut(&table, [&missing, &also_missing])
        .unwrap();
    assert!(x.is_none() && y.is_none());
    let expected = actual(&r, &table);
    let stats = r.stats();
    calls.panic_hash_id.set(Some(2));
    assert!(
        catch_unwind(AssertUnwindSafe(|| {
            let _ = r.flat_get_disjoint_mut(&table, [&a, &b]);
        }))
        .is_err()
    );
    assert_eq!(actual(&r, &table), expected);
    assert_eq!(r.stats(), stats);
    calls.panic_hash_id.set(None);
    r.flat_release(table).unwrap();
    ledger.assert_exactly_once();
}

#[test]
fn a_single_clear_drop_panic_preserves_surviving_probe_chains_and_reuse() {
    let calls = Rc::new(Calls::default());
    let ledger = Rc::new(Ledger::default());
    let mut r = region();
    let mut table = r
        .flat_with_capacity::<Key, Payload>(28, FlatConfig { max_groups: 4 })
        .unwrap();
    let empty_control = r.flat_control_group(&table, 0).unwrap().unwrap()[0];
    let mut model = vec![];
    for id in 0..24 {
        let (k, v) = owned(id, 0, i64::from(id), &calls, &ledger);
        model.push(row(&k, &v));
        r.flat_insert(&mut table, k, v).unwrap();
    }
    let stats = r.stats();
    let capacity = r.flat_capacity(&table).unwrap();
    ledger.panic_next_value.set(true);
    assert!(catch_unwind(AssertUnwindSafe(|| r.flat_clear(&table).unwrap())).is_err());
    let drops = ledger.dropped.borrow().clone();
    let dropped_values = drops
        .iter()
        .filter_map(|&(kind, serial)| (kind == b'v').then_some(serial))
        .collect::<Vec<_>>();
    assert_eq!(dropped_values.len(), 1);
    model.retain(|row| row.value_serial != dropped_values[0]);
    assert_eq!(model.len(), 23);
    assert_model(&r, &table, &model);
    assert_eq!(r.stats(), stats);
    assert_eq!(r.flat_capacity(&table).unwrap(), capacity);
    for expected in &model {
        let q = query(expected.id, 0, &calls, &ledger);
        assert_eq!(
            r.flat_get(&table, &q).unwrap().unwrap().serial,
            expected.value_serial
        );
    }
    let (k, v) = owned(100, 0, 100, &calls, &ledger);
    r.flat_insert(&mut table, k, v).unwrap();
    r.flat_clear(&table).unwrap();
    assert_eq!(r.flat_len(&table).unwrap(), 0);
    assert_eq!(r.flat_capacity(&table).unwrap(), capacity);
    assert!(
        controls(&r, &table)
            .iter()
            .flatten()
            .all(|&tag| tag == empty_control)
    );
    let (k, v) = owned(101, 0, 101, &calls, &ledger);
    r.flat_insert(&mut table, k, v).unwrap();
    r.flat_release(table).unwrap();
    ledger.assert_exactly_once();
}

#[test]
fn zero_sized_values_and_keys_keep_logical_alias_and_drop_semantics() {
    thread_local! {static DROPS:Cell<usize>=const{Cell::new(0)};}
    struct Zero;
    impl Drop for Zero {
        fn drop(&mut self) {
            DROPS.with(|n| n.set(n.get() + 1));
        }
    }
    #[derive(Hash, PartialEq, Eq)]
    struct ZeroKey;
    DROPS.with(|n| n.set(0));
    let mut r = region();
    let mut table = r
        .flat_new::<u32, Zero>(FlatConfig { max_groups: 2 })
        .unwrap();
    r.flat_insert(&mut table, 1, Zero).unwrap();
    r.flat_insert(&mut table, 2, Zero).unwrap();
    let [a, b] = r.flat_get_disjoint_mut(&table, [&1, &2]).unwrap();
    assert!(a.is_some() && b.is_some());
    assert!(matches!(
        r.flat_get_disjoint_mut(&table, [&1, &1]),
        Err(HandleError::Aliased)
    ));
    let [a, b] = r.flat_get_disjoint_mut(&table, [&3, &3]).unwrap();
    assert!(a.is_none() && b.is_none());
    r.flat_release(table).unwrap();
    assert_eq!(DROPS.with(Cell::get), 2);
    let mut table = r
        .flat_new::<ZeroKey, Zero>(FlatConfig { max_groups: 2 })
        .unwrap();
    assert!(r.flat_insert(&mut table, ZeroKey, Zero).unwrap().is_none());
    let old = r.flat_insert(&mut table, ZeroKey, Zero).unwrap().unwrap();
    assert_eq!(r.flat_len(&table).unwrap(), 1);
    drop(old);
    assert_eq!(DROPS.with(Cell::get), 3);
    let removed = r.flat_remove(&table, &ZeroKey).unwrap().unwrap();
    drop(removed);
    assert_eq!(DROPS.with(Cell::get), 4);
    r.flat_release(table).unwrap();
    assert_eq!(DROPS.with(Cell::get), 4);
}

#[test]
fn iterator_exact_size_fusion_and_owner_independence_need_no_callbacks() {
    fn traits<I: ExactSizeIterator + FusedIterator>(_: &I) {}
    let calls = Rc::new(Calls::default());
    let ledger = Rc::new(Ledger::default());
    let mut r = region();
    {
        let mut iterator = {
            let mut table = r
                .flat_with_capacity::<Key, Payload>(28, FlatConfig { max_groups: 2 })
                .unwrap();
            for id in 0..20 {
                let (k, v) = owned(id, 0, i64::from(id), &calls, &ledger);
                r.flat_insert(&mut table, k, v).unwrap();
            }
            for id in [2, 4, 10] {
                let q = query(id, 0, &calls, &ledger);
                drop(r.flat_remove(&table, &q).unwrap());
            }
            calls.panic_hash.set(true);
            calls.panic_eq.set(true);
            r.flat_iter(&table).unwrap()
        }; // The owner borrow is not the lifetime of this region iterator.
        traits(&iterator);
        let mut ids = vec![];
        for remaining in (1..=17).rev() {
            assert_eq!(iterator.len(), remaining);
            assert_eq!(iterator.size_hint(), (remaining, Some(remaining)));
            ids.push(iterator.next().unwrap().0.id);
        }
        assert_eq!(iterator.len(), 0);
        assert_eq!(iterator.size_hint(), (0, Some(0)));
        assert!(iterator.next().is_none());
        assert!(iterator.next().is_none());
        ids.sort_unstable();
        assert_eq!(
            ids,
            (0..20)
                .filter(|id| ![2, 4, 10].contains(id))
                .collect::<Vec<_>>()
        );
    }
    assert_eq!(
        ledger.dropped.borrow().len(),
        6,
        "dropping the table owner does not destroy resident entries"
    );
    drop(r);
    ledger.assert_exactly_once();
}
