use std::collections::hash_map::DefaultHasher;
use std::error::Error;
use std::hash::{Hash, Hasher};
use std::iter::FusedIterator;
use vesper_alloc::{
    AllocError, FLAT_GROUP_WIDTH, FlatConfig, FlatError, HARD_MAX_FLAT_GROUP_BYTES,
    HARD_MAX_FLAT_GROUPS, HandleError, Region, RegionConfig, RegionError,
};

fn region() -> Region {
    Region::new(RegionConfig::bounded(4 * 1024 * 1024)).unwrap()
}

#[test]
fn configuration_and_exact_power_of_two_initial_capacity() {
    assert_eq!(FLAT_GROUP_WIDTH, 16);
    assert_eq!(HARD_MAX_FLAT_GROUPS, 4096);
    assert_eq!(HARD_MAX_FLAT_GROUP_BYTES, 65_536);
    assert_eq!(FlatConfig::default().max_groups, 1024);
    let mut r = region();
    for max_groups in [0, 3, 6, 4097, 8192, usize::MAX] {
        let before = r.stats();
        assert!(matches!(
            r.flat_with_capacity::<u32, u32>(usize::MAX, FlatConfig { max_groups }),
            Err(FlatError::InvalidMaxGroups { max_groups: found }) if found == max_groups
        ));
        assert_eq!(r.stats(), before);
    }
    for (requested, groups) in [(0, 1), (1, 1), (14, 1), (15, 2), (28, 2), (29, 4), (56, 4)] {
        let table = r
            .flat_with_capacity::<u64, u64>(requested, FlatConfig { max_groups: 4 })
            .unwrap();
        assert_eq!(table.region(), r.id());
        assert_eq!(r.flat_len(&table), Ok(0));
        assert_eq!(r.flat_is_empty(&table), Ok(true));
        assert_eq!(r.flat_group_count(&table), Ok(groups));
        assert_eq!(r.flat_capacity(&table), Ok(groups * 14));
        for group in 0..groups {
            assert_eq!(
                r.flat_control_group(&table, group).unwrap().unwrap(),
                &[0x80; 16]
            );
        }
        assert!(r.flat_control_group(&table, groups).unwrap().is_none());
        r.flat_release(table).unwrap();
    }
    let before = r.stats();
    assert!(matches!(
        r.flat_with_capacity::<u8, u8>(57, FlatConfig { max_groups: 4 }),
        Err(FlatError::CapacityLimit {
            requested: 57,
            max: 56
        })
    ));
    assert_eq!(r.stats(), before);
}

#[test]
fn insertion_replacement_and_growth_preserve_actual_values() {
    let mut r = region();
    let mut table = r
        .flat_new::<String, String>(FlatConfig { max_groups: 4 })
        .unwrap();
    for key in 0..14 {
        assert_eq!(
            r.flat_insert(&mut table, format!("key{key}"), format!("value{key}"))
                .unwrap(),
            None
        );
    }
    let before = r.stats();
    assert_eq!(
        r.flat_insert(&mut table, "key7".into(), "replacement".into())
            .unwrap(),
        Some(("key7".into(), "value7".into()))
    );
    assert_eq!(r.stats(), before);
    assert_eq!(r.flat_len(&table), Ok(14));
    r.flat_insert(&mut table, "new".into(), "newvalue".into())
        .unwrap();
    assert_eq!(r.flat_capacity(&table), Ok(28));
    assert_eq!(r.flat_group_count(&table), Ok(2));
    for key in 0..14 {
        let expected = if key == 7 {
            "replacement".into()
        } else {
            format!("value{key}")
        };
        assert_eq!(
            r.flat_get(&table, &format!("key{key}")).unwrap(),
            Some(&expected)
        );
    }
    assert_eq!(
        r.flat_remove(&table, &"new".into()).unwrap(),
        Some(("new".into(), "newvalue".into()))
    );
    assert_eq!(r.flat_remove(&table, &"new".into()).unwrap(), None);
    assert_eq!(r.flat_capacity(&table), Ok(28));
    r.flat_clear(&table).unwrap();
    assert_eq!(r.flat_len(&table), Ok(0));
    assert_eq!(r.flat_capacity(&table), Ok(28));
    for group in 0..2 {
        assert_eq!(
            r.flat_control_group(&table, group).unwrap().unwrap(),
            &[0x80; 16]
        );
    }
}

#[test]
fn reserve_uses_live_length_plus_additional_and_rejects_before_mutation() {
    let mut r = region();
    let mut table = r
        .flat_new::<usize, usize>(FlatConfig { max_groups: 4 })
        .unwrap();
    for key in 0..13 {
        r.flat_insert(&mut table, key, key * 11).unwrap();
    }
    r.flat_reserve(&mut table, 1).unwrap();
    assert_eq!(r.flat_capacity(&table), Ok(14));
    r.flat_reserve(&mut table, 2).unwrap();
    assert_eq!(r.flat_capacity(&table), Ok(28));
    r.flat_reserve(&mut table, 43).unwrap();
    assert_eq!(r.flat_capacity(&table), Ok(56));
    let before = r.stats();
    assert_eq!(
        r.flat_reserve(&mut table, 44),
        Err(FlatError::CapacityLimit {
            requested: 57,
            max: 56
        })
    );
    assert_eq!(
        r.flat_reserve(&mut table, usize::MAX),
        Err(FlatError::Region(RegionError::Alloc(
            AllocError::SizeOverflow
        )))
    );
    assert_eq!(r.stats(), before);
    for key in 0..13 {
        assert_eq!(r.flat_get(&table, &key), Ok(Some(&(key * 11))));
    }
}

