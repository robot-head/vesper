//! The RV64 encoding of each abstract machine instruction (RV64I plus the
//! M and D extensions; no compressed instructions, so the code also runs on any
//! RV64GC core).
//!
//! `R0`, `R1`, `R2` are `a0`, `a1`, `a2`; `t0` and `t1` are scratch inside
//! one instruction's sequence. Frame words are `8n(sp)` (slots and the
//! outgoing area) and `16 + 8n(s0)` (the incoming area), the same frame as
//! the AArch64 encoding. RISC-V has no flags, so every checked operation
//! computes its abort condition explicitly. Conditional branches reach only
//! ±4 KiB, so a branch to a label is an inverted branch over a `jal`
//! (±1 MiB). A call is `auipc` + `jalr` (±2 GiB), and the abort exits are
//! repeated through the code (`exits`) so that a trap branch stays near one. The emitted programs run under QEMU in the tests, and
//! `lean/CodegenRv64Encoding.lean` models this file's `emit`: it proves every
//! instruction that model produces encodes to a word the ISA's format tables
//! decode back to that instruction, and prints the words of sample
//! instructions that the tests compare with [`encoding_listing`].
use super::asm::{Asm, Fix};
use super::machine::{Cond, FloatOp, Label, Loc, M, Reg, Sign, TrapKind, WrapOp};
use super::{ResultProtocol, VerifiedError};

const ZERO: u32 = 0;
const RA: u32 = 1;
const SP: u32 = 2;
const T0: u32 = 5;
const T1: u32 = 6;
const S0: u32 = 8;
const A0: u32 = 10;
const A1: u32 = 11;
const A2: u32 = 12;
const A7: u32 = 17;

fn reg(r: Reg) -> u32 {
    match r {
        Reg::R0 => A0,
        Reg::R1 => A1,
        Reg::R2 => A2,
    }
}

const OP: u32 = 0x33;
const OP_IMM: u32 = 0x13;
const LOAD: u32 = 0x03;
const STORE: u32 = 0x23;
const BRANCH: u32 = 0x63;
const JAL: u32 = 0x6f;
const JALR: u32 = 0x67;
const AUIPC: u32 = 0x17;

/// R-type `op rd, rs1, rs2`.
fn r(a: &mut Asm, funct7: u32, funct3: u32, rd: u32, rs1: u32, rs2: u32) {
    a.u32(funct7 << 25 | rs2 << 20 | rs1 << 15 | funct3 << 12 | rd << 7 | OP);
}
/// I-type with a 12-bit signed immediate.
fn i(a: &mut Asm, opcode: u32, funct3: u32, rd: u32, rs1: u32, imm: i32) {
    debug_assert!((-2048..2048).contains(&imm));
    a.u32(((imm as u32) & 0xfff) << 20 | rs1 << 15 | funct3 << 12 | rd << 7 | opcode);
}
fn s(a: &mut Asm, funct3: u32, rs1: u32, rs2: u32, imm: i32) {
    let imm = imm as u32 & 0xfff;
    a.u32((imm >> 5) << 25 | rs2 << 20 | rs1 << 15 | funct3 << 12 | (imm & 0x1f) << 7 | STORE);
}
/// B-type with a byte offset known now (only short skips inside one
/// instruction's sequence).
fn b(a: &mut Asm, funct3: u32, rs1: u32, rs2: u32, offset: i32) {
    let imm = offset as u32;
    a.u32(
        ((imm >> 12) & 1) << 31
            | ((imm >> 5) & 0x3f) << 25
            | rs2 << 20
            | rs1 << 15
            | funct3 << 12
            | ((imm >> 1) & 0xf) << 8
            | ((imm >> 11) & 1) << 7
            | BRANCH,
    );
}
const BEQ: u32 = 0;
const BNE: u32 = 1;
const BLTU: u32 = 6;
/// The branch with the opposite condition (`beq`/`bne`, `blt`/`bge`,
/// `bltu`/`bgeu` differ in the low bit of `funct3`).
fn invert(funct3: u32) -> u32 {
    funct3 ^ 1
}

