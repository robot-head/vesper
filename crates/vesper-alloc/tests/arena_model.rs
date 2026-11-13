//! Independent scalar identities/values; opaque production keys are never forged.
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    hash::{BuildHasherDefault, DefaultHasher, Hash, Hasher},
};
use vesper_alloc::{
    AllocError, ArenaError, ArenaId, ArenaKey, HandleError, Region, RegionConfig, RegionError,
    RegionSlotArena, SystemBacking,
};

// Deliberately has no Copy, Clone, Default, Debug, Eq or Hash implementation.
struct Payload {
    id: u64,
    value: i64,
    marker: Box<[u8; 8]>,
}
impl Payload {
    fn new(id: u64, value: i64) -> Self {
        Self {
            id,
            value,
            marker: Box::new(id.to_le_bytes()),
        }
    }
    fn marker_address(&self) -> usize {
        self.marker.as_ptr().addr()
    }
}
type Key = ArenaKey<Payload>;
type Keys = HashSet<Key, BuildHasherDefault<DefaultHasher>>;

fn region() -> Region<SystemBacking> {
    Region::new(RegionConfig::bounded(8 * 1024 * 1024)).unwrap()
}
fn error<T>(result: Result<T, ArenaError>) -> ArenaError {
    match result {
        Err(error) => error,
        Ok(_) => panic!("invalid arena operation unexpectedly succeeded"),
    }
}
fn hash<T: Hash>(value: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}
fn clone_generic<T: Clone>(value: &T) -> T {
    value.clone()
}

#[derive(Clone, Copy)]
struct Live {
    key: Key,
    id: u64,
}
#[derive(Clone, Copy)]
struct Scalar {
    value: i64,
    marker_address: usize,
}
struct Model {
    id: ArenaId,
    live: Vec<Live>,
    retired: Vec<Key>,
    values: BTreeMap<u64, Scalar>,
    issued: Keys,
    last_generation: u64,
}
impl Model {
    fn new(id: ArenaId) -> Self {
        Self {
            id,
            live: vec![],
            retired: vec![],
            values: BTreeMap::new(),
            issued: Keys::default(),
            last_generation: 0,
        }
    }
    fn inserted(&mut self, key: Key, id: u64, scalar: Scalar) {
        assert_eq!(key.arena(), self.id);
        assert!(key.generation() > self.last_generation);
        assert_ne!(key.generation(), u64::MAX);
        assert!(
            self.issued.insert(key),
            "an opaque issued identity was reused"
        );
        self.last_generation = key.generation();
        assert!(self.values.insert(id, scalar).is_none());
        self.live.push(Live { key, id });
    }
    fn audit(&self, region: &Region<SystemBacking>, owner: &RegionSlotArena<Payload>) {
        assert_eq!(region.arena_id(owner), Ok(self.id));
        assert_eq!(region.arena_len(owner), Ok(self.values.len()));
        assert_eq!(region.arena_is_empty(owner), Ok(self.values.is_empty()));
        let capacity = region.arena_capacity(owner).unwrap();
        assert!(capacity >= self.values.len());
        for entry in &self.live {
            let expected = self.values[&entry.id];
            let actual = region.arena_get(owner, entry.key).unwrap();
            assert_eq!(actual.id, entry.id);
            assert_eq!(actual.value, expected.value);
            assert_eq!(actual.marker_address(), expected.marker_address);
            assert_eq!(*actual.marker, entry.id.to_le_bytes());
            assert!((entry.key.slot() as usize) < capacity);
        }
    }
}
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0
    }
    fn index(&mut self, len: usize) -> usize {
        (self.next() % len as u64) as usize
    }
}

