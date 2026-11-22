//! The port's scalar domain against the Rust build, result for result:
//! `normalize` of raw component tuples, every ALU transfer function at both
//! widths, every branch refinement on both outcomes, and `meet`, over random
//! scalars of every shape. Rust-exit lane 10 (#1405).
#![cfg(feature = "vesper-port")]

mod common;

use common::Rng;
use vesper_ebpf::insn::{AluOp, JmpOp};
use vesper_ebpf::scalar::{Scalar, Tnum};
use vesper_ebpf::verifier::{refine_jmp, scalar_alu};

use vesper_ebpf_rust as rust;

/// The binary operators: `Mov`, `MovSx` and `Neg` are the caller's.
const ALU: [(AluOp, rust::insn::AluOp); 13] = [
    (AluOp::Add, rust::insn::AluOp::Add),
    (AluOp::Sub, rust::insn::AluOp::Sub),
    (AluOp::Mul, rust::insn::AluOp::Mul),
    (AluOp::Div, rust::insn::AluOp::Div),
    (AluOp::SDiv, rust::insn::AluOp::SDiv),
    (AluOp::Or, rust::insn::AluOp::Or),
    (AluOp::And, rust::insn::AluOp::And),
    (AluOp::Lsh, rust::insn::AluOp::Lsh),
    (AluOp::Rsh, rust::insn::AluOp::Rsh),
    (AluOp::Mod, rust::insn::AluOp::Mod),
    (AluOp::SMod, rust::insn::AluOp::SMod),
    (AluOp::Xor, rust::insn::AluOp::Xor),
    (AluOp::Arsh, rust::insn::AluOp::Arsh),
];

const JMP: [(JmpOp, rust::insn::JmpOp); 11] = [
    (JmpOp::Eq, rust::insn::JmpOp::Eq),
    (JmpOp::Gt, rust::insn::JmpOp::Gt),
    (JmpOp::Ge, rust::insn::JmpOp::Ge),
    (JmpOp::Set, rust::insn::JmpOp::Set),
    (JmpOp::Ne, rust::insn::JmpOp::Ne),
    (JmpOp::SGt, rust::insn::JmpOp::SGt),
    (JmpOp::SGe, rust::insn::JmpOp::SGe),
    (JmpOp::Lt, rust::insn::JmpOp::Lt),
    (JmpOp::Le, rust::insn::JmpOp::Le),
    (JmpOp::SLt, rust::insn::JmpOp::SLt),
    (JmpOp::SLe, rust::insn::JmpOp::SLe),
];

fn to_rust(s: Scalar) -> rust::scalar::Scalar {
    rust::scalar::Scalar {
        umin: s.umin,
        umax: s.umax,
        smin: s.smin,
        smax: s.smax,
        var: rust::scalar::Tnum {
            value: s.var.value,
            mask: s.var.mask,
        },
    }
}

fn same(p: Scalar, r: rust::scalar::Scalar) -> bool {
    p.umin == r.umin
        && p.umax == r.umax
        && p.smin == r.smin
        && p.smax == r.smax
        && p.var.value == r.var.value
        && p.var.mask == r.var.mask
}

/// A small or boundary-heavy word, so that ranges meet and cross.
fn word(rng: &mut Rng) -> u64 {
    match rng.below(5) {
        0 => rng.below(64),
        1 => u64::MAX - rng.below(64),
        2 => (1u64 << 63).wrapping_add(rng.below(64)).wrapping_sub(32),
        3 => rng.next() >> rng.below(64),
        _ => rng.next(),
    }
}

fn scalar(rng: &mut Rng) -> Scalar {
    let (x, y) = (word(rng), word(rng));
    match rng.below(5) {
        0 => Scalar::konst(x),
        1 => Scalar::urange(x.min(y), x.max(y)),
        2 => Scalar::srange((x as i64).min(y as i64), (x as i64).max(y as i64)),
        3 => Scalar::unknown_bits(rng.below(65) as u32),
        _ => Scalar::UNKNOWN,
    }
}

/// Raw components, not yet reduced against each other.
fn raw(rng: &mut Rng) -> Scalar {
    let (a, b, c, d) = (word(rng), word(rng), word(rng), word(rng));
    let mask = word(rng);
    Scalar {
        umin: a.min(b),
        umax: a.max(b),
        smin: (c as i64).min(d as i64),
        smax: (c as i64).max(d as i64),
        var: Tnum {
            value: word(rng) & !mask,
            mask,
        },
    }
}

#[test]
fn port_matches_rust_on_normalize() {
    let mut rng = Rng(0x5ca1_ab1e_0dd5_eed5);
    for case in 0..20_000 {
        let s = raw(&mut rng);
        let p = s.normalize();
        let r = to_rust(s).normalize();
        match (p, r) {
            (None, None) => {}
            (Some(p), Some(r)) if same(p, r) => {}
            _ => panic!("case {case}: normalize({s:?}): port {p:?}, rust {r:?}"),
        }
    }
}

#[test]
fn port_matches_rust_on_transfer_functions() {
    let mut rng = Rng(0x0dd_ba11_cafe_f00d);
    for case in 0..20_000 {
        let (a, b) = (scalar(&mut rng), scalar(&mut rng));
        let (ra, rb) = (to_rust(a), to_rust(b));
        let wide = rng.below(2) == 0;
        let (op, rop) = ALU[rng.below(ALU.len() as u64) as usize];
        let p = scalar_alu(op, wide, a, b);
        let r = rust::verifier::scalar_alu(rop, wide, ra, rb);
        assert!(
            same(p, r),
            "case {case}: {op:?} wide={wide} {a:?} {b:?}: port {p:?}, rust {r:?}"
        );
        let (op, rop) = JMP[rng.below(JMP.len() as u64) as usize];
        let holds = rng.below(2) == 0;
        let p = refine_jmp(op, wide, a, b, holds);
        let r = rust::verifier::refine_jmp(rop, wide, ra, rb, holds);
        match (p, r) {
            (None, None) => {}
            (Some((p0, p1)), Some((r0, r1))) if same(p0, r0) && same(p1, r1) => {}
            _ => panic!("case {case}: {op:?} wide={wide} holds={holds}: port {p:?}, rust {r:?}"),
        }
        let p = Scalar::meet(a, b);
        let r = rust::scalar::Scalar::meet(ra, rb);
        match (p, r) {
            (None, None) => {}
            (Some(p), Some(r)) if same(p, r) => {}
            _ => panic!("case {case}: meet({a:?}, {b:?}): port {p:?}, rust {r:?}"),
        }
    }
}
