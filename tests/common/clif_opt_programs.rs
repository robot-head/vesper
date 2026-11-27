//! Programs whose native CLIF exercises particular native_clif_opt steps:
//! tests/native_clif_opt.rs checks the steps fire on them, and
//! tests/self_host_native_clif_opt.rs adds them to its corpus so the Vesper
//! port of each step has functions to change.
#![allow(dead_code)]

/// An iadd overflow check in a bounds-hoisted fast loop (`eliminate_guarded_loop_add_overflows`).
pub const GUARDED_ADD: &str = r#"fn copy_offset(in src: Slice[u8], inout dst: Slice[u8], at: u64) eff [Fail[ArithOverflow], Fail[RuntimeError]] {
    for i in 0..16 {
        let b = src[2 + i];
        dst.set(at + i, b);
    }
}
fn main() -> u64 {
    let mut a = ChunkSeq::new[u8]();
    let mut b = ChunkSeq::new[u8]();
    for i in 0..32 {
        a.push(wrapping_u8(i));
        b.push(0);
    }
    handle Fail[ArithOverflow] in {
        handle Fail[RuntimeError] in {
            copy_offset(in a.as_slice(), inout b.as_slice_mut(), 4);
            match b.try_get(7) {
                Option::Some(v) => widen_u64(v),
                Option::None => 999,
            }
        } with {
            return x => x,
            raise(_, _) => 998,
        }
    } with {
        return x => x,
        raise(_, _) => 997,
    }
}"#;

/// The same with a constant offset on the sum.
pub const GUARDED_ADD_CONST: &str = r#"fn copy_two_halves(in src: Slice[u8], inout dst: Slice[u8], at: u64) eff [Fail[ArithOverflow], Fail[RuntimeError]] {
    for i in 0..16 {
        let b = src[i];
        dst.set(at + i, b);
        dst.set(at + 16 + i, b);
    }
}
fn main() -> u64 {
    let mut a = ChunkSeq::new[u8]();
    let mut b = ChunkSeq::new[u8]();
    for i in 0..16 {
        a.push(wrapping_u8(i));
    }
    for _ in 0..40 {
        b.push(0);
    }
    handle Fail[ArithOverflow] in {
        handle Fail[RuntimeError] in {
            copy_two_halves(in a.as_slice(), inout b.as_slice_mut(), 4);
            match b.try_get(23) {
                Option::Some(v) => widen_u64(v),
                Option::None => 999,
            }
        } with {
            return x => x,
            raise(_, _) => 998,
        }
    } with {
        return x => x,
        raise(_, _) => 997,
    }
}"#;

/// The same with the guard passed through a join parameter.
pub const GUARDED_JOIN_PARAM: &str = r#"fn copy_blocks(in src: Slice[u8], inout dst: Slice[u8], blocks: u64) eff [Fail[ArithOverflow], Fail[RuntimeError]] {
    for b in 0..blocks {
        let base = 16 * b;
        for i in 0..16 {
            let v = src[i];
            dst.set(base + i, v);
        }
    }
}
fn main() -> u64 {
    let mut a = ChunkSeq::new[u8]();
    let mut b = ChunkSeq::new[u8]();
    for i in 0..16 {
        a.push(wrapping_u8(i));
    }
    for _ in 0..32 {
        b.push(0);
    }
    handle Fail[ArithOverflow] in {
        handle Fail[RuntimeError] in {
            copy_blocks(in a.as_slice(), inout b.as_slice_mut(), 2);
            match b.try_get(19) {
                Option::Some(v) => widen_u64(v),
                Option::None => 999,
            }
        } with {
            return x => x,
            raise(_, _) => 998,
        }
    } with {
        return x => x,
        raise(_, _) => 997,
    }
}"#;

/// Sequence header loads reused across `try_set` in a loop (`reuse_seq_header_loads`).
pub const SEQ_TRY_SET: &str = r#"
struct Bag {
    s: ChunkSeq[u64],
}

fn fill_and_sum(inout b: Bag, n: u64) -> u64 {
    let mut i: u64 = 0;
    let mut acc: u64 = 0;
    loop {
        if i >= n {
            break;
        }
        let _ = b.s.try_set(i, wrapping_mul(i, 3));
        let v = match b.s.try_get(i) {
            Option::Some(x) => x,
            Option::None => 0,
        };
        acc = wrapping_add(acc, v);
        i = wrapping_add(i, 1);
    }
    acc
}

fn make_bag() -> Bag {
    let mut s: ChunkSeq[u64] = ChunkSeq::new();
    s.push(0);
    s.push(0);
    s.push(0);
    s.push(0);
    s.push(0);
    s.push(0);
    Bag { s: s }
}

fn main() -> u64 {
    let mut b = make_bag();
    let a = fill_and_sum(inout b, 6);
    let c = fill_and_sum(inout b, 4);
    wrapping_add(a, c)
}"#;

/// The same after a `try_get` diamond.
pub const SEQ_TRY_GET_DIAMOND: &str = r#"
struct Bag { s: ChunkSeq[u64] }

fn bump(inout b: Bag, i: u64) -> u64 {
    let v = match b.s.try_get(i) {
        Option::Some(x) => x,
        Option::None => 0,
    };
    let _ = b.s.try_set(i, wrapping_add(v, 1));
    v
}

fn main() -> u64 {
    let mut s: ChunkSeq[u64] = ChunkSeq::new();
    s.push(10);
    s.push(20);
    let mut b = Bag { s: s };
    let a = bump(inout b, 0);
    let c = bump(inout b, 0);
    wrapping_add(a, c)
}"#;
