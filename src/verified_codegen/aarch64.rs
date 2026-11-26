//! The AArch64 encoding of each abstract machine instruction.
//!
//! `R0`, `R1`, `R2` are `x0`, `x1`, `x2`; frame words are `[sp, #8n]` (slots
//! and the outgoing area) and `[x29, #16 + 8n]` (the incoming area). `x18`
//! (reserved on Apple platforms) is never used. These encodings are tested by
//! running the emitted programs, and `lean/CodegenA64Encoding.lean` proves a
//! Lean model of `emit` against the A64 encoding diagrams; its listing must
//! equal [`encoding_listing`] (`tests/verified_codegen_lean.rs`).
use super::asm::{Asm, Fix};
use super::machine::{Cond, FloatOp, Label, Loc, M, Reg, Sign, TrapKind, WrapOp};
use super::{Os, ResultProtocol, VerifiedError};

const SP: u32 = 31;
const FP: u32 = 29;
const XZR: u32 = 31;
const X0: u32 = 0;
const X1: u32 = 1;
const X2: u32 = 2;
/// The Linux system-call number register.
const X8: u32 = 8;
/// `ip0`: the heap address scratch, and the macOS system-call number.
const X16: u32 = 16;

fn reg(r: Reg) -> u32 {
    match r {
        Reg::R0 => X0,
        Reg::R1 => X1,
        Reg::R2 => X2,
    }
}

// Field positions in the instruction word (Arm ARM encoding diagrams).
const RN_SHIFT: u32 = 5;
const RM_SHIFT: u32 = 16;
const RA_SHIFT: u32 = 10;
const IMM6_SHIFT: u32 = 10;
const IMM12_SHIFT: u32 = 10;
const IMM16_SHIFT: u32 = 5;
const IMM19_SHIFT: u32 = 5;
const HW_SHIFT: u32 = 21;
/// The shift type of a shifted-register operand.
const SHIFT_TYPE_SHIFT: u32 = 22;
/// The condition of `csinc`.
const COND_SHIFT: u32 = 12;
/// The shift type `asr`.
const SHIFT_ASR: u32 = 2;
/// Bits in an add/sub immediate (`imm12`, optionally `lsl #12`).
const IMM12_BITS: u32 = 12;
const IMM12_MASK: u32 = 0xfff;

/// Bytes per frame or heap word.
const WORD_BYTES: u32 = 8;
/// log2 of [`WORD_BYTES`].
const WORD_SHIFT: u32 = 3;
/// The incoming area's offset from `x29` (past the saved `x29`, `x30`).
const INCOMING_OFFSET: u32 = 16;
/// The sign bit of a 64-bit word.
const SIGN_BIT: u32 = 63;

// Condition codes.
const EQ: u32 = 0;
const NE: u32 = 1;
const HS: u32 = 2;
const LO: u32 = 3;
const MI: u32 = 4;
const VS: u32 = 6;
const HI: u32 = 8;
const LS: u32 = 9;
const GE: u32 = 10;
const LT: u32 = 11;
const GT: u32 = 12;
const LE: u32 = 13;