#[test]
fn bounded_full_capacity_refusal_returns_incoming_tuple() {
    let mut r = region();
    let mut table = r
        .flat_new::<u32, String>(FlatConfig { max_groups: 1 })
        .unwrap();
    for key in 0..14 {
        r.flat_insert(&mut table, key, format!("v{key}")).unwrap();
    }
    let before = r.stats();
    let error = r
        .flat_insert(&mut table, 99, "incoming".into())
        .unwrap_err();
    assert_eq!(error.value, (99, "incoming".into()));
    assert_eq!(
        error.error,
        FlatError::CapacityLimit {
            requested: 15,
            max: 14
        }
    );
    assert_eq!(r.stats(), before);
    assert_eq!(r.flat_len(&table), Ok(14));
    assert_eq!(
        r.flat_insert(&mut table, 7, "replacement".into()).unwrap(),
        Some((7, "v7".into()))
    );
    assert_eq!(r.flat_remove(&table, &2).unwrap(), Some((2, "v2".into())));
    r.flat_insert(&mut table, error.value.0, error.value.1)
        .unwrap();
    assert_eq!(
        r.flat_get(&table, &99).unwrap().map(String::as_str),
        Some("incoming")
    );
}

#[test]
fn mutable_and_disjoint_queries_preserve_order_and_detect_actual_alias() {
    let mut r = region();
    let mut table = r
        .flat_with_capacity::<u64, u64>(70, FlatConfig::default())
        .unwrap();
    for key in 0..70 {
        r.flat_insert(&mut table, key, key * 10).unwrap();
    }
    *r.flat_get_mut(&table, &4).unwrap().unwrap() = 444;
    for (a, b) in [(0, 1), (1, 0), (3, 65), (65, 3), (20, 50), (50, 20)] {
        let [first, second] = r.flat_get_disjoint_mut(&table, [&a, &b]).unwrap();
        *first.unwrap() = a + 1000;
        *second.unwrap() = b + 2000;
        assert_eq!(r.flat_get(&table, &a), Ok(Some(&(a + 1000))));
        assert_eq!(r.flat_get(&table, &b), Ok(Some(&(b + 2000))));
    }
    assert_eq!(
        r.flat_get_disjoint_mut(&table, [&4, &4]).unwrap_err(),
        HandleError::Aliased
    );
    let [present, missing] = r.flat_get_disjoint_mut(&table, [&4, &999]).unwrap();
    assert_eq!(present.map(|value| *value), Some(444));
    assert!(missing.is_none());
    let [missing, present] = r.flat_get_disjoint_mut(&table, [&999, &4]).unwrap();
    assert!(missing.is_none());
    assert_eq!(present.map(|value| *value), Some(444));
    let [first, second] = r.flat_get_disjoint_mut(&table, [&999, &999]).unwrap();
    assert!(first.is_none() && second.is_none());
    assert_eq!(r.flat_get_mut(&table, &999).unwrap(), None);
}

#[test]
fn iteration_is_exact_fused_and_borrows_region_not_owner() {
    fn assert_fused<T: FusedIterator + ExactSizeIterator>(_: &T) {}
    let mut r = region();
    let mut table = r
        .flat_with_capacity::<u64, u64>(56, FlatConfig::default())
        .unwrap();
    for key in 0..35 {
        r.flat_insert(&mut table, key, key + 100).unwrap();
    }
    for key in [2, 8, 31] {
        r.flat_remove(&table, &key).unwrap();
    }
    let mut iter = r.flat_iter(&table).unwrap();
    assert_fused(&iter);
    let mut seen = Vec::new();
    for remaining in (1..=32).rev() {
        assert_eq!(iter.len(), remaining);
        assert_eq!(iter.size_hint(), (remaining, Some(remaining)));
        let (&key, &value) = iter.next().unwrap();
        assert_eq!(value, key + 100);
        seen.push(key);
    }
    assert_eq!(iter.next(), None);
    assert_eq!(iter.next(), None);
    assert_eq!(iter.len(), 0);
    seen.sort_unstable();
    assert_eq!(
        seen,
        (0..35)
            .filter(|key| ![2, 8, 31].contains(key))
            .collect::<Vec<_>>()
    );
    let mut survivor = {
        let mut local_owner = r.flat_new::<u64, u64>(FlatConfig::default()).unwrap();
        r.flat_insert(&mut local_owner, 9, 10).unwrap();
        r.flat_iter(&local_owner).unwrap()
    };
    assert_eq!(survivor.next(), Some((&9, &10)));
}