#[test]
fn randomized_operations_match_scalar_oracles_across_independent_arenas() {
    for seed in [1, 7, 0x5eed_cafe, 0x1234_5678_9abc_def0] {
        let mut rng = Rng(seed);
        let mut region = region();
        let mut owners = [
            region.arena_new::<Payload>().unwrap(),
            region.arena_new::<Payload>().unwrap(),
        ];
        let mut models = [
            Model::new(region.arena_id(&owners[0]).unwrap()),
            Model::new(region.arena_id(&owners[1]).unwrap()),
        ];
        assert_ne!(models[0].id, models[1].id);
        let mut foreign_region = self::region();
        let mut foreign_owner = foreign_region.arena_new::<Payload>().unwrap();
        let foreign = foreign_region
            .arena_insert(&mut foreign_owner, Payload::new(u64::MAX, -7))
            .unwrap();
        for step in 0..2048_u64 {
            let arena = rng.index(2);
            let mut operation = rng.index(10);
            if models[arena].live.is_empty() {
                operation = 0;
            }
            if operation <= 2 && models[arena].live.len() >= 48 {
                operation = 3;
            }
            match operation {
                0..=2 => {
                    let value = (rng.next() % 10_000) as i64 - 5000;
                    let payload = Payload::new(step, value);
                    let scalar = Scalar {
                        value,
                        marker_address: payload.marker_address(),
                    };
                    let key = region.arena_insert(&mut owners[arena], payload).unwrap();
                    models[arena].inserted(key, step, scalar);
                }
                3 | 4 => {
                    let index = rng.index(models[arena].live.len());
                    let entry = models[arena].live.swap_remove(index);
                    let expected = models[arena].values.remove(&entry.id).unwrap();
                    let removed = region.arena_remove(&mut owners[arena], entry.key).unwrap();
                    assert_eq!(
                        (removed.id, removed.value, removed.marker_address()),
                        (entry.id, expected.value, expected.marker_address)
                    );
                    models[arena].retired.push(entry.key);
                    assert_eq!(
                        error(region.arena_get(&owners[arena], entry.key)),
                        ArenaError::StaleKey
                    );
                    assert_eq!(
                        error(region.arena_remove(&mut owners[arena], entry.key)),
                        ArenaError::StaleKey
                    );
                }
                5 => {
                    let entry = models[arena].live[rng.index(models[arena].live.len())];
                    let delta = (rng.next() % 63) as i64 - 31;
                    region
                        .arena_get_mut(&owners[arena], entry.key)
                        .unwrap()
                        .value += delta;
                    models[arena].values.get_mut(&entry.id).unwrap().value += delta;
                }
                6 if models[arena].live.len() >= 2 => {
                    let len = models[arena].live.len();
                    let first = rng.index(len);
                    let second = (first + 1 + rng.index(len - 1)) % len;
                    let a = models[arena].live[first];
                    let b = models[arena].live[second];
                    let delta = (rng.next() % 17) as i64 - 8;
                    let (left, right) = region
                        .arena_get_disjoint_mut(&owners[arena], a.key, b.key)
                        .unwrap();
                    assert_eq!((left.id, right.id), (a.id, b.id));
                    left.value += delta;
                    right.value -= delta;
                    models[arena].values.get_mut(&a.id).unwrap().value += delta;
                    models[arena].values.get_mut(&b.id).unwrap().value -= delta;
                }
                7 => {
                    let additional = rng.index(16);
                    let before = region.stats();
                    let old_capacity = region.arena_capacity(&owners[arena]).unwrap();
                    let live = models[arena].live.len();
                    region
                        .arena_reserve(&mut owners[arena], additional)
                        .unwrap();
                    assert!(region.arena_capacity(&owners[arena]).unwrap() >= live + additional);
                    if additional <= old_capacity - live {
                        assert_eq!(region.stats(), before);
                    }
                }
                8 if !models[arena].retired.is_empty() => {
                    let key = models[arena].retired[rng.index(models[arena].retired.len())];
                    let before = region.stats();
                    assert_eq!(
                        error(region.arena_get_mut(&owners[arena], key)),
                        ArenaError::StaleKey
                    );
                    assert_eq!(
                        error(region.arena_get_disjoint_mut(&owners[arena], key, key)),
                        ArenaError::StaleKey
                    );
                    assert_eq!(region.stats(), before);
                }
                _ => {
                    let other = 1 - arena;
                    let key = models[other]
                        .live
                        .first()
                        .map_or(foreign, |entry| entry.key);
                    let expected = ArenaError::ForeignArena {
                        expected: models[arena].id,
                        found: key.arena(),
                    };
                    let before = region.stats();
                    assert_eq!(error(region.arena_get(&owners[arena], key)), expected);
                    assert_eq!(
                        error(region.arena_get_disjoint_mut(&owners[arena], key, key)),
                        expected
                    );
                    assert_eq!(
                        error(region.arena_remove(&mut owners[arena], key)),
                        expected
                    );
                    assert_eq!(region.stats(), before);
                }
            }
            models[arena].audit(&region, &owners[arena]);
            if step % 31 == 0 {
                models[1 - arena].audit(&region, &owners[1 - arena]);
            }
        }
        for (owner, model) in owners.iter_mut().zip(&mut models) {
            model.audit(&region, owner);
            for key in &model.retired {
                assert_eq!(error(region.arena_get(owner, *key)), ArenaError::StaleKey);
            }
            for live in model.live.drain(..) {
                let removed = region.arena_remove(owner, live.key).unwrap();
                let expected = model.values.remove(&live.id).unwrap();
                assert_eq!((removed.id, removed.value), (live.id, expected.value));
            }
            assert_eq!(region.arena_len(owner), Ok(0));
        }
        for owner in owners {
            region.arena_release(owner).unwrap();
        }
        foreign_region.arena_release(foreign_owner).unwrap();
    }
}

