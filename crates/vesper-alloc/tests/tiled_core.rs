use std::alloc::Layout;
use std::cell::Cell;
use std::ptr::NonNull;
use std::rc::Rc;
use vesper_alloc::{
    AllocError, ChunkBacking, HandleError, Region, RegionConfig, RegionError, RegionTiled2,
    TiledError,
};

#[derive(Clone, Copy, PartialEq, Eq)]
struct NoDefault(u64);

fn width_oracle<const N: usize>() {
    let mut region = Region::new(RegionConfig::bounded(32 * 1024)).unwrap();
    let mut owner = region.tiled2_new::<u64, u32, N>().unwrap();
    assert_eq!(owner.region(), region.id());
    assert!(region.tiled2_is_empty(&owner).unwrap());
    assert_eq!(region.tiled2_capacity(&owner).unwrap(), 0);
    assert_eq!(region.tiled2_tile_count(&owner).unwrap(), 0);
    assert_eq!(region.tiled2_pop(&mut owner).unwrap(), None);
    let mut expected = Vec::new();
    for index in 0..(2 * N + 3) {
        let row = (1000 + index as u64, (index * 7) as u32);
        region.tiled2_push(&mut owner, row.0, row.1).unwrap();
        expected.push(row);
        assert_eq!(region.tiled2_len(&owner).unwrap(), expected.len());
        assert_eq!(region.tiled2_get(&owner, index).unwrap(), Some(row));
        assert_eq!(region.tiled2_get(&owner, index + 1).unwrap(), None);
    }
    assert_eq!(region.tiled2_tile_count(&owner).unwrap(), 3);
    for index in 0..3 {
        let (first, second) = region.tiled2_columns(&owner, index).unwrap().unwrap();
        let begin = index * N;
        let end = (begin + N).min(expected.len());
        assert_eq!(first.len(), end - begin);
        assert_eq!(second.len(), end - begin);
        assert_eq!(
            first,
            expected[begin..end]
                .iter()
                .map(|row| row.0)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            second,
            expected[begin..end]
                .iter()
                .map(|row| row.1)
                .collect::<Vec<_>>()
        );
    }
    assert!(region.tiled2_columns(&owner, usize::MAX).unwrap().is_none());
    while let Some(row) = expected.pop() {
        assert_eq!(region.tiled2_pop(&mut owner).unwrap(), Some(row));
        assert_eq!(region.tiled2_len(&owner).unwrap(), expected.len());
        assert_eq!(
            region.tiled2_tile_count(&owner).unwrap(),
            expected.len().div_ceil(N)
        );
    }
    assert_eq!(region.tiled2_pop(&mut owner).unwrap(), None);
    assert!(region.tiled2_columns(&owner, 0).unwrap().is_none());
    // Reuse capacity after removing every tile, without exposing old lanes.
    region.tiled2_push(&mut owner, 91, 27).unwrap();
    assert_eq!(
        region.tiled2_columns(&owner, 0).unwrap().unwrap(),
        (&[91][..], &[27][..])
    );
}

#[test]
fn every_width_preserves_initialized_rows_through_growth_and_pop() {
    width_oracle::<8>();
    width_oracle::<16>();
    width_oracle::<32>();
    width_oracle::<64>();
}

#[test]
fn capacity_rounds_by_tiles_and_reserve_counts_rows_from_partial_tail() {
    let mut region = Region::new(RegionConfig::bounded(8192)).unwrap();
    let mut owner = region.tiled2_with_capacity::<u64, u32, 16>(17).unwrap();
    assert_eq!(region.tiled2_capacity(&owner).unwrap(), 32);
    for index in 0..17 {
        region.tiled2_push(&mut owner, index, index as u32).unwrap();
    }
    let before = region.stats();
    region.tiled2_reserve(&mut owner, 15).unwrap();
    assert_eq!(region.stats(), before);
    region.tiled2_reserve(&mut owner, 16).unwrap();
    assert!(region.tiled2_capacity(&owner).unwrap() >= 33);
    assert_eq!(region.tiled2_len(&owner).unwrap(), 17);
    assert_eq!(region.tiled2_get(&owner, 16).unwrap(), Some((16, 16)));
}

