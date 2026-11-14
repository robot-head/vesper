//! The AArch64 encoding of each abstract machine instruction.
//!
//! `R0`, `R1`, `R2` are `x0`, `x1`, `x2`; frame words are `[sp, #8n]` (slots
//! and the outgoing area) and `[x29, #16 + 8n]` (the incoming area). `x18`
//! (reserved on Apple platforms) is never used. These encodings are tested by
//! running the emitted programs; they are not covered by the Lean theorem.
use super::asm::{Asm, Fix};
use super::machine::{Cond, Label, Loc, M, Reg, Sign, TrapKind, WrapOp};
use super::{Os, ResultProtocol, VerifiedError};

const SP: u32 = 31;
const FP: u32 = 29;
const XZR: u32 = 31;

fn reg(r: Reg) -> u32 {
    match r {
        Reg::R0 => 0,
        Reg::R1 => 1,
        Reg::R2 => 2,
    }
}
// Condition codes.
const EQ: u32 = 0;
const NE: u32 = 1;
const HS: u32 = 2;
const LO: u32 = 3;
const VS: u32 = 6;
const HI: u32 = 8;
const LS: u32 = 9;
const GE: u32 = 10;
const LT: u32 = 11;
const GT: u32 = 12;
const LE: u32 = 13;

fn ldst(a: &mut Asm, load: bool, r: Reg, loc: Loc) {
    let (base, offset) = match loc {
        Loc::Slot(n) | Loc::Out(n) => (SP, n * 8),
        Loc::In(n) => (FP, 16 + n * 8),
    };
    let op = if load { 0xf940_0000 } else { 0xf900_0000 };
    a.u32(op | (offset / 8) << 10 | base << 5 | reg(r));
}
/// `movz`/`movk` the 64-bit immediate (always four instructions).
fn imm(a: &mut Asm, rd: u32, w: u64) {
    for hw in 0..4u32 {
        let part = ((w >> (16 * hw)) & 0xffff) as u32;
        let op = if hw == 0 { 0xd280_0000 } else { 0xf280_0000 };
        a.u32(op | hw << 21 | part << 5 | rd);
    }
}
/// `b.cond label`.
fn bcond(a: &mut Asm, cond: u32, l: Label) {
    let at = a.here();
    a.u32(0x5400_0000 | cond);
    a.fixup(at, l, Fix::Imm19);
}
fn branch(a: &mut Asm, op: u32, l: Label) {
    let at = a.here();
    a.u32(op);
    a.fixup(at, l, Fix::Imm26);
}
/// `cbz`/`cbnz xt, label`.
fn cb(a: &mut Asm, nonzero: bool, rt: u32, l: Label) {
    let at = a.here();
    a.u32(if nonzero { 0xb500_0000 } else { 0xb400_0000 } | rt);
    a.fixup(at, l, Fix::Imm19);
}
/// Data-processing (shifted register) `op xd, xn, xm`.
fn rrr(a: &mut Asm, op: u32, d: u32, n: u32, m: u32) {
    a.u32(op | m << 16 | n << 5 | d);
}
const ADD: u32 = 0x8b00_0000;
const ADDS: u32 = 0xab00_0000;
const SUB: u32 = 0xcb00_0000;
const SUBS: u32 = 0xeb00_0000;
const AND: u32 = 0x8a00_0000;
/// `madd xd, xn, xm, xzr` = `mul`.
const MUL: u32 = 0x9b00_7c00;
const SMULH: u32 = 0x9b40_7c00;
const UMULH: u32 = 0x9bc0_7c00;
const SDIV: u32 = 0x9ac0_0c00;
const UDIV: u32 = 0x9ac0_0800;
/// `msub xd, xn, xm, xa`: `xd = xa - xn * xm`.
fn msub(a: &mut Asm, d: u32, n: u32, m: u32, acc: u32) {
    a.u32(0x9b00_8000 | m << 16 | acc << 10 | n << 5 | d);
}
/// `sub sp, sp, #bytes` (bytes < 2^24).
fn sub_sp(a: &mut Asm, bytes: u32) {
    if bytes >> 12 != 0 {
        a.u32(0xd140_0000 | (bytes >> 12) << 10 | SP << 5 | SP);
    }
    if bytes & 0xfff != 0 || bytes == 0 {
        a.u32(0xd100_0000 | (bytes & 0xfff) << 10 | SP << 5 | SP);
    }
}

