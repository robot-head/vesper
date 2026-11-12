//! The scalar domain over-approximates the concrete BPF semantics: for
//! random abstract values and random members of them, every transfer
//! function's result contains the concrete result, and branch refinement
//! never removes a value for which the branch outcome holds.

mod common;

use common::Rng;
use vesper_ebpf::insn::{AluOp, JmpOp};
use vesper_ebpf::interp::{alu_concrete, cmp_concrete};
use vesper_ebpf::scalar::Scalar;
use vesper_ebpf::verifier::{refine_jmp, scalar_alu};

fn interesting(rng: &mut Rng) -> u64 {
    match rng.below(8) {
        0 => 0,
        1 => u64::MAX,
        2 => 1u64 << 63,
        3 => (1u64 << 63) - 1,
        4 => rng.below(256),
        5 => u32::MAX as u64 - rng.below(4),
        6 => (rng.next() as i64 >> rng.below(64)) as u64,
        _ => rng.next(),
    }
}

/// A random abstract value together with a member of it.
fn abstract_with_member(rng: &mut Rng) -> (Scalar, u64) {
    let x = interesting(rng);
    let s = match rng.below(6) {
        0 => Scalar::konst(x),
        1 => Scalar::hull(Scalar::konst(x), Scalar::konst(interesting(rng))),
        2 => {
            let bits = rng.pick(&[8u32, 16, 32, 64]);
            let x = if bits == 64 {
                x
            } else {
                x & ((1u64 << bits) - 1)
            };
            return (Scalar::unknown_bits(bits), x);
        }
        3 => {
            let lo = x.saturating_sub(rng.below(1000));
            let hi = x.saturating_add(rng.below(1000));
            Scalar::urange(lo, hi)
        }
        4 => {
            let xi = x as i64;
            Scalar::srange(
                xi.saturating_sub(rng.below(1000) as i64),
                xi.saturating_add(rng.below(1000) as i64),
            )
        }
        _ => {
            let y = interesting(rng);
            Scalar::hull(
                Scalar::hull(Scalar::konst(x), Scalar::konst(y)),
                Scalar::konst(x ^ y),
            )
        }
    };
    assert!(s.contains(x), "constructor lost its member: {s:?} {x:#x}");
    (s, x)
}

const OPS: [AluOp; 9] = [
    AluOp::Add,
    AluOp::Sub,
    AluOp::Mul,
    AluOp::Or,
    AluOp::And,
    AluOp::Xor,
    AluOp::Lsh,
    AluOp::Rsh,
    AluOp::Arsh,
];

#[test]
fn alu_transfer_functions_contain_concrete_results() {
    let mut rng = Rng(0xdead_beef_cafe_f00d);
    for _ in 0..300_000 {
        let op = rng.pick(&OPS);
        let wide = rng.chance(60);
        let bits = if wide { 64 } else { 32 };
        let (mut a, x) = abstract_with_member(&mut rng);
        let (mut b, mut y) = abstract_with_member(&mut rng);
        if matches!(op, AluOp::Lsh | AluOp::Rsh | AluOp::Arsh) {
            // the verifier only tracks shifts by a known in-range amount
            y = rng.below(bits);
            b = Scalar::konst(y);
        }
        if !wide {
            a = Scalar::truncate(a, 32);
            b = Scalar::truncate(b, 32);
        }
        let mut r = scalar_alu(op, wide, a, b);
        if !wide {
            r = Scalar::zext32(r);
        }
        let c = alu_concrete(op, wide, x, y);
        assert!(
            r.contains(c),
            "{op:?} wide={wide}: {a:?} op {b:?} = {r:?} misses {x:#x} op {y:#x} = {c:#x}"
        );
    }
}

#[test]
fn width_conversions_contain_concrete_results() {
    let mut rng = Rng(0x0123_4567_89ab_cdef);
    for _ in 0..200_000 {
        let (a, x) = abstract_with_member(&mut rng);
        let bits = rng.pick(&[8u32, 16, 32]);
        let mask = (1u64 << bits) - 1;
        let t = Scalar::truncate(a, bits);
        assert!(
            t.contains(x & mask),
            "truncate {bits}: {a:?} -> {t:?} misses {x:#x}"
        );
        let se = Scalar::sext(a, bits);
        let sx = vesper_ebpf::scalar::sext_const(x & mask, bits);
        assert!(
            se.contains(sx),
            "sext {bits}: {a:?} -> {se:?} misses {sx:#x}"
        );
        let z = Scalar::zext32(a);
        assert!(
            z.contains(x as u32 as u64),
            "zext32: {a:?} -> {z:?} misses {x:#x}"
        );
    }
}

const JMPS: [JmpOp; 11] = [
    JmpOp::Eq,
    JmpOp::Gt,
    JmpOp::Ge,
    JmpOp::Set,
    JmpOp::Ne,
    JmpOp::SGt,
    JmpOp::SGe,
    JmpOp::Lt,
    JmpOp::Le,
    JmpOp::SLt,
    JmpOp::SLe,
];

#[test]
fn branch_refinement_keeps_every_value_on_its_branch() {
    let mut rng = Rng(0xfeed_face_0bad_c0de);
    for _ in 0..300_000 {
        let op = rng.pick(&JMPS);
        let wide = rng.chance(60);
        let (a, x) = abstract_with_member(&mut rng);
        let (b, y) = if rng.chance(30) {
            let (b, y) = abstract_with_member(&mut rng);
            (b, y)
        } else {
            // a constant operand, often equal to or adjacent to x
            let y = match rng.below(4) {
                0 => x,
                1 => x.wrapping_add(1),
                2 => x.wrapping_sub(1),
                _ => interesting(&mut rng),
            };
            (Scalar::konst(y), y)
        };
        let holds = cmp_concrete(op, wide, x, y);
        match refine_jmp(op, wide, a, b, holds) {
            Some((ra, rb)) => assert!(
                ra.contains(x) && rb.contains(y),
                "{op:?} wide={wide} holds={holds}: {a:?},{b:?} -> {ra:?},{rb:?} loses {x:#x},{y:#x}"
            ),
            None => panic!(
                "{op:?} wide={wide} holds={holds}: {a:?},{b:?} declared infeasible for {x:#x},{y:#x}"
            ),
        }
    }
}