#[test]
fn control_tags_are_actual_stored_hash_tags_and_removal_marks_deleted() {
    let mut r = region();
    let mut table = r.flat_new::<u64, u64>(FlatConfig::default()).unwrap();
    for key in 0u64..8 {
        r.flat_insert(&mut table, key, key).unwrap();
    }
    let controls = r.flat_control_group(&table, 0).unwrap().unwrap();
    for key in 0u64..8 {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        assert_eq!(controls[key as usize], (hasher.finish() >> 57) as u8);
    }
    assert_eq!(&controls[8..], &[0x80; 8]);
    r.flat_remove(&table, &3).unwrap();
    assert_eq!(r.flat_control_group(&table, 0).unwrap().unwrap()[3], 0xfe);
    for key in [0, 1, 2, 4, 5, 6, 7] {
        assert_eq!(r.flat_get(&table, &key), Ok(Some(&key)));
    }
    assert!(r.flat_control_group(&table, usize::MAX).unwrap().is_none());
}

#[derive(Hash, Eq, PartialEq)]
struct NonDebugKey(u32);
struct NonDebugValue(String);

#[test]
fn noncopy_nondefault_nondebug_payloads_and_error_sources_are_supported() {
    let mut r = region();
    let mut table = r
        .flat_new::<NonDebugKey, NonDebugValue>(FlatConfig { max_groups: 1 })
        .unwrap();
    assert!(format!("{table:?}").contains("RegionFlatTable"));
    r.flat_insert(&mut table, NonDebugKey(3), NonDebugValue("payload".into()))
        .unwrap();
    assert_eq!(
        r.flat_get(&table, &NonDebugKey(3)).unwrap().unwrap().0,
        "payload"
    );
    let old = r
        .flat_insert(&mut table, NonDebugKey(3), NonDebugValue("new".into()))
        .unwrap()
        .unwrap();
    assert_eq!((old.0.0, old.1.0.as_str()), (3, "payload"));
    let wrapped = FlatError::from(AllocError::SizeOverflow);
    assert!(wrapped.source().is_some());
    assert!(
        FlatError::InvalidMaxGroups { max_groups: 3 }
            .source()
            .is_none()
    );
    let foreign = region();
    let error = r.flat_insert(&mut table, NonDebugKey(5), NonDebugValue("other".into()));
    assert!(error.is_ok());
    assert!(matches!(
        foreign.flat_len(&table),
        Err(HandleError::CrossRegion { .. })
    ));
    let stale_error = {
        r.reset().unwrap();
        match r.flat_insert(&mut table, NonDebugKey(9), NonDebugValue("returned".into())) {
            Err(error) => error,
            Ok(_) => panic!("stale insertion unexpectedly succeeded"),
        }
    };
    assert_eq!(stale_error.value.1.0, "returned");
    assert!(stale_error.source().is_some());
    assert!(format!("{stale_error}").contains("Stale"));
}

#[test]
fn zero_sized_payloads_and_keys_have_real_slot_identity() {
    let mut r = region();
    let mut values = r.flat_new::<u64, ()>(FlatConfig::default()).unwrap();
    r.flat_insert(&mut values, 1, ()).unwrap();
    r.flat_insert(&mut values, 2, ()).unwrap();
    let [first, second] = r.flat_get_disjoint_mut(&values, [&1, &2]).unwrap();
    assert!(first.is_some() && second.is_some());
    assert_eq!(
        r.flat_get_disjoint_mut(&values, [&1, &1]).unwrap_err(),
        HandleError::Aliased
    );
    let mut keys = r.flat_new::<(), String>(FlatConfig::default()).unwrap();
    assert_eq!(r.flat_insert(&mut keys, (), "one".into()).unwrap(), None);
    assert_eq!(
        r.flat_insert(&mut keys, (), "two".into()).unwrap(),
        Some(((), "one".into()))
    );
    assert_eq!(r.flat_len(&keys), Ok(1));
    assert_eq!(r.flat_remove(&keys, &()).unwrap(), Some(((), "two".into())));
}

#[test]
fn oversized_entry_layout_is_refused_before_group_creation_or_allocation() {
    let mut r = region();
    let before = r.stats();
    assert!(matches!(
        r.flat_with_capacity::<u64, [u8; 8192]>(usize::MAX, FlatConfig::default()),
        Err(FlatError::UnsupportedEntryLayout)
    ));
    assert_eq!(r.stats(), before);
    // Invalid configuration takes precedence over both shape and capacity.
    assert!(matches!(
        r.flat_with_capacity::<u64, [u8; 8192]>(usize::MAX, FlatConfig { max_groups: 3 }),
        Err(FlatError::InvalidMaxGroups { max_groups: 3 })
    ));
    assert_eq!(r.stats(), before);
    // A representable 16MiB group is refused without entering its stack-value
    // initializer; this is larger than an ordinary test thread's stack.
    assert!(matches!(
        r.flat_new::<u64, [u8; 1024 * 1024]>(FlatConfig::default()),
        Err(FlatError::UnsupportedEntryLayout)
    ));
    assert_eq!(r.stats(), before);
}