#[test]
fn keys_require_no_payload_traits_and_remain_hash_stable_across_relocation() {
    fn key_traits<T: Copy + Clone + Eq + Hash + std::fmt::Debug>() {}
    fn debug<T: std::fmt::Debug>() {}
    key_traits::<Key>();
    key_traits::<ArenaId>();
    debug::<RegionSlotArena<Payload>>();
    debug::<vesper_alloc::ArenaInsertError<Payload>>();
    let mut region = region();
    let mut owner = region.arena_with_capacity::<Payload>(1).unwrap();
    let payload = Payload::new(10, 42);
    let marker = payload.marker_address();
    let key = region.arena_insert(&mut owner, payload).unwrap();
    let copied = clone_generic(&key);
    assert_eq!(key, copied);
    let before_hash = hash(&key);
    let resident_before = (region.arena_get(&owner, key).unwrap() as *const Payload).addr();
    let blocker = region.alloc_value([9_u8; 32]).unwrap();
    region.arena_reserve(&mut owner, 64).unwrap();
    let resident_after = (region.arena_get(&owner, key).unwrap() as *const Payload).addr();
    assert_ne!(
        resident_before, resident_after,
        "interleaved live allocation forces relocation"
    );
    assert_eq!(hash(&copied), before_hash);
    let mut set = Keys::default();
    assert!(set.insert(key));
    assert!(!set.insert(copied));
    let value = region.arena_get(&owner, copied).unwrap();
    assert_eq!(
        (value.id, value.value, value.marker_address()),
        (10, 42, marker)
    );
    assert_eq!(region.get(&blocker).unwrap(), &[9_u8; 32]);
    assert_eq!(owner.region(), region.id());
}

#[test]
fn empty_reuse_never_revives_any_previously_issued_key() {
    let mut region = region();
    let mut owner = region.arena_with_capacity::<Payload>(1).unwrap();
    let mut retired = vec![];
    let mut issued = Keys::default();
    let mut last_generation = 0;
    for id in 0..256 {
        let key = region
            .arena_insert(&mut owner, Payload::new(id, id as i64))
            .unwrap();
        assert_eq!(key.slot(), 0);
        assert!(key.generation() > last_generation);
        assert!(issued.insert(key));
        last_generation = key.generation();
        for old in &retired {
            assert_eq!(error(region.arena_get(&owner, *old)), ArenaError::StaleKey);
        }
        let before = region.stats();
        let removed = region.arena_remove(&mut owner, key).unwrap();
        assert_eq!(removed.id, id);
        assert_eq!(region.arena_is_empty(&owner), Ok(true));
        assert_eq!(region.stats(), before);
        retired.push(key);
    }
    for old in retired {
        assert_eq!(error(region.arena_get(&owner, old)), ArenaError::StaleKey);
    }
}

#[test]
fn pair_access_preserves_requested_order_and_invalid_keys_outrank_aliases() {
    let mut region = region();
    let mut owner = region.arena_new::<Payload>().unwrap();
    let keys = [10, 20, 30].map(|id| {
        region
            .arena_insert(&mut owner, Payload::new(id, id as i64))
            .unwrap()
    });
    assert!(keys[2].slot() > keys[0].slot());
    let (first, second) = region
        .arena_get_disjoint_mut(&owner, keys[2], keys[0])
        .unwrap();
    assert_eq!((first.id, second.id), (30, 10));
    first.value = 300;
    second.value = 100;
    let (first, second) = region
        .arena_get_disjoint_mut(&owner, keys[0], keys[2])
        .unwrap();
    assert_eq!((first.value, second.value), (100, 300));
    assert_eq!(
        error(region.arena_get_disjoint_mut(&owner, keys[0], keys[0])),
        ArenaError::AliasedKey
    );
    let removed = region.arena_remove(&mut owner, keys[1]).unwrap();
    assert_eq!(removed.id, 20);
    for (first, second) in [(keys[1], keys[1]), (keys[0], keys[1]), (keys[1], keys[0])] {
        assert_eq!(
            error(region.arena_get_disjoint_mut(&owner, first, second)),
            ArenaError::StaleKey
        );
    }
    let mut other = region.arena_new::<Payload>().unwrap();
    let foreign = region
        .arena_insert(&mut other, Payload::new(40, 40))
        .unwrap();
    assert_eq!(
        error(region.arena_get_disjoint_mut(&owner, foreign, foreign)),
        ArenaError::ForeignArena {
            expected: region.arena_id(&owner).unwrap(),
            found: foreign.arena()
        }
    );
}