fn addi(a: &mut Asm, rd: u32, rs1: u32, imm: i32) {
    i(a, OP_IMM, 0, rd, rs1, imm);
}
fn slli(a: &mut Asm, rd: u32, rs1: u32, shamt: u32) {
    i(a, OP_IMM, 1, rd, rs1, shamt as i32);
}
fn srai(a: &mut Asm, rd: u32, rs1: u32, shamt: u32) {
    i(a, OP_IMM, 5, rd, rs1, (0x400 | shamt) as i32);
}
fn ori(a: &mut Asm, rd: u32, rs1: u32, imm: i32) {
    i(a, OP_IMM, 6, rd, rs1, imm);
}
fn xori(a: &mut Asm, rd: u32, rs1: u32, imm: i32) {
    i(a, OP_IMM, 4, rd, rs1, imm);
}
fn add(a: &mut Asm, rd: u32, rs1: u32, rs2: u32) {
    r(a, 0, 0, rd, rs1, rs2);
}
fn sub(a: &mut Asm, rd: u32, rs1: u32, rs2: u32) {
    r(a, 0x20, 0, rd, rs1, rs2);
}
fn slt(a: &mut Asm, rd: u32, rs1: u32, rs2: u32) {
    r(a, 0, 2, rd, rs1, rs2);
}
fn sltu(a: &mut Asm, rd: u32, rs1: u32, rs2: u32) {
    r(a, 0, 3, rd, rs1, rs2);
}
/// M extension, by `funct3`.
fn muldiv(a: &mut Asm, funct3: u32, rd: u32, rs1: u32, rs2: u32) {
    r(a, 1, funct3, rd, rs1, rs2);
}
const MUL: u32 = 0;
const MULH: u32 = 1;
const MULHU: u32 = 3;
const DIV: u32 = 4;
const DIVU: u32 = 5;
const REM: u32 = 6;
const REMU: u32 = 7;

/// OP-FP `funct7 rd, rs1, rs2` with `funct3` (the rounding mode for
/// arithmetic: 0 is round to nearest, ties to even).
fn fp(a: &mut Asm, funct7: u32, funct3: u32, rd: u32, rs1: u32, rs2: u32) {
    a.u32(funct7 << 25 | rs2 << 20 | rs1 << 15 | funct3 << 12 | rd << 7 | 0x53);
}
/// `fmv.d.x fd, rs1`.
fn fmv_d_x(a: &mut Asm, fd: u32, rs1: u32) {
    fp(a, 0x79, 0, fd, rs1, 0);
}
/// `fmv.x.d rd, fs1`.
fn fmv_x_d(a: &mut Asm, rd: u32, fs1: u32) {
    fp(a, 0x71, 0, rd, fs1, 0);
}

/// `li rd, w` as a fixed eleven-instruction sequence: the top nine bits,
/// then five 11-bit chunks shifted in (`ori` takes non-negative 11-bit
/// immediates without sign extension).
fn imm(a: &mut Asm, rd: u32, w: u64) {
    addi(a, rd, ZERO, (w >> 55) as i32);
    for chunk in (0..5).rev() {
        slli(a, rd, rd, 11);
        ori(a, rd, rd, ((w >> (11 * chunk)) & 0x7ff) as i32);
    }
}

/// `jal rd, label`.
fn jal(a: &mut Asm, rd: u32, l: Label) {
    let at = a.here();
    a.u32(rd << 7 | JAL);
    a.fixup(at, l, Fix::Jal);
}
/// `auipc rd, hi; jalr rd, lo(rd)`: call `l` anywhere within ±2 GiB.
fn far_call(a: &mut Asm, rd: u32, l: Label) {
    let at = a.here();
    a.u32(rd << 7 | AUIPC);
    a.u32(rd << 15 | rd << 7 | JALR);
    a.fixup(at, l, Fix::AuipcJalr);
}
/// Branch to `l` when `rs1 cond rs2`: the inverted branch skips a `jal`.
fn branch_to(a: &mut Asm, funct3: u32, rs1: u32, rs2: u32, l: Label) {
    b(a, invert(funct3), rs1, rs2, 8);
    jal(a, ZERO, l);
}