#[test]
fn copy_fields_need_neither_default_nor_debug() {
    let mut region = Region::new(RegionConfig::default()).unwrap();
    let mut owner = region.tiled2_new::<NoDefault, NoDefault, 8>().unwrap();
    assert!(format!("{owner:?}").contains("RegionTiled2"));
    region
        .tiled2_push(&mut owner, NoDefault(19), NoDefault(29))
        .unwrap();
    let (first, second) = region.tiled2_columns_mut(&owner, 0).unwrap().unwrap();
    first[0] = NoDefault(41);
    second[0] = NoDefault(43);
    let row = region.tiled2_get(&owner, 0).unwrap().unwrap();
    assert!(row == (NoDefault(41), NoDefault(43)));
}

struct RefuseBacking(Rc<Cell<usize>>);

// SAFETY: this backing always returns None, never minting an allocation.
unsafe impl ChunkBacking for RefuseBacking {
    fn allocate(&mut self, _: Layout) -> Option<NonNull<u8>> {
        self.0.set(self.0.get() + 1);
        None
    }

    unsafe fn deallocate(&mut self, _: NonNull<u8>, _: Layout) {
        unreachable!("refusing backing cannot own an allocation")
    }
}

#[repr(align(128))]
#[derive(Clone, Copy)]
struct Overaligned(u8);

#[test]
fn unsupported_representable_shapes_refuse_before_any_backing_request() {
    let calls = Rc::new(Cell::new(0));
    let mut region =
        Region::with_backing(RegionConfig::default(), RefuseBacking(calls.clone())).unwrap();
    let before = region.stats();
    assert!(matches!(
        region.tiled2_new::<u8, u8, 0>(),
        Err(TiledError::UnsupportedTileWidth { width: 0 })
    ));
    assert!(matches!(
        region.tiled2_new::<u8, u8, 3>(),
        Err(TiledError::UnsupportedTileWidth { width: 3 })
    ));
    assert!(matches!(
        region.tiled2_new::<u8, u8, 128>(),
        Err(TiledError::UnsupportedTileWidth { width: 128 })
    ));
    assert!(matches!(
        region.tiled2_new::<(), u8, 8>(),
        Err(TiledError::UnsupportedFieldLayout)
    ));
    assert!(matches!(
        region.tiled2_new::<u8, (), 8>(),
        Err(TiledError::UnsupportedFieldLayout)
    ));
    assert_eq!(Overaligned(7).0, 7);
    assert!(matches!(
        region.tiled2_new::<Overaligned, u8, 8>(),
        Err(TiledError::UnsupportedFieldLayout)
    ));
    assert!(matches!(
        region.tiled2_new::<[u8; 8192], u8, 8>(),
        Err(TiledError::UnsupportedFieldLayout)
    ));
    assert_eq!(calls.get(), 0);
    assert_eq!(region.stats(), before);
    assert!(matches!(
        region.tiled2_with_capacity::<u64, u32, 16>(1),
        Err(TiledError::Region(RegionError::Alloc(
            AllocError::OutOfMemory { .. }
        )))
    ));
    assert_eq!(calls.get(), 1);
    assert_eq!(region.stats(), before);
}

#[test]
fn row_capacity_and_addition_overflows_leave_the_region_unchanged() {
    let mut region = Region::new(RegionConfig::default()).unwrap();
    let initial = region.stats();
    assert!(matches!(
        region.tiled2_with_capacity::<u64, u32, 16>(usize::MAX),
        Err(TiledError::Region(RegionError::Alloc(
            AllocError::SizeOverflow
        )))
    ));
    assert_eq!(region.stats(), initial);
    let mut owner = region.tiled2_new::<u64, u32, 16>().unwrap();
    let before = region.stats();
    assert_eq!(
        region.tiled2_reserve(&mut owner, usize::MAX),
        Err(RegionError::Alloc(AllocError::SizeOverflow))
    );
    assert_eq!(region.stats(), before);
    region.tiled2_push(&mut owner, 13, 17).unwrap();
    let before = region.stats();
    assert_eq!(
        region.tiled2_reserve(&mut owner, usize::MAX),
        Err(RegionError::Alloc(AllocError::SizeOverflow))
    );
    assert_eq!(region.stats(), before);
    assert_eq!(region.tiled2_get(&owner, 0).unwrap(), Some((13, 17)));
}