#[test]
fn reserve_accounts_for_vacancies_and_does_not_consume_generations() {
    let mut region = region();
    let mut owner = region.arena_with_capacity::<Payload>(8).unwrap();
    let capacity = region.arena_capacity(&owner).unwrap();
    assert!(capacity >= 8);
    let keys: Vec<_> = (0..capacity)
        .map(|id| {
            region
                .arena_insert(&mut owner, Payload::new(id as u64, id as i64))
                .unwrap()
        })
        .collect();
    let mut vacancies = HashSet::new();
    let mut retired = vec![];
    for key in keys.iter().step_by(2) {
        region.arena_remove(&mut owner, *key).unwrap();
        vacancies.insert(key.slot());
        retired.push(*key);
    }
    let before = region.stats();
    region.arena_reserve(&mut owner, vacancies.len()).unwrap();
    assert_eq!(region.stats(), before);
    assert_eq!(region.arena_capacity(&owner), Ok(capacity));
    let mut last_generation = keys.last().unwrap().generation();
    for id in 0..vacancies.len() {
        let key = region
            .arena_insert(&mut owner, Payload::new(100 + id as u64, -1))
            .unwrap();
        assert!(vacancies.remove(&key.slot()));
        assert_eq!(key.generation(), last_generation + 1);
        last_generation = key.generation();
        for old in &retired {
            assert_eq!(error(region.arena_get(&owner, *old)), ArenaError::StaleKey);
        }
    }
    assert!(vacancies.is_empty());
    assert_eq!(region.stats(), before);
    assert_eq!(region.arena_len(&owner), Ok(capacity));
    region.arena_reserve(&mut owner, 32).unwrap();
    let key = region
        .arena_insert(&mut owner, Payload::new(999, 999))
        .unwrap();
    assert_eq!(key.generation(), last_generation + 1);
    for key in keys.iter().skip(1).step_by(2) {
        assert_eq!(
            region.arena_get(&owner, *key).unwrap().id,
            key.slot() as u64
        );
    }
}

#[test]
fn owner_validation_precedes_key_validation_for_foreign_and_stale_owners() {
    let mut first = region();
    let mut second = region();
    let mut owner = first.arena_new::<Payload>().unwrap();
    let key = first.arena_insert(&mut owner, Payload::new(1, 1)).unwrap();
    let mut second_owner = second.arena_new::<Payload>().unwrap();
    let other_key = second
        .arena_insert(&mut second_owner, Payload::new(2, 2))
        .unwrap();
    let cross = ArenaError::Region(RegionError::Handle(HandleError::CrossRegion {
        expected: second.id(),
        found: first.id(),
    }));
    assert_eq!(error(second.arena_get(&owner, other_key)), cross);
    assert_eq!(
        error(second.arena_get_disjoint_mut(&owner, key, key)),
        cross
    );
    assert_eq!(error(second.arena_reserve(&mut owner, 0)), cross);
    assert_eq!(
        error(second.arena_get(&second_owner, key)),
        ArenaError::ForeignArena {
            expected: other_key.arena(),
            found: key.arena()
        }
    );
    first.reset().unwrap();
    let mut fresh = first.arena_new::<Payload>().unwrap();
    let fresh_key = first.arena_insert(&mut fresh, Payload::new(3, 3)).unwrap();
    assert_ne!(fresh_key.arena(), key.arena());
    let stale = ArenaError::Region(RegionError::Handle(HandleError::Stale));
    assert_eq!(
        error(first.arena_get_disjoint_mut(&owner, fresh_key, fresh_key)),
        stale
    );
    assert_eq!(error(first.arena_reserve(&mut owner, 0)), stale);
    assert_eq!(error(first.arena_id(&owner)), stale);
    assert_eq!(error(first.arena_len(&owner)), stale);
    assert_eq!(error(first.arena_capacity(&owner)), stale);
    assert_eq!(error(first.arena_is_empty(&owner)), stale);
    assert_eq!(
        error(first.arena_get(&fresh, key)),
        ArenaError::ForeignArena {
            expected: fresh_key.arena(),
            found: key.arena()
        }
    );
}