fn ldst(a: &mut Asm, load: bool, r: Reg, loc: Loc) {
    let (base, offset) = match loc {
        Loc::Slot(n) | Loc::Out(n) => (SP, n * WORD_BYTES),
        Loc::In(n) => (FP, INCOMING_OFFSET + n * WORD_BYTES),
    };
    let op = if load { LDR_X } else { STR_X };
    a.u32(op | (offset / WORD_BYTES) << IMM12_SHIFT | base << RN_SHIFT | reg(r));
}
/// `ldr xt, [xn, #imm12 * 8]`.
const LDR_X: u32 = 0xf940_0000;
/// `str xt, [xn, #imm12 * 8]`.
const STR_X: u32 = 0xf900_0000;
/// `movz xd, #imm16, lsl #(16 * hw)`.
const MOVZ: u32 = 0xd280_0000;
/// `movk xd, #imm16, lsl #(16 * hw)`.
const MOVK: u32 = 0xf280_0000;
/// `movz`/`movk` the 64-bit immediate (always four instructions).
fn imm(a: &mut Asm, rd: u32, w: u64) {
    for hw in 0..4u32 {
        let part = ((w >> (16 * hw)) & 0xffff) as u32;
        let op = if hw == 0 { MOVZ } else { MOVK };
        a.u32(op | hw << HW_SHIFT | part << IMM16_SHIFT | rd);
    }
}
/// `b.cond label`.
fn bcond(a: &mut Asm, cond: u32, l: Label) {
    let at = a.here();
    a.u32(B_COND | cond);
    a.fixup(at, l, Fix::Imm19);
}
/// `b.cond`.
const B_COND: u32 = 0x5400_0000;
/// `b`.
const B: u32 = 0x1400_0000;
/// `bl`.
const BL: u32 = 0x9400_0000;
/// `cbz xt`.
const CBZ: u32 = 0xb400_0000;
/// `cbnz xt`.
const CBNZ: u32 = 0xb500_0000;
fn branch(a: &mut Asm, op: u32, l: Label) {
    let at = a.here();
    a.u32(op);
    a.fixup(at, l, Fix::Imm26);
}
/// `cbz`/`cbnz xt, label`.
fn cb(a: &mut Asm, nonzero: bool, rt: u32, l: Label) {
    let at = a.here();
    a.u32(if nonzero { CBNZ } else { CBZ } | rt);
    a.fixup(at, l, Fix::Imm19);
}
/// Data-processing (shifted register) `op xd, xn, xm`.
fn rrr(a: &mut Asm, op: u32, d: u32, n: u32, m: u32) {
    a.u32(op | m << RM_SHIFT | n << RN_SHIFT | d);
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
    a.u32(MSUB | m << RM_SHIFT | acc << RA_SHIFT | n << RN_SHIFT | d);
}
const MSUB: u32 = 0x9b00_8000;
/// `sub xd, xn, #imm12`.
const SUB_IMM: u32 = 0xd100_0000;
/// `sub xd, xn, #imm12, lsl #12`.
const SUB_IMM_LSL12: u32 = 0xd140_0000;
/// `sub sp, sp, #bytes` (bytes < 2^24).
fn sub_sp(a: &mut Asm, bytes: u32) {
    if bytes >> IMM12_BITS != 0 {
        a.u32(SUB_IMM_LSL12 | (bytes >> IMM12_BITS) << IMM12_SHIFT | SP << RN_SHIFT | SP);
    }
    if bytes & IMM12_MASK != 0 || bytes == 0 {
        a.u32(SUB_IMM | (bytes & IMM12_MASK) << IMM12_SHIFT | SP << RN_SHIFT | SP);
    }
}

/// `cmn xn, #1` (`adds xzr, xn, #1`), without its `Rn`.
const CMN_ONE: u32 = 0xb100_041f;
/// `eor x0, x0, #1`.
const EOR_X0_X0_1: u32 = 0xd240_0000;
/// `csinc x0, xzr, xzr, cond` (`cset x0, !cond`), without its condition.
const CSINC_X0_XZR_XZR: u32 = 0x9a9f_07e0;
/// `fmov d0, x0`.
const FMOV_D0_X0: u32 = 0x9e67_0000;
/// `fmov d1, x1`.
const FMOV_D1_X1: u32 = 0x9e67_0021;
/// `fmov x0, d0`.
const FMOV_X0_D0: u32 = 0x9e66_0000;
/// `fadd d0, d0, d1`.
const FADD_D0_D0_D1: u32 = 0x1e61_2800;
/// `fsub d0, d0, d1`.
const FSUB_D0_D0_D1: u32 = 0x1e61_3800;
/// `fmul d0, d0, d1`.
const FMUL_D0_D0_D1: u32 = 0x1e61_0800;
/// `fdiv d0, d0, d1`.
const FDIV_D0_D0_D1: u32 = 0x1e61_1800;
/// `fneg d0, d0`.
const FNEG_D0_D0: u32 = 0x1e61_4000;
/// `fcmp d0, d1`.
const FCMP_D0_D1: u32 = 0x1e61_2000;
/// `stp x29, x30, [sp, #-16]!`.
const STP_FP_LR_PUSH: u32 = 0xa9bf_7bfd;
/// `ldp x29, x30, [sp], #16`.
const LDP_FP_LR_POP: u32 = 0xa8c1_7bfd;
/// `mov x29, sp`.
const MOV_FP_SP: u32 = 0x9100_03fd;
/// `mov sp, x29`.
const MOV_SP_FP: u32 = 0x9100_03bf;
/// `mov x1, sp`.
const MOV_X1_SP: u32 = 0x9100_03e1;
/// `ret`.
const RET: u32 = 0xd65f_03c0;
/// `svc #0` (Linux).
const SVC_0: u32 = 0xd400_0001;
/// `svc #0x80` (macOS).
const SVC_0X80: u32 = 0xd400_1001;

