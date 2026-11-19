//! The pilot module, function by function: the Vesper word-layout table
//! (modules/layout, linked by build.rs) against the compiler's Rust table
//! (`vesper::layout::RUST_WORD_LAYOUT`) on the edge values of each
//! argument, including the ones that only overflow checks reach.
#[rustfmt::skip]
#[allow(clippy::all, dead_code)]
#[path = "../port/layout.rs"]
mod binding;

use vesper::layout::RUST_WORD_LAYOUT as RUST;

const I32: &[i32] = &[
    i32::MIN,
    i32::MIN + 1,
    -9,
    -8,
    -1,
    0,
    1,
    7,
    8,
    9,
    16,
    24,
    4096,
    i32::MAX - 8,
    i32::MAX - 1,
    i32::MAX,
];
const U32: &[u32] = &[
    0,
    1,
    7,
    8,
    9,
    24,
    4096,
    i32::MAX as u32 - 8,
    i32::MAX as u32,
    i32::MAX as u32 + 1,
    u32::MAX - 1,
    u32::MAX,
];

fn vesper_next(offset: i32, size: u32) -> Option<i32> {
    let next = binding::next_offset(offset, size);
    (next != binding::no_fit()).then_some(next as i32)
}

#[test]
fn first_offset_matches() {
    for is_enum in [false, true] {
        assert_eq!(binding::first_offset(is_enum), (RUST.first_offset)(is_enum));
    }
}

#[test]
fn next_offset_matches() {
    for &offset in I32 {
        for &size in U32 {
            assert_eq!(
                vesper_next(offset, size),
                (RUST.next_offset)(offset, size),
                "next_offset({offset}, {size})"
            );
        }
    }
}

#[test]
fn sizes_match() {
    for &end in I32 {
        assert_eq!(
            binding::product_size(end),
            (RUST.product_size)(end),
            "product_size({end})"
        );
        for &size in U32 {
            assert_eq!(
                binding::enum_size(size, end),
                (RUST.enum_size)(size, end),
                "enum_size({size}, {end})"
            );
        }
    }
}

#[test]
fn word_offsets_match() {
    // Rust's `index * 8` overflow-checks in debug builds; layouts never
    // have more than a few hundred words.
    for index in (0..4096).chain([u32::MAX / 8 - 1, u32::MAX / 8]) {
        assert_eq!(
            binding::word_offset(index),
            (RUST.word_offset)(index),
            "word_offset({index})"
        );
    }
}
