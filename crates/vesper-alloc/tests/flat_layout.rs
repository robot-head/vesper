//! Physical inspection uses only public borrowed controls/entries, never casts.
use std::{
    alloc::Layout,
    cell::RefCell,
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    mem::{align_of, size_of},
    ptr::NonNull,
    rc::Rc,
};
use vesper_alloc::{
    AllocError, ChunkBacking, FLAT_GROUP_WIDTH, FlatConfig, FlatError, HARD_MAX_FLAT_GROUP_BYTES,
    HARD_MAX_FLAT_GROUPS, Region, RegionConfig, RegionError, SystemBacking,
};

#[derive(Default)]
struct Ledger {
    attempts: usize,
    live: Vec<(usize, usize)>,
}
struct Backing(Rc<RefCell<Ledger>>);
// SAFETY: every successful allocation is forwarded unchanged to SystemBacking;
// address ranges are observation-only and never reconstructed into pointers.
unsafe impl ChunkBacking for Backing {
    fn allocate(&mut self, layout: Layout) -> Option<NonNull<u8>> {
        self.0.borrow_mut().attempts += 1;
        let pointer = SystemBacking.allocate(layout)?;
        self.0
            .borrow_mut()
            .live
            .push((pointer.as_ptr().addr(), layout.size()));
        Some(pointer)
    }
    unsafe fn deallocate(&mut self, pointer: NonNull<u8>, layout: Layout) {
        let address = pointer.as_ptr().addr();
        let mut ledger = self.0.borrow_mut();
        let at = ledger
            .live
            .iter()
            .position(|entry| *entry == (address, layout.size()))
            .unwrap();
        ledger.live.swap_remove(at);
        drop(ledger);
        // SAFETY: ChunkBacking's caller supplies the original live pointer and
        // Layout; this wrapper has not freed or accessed its payload elsewhere.
        unsafe { SystemBacking.deallocate(pointer, layout) };
    }
}
fn setup() -> (Region<Backing>, Rc<RefCell<Ledger>>) {
    let ledger = Rc::new(RefCell::new(Ledger::default()));
    let config = RegionConfig {
        initial_chunk_bytes: 64,
        max_chunk_bytes: 64,
        max_reserved_bytes: 8 * 1024 * 1024,
        max_align: 4096,
        max_allocations: 1024,
        max_checkpoints: 16,
        retained_bytes: 0,
    };
    (
        Region::with_backing(config, Backing(ledger.clone())).unwrap(),
        ledger,
    )
}
fn inspect<K: Hash + Eq + 'static, V: 'static>(pairs: Vec<(K, V)>, alignment: usize) {
    let (mut region, ledger) = setup();
    let mut table = region
        .flat_with_capacity::<K, V>(43, FlatConfig { max_groups: 8 })
        .unwrap();
    assert_eq!(region.flat_group_count(&table), Ok(4));
    assert_eq!(region.flat_capacity(&table), Ok(56));
    assert_eq!(ledger.borrow().attempts, 1);
    let addresses: Vec<_> = (0..4)
        .map(|group| {
            let control = region.flat_control_group(&table, group).unwrap().unwrap();
            assert_eq!(control.len(), 16);
            assert!(control.iter().all(|byte| byte & 0x80 != 0));
            control.as_ptr().addr()
        })
        .collect();
    let stride = addresses[1] - addresses[0];
    assert!(stride >= 16);
    assert_eq!(stride % alignment, 0);
    assert!(stride <= HARD_MAX_FLAT_GROUP_BYTES);
    for (i, address) in addresses.iter().enumerate() {
        assert_eq!(*address, addresses[0] + i * stride);
        assert_eq!(*address % 16, 0);
        assert_eq!(*address % alignment, 0);
        assert!(
            ledger
                .borrow()
                .live
                .iter()
                .any(|(base, size)| *address >= *base && *address + 16 <= *base + *size)
        );
    }
    assert_eq!(
        region.stats().used_bytes,
        4 * stride,
        "one exact region buffer, without hidden group over-reservation"
    );
    let mut expected_tags = Vec::new();
    let count = pairs.len();
    for (key, value) in pairs {
        let mut hash = DefaultHasher::new();
        key.hash(&mut hash);
        expected_tags.push((hash.finish() >> 57) as u8);
        assert!(
            region
                .flat_insert(&mut table, key, value)
                .unwrap()
                .is_none()
        );
    }
    assert_eq!(
        ledger.borrow().attempts,
        1,
        "in-capacity insertions obtain no extra backing chunk"
    );
    let mut observed: Vec<_> = (0..4)
        .flat_map(|group| {
            region
                .flat_control_group(&table, group)
                .unwrap()
                .unwrap()
                .iter()
                .copied()
        })
        .filter(|byte| *byte < 0x80)
        .collect();
    expected_tags.sort_unstable();
    observed.sort_unstable();
    assert_eq!(observed, expected_tags);
    for (key, value) in region.flat_iter(&table).unwrap() {
        for (address, bytes) in [
            ((key as *const K).addr(), size_of::<K>()),
            ((value as *const V).addr(), size_of::<V>()),
        ] {
            if bytes != 0 {
                assert!(address >= addresses[0] && address + bytes <= addresses[0] + 4 * stride);
                assert!(
                    ledger
                        .borrow()
                        .live
                        .iter()
                        .any(|(base, size)| address >= *base && address + bytes <= *base + *size)
                );
            }
        }
    }
    region.flat_reserve(&mut table, 60).unwrap();
    assert_eq!(region.flat_group_count(&table), Ok(8));
    assert_eq!(region.flat_len(&table), Ok(count));
    assert_eq!(ledger.borrow().attempts, 2);
    let first = region
        .flat_control_group(&table, 0)
        .unwrap()
        .unwrap()
        .as_ptr()
        .addr();
    for group in 0..8 {
        assert_eq!(
            region
                .flat_control_group(&table, group)
                .unwrap()
                .unwrap()
                .as_ptr()
                .addr(),
            first + group * stride
        );
    }
    drop(region);
    assert!(ledger.borrow().live.is_empty());
}

