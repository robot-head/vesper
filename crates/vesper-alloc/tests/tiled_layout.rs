//! Independent physical-layout and initialized-prefix checks through public views.

use std::fmt::Debug;
use std::mem::{size_of, size_of_val};
use vesper_alloc::{HARD_MAX_TILE_BYTES, Region, RegionConfig, RegionTiled2, TiledError};

fn region() -> Region {
    Region::new(RegionConfig::bounded(4 * 1024 * 1024)).unwrap()
}

fn rounded_column<T, const N: usize>() -> usize {
    (N * size_of::<T>()).div_ceil(64) * 64
}

fn assert_rows<A: Copy + Debug + Eq + 'static, B: Copy + Debug + Eq + 'static, const N: usize>(
    region: &Region,
    owner: &RegionTiled2<A, B, N>,
    expected: &[(A, B)],
) {
    assert_eq!(region.tiled2_len(owner).unwrap(), expected.len());
    assert_eq!(region.tiled2_is_empty(owner).unwrap(), expected.is_empty());
    let tiles = expected.len().div_ceil(N);
    assert_eq!(region.tiled2_tile_count(owner).unwrap(), tiles);
    for (row, pair) in expected.iter().copied().enumerate() {
        assert_eq!(region.tiled2_get(owner, row).unwrap(), Some(pair));
    }
    assert_eq!(region.tiled2_get(owner, expected.len()).unwrap(), None);
    assert_eq!(region.tiled2_get(owner, usize::MAX).unwrap(), None);
    for tile in 0..tiles {
        let (first, second) = region.tiled2_columns(owner, tile).unwrap().unwrap();
        let start = tile * N;
        let end = (start + N).min(expected.len());
        assert_eq!(first.len(), end - start);
        assert_eq!(second.len(), end - start);
        for (lane, &(a, b)) in expected[start..end].iter().enumerate() {
            assert_eq!((first[lane], second[lane]), (a, b));
        }
    }
    assert!(region.tiled2_columns(owner, tiles).unwrap().is_none());
    assert!(region.tiled2_columns(owner, usize::MAX).unwrap().is_none());
}

fn physical_layout<A, B, const N: usize>(make: impl Fn(usize) -> (A, B))
where
    A: Copy + Debug + Eq + 'static,
    B: Copy + Debug + Eq + 'static,
{
    let mut region = region();
    let mut owner = region.tiled2_with_capacity::<A, B, N>(4 * N).unwrap();
    let expected: Vec<_> = (0..3 * N + 3).map(make).collect();
    for &(first, second) in &expected {
        region.tiled2_push(&mut owner, first, second).unwrap();
    }
    assert_rows(&region, &owner, &expected);
    let first_bytes = rounded_column::<A, N>();
    let second_bytes = rounded_column::<B, N>();
    let stride = (first_bytes + second_bytes + size_of::<usize>()).div_ceil(64) * 64;
    let mut previous = None;
    for tile in 0..4 {
        let (first, second) = region.tiled2_columns(&owner, tile).unwrap().unwrap();
        let first_addr = first.as_ptr().addr();
        let second_addr = second.as_ptr().addr();
        assert_eq!(first_addr % 64, 0);
        assert_eq!(second_addr % 64, 0);
        assert_eq!(second_addr - first_addr, first_bytes);
        assert!(first_addr + size_of_val(first) <= second_addr);
        if let Some((previous_first, previous_second)) = previous {
            assert_eq!(first_addr - previous_first, stride);
            assert_eq!(second_addr - previous_second, stride);
        }
        for (lane, value) in first.iter().enumerate() {
            assert_eq!(
                std::ptr::from_ref(value).addr() - first_addr,
                lane * size_of::<A>()
            );
        }
        for (lane, value) in second.iter().enumerate() {
            assert_eq!(
                std::ptr::from_ref(value).addr() - second_addr,
                lane * size_of::<B>()
            );
        }
        previous = Some((first_addr, second_addr));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C, align(64))]
struct CacheLine([u8; 64]);

fn varied_fields<const N: usize>() {
    physical_layout::<u8, u16, N>(|i| (i as u8, (i * 13) as u16));
    physical_layout::<u32, u64, N>(|i| ((i * 7) as u32, (i * 19) as u64));
    physical_layout::<u128, u8, N>(|i| ((i as u128) << 80 | i as u128, i as u8));
    physical_layout::<[u8; 17], [u32; 3], N>(|i| ([i as u8; 17], [i as u32; 3]));
    physical_layout::<CacheLine, u16, N>(|i| (CacheLine([i as u8; 64]), i as u16));
}

