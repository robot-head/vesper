//! The fixed-size array library (#1091) end to end: `vesper/arrays.ves`
//! linked through the committed `port/arrays.rs`. Array fields of copy
//! structs are lent in place as their element cells; a Rust model of each
//! function gives the expected values. Vesper build only.
#![cfg(feature = "vesper-port")]

use vesper_port_abi::{Mirror, RuntimeError, Word, mirror_words};
use vesper_port_pilot::arrays::{self, Block, Board, Report, repr};

fn expected_report(id: u64, seed: u8) -> Report {
    Report {
        id,
        hash: core::array::from_fn(|i| seed.wrapping_add(i as u8)),
        words: core::array::from_fn(|i| id.wrapping_mul(i as u64)),
        pair: (1, 2),
    }
}

fn sum(r: &Report) -> u64 {
    let mut s = r.id;
    for h in r.hash {
        s = s.wrapping_add(u64::from(h));
    }
    for w in r.words {
        s = s.wrapping_add(w);
    }
    s.wrapping_add(u64::from(r.pair.0) + u64::from(r.pair.1))
}

#[test]
fn array_fields_are_element_cells_in_order() {
    assert_eq!(<repr::Report as Mirror>::WORDS, 51);
    assert_eq!(core::mem::size_of::<repr::Report>(), 51 * 8);
    assert_eq!(core::mem::offset_of!(repr::Report, words), 33 * 8);
    let r = arrays::report_new(7, 250);
    assert_eq!(r.to_value(), expected_report(7, 250));
    // A `[u8; 32]` field is 32 narrow cells, one per word.
    assert_eq!(r.hash[5], Word::new(255));
    assert_eq!(r.hash[6].get(), 0);
    let words = mirror_words(&r);
    assert_eq!(words[0], 7);
    assert_eq!(&words[1..4], &[250, 251, 252]);
    assert_eq!(words[33 + 3], 21);
    assert_eq!(&words[49..51], &[1, 2]);
}

#[test]
fn array_fields_are_read_and_written_in_place() {
    for (id, seed) in [(0, 0), (7, 250), (u64::MAX, 255), (1 << 40, 3)] {
        let mut r = arrays::report_new(id, seed);
        let mut model = expected_report(id, seed);
        assert_eq!(arrays::report_sum(&r), sum(&model));
        for at in [0, 15, 16, 31, 33, 1000] {
            arrays::report_bump(&mut r, at);
            let w = (at & 15) as usize;
            model.words[w] = model.words[w].wrapping_add(1);
            model.hash[(at & 31) as usize] = 255;
            model.pair.0 = model.pair.0.wrapping_add(1);
            assert_eq!(r.to_value(), model);
        }
        assert_eq!(arrays::report_sum(&r), sum(&model));
        let lent = repr::Report::from_value(&model);
        assert_eq!(arrays::report_sum(&lent), sum(&model));
    }
}

#[test]
fn an_unproved_index_raises_index_out_of_bounds() {
    let mut r = arrays::report_new(1, 1);
    assert_eq!(arrays::report_set(&mut r, 15, 99), Ok(()));
    assert_eq!(r.words[15], 99);
    let before = r;
    assert_eq!(
        arrays::report_set(&mut r, 16, 5),
        Err(RuntimeError::IndexOutOfBounds)
    );
    assert_eq!(
        arrays::report_set(&mut r, u64::MAX, 5),
        Err(RuntimeError::IndexOutOfBounds)
    );
    assert_eq!(r, before, "a failed write leaves the array unchanged");
}

#[test]
fn whole_arrays_cross_as_parameters_and_results() {
    let xs: [u64; 16] = core::array::from_fn(|i| (i as u64 + 1) * 1000);
    assert_eq!(arrays::words_total(xs), xs.iter().sum::<u64>());
    let mut rev = xs;
    rev.reverse();
    assert_eq!(arrays::words_reversed(xs), rev);
    assert_eq!(arrays::words_total([u64::MAX; 16]), 0u64.wrapping_sub(16));
}

/// The board's Rust model: merge into the first overlapping block, else
/// append, else drop the lowest block.
fn add(b: &mut Board, lo: u64, hi: u64) -> u64 {
    let n = b.len as usize;
    if let Some(x) = b.blocks[..n].iter_mut().find(|x| lo <= x.hi && x.lo <= hi) {
        *x = Block {
            lo: lo.min(x.lo),
            hi: hi.max(x.hi),
        };
    } else if n < 16 {
        b.blocks[n] = Block { lo, hi };
        b.len += 1;
    } else {
        b.blocks.copy_within(1.., 0);
        b.blocks[15] = Block { lo, hi };
    }
    b.len
}

#[test]
fn an_array_of_mirrors_is_a_sack_scoreboard() {
    let mut b = arrays::board_new();
    let mut model = b.to_value();
    assert_eq!(model.len, 0);
    assert_eq!(<repr::Board as Mirror>::WORDS, 33);
    let mut x: u64 = 0x9e37_79b9;
    for step in 0..200u64 {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let lo = (x >> 33) % 4000 + step * 50;
        let hi = lo + (x >> 20) % 40;
        assert_eq!(arrays::board_add(&mut b, lo, hi), add(&mut model, lo, hi));
        assert_eq!(b.to_value(), model, "step {step}");
    }
    let covered: u64 = model.blocks[..model.len as usize]
        .iter()
        .map(|x| x.hi - x.lo)
        .sum();
    assert_eq!(arrays::board_covered(&b), covered);
    assert_eq!(b.blocks[3].lo, model.blocks[3].lo);
}
