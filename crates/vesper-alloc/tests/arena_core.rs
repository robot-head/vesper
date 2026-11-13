use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use vesper_alloc::{
    AllocError, ArenaError, ArenaKey, HARD_MAX_ARENA_ENTRY_BYTES, HandleError, Region,
    RegionConfig, RegionError,
};

struct Payload {
    tag: u32,
    text: String,
    drops: Rc<RefCell<Vec<u32>>>,
}
impl Drop for Payload {
    fn drop(&mut self) {
        self.drops.borrow_mut().push(self.tag);
    }
}

#[test]
fn noncopy_values_and_trait_free_payload_keys_preserve_actual_ownership() {
    let drops = Rc::new(RefCell::new(Vec::new()));
    let mut region = Region::new(RegionConfig::default()).unwrap();
    let mut arena = region.arena_new::<Payload>().unwrap();
    assert!(format!("{arena:?}").contains("RegionSlotArena"));
    let mut keys = HashSet::new();
    for tag in 0..3 {
        let key = region
            .arena_insert(
                &mut arena,
                Payload {
                    tag,
                    text: format!("value {tag}"),
                    drops: drops.clone(),
                },
            )
            .unwrap();
        let copied = key;
        assert_eq!(key, copied);
        assert!(keys.insert(key));
        assert!(keys.contains(&copied));
        assert_eq!(key.arena(), region.arena_id(&arena).unwrap());
        assert!(key.arena().bits() > 0);
        assert_eq!(
            region.arena_get(&arena, key).unwrap().text,
            format!("value {tag}")
        );
    }
    assert!(drops.borrow().is_empty());
    let removed_key = *keys.iter().next().unwrap();
    let removed = region.arena_remove(&mut arena, removed_key).unwrap();
    assert!(drops.borrow().is_empty());
    assert!(matches!(
        region.arena_get(&arena, removed_key),
        Err(ArenaError::StaleKey)
    ));
    region.arena_release(arena).unwrap();
    assert_eq!(drops.borrow().len(), 2);
    drop(removed);
    let mut finalized = drops.borrow().clone();
    finalized.sort_unstable();
    assert_eq!(finalized, vec![0, 1, 2]);
}

#[test]
fn reuse_changes_generation_and_reservation_discounts_vacancies() {
    let mut region = Region::new(RegionConfig::default()).unwrap();
    let mut arena = region.arena_with_capacity::<String>(3).unwrap();
    let a = region.arena_insert(&mut arena, String::from("a")).unwrap();
    let b = region.arena_insert(&mut arena, String::from("b")).unwrap();
    let c = region.arena_insert(&mut arena, String::from("c")).unwrap();
    assert_eq!(region.arena_remove(&mut arena, b).unwrap(), "b");
    let before = region.stats();
    region.arena_reserve(&mut arena, 1).unwrap();
    assert_eq!(region.stats(), before);
    let replacement = region
        .arena_insert(&mut arena, String::from("replacement"))
        .unwrap();
    assert_eq!(replacement.slot(), b.slot());
    assert!(replacement.generation() > c.generation());
    assert_eq!(region.stats(), before);
    assert_eq!(region.arena_get(&arena, b), Err(ArenaError::StaleKey));
    assert_eq!(
        region.arena_get(&arena, replacement).unwrap(),
        "replacement"
    );
    region.arena_reserve(&mut arena, 5).unwrap();
    assert!(region.arena_capacity(&arena).unwrap() >= 8);
    assert_eq!(region.arena_len(&arena).unwrap(), 3);
    assert_eq!(region.arena_get(&arena, a).unwrap(), "a");
    assert_eq!(region.arena_get(&arena, c).unwrap(), "c");
}

#[test]
fn pair_access_preserves_argument_order_and_validates_before_aliasing() {
    let mut region = Region::new(RegionConfig::default()).unwrap();
    let mut arena = region.arena_new::<String>().unwrap();
    let a = region.arena_insert(&mut arena, String::from("a")).unwrap();
    let b = region.arena_insert(&mut arena, String::from("b")).unwrap();
    let (second, first) = region.arena_get_disjoint_mut(&arena, b, a).unwrap();
    second.push_str(" second");
    first.push_str(" first");
    assert_eq!(region.arena_get(&arena, a).unwrap(), "a first");
    assert_eq!(region.arena_get(&arena, b).unwrap(), "b second");
    assert_eq!(
        region.arena_get_disjoint_mut(&arena, a, a),
        Err(ArenaError::AliasedKey)
    );
    region.arena_remove(&mut arena, a).unwrap();
    assert_eq!(
        region.arena_get_disjoint_mut(&arena, a, a),
        Err(ArenaError::StaleKey)
    );
    assert_eq!(
        region.arena_get_disjoint_mut(&arena, b, a),
        Err(ArenaError::StaleKey)
    );
    let mut foreign = region.arena_new::<String>().unwrap();
    let foreign_key = region
        .arena_insert(&mut foreign, String::from("foreign"))
        .unwrap();
    let expected = region.arena_id(&arena).unwrap();
    assert_eq!(
        region.arena_get_disjoint_mut(&arena, b, foreign_key),
        Err(ArenaError::ForeignArena {
            expected,
            found: foreign_key.arena(),
        })
    );
}