#[test]
fn eight_lane_columns_have_actual_aligned_contiguous_storage() {
    varied_fields::<8>();
}

#[test]
fn sixteen_lane_columns_have_actual_aligned_contiguous_storage() {
    varied_fields::<16>();
}

#[test]
fn thirty_two_lane_columns_have_actual_aligned_contiguous_storage() {
    varied_fields::<32>();
}

#[test]
fn sixty_four_lane_columns_have_actual_aligned_contiguous_storage() {
    varied_fields::<64>();
}

fn mutate_equal_columns<const N: usize>() {
    let mut region = region();
    let mut owner = region.tiled2_new::<u64, u64, N>().unwrap();
    let mut expected = Vec::new();
    for row in 0..2 * N + 5 {
        let pair = (row as u64 * 3, 100_000 + row as u64 * 7);
        region.tiled2_push(&mut owner, pair.0, pair.1).unwrap();
        expected.push(pair);
    }
    for tile in 0..expected.len().div_ceil(N) {
        let (first, second) = region.tiled2_columns_mut(&owner, tile).unwrap().unwrap();
        let first_range = first.as_ptr().addr()..first.as_ptr().addr() + size_of_val(first);
        let second_range = second.as_ptr().addr()..second.as_ptr().addr() + size_of_val(second);
        assert!(first_range.end <= second_range.start || second_range.end <= first_range.start);
        for lane in 0..first.len() {
            let row = tile * N + lane;
            // Keep both mutable projections live and observe each after writing the other.
            let old_second = second[lane];
            first[lane] = first[lane].wrapping_add(row as u64 + 31);
            assert_eq!(second[lane], old_second);
            second[lane] ^= first[lane].rotate_left(11);
            let old_first = first[lane];
            second[lane] = second[lane].wrapping_sub(17);
            assert_eq!(first[lane], old_first);
        }
    }
    // Scalar calculation uses only the original logical rows, never projected data.
    for (row, (first, second)) in expected.iter_mut().enumerate() {
        *first += row as u64 + 31;
        *second = (*second ^ first.rotate_left(11)).wrapping_sub(17);
    }
    assert_rows(&region, &owner, &expected);
    for _ in 0..N + 7 {
        assert_eq!(region.tiled2_pop(&mut owner).unwrap(), expected.pop());
        assert_rows(&region, &owner, &expected);
    }
    for row in 0..2 * N + 1 {
        let pair = (u64::MAX - row as u64, row as u64 + 99);
        region.tiled2_push(&mut owner, pair.0, pair.1).unwrap();
        expected.push(pair);
        assert_rows(&region, &owner, &expected);
    }
    while let Some(pair) = expected.pop() {
        assert_eq!(region.tiled2_pop(&mut owner).unwrap(), Some(pair));
        assert_rows(&region, &owner, &expected);
    }
    assert_eq!(region.tiled2_pop(&mut owner).unwrap(), None);
    assert!(region.tiled2_columns_mut(&owner, 0).unwrap().is_none());
}