/// The base register and byte offset of a frame word.
fn frame(loc: Loc) -> (u32, u32) {
    match loc {
        Loc::Slot(n) | Loc::Out(n) => (SP, n * 8),
        Loc::In(n) => (S0, 16 + n * 8),
    }
}
fn ldst(a: &mut Asm, load: bool, r: u32, loc: Loc) {
    let (mut base, mut offset) = frame(loc);
    if offset >= 2048 {
        imm(a, T0, u64::from(offset));
        add(a, T0, T0, base);
        (base, offset) = (T0, 0);
    }
    if load {
        i(a, LOAD, 3, r, base, offset as i32);
    } else {
        s(a, 3, base, r, offset as i32);
    }
}
/// `sp := sp - bytes`.
fn sub_sp(a: &mut Asm, bytes: u32) {
    if bytes < 2048 {
        addi(a, SP, SP, -(bytes as i32));
    } else {
        imm(a, T0, u64::from(bytes));
        sub(a, SP, SP, T0);
    }
}
/// `rd := i64::MIN`.
fn int_min(a: &mut Asm, rd: u32) {
    addi(a, rd, ZERO, 1);
    slli(a, rd, rd, 63);
}

pub fn emit(a: &mut Asm, m: &M) -> Result<(), VerifiedError> {
    let ovf = a.trap_label(TrapKind::Overflow);
    match *m {
        M::Ld(r, l) => ldst(a, true, reg(r), l),
        M::St(l, r) => ldst(a, false, reg(r), l),
        M::Imm(r, w) => imm(a, reg(r), w),
        // add a0, a0, a1; carry iff the sum is below an operand
        M::AddChk(Sign::U) => {
            add(a, A0, A0, A1);
            branch_to(a, BLTU, A0, A1, ovf);
        }
        // signed overflow iff (b < 0) != (sum < a)
        M::AddChk(Sign::S) => {
            addi(a, T0, A0, 0);
            add(a, A0, A0, A1);
            slt(a, T1, A1, ZERO);
            slt(a, T0, A0, T0);
            branch_to(a, BNE, T1, T0, ovf);
        }
        // borrow iff a < b
        M::SubChk(Sign::U) => {
            branch_to(a, BLTU, A0, A1, ovf);
            sub(a, A0, A0, A1);
        }
        // signed overflow iff (0 < b) != (difference < a)
        M::SubChk(Sign::S) => {
            addi(a, T0, A0, 0);
            sub(a, A0, A0, A1);
            slt(a, T1, ZERO, A1);
            slt(a, T0, A0, T0);
            branch_to(a, BNE, T1, T0, ovf);
        }
        // the high word must be the sign extension of the low word
        M::MulChk(Sign::S) => {
            muldiv(a, MULH, A2, A0, A1);
            muldiv(a, MUL, A0, A0, A1);
            srai(a, T0, A0, 63);
            branch_to(a, BNE, A2, T0, ovf);
        }
        M::MulChk(Sign::U) => {
            muldiv(a, MULHU, A2, A0, A1);
            muldiv(a, MUL, A0, A0, A1);
            branch_to(a, BNE, A2, ZERO, ovf);
        }
        M::Wrap(WrapOp::Add) => add(a, A0, A0, A1),
        M::Wrap(WrapOp::Sub) => sub(a, A0, A0, A1),
        M::Wrap(WrapOp::Mul) => muldiv(a, MUL, A0, A0, A1),
        M::AndR2 => r(a, 0, 7, A0, A0, A2),
        M::TrapGtU => branch_to(a, BLTU, A2, A0, ovf),
        M::TrapZero => {
            let l = a.trap_label(TrapKind::DivisionByZero);
            branch_to(a, BEQ, A1, ZERO, l)
        }
        // a2 := MIN; bne a0, a2, +16; li t0, -1; bne a1, t0, +8; j ovf
        M::TrapDivOvf => {
            int_min(a, A2);
            b(a, BNE, A0, A2, 16);
            addi(a, T0, ZERO, -1);
            b(a, BNE, A1, T0, 8);
            jal(a, ZERO, ovf);
        }
        M::Div(Sign::S) => muldiv(a, DIV, A0, A0, A1),
        M::Div(Sign::U) => muldiv(a, DIVU, A0, A0, A1),
        // RISC-V `rem` has the sign of the dividend and MIN rem -1 = 0.
        M::Rem(Sign::S) => muldiv(a, REM, A0, A0, A1),
        M::Rem(Sign::U) => muldiv(a, REMU, A0, A0, A1),
        M::NegChk(Sign::S) => {
            int_min(a, T0);
            branch_to(a, BEQ, A0, T0, ovf);
            sub(a, A0, ZERO, A0);
        }
        M::NegChk(Sign::U) => branch_to(a, BNE, A0, ZERO, ovf),
        M::XorOne => xori(a, A0, A0, 1),
        M::Cmp(c, s) => {
            let lt = |a: &mut Asm, rd, x, y| {
                if s == Sign::S {
                    slt(a, rd, x, y)
                } else {
                    sltu(a, rd, x, y)
                }
            };
            match c {
                Cond::Eq => {
                    sub(a, T0, A0, A1);
                    i(a, OP_IMM, 3, A0, T0, 1); // sltiu a0, t0, 1
                }
                Cond::Ne => {
                    sub(a, T0, A0, A1);
                    sltu(a, A0, ZERO, T0);
                }
                Cond::Lt => lt(a, A0, A0, A1),
                Cond::Gt => lt(a, A0, A1, A0),
                Cond::Le => {
                    lt(a, A0, A1, A0);
                    xori(a, A0, A0, 1);
                }
                Cond::Ge => {
                    lt(a, A0, A0, A1);
                    xori(a, A0, A0, 1);
                }
            }
        }
        // fmv.d.x f0, a0; fmv.d.x f1, a1; f<op>.d f0, f0, f1 (rne); fmv.x.d a0, f0
        M::FOp(op) => {
            let funct7 = match op {
                FloatOp::Add => 0x01,
                FloatOp::Sub => 0x05,
                FloatOp::Mul => 0x09,
                FloatOp::Div => 0x0d,
            };
            fmv_d_x(a, 0, A0);
            fmv_d_x(a, 1, A1);
            fp(a, funct7, 0, 0, 0, 1);
            fmv_x_d(a, A0, 0);
        }
        // fmv.d.x f0, a0; fsgnjn.d f0, f0, f0; fmv.x.d a0, f0
        M::FNeg => {
            fmv_d_x(a, 0, A0);
            fp(a, 0x11, 1, 0, 0, 0);
            fmv_x_d(a, A0, 0);
        }
        // feq/flt/fle.d write 0 for an unordered pair; `!=` is `feq` negated.
        M::FCmp(c) => {
            fmv_d_x(a, 0, A0);
            fmv_d_x(a, 1, A1);
            let (funct3, x, y) = match c {
                Cond::Eq | Cond::Ne => (2, 0, 1),
                Cond::Lt => (1, 0, 1),
                Cond::Le => (0, 0, 1),
                Cond::Gt => (1, 1, 0),
                Cond::Ge => (0, 1, 0),
            };
            fp(a, 0x51, funct3, A0, x, y);
            if c == Cond::Ne {
                xori(a, A0, A0, 1);
            }
        }
        // t0 := HEAP_BASE (1 << 28); t0 += a1 << 3; ld a0, 8*off(t0)
        // (sd a0, ...).
        M::LdH(off) | M::StH(off) => {
            const _: () = assert!(super::HEAP_BASE == 1 << 28);
            addi(a, T0, ZERO, 1);
            slli(a, T0, T0, 28);
            slli(a, T1, A1, 3);
            add(a, T0, T0, T1);
            let mut offset = off * 8;
            if offset >= 2048 {
                imm(a, T1, u64::from(offset));
                add(a, T0, T0, T1);
                offset = 0;
            }
            if matches!(m, M::LdH(_)) {
                i(a, LOAD, 3, A0, T0, offset as i32);
            } else {
                s(a, 3, T0, A0, offset as i32);
            }
        }
        M::Label(l) => a.bind(l)?,
        M::Jmp(l) => jal(a, ZERO, l),
        M::Bz(l) => branch_to(a, BEQ, A0, ZERO, l),
        M::Call(l) => far_call(a, RA, l),
        // addi sp, sp, -16; sd ra, 8(sp); sd s0, 0(sp); mv s0, sp; sp -= bytes
        M::Enter(words) => {
            addi(a, SP, SP, -16);
            s(a, 3, SP, RA, 8);
            s(a, 3, SP, S0, 0);
            addi(a, S0, SP, 0);
            sub_sp(a, words * 8);
        }
        // mv sp, s0; ld s0, 0(sp); ld ra, 8(sp); addi sp, sp, 16; ret
        M::Leave => {
            addi(a, SP, S0, 0);
            i(a, LOAD, 3, S0, SP, 0);
            i(a, LOAD, 3, RA, SP, 8);
            addi(a, SP, SP, 16);
            i(a, JALR, 0, ZERO, RA, 0);
        }
        M::Trap(k) => {
            let l = a.trap_label(k);
            jal(a, ZERO, l)
        }
        M::Scope(ovf, div0) => a.set_scope(ovf, div0),
    }
    Ok(())
}