#[test]
fn protected_insert_returns_original_allocation_and_does_not_consume_generation() {
    let mut region = Region::new(RegionConfig::default()).unwrap();
    let mut arena = region.arena_with_capacity::<String>(3).unwrap();
    let capacity = region.arena_capacity(&arena).unwrap();
    let mut keys = Vec::new();
    for index in 0..capacity {
        keys.push(
            region
                .arena_insert(&mut arena, format!("value {index}"))
                .unwrap(),
        );
    }
    let last_generation = keys.last().unwrap().generation();
    let mark = region.checkpoint().unwrap();
    let before = region.stats();
    let original = String::from("original allocation");
    let pointer = original.as_ptr();
    let failure = region.arena_insert(&mut arena, original).unwrap_err();
    assert_eq!(
        failure.error,
        ArenaError::Region(RegionError::ProtectedByWatermark)
    );
    assert_eq!(failure.value.as_ptr(), pointer);
    assert_eq!(region.stats(), before);
    assert_eq!(region.arena_len(&arena).unwrap(), capacity);
    assert_eq!(region.arena_remove(&mut arena, keys[0]).unwrap(), "value 0");
    let reused = region.arena_insert(&mut arena, failure.value).unwrap();
    assert_eq!(reused.generation(), last_generation + 1);
    region.rewind(mark).unwrap();
    assert_eq!(region.arena_get(&arena, keys[0]), Err(ArenaError::StaleKey));
    assert_eq!(region.arena_get(&arena, reused).unwrap().as_ptr(), pointer);
}

#[test]
fn noops_and_failed_consuming_release_validate_and_preserve_the_owner() {
    let mut region = Region::new(RegionConfig::default()).unwrap();
    let mut other = Region::new(RegionConfig::default()).unwrap();
    let mut arena = region.arena_new::<String>().unwrap();
    let key = region
        .arena_insert(&mut arena, String::from("live"))
        .unwrap();
    assert!(matches!(
        other.arena_reserve(&mut arena, 0),
        Err(ArenaError::Region(RegionError::Handle(
            HandleError::CrossRegion { .. }
        )))
    ));
    let failure = other.arena_release(arena).unwrap_err();
    assert!(matches!(
        failure.error,
        RegionError::Handle(HandleError::CrossRegion { .. })
    ));
    let mut arena = failure.owner;
    assert_eq!(arena.region(), region.id());
    assert_eq!(region.arena_get(&arena, key).unwrap(), "live");
    let mark = region.checkpoint().unwrap();
    let mut stale = region.arena_new::<String>().unwrap();
    region.rewind(mark).unwrap();
    assert_eq!(
        region.arena_reserve(&mut stale, 0),
        Err(ArenaError::Region(RegionError::Handle(HandleError::Stale)))
    );
    assert_eq!(
        region.arena_len(&stale),
        Err(ArenaError::Region(RegionError::Handle(HandleError::Stale)))
    );
    region.arena_remove(&mut arena, key).unwrap();
    assert!(region.arena_is_empty(&arena).unwrap());
}

#[test]
fn oversized_and_overflowing_requests_refuse_before_region_changes() {
    let mut region = Region::new(RegionConfig::default()).unwrap();
    let before = region.stats();
    assert_eq!(HARD_MAX_ARENA_ENTRY_BYTES, 64 * 1024);
    // No payload value is created. The validation frame must not contain this
    // oversized enum's Header temporary before returning its shape refusal.
    assert!(matches!(
        region.arena_new::<[u8; 16 * 1024 * 1024 + 1]>(),
        Err(ArenaError::UnsupportedEntryLayout)
    ));
    assert_eq!(region.stats(), before);
    assert!(matches!(
        region.arena_with_capacity::<u64>(usize::MAX),
        Err(ArenaError::Region(RegionError::Alloc(
            AllocError::SizeOverflow
        )))
    ));
    assert_eq!(region.stats(), before);
    let mut arena = region.arena_new::<u64>().unwrap();
    let key = region.arena_insert(&mut arena, 29).unwrap();
    let before = region.stats();
    assert_eq!(
        region.arena_reserve(&mut arena, usize::MAX),
        Err(ArenaError::Region(RegionError::Alloc(
            AllocError::SizeOverflow
        )))
    );
    assert_eq!(region.stats(), before);
    assert_eq!(*region.arena_get(&arena, key).unwrap(), 29);
}

#[repr(align(128))]
struct Aligned(u8);

#[test]
fn zero_sized_and_over_64_alignment_values_follow_actual_region_layout_policy() {
    let mut region = Region::new(RegionConfig::default()).unwrap();
    let mut unit = region.arena_new::<()>().unwrap();
    let first = region.arena_insert(&mut unit, ()).unwrap();
    let second = region.arena_insert(&mut unit, ()).unwrap();
    assert_ne!(first, second);
    region.arena_get_disjoint_mut(&unit, first, second).unwrap();
    region.arena_remove(&mut unit, first).unwrap();
    assert_eq!(region.arena_len(&unit).unwrap(), 1);
    let mut aligned = region.arena_new::<Aligned>().unwrap();
    let key = region.arena_insert(&mut aligned, Aligned(17)).unwrap();
    let value = region.arena_get(&aligned, key).unwrap();
    assert_eq!(value.0, 17);
    assert_eq!((value as *const Aligned as usize) % 128, 0);
}

#[test]
fn copied_key_retains_identity_when_owner_moves_or_storage_grows() {
    let mut region = Region::new(RegionConfig::default()).unwrap();
    let mut original = region.arena_new::<String>().unwrap();
    let key: ArenaKey<String> = region
        .arena_insert(&mut original, String::from("stable key"))
        .unwrap();
    let id = region.arena_id(&original).unwrap();
    let mut moved = original;
    region.arena_reserve(&mut moved, 100).unwrap();
    assert_eq!(region.arena_id(&moved).unwrap(), id);
    assert_eq!(region.arena_get(&moved, key).unwrap(), "stable key");
    *region.arena_get_mut(&moved, key).unwrap() = String::from("updated");
    assert_eq!(region.arena_remove(&mut moved, key).unwrap(), "updated");
}