#[test]
fn equal_typed_columns_are_disjoint_and_mutations_match_scalar_rows() {
    mutate_equal_columns::<8>();
    mutate_equal_columns::<16>();
    mutate_equal_columns::<32>();
    mutate_equal_columns::<64>();
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NoDefault(u32);

#[test]
fn copy_fields_without_default_expose_only_initialized_lanes() {
    let mut region = region();
    let mut owner = region
        .tiled2_with_capacity::<NoDefault, NoDefault, 16>(48)
        .unwrap();
    let mut expected = Vec::new();
    assert_rows(&region, &owner, &expected);
    for i in 0..35 {
        let pair = (NoDefault(i), NoDefault(i + 100));
        region.tiled2_push(&mut owner, pair.0, pair.1).unwrap();
        expected.push(pair);
        assert_rows(&region, &owner, &expected);
    }
    for _ in 0..20 {
        assert_eq!(region.tiled2_pop(&mut owner).unwrap(), expected.pop());
        assert_rows(&region, &owner, &expected);
    }
    let (first, second) = region.tiled2_columns_mut(&owner, 0).unwrap().unwrap();
    assert_eq!((first.len(), second.len()), (15, 15));
    first[14] = NoDefault(999);
    second[14] = NoDefault(777);
    expected[14] = (NoDefault(999), NoDefault(777));
    assert_rows(&region, &owner, &expected);
    region
        .tiled2_push(&mut owner, NoDefault(33), NoDefault(44))
        .unwrap();
    expected.push((NoDefault(33), NoDefault(44)));
    assert_rows(&region, &owner, &expected);
}

fn unsupported_width<const N: usize>(region: &mut Region) {
    let before = region.stats();
    assert!(matches!(
        region.tiled2_new::<u8, u16, N>(),
        Err(TiledError::UnsupportedTileWidth { width }) if width == N
    ));
    assert!(matches!(
        region.tiled2_with_capacity::<u8, u16, N>(usize::MAX),
        Err(TiledError::UnsupportedTileWidth { width }) if width == N
    ));
    assert_eq!(region.stats(), before);
}

#[test]
fn unsupported_representable_widths_are_rejected_before_region_allocation() {
    let mut region = region();
    unsupported_width::<0>(&mut region);
    unsupported_width::<1>(&mut region);
    unsupported_width::<7>(&mut region);
    unsupported_width::<9>(&mut region);
    unsupported_width::<15>(&mut region);
    unsupported_width::<17>(&mut region);
    unsupported_width::<31>(&mut region);
    unsupported_width::<33>(&mut region);
    unsupported_width::<63>(&mut region);
    unsupported_width::<65>(&mut region);
    unsupported_width::<128>(&mut region);
}

#[derive(Clone, Copy)]
#[repr(C, align(128))]
struct OverAligned([u8; 128]);

fn unsupported_fields<A: Copy + 'static, B: Copy + 'static, const N: usize>(region: &mut Region) {
    let before = region.stats();
    assert!(matches!(
        region.tiled2_new::<A, B, N>(),
        Err(TiledError::UnsupportedFieldLayout)
    ));
    assert!(matches!(
        region.tiled2_with_capacity::<A, B, N>(usize::MAX),
        Err(TiledError::UnsupportedFieldLayout)
    ));
    assert_eq!(region.stats(), before);
}

#[test]
fn zero_sized_overaligned_and_oversized_fields_cannot_mint_an_owner() {
    let mut region = region();
    unsupported_fields::<(), u8, 8>(&mut region);
    unsupported_fields::<u8, (), 64>(&mut region);
    unsupported_fields::<[u8; 0], [u8; 0], 16>(&mut region);
    unsupported_fields::<OverAligned, u8, 8>(&mut region);
    unsupported_fields::<u8, OverAligned, 32>(&mut region);
    unsupported_fields::<[u8; 1024], [u8; 1024], 64>(&mut region);
    // Field bytes alone fit 64KiB; the initialized header forces a larger tile.
    unsupported_fields::<[u8; 512], [u8; 512], 64>(&mut region);
}

#[test]
fn complete_tile_size_boundary_includes_header_and_final_padding() {
    assert_eq!(HARD_MAX_TILE_BYTES, 65_536);
    let mut region = region();
    // 32768 + 32704 column bytes + usize header rounds to exactly 65536.
    let mut owner = region
        .tiled2_with_capacity::<[u8; 512], [u8; 511], 64>(128)
        .unwrap();
    for row in 0..65 {
        region
            .tiled2_push(&mut owner, [row as u8; 512], [255 - row as u8; 511])
            .unwrap();
    }
    let (first, second) = region.tiled2_columns(&owner, 0).unwrap().unwrap();
    let (next_first, next_second) = region.tiled2_columns(&owner, 1).unwrap().unwrap();
    assert_eq!(first.len(), 64);
    assert_eq!(second.len(), 64);
    assert_eq!(next_first.len(), 1);
    assert_eq!(next_second.len(), 1);
    assert_eq!(second.as_ptr().addr() - first.as_ptr().addr(), 32_768);
    assert_eq!(
        next_first.as_ptr().addr() - first.as_ptr().addr(),
        HARD_MAX_TILE_BYTES
    );
    assert_eq!(
        next_second.as_ptr().addr() - second.as_ptr().addr(),
        HARD_MAX_TILE_BYTES
    );
    assert_eq!(next_first[0], [64; 512]);
    assert_eq!(next_second[0], [191; 511]);
}
