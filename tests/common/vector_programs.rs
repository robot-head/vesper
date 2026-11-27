//! Vector programs (#1500) whose `Vec[T, N]` temporaries live in stack
//! slots before the native CLIF clean-up: tests/native_vector_registers.rs
//! runs them, and tests/self_host_native_clif_opt.rs adds them to its
//! corpus so `vector_registers` has slots to work on.
#![allow(dead_code)]

/// The Kinetic/Resonance report's repro (gap 2): a four-lane `s[k] += x[k]`
/// loop, with the sum bound by `let` (an aggregate copy).
pub const ACC4: &str = r#"
fn acc4(inout s: Slice[u32], in x: Slice[u32]) eff [Isa[Baseline128], Fail[RuntimeError]] {
    for j in 0..s.len() / 4 {
        let k = wrapping_mul(j, 4);
        let v = Vec::add(Vec::load[u32, 4](in s, k), Vec::load[u32, 4](in x, k));
        Vec::store(inout s, k, v);
    }
}
fn run() -> u64 eff [Isa[Baseline128], Fail[RuntimeError]] {
    let mut s: ChunkSeq[u32] = ChunkSeq::new[u32]();
    let mut x: ChunkSeq[u32] = ChunkSeq::new[u32]();
    for i in 0..16 { s.push(wrapping_u32(i)); x.push(wrapping_u32(wrapping_mul(i, 3))); }
    acc4(inout s.as_slice_mut(), in x.as_slice());
    let mut sum: u64 = 0;
    for i in 0..16 { sum = wrapping_add(sum, widen_u64(s[wrapping_u64(i)])); }
    sum
}
fn main() -> u64 { if Isa::supported[Baseline128]() { handle Fail[RuntimeError] in { run() } with { return x => x, raise(e, k) => 5, } } else { 480 } }
"#;

/// Resonance's `v_tick4` shape: f32 lanes from `u32` bits, splat
/// constants, `div`/`mul`/`sub` chains and two stores per step.
pub const TICK: &str = r#"
fn tick(inout t: Slice[u32], v0: f32) eff [Isa[Baseline128], Fail[RuntimeError]] {
    let one = Vec::splat[f32, 4](1.0);
    let two = Vec::splat[f32, 4](2.0);
    let input = Vec::splat[f32, 4](v0);
    for q in 0..t.len() / 12 {
        let i = wrapping_mul(q, 12);
        let g = Vec::from_bits(Vec::load[u32, 4](in t, i));
        let ic1 = Vec::from_bits(Vec::load[u32, 4](in t, wrapping_add(i, 4)));
        let ic2 = Vec::from_bits(Vec::load[u32, 4](in t, wrapping_add(i, 8)));
        let a1 = Vec::div(one, Vec::add(one, Vec::mul(g, g)));
        let v3 = Vec::sub(input, ic2);
        let v1 = Vec::add(Vec::mul(a1, ic1), Vec::mul(g, v3));
        Vec::store(inout t, wrapping_add(i, 4), Vec::to_bits(Vec::sub(Vec::mul(two, v1), ic1)));
        Vec::store(inout t, wrapping_add(i, 8), Vec::to_bits(Vec::mul(a1, v3)));
    }
}
fn run() -> u64 eff [Isa[Baseline128], Fail[RuntimeError]] {
    let mut t: ChunkSeq[u32] = ChunkSeq::new[u32]();
    for i in 0..24 { t.push(wrapping_u32(wrapping_add(1065353216, wrapping_mul(i, 99991)))); }
    for n in 0..3 { tick(inout t.as_slice_mut(), 0.5); }
    let mut h: u64 = 1469598103934665603;
    for i in 0..24 { h = wrapping_mul(h ^ widen_u64(t[wrapping_u64(i)]), 1099511628211); }
    h
}
fn main() -> u64 { if Isa::supported[Baseline128]() { handle Fail[RuntimeError] in { run() } with { return x => x, raise(e, k) => 5, } } else { 0 } }
"#;

/// Resonance's `v_ramp4` shape: lane compares, masks and selects.
pub const RAMP: &str = r#"
fn ramp(inout r: Slice[u32]) eff [Isa[Baseline128], Fail[RuntimeError]] {
    let zero = Vec::splat[u32, 4](0);
    let ones = Vec::splat[u32, 4](1);
    for q in 0..r.len() / 12 {
        let i = wrapping_mul(q, 12);
        let cur = Vec::load[u32, 4](in r, i);
        let tgt = Vec::load[u32, 4](in r, wrapping_add(i, 4));
        let rem = Vec::load[u32, 4](in r, wrapping_add(i, 8));
        let resting = Vec::eq(rem, zero);
        let next = Vec::select(resting, zero, Vec::sub(rem, ones));
        let landed = Vec::eq(next, zero);
        let moved = Vec::select(landed, tgt, Vec::add(cur, ones));
        Vec::store(inout r, i, Vec::select(resting, cur, moved));
        Vec::store(inout r, wrapping_add(i, 8), next);
    }
}
fn run() -> u64 eff [Isa[Baseline128], Fail[RuntimeError]] {
    let mut r: ChunkSeq[u32] = ChunkSeq::new[u32]();
    for i in 0..24 { r.push(wrapping_u32(i % 5)); }
    for n in 0..4 { ramp(inout r.as_slice_mut()); }
    let mut h: u64 = 0;
    for i in 0..24 { h = wrapping_add(wrapping_mul(h, 31), widen_u64(r[wrapping_u64(i)])); }
    h
}
fn main() -> u64 { if Isa::supported[Baseline128]() { handle Fail[RuntimeError] in { run() } with { return x => x, raise(e, k) => 5, } } else { 0 } }
"#;

/// A loop-carried accumulator (a vector phi), reduced after the loop, and
/// an eight-lane vector (two chunks).
pub const SUMS: &str = r#"
fn sum4(in x: Slice[u64]) -> u64 eff [Isa[Baseline128], Fail[RuntimeError]] {
    let mut acc = Vec::splat[u64, 2](0);
    for j in 0..x.len() / 2 {
        acc = Vec::add(acc, Vec::load[u64, 2](in x, wrapping_mul(j, 2)));
    }
    Vec::reduce_add(acc)
}
fn sum8(in x: Slice[u32]) -> u64 eff [Isa[Avx2], Fail[RuntimeError]] {
    let mut acc = Vec::splat[u32, 8](0);
    for j in 0..x.len() / 8 {
        acc = Vec::add(acc, Vec::load[u32, 8](in x, wrapping_mul(j, 8)));
    }
    widen_u64(Vec::reduce_add(acc))
}
fn run() -> u64 eff [Isa[Baseline128], Fail[RuntimeError]] {
    let mut a: ChunkSeq[u64] = ChunkSeq::new[u64]();
    let mut b: ChunkSeq[u32] = ChunkSeq::new[u32]();
    for i in 0..32 { a.push(wrapping_mul(i, 7)); b.push(wrapping_u32(wrapping_mul(i, 5))); }
    let wide = if Isa::supported[Avx2]() { sum8(in b.as_slice()) } else { 2480 };
    wrapping_add(wrapping_mul(sum4(in a.as_slice()), 10000), wide)
}
fn main() -> u64 { if Isa::supported[Baseline128]() { handle Fail[RuntimeError] in { run() } with { return x => x, raise(e, k) => 5, } } else { 0 } }
"#;