pub fn emit(a: &mut Asm, m: &M) -> Result<(), VerifiedError> {
    let ovf = TrapKind::Overflow.label();
    match *m {
        M::Ld(r, l) => ldst(a, true, r, l),
        M::St(l, r) => ldst(a, false, r, l),
        M::Imm(r, w) => imm(a, reg(r), w),
        // adds x0, x0, x1; b.vs / b.hs
        M::AddChk(s) => {
            rrr(a, ADDS, 0, 0, 1);
            bcond(a, if s == Sign::S { VS } else { HS }, ovf);
        }
        // subs x0, x0, x1; b.vs / b.lo (borrow)
        M::SubChk(s) => {
            rrr(a, SUBS, 0, 0, 1);
            bcond(a, if s == Sign::S { VS } else { LO }, ovf);
        }
        // smulh x2, x0, x1; mul x0, x0, x1; cmp x2, x0, asr #63; b.ne
        M::MulChk(Sign::S) => {
            rrr(a, SMULH, 2, 0, 1);
            rrr(a, MUL, 0, 0, 1);
            a.u32(SUBS | 2 << 22 | 63 << 10 | 2 << 5 | XZR);
            bcond(a, NE, ovf);
        }
        // umulh x2, x0, x1; mul x0, x0, x1; cbnz x2
        M::MulChk(Sign::U) => {
            rrr(a, UMULH, 2, 0, 1);
            rrr(a, MUL, 0, 0, 1);
            cb(a, true, 2, ovf);
        }
        M::Wrap(WrapOp::Add) => rrr(a, ADD, 0, 0, 1),
        M::Wrap(WrapOp::Sub) => rrr(a, SUB, 0, 0, 1),
        M::Wrap(WrapOp::Mul) => rrr(a, MUL, 0, 0, 1),
        M::AndR2 => rrr(a, AND, 0, 0, 2),
        // cmp x0, x2; b.hi
        M::TrapGtU => {
            rrr(a, SUBS, XZR, 0, 2);
            bcond(a, HI, ovf);
        }
        M::TrapZero => cb(a, false, 1, TrapKind::DivisionByZero.label()),
        // movz x2, #0x8000, lsl #48; cmp x0, x2; b.ne +3; cmn x1, #1; b.eq ovf
        M::TrapDivOvf => {
            a.u32(0xd280_0000 | 3 << 21 | 0x8000 << 5 | 2);
            rrr(a, SUBS, XZR, 0, 2);
            a.u32(0x5400_0000 | 3 << 5 | NE);
            a.u32(0xb100_041f | 1 << 5);
            bcond(a, EQ, ovf);
        }
        M::Div(Sign::S) => rrr(a, SDIV, 0, 0, 1),
        M::Div(Sign::U) => rrr(a, UDIV, 0, 0, 1),
        // sdiv x2, x0, x1; msub x0, x2, x1, x0 (MIN / -1 does not trap here
        // and the remainder is 0, as required)
        M::Rem(s) => {
            rrr(a, if s == Sign::S { SDIV } else { UDIV }, 2, 0, 1);
            msub(a, 0, 2, 1, 0);
        }
        // negs x0, x0; b.vs  |  cbnz x0
        M::NegChk(Sign::S) => {
            rrr(a, SUBS, 0, XZR, 0);
            bcond(a, VS, ovf);
        }
        M::NegChk(Sign::U) => cb(a, true, 0, ovf),
        // eor x0, x0, #1
        M::XorOne => a.u32(0xd240_0000),
        // cmp x0, x1; cset x0, cond (= csinc x0, xzr, xzr, !cond)
        M::Cmp(c, s) => {
            let cond = match (c, s) {
                (Cond::Eq, _) => EQ,
                (Cond::Ne, _) => NE,
                (Cond::Lt, Sign::S) => LT,
                (Cond::Le, Sign::S) => LE,
                (Cond::Gt, Sign::S) => GT,
                (Cond::Ge, Sign::S) => GE,
                (Cond::Lt, Sign::U) => LO,
                (Cond::Le, Sign::U) => LS,
                (Cond::Gt, Sign::U) => HI,
                (Cond::Ge, Sign::U) => HS,
            };
            rrr(a, SUBS, XZR, 0, 1);
            a.u32(0x9a9f_07e0 | (cond ^ 1) << 12);
        }
        M::Label(l) => a.bind(l)?,
        M::Jmp(l) => branch(a, 0x1400_0000, l),
        M::Bz(l) => cb(a, false, 0, l),
        M::Call(l) => branch(a, 0x9400_0000, l),
        // stp x29, x30, [sp, #-16]!; mov x29, sp; sub sp, sp, #bytes
        M::Enter(words) => {
            a.u32(0xa9bf_7bfd);
            a.u32(0x9100_03fd);
            sub_sp(a, words * 8);
        }
        // mov sp, x29; ldp x29, x30, [sp], #16; ret
        M::Leave => {
            a.u32(0x9100_03bf);
            a.u32(0xa8c1_7bfd);
            a.u32(0xd65f_03c0);
        }
        M::Trap(k) => branch(a, 0x1400_0000, k.label()),
    }
    Ok(())
}