/// `ecall` with the Linux system-call number in `a7`.
fn syscall(a: &mut Asm, number: i32) {
    addi(a, A7, ZERO, number);
    a.u32(0x0000_0073);
}

/// The Linux `_start` entry (no libc): reserves the result area, calls the
/// entry, optionally writes the result words to stdout and exits.
pub fn start(
    a: &mut Asm,
    protocol: ResultProtocol,
    result_words: u32,
    entry: Label,
) -> Result<(), VerifiedError> {
    let area = ((result_words.max(1) + 1) & !1) * 8;
    sub_sp(a, area);
    far_call(a, RA, entry);
    if protocol == ResultProtocol::Words && result_words > 0 {
        addi(a, A0, ZERO, 1);
        addi(a, A1, SP, 0);
        imm(a, A2, u64::from(result_words) * 8);
        syscall(a, 64);
    }
    if protocol == ResultProtocol::ExitStatus && result_words > 0 {
        i(a, LOAD, 3, A0, SP, 0);
    } else {
        addi(a, A0, ZERO, 0);
    }
    syscall(a, 93);
    for k in TrapKind::ALL {
        a.bind(k.label())?;
        exit(a, k);
    }
    Ok(())
}

fn exit(a: &mut Asm, k: TrapKind) {
    addi(a, A0, ZERO, i32::from(k.exit_status()));
    syscall(a, 93);
}