#[test]
fn exhausted_region_preserves_failed_row_and_all_existing_rows() {
    // Two 64-byte column wrappers and a padded initialized-count cache line.
    let mut region = Region::new(RegionConfig::bounded(192)).unwrap();
    let mut owner = region.tiled2_with_capacity::<u32, u16, 8>(8).unwrap();
    for index in 0..8 {
        region
            .tiled2_push(&mut owner, index, index as u16 + 100)
            .unwrap();
    }
    let before = region.stats();
    let failure = region.tiled2_push(&mut owner, 123, 456).unwrap_err();
    assert_eq!(failure.value, (123, 456));
    assert!(matches!(
        failure.error,
        RegionError::Alloc(AllocError::CapacityExceeded { .. })
    ));
    assert_eq!(region.stats(), before);
    assert_eq!(region.tiled2_len(&owner).unwrap(), 8);
    for index in 0..8 {
        assert_eq!(
            region.tiled2_get(&owner, index).unwrap(),
            Some((index as u32, index as u16 + 100))
        );
    }
}

#[test]
fn protected_growth_returns_the_actual_pair_and_preserves_stats() {
    let mut region = Region::new(RegionConfig::default()).unwrap();
    let mut owner = region.tiled2_with_capacity::<u64, u32, 8>(8).unwrap();
    for index in 0..8 {
        region.tiled2_push(&mut owner, index, index as u32).unwrap();
    }
    let mark = region.checkpoint().unwrap();
    let before = region.stats();
    let failure = region.tiled2_push(&mut owner, 91, 37).unwrap_err();
    assert_eq!(failure.value, (91, 37));
    assert_eq!(failure.error, RegionError::ProtectedByWatermark);
    assert_eq!(region.stats(), before);
    assert_eq!(region.tiled2_get(&owner, 7).unwrap(), Some((7, 7)));
    region.rewind(mark).unwrap();
    region
        .tiled2_push(&mut owner, failure.value.0, failure.value.1)
        .unwrap();
    assert_eq!(region.tiled2_get(&owner, 8).unwrap(), Some((91, 37)));
}

#[test]
fn failed_consuming_release_returns_a_live_owner_to_its_original_region() {
    let mut first = Region::new(RegionConfig::default()).unwrap();
    let mut second = Region::new(RegionConfig::default()).unwrap();
    let mut owner: RegionTiled2<u64, u32, 16> = first.tiled2_new().unwrap();
    first.tiled2_push(&mut owner, 71, 73).unwrap();
    let failure = second.tiled2_release(owner).unwrap_err();
    assert!(matches!(
        failure.error,
        RegionError::Handle(HandleError::CrossRegion { .. })
    ));
    let owner = failure.owner;
    assert_eq!(owner.region(), first.id());
    assert_eq!(first.tiled2_get(&owner, 0).unwrap(), Some((71, 73)));
    first.tiled2_release(owner).unwrap();
}

#[test]
fn stale_owners_are_validated_before_bounds_or_noop_answers() {
    let mut region = Region::new(RegionConfig::default()).unwrap();
    let mark = region.checkpoint().unwrap();
    let mut owner = region.tiled2_new::<u64, u32, 8>().unwrap();
    region.tiled2_push(&mut owner, 7, 9).unwrap();
    region.rewind(mark).unwrap();
    assert_eq!(
        region.tiled2_get(&owner, usize::MAX),
        Err(HandleError::Stale)
    );
    assert!(matches!(
        region.tiled2_columns(&owner, usize::MAX),
        Err(HandleError::Stale)
    ));
    assert_eq!(
        region.tiled2_reserve(&mut owner, 0),
        Err(RegionError::Handle(HandleError::Stale))
    );
    let failure = region.tiled2_push(&mut owner, 11, 13).unwrap_err();
    assert_eq!(failure.value, (11, 13));
    assert_eq!(failure.error, RegionError::Handle(HandleError::Stale));
}