#[repr(align(64))]
#[derive(Hash, PartialEq, Eq)]
struct AlignedKey(u64);
#[repr(align(128))]
struct AlignedValue(u8);

#[test]
fn real_control_width_alignment_stride_tags_and_inline_entries_match_native_shapes() {
    assert_eq!(FLAT_GROUP_WIDTH, 16);
    inspect(vec![(1u8, 11u16), (2, 22), (3, 33)], 16);
    inspect(vec![([1u8; 17], [2u8; 3]), ([3u8; 17], [4u8; 3])], 16);
    inspect(vec![((), ())], 16);
    let value = AlignedValue(7);
    assert_eq!(value.0, 7);
    inspect(vec![(AlignedKey(9), value)], 128);
}

#[test]
fn complete_group_limit_config_and_capacity_refuse_before_any_backing_request() {
    let (mut region, ledger) = setup();
    let before = region.stats();
    assert_eq!(FlatConfig::default().max_groups, 1024);
    assert_eq!(HARD_MAX_FLAT_GROUPS, 4096);
    assert_eq!(HARD_MAX_FLAT_GROUP_BYTES, 65536);
    for max_groups in [0, 3, HARD_MAX_FLAT_GROUPS * 2] {
        assert_eq!(
            region
                .flat_with_capacity::<u8, [u8; 4096]>(usize::MAX, FlatConfig { max_groups })
                .err(),
            Some(FlatError::InvalidMaxGroups { max_groups })
        );
    }
    for entries in [0, usize::MAX] {
        assert_eq!(
            region
                .flat_with_capacity::<u8, [u8; 4096]>(entries, FlatConfig { max_groups: 1 })
                .err(),
            Some(FlatError::UnsupportedEntryLayout)
        );
    }
    assert_eq!(
        region
            .flat_with_capacity::<u64, u64>(57, FlatConfig { max_groups: 4 })
            .err(),
        Some(FlatError::CapacityLimit {
            requested: 57,
            max: 56
        })
    );
    assert_eq!(ledger.borrow().attempts, 0);
    assert_eq!(region.stats(), before);
    let table = region
        .flat_new::<u8, [u8; 4000]>(FlatConfig { max_groups: 1 })
        .unwrap();
    assert_eq!(region.flat_group_count(&table), Ok(1));
    assert!(region.stats().used_bytes <= HARD_MAX_FLAT_GROUP_BYTES);
    assert!(region.stats().used_bytes >= 16 * 4000);
    assert_eq!(ledger.borrow().attempts, 1);
}

#[test]
fn alignment_and_reserve_overflow_keep_actual_region_state_unchanged() {
    assert_eq!(align_of::<AlignedValue>(), 128);
    let config = RegionConfig {
        max_align: 16,
        ..RegionConfig::bounded(1024 * 1024)
    };
    let mut region = Region::new(config).unwrap();
    let before = region.stats();
    assert_eq!(
        region
            .flat_new::<u8, AlignedValue>(FlatConfig::default())
            .err(),
        Some(FlatError::Region(RegionError::Alloc(
            AllocError::AlignmentTooLarge {
                align: 128,
                max: 16
            }
        )))
    );
    assert_eq!(region.stats(), before);
    let mut region = Region::new(RegionConfig::bounded(1024 * 1024)).unwrap();
    let mut table = region
        .flat_new::<u64, u64>(FlatConfig { max_groups: 1 })
        .unwrap();
    region.flat_insert(&mut table, 1, 11).unwrap();
    let before = region.stats();
    assert_eq!(
        region.flat_reserve(&mut table, usize::MAX),
        Err(FlatError::Region(RegionError::Alloc(
            AllocError::SizeOverflow
        )))
    );
    assert_eq!(region.stats(), before);
    assert_eq!(region.flat_get(&table, &1), Ok(Some(&11)));
}