/// Another copy of the abort exits, between functions.
pub fn exits(a: &mut Asm) {
    for k in TrapKind::ALL {
        a.bind_copy(k.label());
        exit(a, k);
    }
}

/// The words `emit` writes for the shared sample instructions
/// ([`super::asm::encoding_samples`]), in the format
/// `lean/CodegenRv64Encoding.lean` prints.
pub fn encoding_listing() -> String {
    super::asm::encoding_listing(&super::asm::encoding_samples(), emit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(f: impl FnOnce(&mut Asm)) -> u32 {
        let mut a = Asm::default();
        f(&mut a);
        assert_eq!(a.code.len(), 4);
        u32::from_le_bytes(a.code[..4].try_into().unwrap())
    }

    /// Reference encodings from the RISC-V unprivileged specification
    /// (as printed by GNU `as` for the same instructions).
    #[test]
    fn encodings_match_the_reference_assembler() {
        assert_eq!(one(|a| add(a, A0, A0, A1)), 0x00b5_0533); // add a0,a0,a1
        assert_eq!(one(|a| sub(a, A0, A0, A1)), 0x40b5_0533); // sub a0,a0,a1
        assert_eq!(one(|a| muldiv(a, MUL, A0, A0, A1)), 0x02b5_0533); // mul
        assert_eq!(one(|a| muldiv(a, MULH, A2, A0, A1)), 0x02b5_1633); // mulh a2,a0,a1
        assert_eq!(one(|a| muldiv(a, DIV, A0, A0, A1)), 0x02b5_4533); // div
        assert_eq!(one(|a| muldiv(a, REMU, A0, A0, A1)), 0x02b5_7533); // remu
        assert_eq!(one(|a| addi(a, SP, SP, -16)), 0xff01_0113); // addi sp,sp,-16
        assert_eq!(one(|a| s(a, 3, SP, RA, 8)), 0x0011_3423); // sd ra,8(sp)
        assert_eq!(one(|a| i(a, LOAD, 3, RA, SP, 8)), 0x0081_3083); // ld ra,8(sp)
        assert_eq!(one(|a| srai(a, T0, A0, 63)), 0x43f5_5293); // srai t0,a0,63
        assert_eq!(one(|a| slli(a, A0, A0, 11)), 0x00b5_1513); // slli a0,a0,11
        assert_eq!(one(|a| xori(a, A0, A0, 1)), 0x0015_4513); // xori a0,a0,1
        assert_eq!(one(|a| sltu(a, A0, ZERO, T0)), 0x0050_3533); // snez a0,t0
        assert_eq!(one(|a| b(a, BNE, A0, A2, 16)), 0x00c5_1863); // bne a0,a2,+16
        assert_eq!(one(|a| i(a, JALR, 0, ZERO, RA, 0)), 0x0000_8067); // ret
        assert_eq!(one(|a| fmv_d_x(a, 0, A0)), 0xf205_0053); // fmv.d.x ft0,a0
        assert_eq!(one(|a| fmv_x_d(a, A0, 0)), 0xe200_0553); // fmv.x.d a0,ft0
        assert_eq!(one(|a| fp(a, 0x01, 0, 0, 0, 1)), 0x0210_0053); // fadd.d ft0,ft0,ft1,rne
        assert_eq!(one(|a| fp(a, 0x51, 2, A0, 0, 1)), 0xa210_2553); // feq.d a0,ft0,ft1
        assert_eq!(one(|a| fp(a, 0x11, 1, 0, 0, 0)), 0x2200_1053); // fneg.d ft0,ft0
    }

    #[test]
    fn immediates_round_trip() {
        // Interpret the eleven-instruction sequence.
        for w in [0, 1, 0x7ff, 0x800, u64::MAX, 1 << 63, 0x0123_4567_89ab_cdef] {
            let mut a = Asm::default();
            imm(&mut a, A0, w);
            let mut x = 0u64;
            for word in a.code.as_chunks::<4>().0 {
                let w = u32::from_le_bytes(*word);
                let imm = u64::from(w >> 20);
                match (w >> 12) & 7 {
                    0 => x = imm, // addi rd, zero, imm (imm < 2^9)
                    1 => x <<= imm & 0x3f,
                    6 => x |= imm,
                    other => panic!("funct3 {other}"),
                }
            }
            assert_eq!(x, w);
        }
    }
}