// System-call numbers (Linux, macOS) and the standard output descriptor.
const SYS_WRITE_LINUX: u32 = 64;
const SYS_WRITE_MACOS: u32 = 4;
const SYS_EXIT_LINUX: u32 = 93;
const SYS_EXIT_MACOS: u32 = 1;
const STDOUT: u32 = 1;

pub fn emit(a: &mut Asm, m: &M) -> Result<(), VerifiedError> {
    let ovf = a.trap_label(TrapKind::Overflow);
    match *m {
        M::Ld(r, l) => ldst(a, true, r, l),
        M::St(l, r) => ldst(a, false, r, l),
        M::Imm(r, w) => imm(a, reg(r), w),
        // adds x0, x0, x1; b.vs / b.hs
        M::AddChk(s) => {
            rrr(a, ADDS, X0, X0, X1);
            bcond(a, if s == Sign::S { VS } else { HS }, ovf);
        }
        // subs x0, x0, x1; b.vs / b.lo (borrow)
        M::SubChk(s) => {
            rrr(a, SUBS, X0, X0, X1);
            bcond(a, if s == Sign::S { VS } else { LO }, ovf);
        }
        // smulh x2, x0, x1; mul x0, x0, x1; cmp x2, x0, asr #63; b.ne
        M::MulChk(Sign::S) => {
            rrr(a, SMULH, X2, X0, X1);
            rrr(a, MUL, X0, X0, X1);
            a.u32(
                SUBS | SHIFT_ASR << SHIFT_TYPE_SHIFT
                    | SIGN_BIT << IMM6_SHIFT
                    | X2 << RN_SHIFT
                    | XZR,
            );
            bcond(a, NE, ovf);
        }
        // umulh x2, x0, x1; mul x0, x0, x1; cbnz x2
        M::MulChk(Sign::U) => {
            rrr(a, UMULH, X2, X0, X1);
            rrr(a, MUL, X0, X0, X1);
            cb(a, true, X2, ovf);
        }
        M::Wrap(WrapOp::Add) => rrr(a, ADD, X0, X0, X1),
        M::Wrap(WrapOp::Sub) => rrr(a, SUB, X0, X0, X1),
        M::Wrap(WrapOp::Mul) => rrr(a, MUL, X0, X0, X1),
        M::AndR2 => rrr(a, AND, X0, X0, X2),
        // cmp x0, x2; b.hi
        M::TrapGtU => {
            rrr(a, SUBS, XZR, X0, X2);
            bcond(a, HI, ovf);
        }
        M::TrapZero => {
            let l = a.trap_label(TrapKind::DivisionByZero);
            cb(a, false, X1, l)
        }
        // movz x2, #0x8000, lsl #48; cmp x0, x2; b.ne +3; cmn x1, #1; b.eq ovf
        M::TrapDivOvf => {
            a.u32(MOVZ | 3 << HW_SHIFT | 0x8000 << IMM16_SHIFT | X2);
            rrr(a, SUBS, XZR, X0, X2);
            a.u32(B_COND | 3 << IMM19_SHIFT | NE);
            a.u32(CMN_ONE | X1 << RN_SHIFT);
            bcond(a, EQ, ovf);
        }
        M::Div(Sign::S) => rrr(a, SDIV, X0, X0, X1),
        M::Div(Sign::U) => rrr(a, UDIV, X0, X0, X1),
        // sdiv x2, x0, x1; msub x0, x2, x1, x0 (MIN / -1 does not trap here
        // and the remainder is 0, as required)
        M::Rem(s) => {
            rrr(a, if s == Sign::S { SDIV } else { UDIV }, X2, X0, X1);
            msub(a, X0, X2, X1, X0);
        }
        // negs x0, x0; b.vs  |  cbnz x0
        M::NegChk(Sign::S) => {
            rrr(a, SUBS, X0, XZR, X0);
            bcond(a, VS, ovf);
        }
        M::NegChk(Sign::U) => cb(a, true, X0, ovf),
        // eor x0, x0, #1
        M::XorOne => a.u32(EOR_X0_X0_1),
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
            rrr(a, SUBS, XZR, X0, X1);
            a.u32(CSINC_X0_XZR_XZR | (cond ^ 1) << COND_SHIFT);
        }
        // fmov d0, x0; fmov d1, x1; f<op> d0, d0, d1; fmov x0, d0
        M::FOp(op) => {
            let opcode = match op {
                FloatOp::Add => FADD_D0_D0_D1,
                FloatOp::Sub => FSUB_D0_D0_D1,
                FloatOp::Mul => FMUL_D0_D0_D1,
                FloatOp::Div => FDIV_D0_D0_D1,
            };
            a.u32(FMOV_D0_X0);
            a.u32(FMOV_D1_X1);
            a.u32(opcode);
            a.u32(FMOV_X0_D0);
        }
        // fmov d0, x0; fneg d0, d0; fmov x0, d0
        M::FNeg => {
            a.u32(FMOV_D0_X0);
            a.u32(FNEG_D0_D0);
            a.u32(FMOV_X0_D0);
        }
        // fmov d0, x0; fmov d1, x1; fcmp d0, d1; cset x0, cond. An unordered
        // pair sets C and V, so MI, LS, GT, GE and EQ are false and NE true.
        M::FCmp(c) => {
            let cond = match c {
                Cond::Eq => EQ,
                Cond::Ne => NE,
                Cond::Lt => MI,
                Cond::Le => LS,
                Cond::Gt => GT,
                Cond::Ge => GE,
            };
            a.u32(FMOV_D0_X0);
            a.u32(FMOV_D1_X1);
            a.u32(FCMP_D0_D1);
            a.u32(CSINC_X0_XZR_XZR | (cond ^ 1) << COND_SHIFT);
        }
        // movz x16, #HEAP_BASE >> 16, lsl #16; add x16, x16, x1, lsl #3;
        // ldr x0, [x16, #8*off] (str x0, ...).
        M::LdH(off) | M::StH(off) => {
            a.u32(MOVZ | 1 << HW_SHIFT | ((super::HEAP_BASE >> 16) as u32) << IMM16_SHIFT | X16);
            a.u32(ADD | X1 << RM_SHIFT | WORD_SHIFT << IMM6_SHIFT | X16 << RN_SHIFT | X16);
            let op = if matches!(m, M::LdH(_)) { LDR_X } else { STR_X };
            a.u32(op | off << IMM12_SHIFT | X16 << RN_SHIFT);
        }
        M::Label(l) => a.bind(l)?,
        M::Jmp(l) => branch(a, B, l),
        M::Bz(l) => cb(a, false, X0, l),
        M::Call(l) => branch(a, BL, l),
        // stp x29, x30, [sp, #-16]!; mov x29, sp; sub sp, sp, #bytes
        M::Enter(words) => {
            a.u32(STP_FP_LR_PUSH);
            a.u32(MOV_FP_SP);
            sub_sp(a, words * WORD_BYTES);
        }
        // mov sp, x29; ldp x29, x30, [sp], #16; ret
        M::Leave => {
            a.u32(MOV_SP_FP);
            a.u32(LDP_FP_LR_POP);
            a.u32(RET);
        }
        M::Trap(k) => {
            let l = a.trap_label(k);
            branch(a, B, l)
        }
        M::Scope(ovf, div0) => a.set_scope(ovf, div0),
    }
    Ok(())
}