#[test]
fn failed_checked_reservation_preserves_values_keys_and_region_accounting() {
    let mut region = region();
    let mut owner = region.arena_new::<Payload>().unwrap();
    let key = region
        .arena_insert(&mut owner, Payload::new(99, 42))
        .unwrap();
    let capacity = region.arena_capacity(&owner).unwrap();
    let before = region.stats();
    assert_eq!(
        region.arena_reserve(&mut owner, usize::MAX),
        Err(ArenaError::Region(RegionError::Alloc(
            AllocError::SizeOverflow
        )))
    );
    assert_eq!(region.stats(), before);
    assert_eq!(region.arena_capacity(&owner), Ok(capacity));
    assert_eq!(region.arena_len(&owner), Ok(1));
    assert_eq!(
        (
            region.arena_get(&owner, key).unwrap().id,
            region.arena_get(&owner, key).unwrap().value
        ),
        (99, 42)
    );
    let next = region
        .arena_insert(&mut owner, Payload::new(100, 7))
        .unwrap();
    assert_eq!(next.generation(), key.generation() + 1);
}

struct Vertex {
    value: i64,
    edges: Vec<ArenaKey<Vertex>>,
}
fn traverse(
    region: &Region<SystemBacking>,
    owner: &RegionSlotArena<Vertex>,
    start: ArenaKey<Vertex>,
) -> (i64, usize, usize) {
    let mut queue = VecDeque::from([start]);
    let mut seen = HashSet::new();
    let mut sum = 0;
    let mut stale = 0;
    while let Some(key) = queue.pop_front() {
        match region.arena_get(owner, key) {
            Ok(vertex) => {
                if seen.insert(key) {
                    sum += vertex.value;
                    queue.extend(vertex.edges.iter().copied());
                }
            }
            Err(ArenaError::StaleKey) => stale += 1,
            Err(other) => panic!("unexpected graph access failure: {other}"),
        }
    }
    (sum, seen.len(), stale)
}

#[test]
fn cyclic_graph_weak_edges_stay_stale_after_slot_reuse_until_explicit_repair() {
    let mut region = region();
    let mut owner = region.arena_with_capacity::<Vertex>(4).unwrap();
    let keys = [10, 20, 30, 40].map(|value| {
        region
            .arena_insert(
                &mut owner,
                Vertex {
                    value,
                    edges: vec![],
                },
            )
            .unwrap()
    });
    for (key, edges) in [
        (keys[0], vec![keys[1], keys[2]]),
        (keys[1], vec![keys[2]]),
        (keys[2], vec![keys[0], keys[3]]),
        (keys[3], vec![keys[1]]),
    ] {
        region.arena_get_mut(&owner, key).unwrap().edges = edges;
    }
    assert_eq!(traverse(&region, &owner, keys[0]), (100, 4, 0));
    for key in keys {
        region.arena_get_mut(&owner, key).unwrap().value += 100;
    }
    let (last, first) = region
        .arena_get_disjoint_mut(&owner, keys[3], keys[0])
        .unwrap();
    std::mem::swap(&mut last.value, &mut first.value);
    assert_eq!(traverse(&region, &owner, keys[0]), (500, 4, 0));
    let removed = region.arena_remove(&mut owner, keys[2]).unwrap();
    assert_eq!(removed.value, 130);
    assert_eq!(traverse(&region, &owner, keys[0]), (260, 2, 2));
    let replacement = region
        .arena_insert(
            &mut owner,
            Vertex {
                value: 99,
                edges: vec![keys[0], keys[3]],
            },
        )
        .unwrap();
    assert_eq!(replacement.slot(), keys[2].slot());
    assert_ne!(replacement.generation(), keys[2].generation());
    assert_eq!(traverse(&region, &owner, keys[0]), (260, 2, 2));
    for key in [keys[0], keys[1]] {
        for edge in &mut region.arena_get_mut(&owner, key).unwrap().edges {
            if *edge == keys[2] {
                *edge = replacement;
            }
        }
    }
    assert_eq!(traverse(&region, &owner, keys[0]), (469, 4, 0));
    region.arena_release(owner).unwrap();
    assert_eq!(
        removed.value, 130,
        "removed object remains owned by caller after arena cleanup"
    );
    assert_eq!(removed.edges.len(), 2);
}