/// `svc` with the system-call number in `x8` (Linux) or `x16` (macOS).
fn syscall(a: &mut Asm, os: Os, linux: u32, macos: u32) {
    match os {
        Os::Linux => {
            a.u32(0xd280_0000 | linux << 5 | 8);
            a.u32(0xd400_0001);
        }
        Os::MacOs => {
            a.u32(0xd280_0000 | macos << 5 | 16);
            a.u32(0xd400_1001);
        }
    }
}

/// The entry: `_start` (Linux, no libc) or the C `_main` (macOS, linked by
/// the system linker). Reserves the result area, calls the entry, optionally
/// writes the result words to stdout and exits or returns the status.
pub fn start(
    a: &mut Asm,
    os: Os,
    protocol: ResultProtocol,
    result_words: u32,
    entry: Label,
) -> Result<(), VerifiedError> {
    let area = ((result_words.max(1) + 1) & !1) * 8;
    if os == Os::MacOs {
        // stp x29, x30, [sp, #-16]!; mov x29, sp
        a.u32(0xa9bf_7bfd);
        a.u32(0x9100_03fd);
    }
    sub_sp(a, area);
    branch(a, 0x9400_0000, entry);
    if protocol == ResultProtocol::Words && result_words > 0 {
        // mov x0, #1; mov x1, sp; mov x2, #len; write
        a.u32(0xd280_0020);
        a.u32(0x9100_03e1);
        imm(a, 2, u64::from(result_words) * 8);
        syscall(a, os, 64, 4);
    }
    if protocol == ResultProtocol::ExitStatus && result_words > 0 {
        // ldr x0, [sp]
        a.u32(0xf940_03e0);
    } else {
        // mov x0, #0
        a.u32(0xd280_0000);
    }
    match os {
        Os::Linux => syscall(a, os, 93, 1),
        Os::MacOs => {
            // mov sp, x29; ldp x29, x30, [sp], #16; ret
            a.u32(0x9100_03bf);
            a.u32(0xa8c1_7bfd);
            a.u32(0xd65f_03c0);
        }
    }
    for k in TrapKind::ALL {
        a.bind(k.label())?;
        // mov x0, #status; exit
        a.u32(0xd280_0000 | u32::from(k.exit_status()) << 5);
        syscall(a, os, 93, 1);
    }
    Ok(())
}