/// The words `emit` writes for the shared samples plus two frame sizes that
/// need the shifted `sub sp` form, in the format
/// `lean/CodegenA64Encoding.lean` prints.
pub fn encoding_listing() -> String {
    let mut samples = super::asm::encoding_samples();
    let at = samples
        .iter()
        .position(|m| *m == M::Enter(300))
        .expect("the samples hold enter 300");
    samples.splice(at + 1..at + 1, [M::Enter(1000), M::Enter(512)]);
    super::asm::encoding_listing(&samples, emit)
}

/// `svc` with the system-call number in `x8` (Linux) or `x16` (macOS).
fn syscall(a: &mut Asm, os: Os, linux: u32, macos: u32) {
    match os {
        // Windows AArch64 is rejected before code generation.
        Os::Linux | Os::Windows => {
            a.u32(MOVZ | linux << IMM16_SHIFT | X8);
            a.u32(SVC_0);
        }
        Os::MacOs => {
            a.u32(MOVZ | macos << IMM16_SHIFT | X16);
            a.u32(SVC_0X80);
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
    let area = ((result_words.max(1) + 1) & !1) * WORD_BYTES;
    if os == Os::MacOs {
        // stp x29, x30, [sp, #-16]!; mov x29, sp
        a.u32(STP_FP_LR_PUSH);
        a.u32(MOV_FP_SP);
    }
    sub_sp(a, area);
    branch(a, BL, entry);
    if protocol == ResultProtocol::Words && result_words > 0 {
        // mov x0, #1; mov x1, sp; mov x2, #len; write
        a.u32(MOVZ | STDOUT << IMM16_SHIFT | X0);
        a.u32(MOV_X1_SP);
        imm(a, X2, u64::from(result_words) * u64::from(WORD_BYTES));
        syscall(a, os, SYS_WRITE_LINUX, SYS_WRITE_MACOS);
    }
    if protocol == ResultProtocol::ExitStatus && result_words > 0 {
        // ldr x0, [sp]
        a.u32(LDR_X | SP << RN_SHIFT | X0);
    } else {
        // mov x0, #0
        a.u32(MOVZ | X0);
    }
    match os {
        Os::Linux | Os::Windows => syscall(a, os, SYS_EXIT_LINUX, SYS_EXIT_MACOS),
        Os::MacOs => {
            // mov sp, x29; ldp x29, x30, [sp], #16; ret
            a.u32(MOV_SP_FP);
            a.u32(LDP_FP_LR_POP);
            a.u32(RET);
        }
    }
    for k in TrapKind::ALL {
        a.bind(k.label())?;
        exit(a, os, k);
    }
    Ok(())
}

fn exit(a: &mut Asm, os: Os, k: TrapKind) {
    // mov x0, #status; exit
    a.u32(MOVZ | u32::from(k.exit_status()) << IMM16_SHIFT | X0);
    syscall(a, os, SYS_EXIT_LINUX, SYS_EXIT_MACOS);
}

/// Another copy of the abort exits, between functions: `cbz` and `b.cond`
/// reach only ±1 MiB, so a trap branch resolves to the nearest copy.
pub fn exits(a: &mut Asm, os: Os) {
    for k in TrapKind::ALL {
        a.bind_copy(k.label());
        exit(a, os, k);
    }
}
