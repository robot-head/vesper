//! The x86-64 encoding of each abstract machine instruction.
//!
//! `R0`, `R1`, `R2` are `rax`, `rcx`, `rdx`; frame words are `[rsp + 8n]`
//! (slots and the outgoing area) and `[rbp + 16 + 8n]` (the incoming area).
//! These encodings are tested by running the emitted programs.
//! `lean/CodegenX64Encoding.lean` proves the encoder model against the Intel
//! SDM instruction formats (a prefix decoder reads every emitted sequence
//! back); its printed bytes for sample instructions must equal
//! [`encoding_listing`] (`tests/verified_codegen_lean.rs`).
use super::asm::{Asm, Fix};
use super::machine::{Cond, FloatOp, Label, Loc, M, Reg, Sign, TrapKind, WrapOp};
use super::{Os, ResultProtocol, VerifiedError};

// Register numbers.
const RAX: u8 = 0;
const RCX: u8 = 1;
const RDX: u8 = 2;
const RDI: u8 = 7;

fn reg(r: Reg) -> u8 {
    match r {
        Reg::R0 => RAX,
        Reg::R1 => RCX,
        Reg::R2 => RDX,
    }
}

/// Bytes per frame or heap word.
const WORD_BYTES: u32 = 8;
/// The incoming area's offset from `rbp` (past the saved `rbp` and the
/// return address).
const INCOMING_OFFSET: u32 = 16;
/// Bytes per page: Windows stack probes touch each one in turn.
const PAGE_BYTES: u32 = 4096;

/// The REX prefix with `W` (64-bit operand size).
const REX_W: u8 = 0x48;
/// The REX prefix with `W` and `B` (64-bit operand size, `r8`-`r15` base).
const REX_WB: u8 = 0x49;
/// The escape byte of the two-byte opcodes.
const ESCAPE_0F: u8 = 0x0f;
/// `mov r64, r/m64`.
const OPCODE_MOV_LOAD: u8 = 0x8b;
/// `mov r/m64, r64`.
const OPCODE_MOV_STORE: u8 = 0x89;
/// `mov r32, imm32` (`mov r64, imm64` under `REX.W`), plus the register.
const OPCODE_MOV_IMM: u8 = 0xb8;
/// `jmp rel32`.
const OPCODE_JMP_REL32: u8 = 0xe9;
/// `call rel32`.
const OPCODE_CALL_REL32: u8 = 0xe8;
/// `jcc rel32` is `0f 80+cc`.
const OPCODE_JCC_REL32: u8 = 0x80;
/// `jp rel8`.
const OPCODE_JP_REL8: u8 = 0x7a;
/// `int3`.
const INT3: u8 = 0xcc;

/// The ModRM `reg` field's position.
const MODRM_REG_SHIFT: u8 = 3;
/// ModRM `mod = 10, rm = 100`: `[SIB + disp32]`.
const MODRM_SIB_DISP32: u8 = 0x84;
/// ModRM `mod = 10, rm = 101`: `[rbp + disp32]`.
const MODRM_RBP_DISP32: u8 = 0x85;
/// ModRM `mod = 11, reg = 000, rm = 000`: `al`/`rax` direct.
const MODRM_DIRECT_RAX: u8 = 0xc0;
/// SIB: base `rsp`, no index.
const SIB_RSP: u8 = 0x24;
/// SIB: base `r11`, index `rcx`, scale 8.
const SIB_R11_RCX_8: u8 = 0xcb;

/// `add rax, rcx`.
const ADD_RAX_RCX: [u8; 3] = [0x48, 0x01, 0xc8];
/// `sub rax, rcx`.
const SUB_RAX_RCX: [u8; 3] = [0x48, 0x29, 0xc8];
/// `imul rax, rcx`.
const IMUL_RAX_RCX: [u8; 4] = [0x48, 0x0f, 0xaf, 0xc1];
/// `mul rcx` (`rdx:rax := rax * rcx`).
const MUL_RCX: [u8; 3] = [0x48, 0xf7, 0xe1];
/// `and rax, rdx`.
const AND_RAX_RDX: [u8; 3] = [0x48, 0x21, 0xd0];
/// `cmp rax, rdx`.
const CMP_RAX_RDX: [u8; 3] = [0x48, 0x39, 0xd0];
/// `cmp rax, rcx`.
const CMP_RAX_RCX: [u8; 3] = [0x48, 0x39, 0xc8];
/// `test rcx, rcx`.
const TEST_RCX_RCX: [u8; 3] = [0x48, 0x85, 0xc9];
/// `test rax, rax`.
const TEST_RAX_RAX: [u8; 3] = [0x48, 0x85, 0xc0];
/// `cmp rax, rdx; jne +10; cmp rcx, -1`: falls through to the `je rel32`
/// after it only when `rax = rdx` and `rcx = -1`.
const DIV_OVF_CMPS: [u8; 9] = [0x48, 0x39, 0xd0, 0x75, 0x0a, 0x48, 0x83, 0xf9, 0xff];
/// `cqo; idiv rcx`.
const IDIV_RCX: [u8; 5] = [0x48, 0x99, 0x48, 0xf7, 0xf9];
/// `xor edx, edx; div rcx`.
const DIV_RCX: [u8; 5] = [0x31, 0xd2, 0x48, 0xf7, 0xf1];
/// `cmp rcx, -1; jne +4; xor eax, eax; jmp +8; cqo; idiv rcx; mov rax, rdx`
/// (`idiv` faults on MIN / -1; every remainder by -1 is 0).
const IREM_RCX: [u8; 18] = [
    0x48, 0x83, 0xf9, 0xff, 0x75, 0x04, 0x31, 0xc0, 0xeb, 0x08, 0x48, 0x99, 0x48, 0xf7, 0xf9, 0x48,
    0x89, 0xd0,
];
/// `xor edx, edx; div rcx; mov rax, rdx`.
const REM_RCX: [u8; 8] = [0x31, 0xd2, 0x48, 0xf7, 0xf1, 0x48, 0x89, 0xd0];
/// `neg rax`.
const NEG_RAX: [u8; 3] = [0x48, 0xf7, 0xd8];
/// `xor rax, 1`.
const XOR_RAX_ONE: [u8; 4] = [0x48, 0x83, 0xf0, 0x01];
/// `movzx eax, al`.
const MOVZX_EAX_AL: [u8; 3] = [0x0f, 0xb6, 0xc0];
/// `movq xmm0, rax; movq xmm1, rcx`.
const MOVQ_XMM0_RAX_XMM1_RCX: [u8; 10] =
    [0x66, 0x48, 0x0f, 0x6e, 0xc0, 0x66, 0x48, 0x0f, 0x6e, 0xc9];
/// `movq rax, xmm0`.
const MOVQ_RAX_XMM0: [u8; 5] = [0x66, 0x48, 0x0f, 0x7e, 0xc0];
/// The scalar-double prefix of `addsd` and friends.
const PREFIX_SD: u8 = 0xf2;
/// ModRM `mod = 11, reg = xmm0, rm = xmm1`.
const MODRM_XMM0_XMM1: u8 = 0xc1;
// The second opcode bytes of `<op>sd xmm, xmm/m64` (`f2 0f xx`).
const OPCODE_ADDSD: u8 = 0x58;
const OPCODE_SUBSD: u8 = 0x5c;
const OPCODE_MULSD: u8 = 0x59;
const OPCODE_DIVSD: u8 = 0x5e;
/// `btc rax, 63` (flip the sign bit).
const BTC_RAX_63: [u8; 5] = [0x48, 0x0f, 0xba, 0xf8, 0x3f];
/// `mov r11d, imm32`.
const MOV_R11D_IMM32: [u8; 2] = [0x41, 0xbb];
/// `add rsp, imm32`.
const ADD_RSP_IMM32: [u8; 3] = [0x48, 0x81, 0xc4];
/// `sub rsp, imm32`.
const SUB_RSP_IMM32: [u8; 3] = [0x48, 0x81, 0xec];
/// `pop rbp; ret`.
const POP_RBP_RET: [u8; 2] = [0x5d, 0xc3];
/// `mov rsp, rbp; pop rbp; ret`.
const MOV_RSP_RBP_POP_RBP_RET: [u8; 5] = [0x48, 0x89, 0xec, 0x5d, 0xc3];
/// `push rbp`.
const PUSH_RBP: [u8; 1] = [0x55];
/// `mov rbp, rsp`.
const MOV_RBP_RSP: [u8; 3] = [0x48, 0x89, 0xe5];
/// `test [rsp + disp32], esp`, without its `disp32`.
const TEST_RSP_DISP32_ESP: [u8; 3] = [0x85, 0xa4, 0x24];
/// `sub rsp, 32` (the shadow space).
const SUB_RSP_32: [u8; 4] = [0x48, 0x83, 0xec, 0x20];
/// `sub rsp, 48` (the shadow space, the fifth argument and `written`).
const SUB_RSP_48: [u8; 4] = [0x48, 0x83, 0xec, 0x30];
/// `add rsp, 48`.
const ADD_RSP_48: [u8; 4] = [0x48, 0x83, 0xc4, 0x30];
/// `mov rcx, rax; lea rdx, [rsp + 48]; mov r8d, imm32`, without its `imm32`.
const WRITE_FILE_ARGS: [u8; 10] = [0x48, 0x89, 0xc1, 0x48, 0x8d, 0x54, 0x24, 0x30, 0x41, 0xb8];
/// `lea r9, [rsp + 40]`.
const LEA_R9_RSP_40: [u8; 5] = [0x4c, 0x8d, 0x4c, 0x24, 0x28];
/// `mov qword [rsp + 32], 0`.
const MOV_QWORD_RSP_32_ZERO: [u8; 9] = [0x48, 0xc7, 0x44, 0x24, 0x20, 0, 0, 0, 0];
/// `movzx eax, byte [rsp]`.
const MOVZX_EAX_BYTE_RSP: [u8; 4] = [0x0f, 0xb6, 0x04, 0x24];
/// `xor eax, eax`.
const XOR_EAX_EAX: [u8; 2] = [0x31, 0xc0];
/// `mov rsi, rsp`.
const MOV_RSI_RSP: [u8; 3] = [0x48, 0x89, 0xe6];
/// `mov rdi, [rsp]`.
const MOV_RDI_RSP_WORD: [u8; 4] = [0x48, 0x8b, 0x3c, 0x24];
/// `xor edi, edi`.
const XOR_EDI_EDI: [u8; 2] = [0x31, 0xff];
/// `syscall`.
const SYSCALL: [u8; 2] = [0x0f, 0x05];

/// Windows `STD_OUTPUT_HANDLE`.
const STD_OUTPUT_HANDLE: i32 = -11;
/// Linux system calls and the standard output descriptor.
const SYS_WRITE: u8 = 1;
const SYS_EXIT: u8 = 60;
const STDOUT: u8 = 1;

fn mem(a: &mut Asm, opcode: u8, r: Reg, loc: Loc) {
    let r = reg(r) << MODRM_REG_SHIFT;
    match loc {
        Loc::Slot(n) | Loc::Out(n) => {
            a.bytes(&[REX_W, opcode, MODRM_SIB_DISP32 | r, SIB_RSP]);
            a.bytes(&(n * WORD_BYTES).to_le_bytes());
        }
        Loc::In(n) => {
            a.bytes(&[REX_W, opcode, MODRM_RBP_DISP32 | r]);
            a.bytes(&(INCOMING_OFFSET + n * WORD_BYTES).to_le_bytes());
        }
    }
}

/// `jcc rel32` (`0f 8x`) or `jmp`/`call` to a label.
fn jcc(a: &mut Asm, cc: u8, l: Label) {
    a.bytes(&[ESCAPE_0F, OPCODE_JCC_REL32 | cc]);
    let at = a.here();
    a.bytes(&[0; 4]);
    a.fixup(at, l, Fix::Rel32);
}
fn rel(a: &mut Asm, opcode: u8, l: Label) {
    a.bytes(&[opcode]);
    let at = a.here();
    a.bytes(&[0; 4]);
    a.fixup(at, l, Fix::Rel32);
}
const JO: u8 = 0x0;
const JB: u8 = 0x2;
const JE: u8 = 0x4;
const JNE: u8 = 0x5;
const JA: u8 = 0x7;

// The second opcode bytes of `setcc r/m8` (`0f 9x`).
const SETB: u8 = 0x92;
const SETAE: u8 = 0x93;
const SETE: u8 = 0x94;
const SETNE: u8 = 0x95;
const SETBE: u8 = 0x96;
const SETA: u8 = 0x97;
const SETL: u8 = 0x9c;
const SETGE: u8 = 0x9d;
const SETLE: u8 = 0x9e;
const SETG: u8 = 0x9f;

pub fn emit(a: &mut Asm, m: &M) -> Result<(), VerifiedError> {
    let ovf = a.trap_label(TrapKind::Overflow);
    match *m {
        M::Ld(r, l) => mem(a, OPCODE_MOV_LOAD, r, l),
        M::St(l, r) => mem(a, OPCODE_MOV_STORE, r, l),
        M::Imm(r, w) => {
            a.bytes(&[REX_W, OPCODE_MOV_IMM + reg(r)]);
            a.bytes(&w.to_le_bytes());
        }
        // add rax, rcx; jo/jb
        M::AddChk(s) => {
            a.bytes(&ADD_RAX_RCX);
            jcc(a, if s == Sign::S { JO } else { JB }, ovf);
        }
        // sub rax, rcx; jo/jb
        M::SubChk(s) => {
            a.bytes(&SUB_RAX_RCX);
            jcc(a, if s == Sign::S { JO } else { JB }, ovf);
        }
        // imul rax, rcx; jo  |  mul rcx (rdx:rax); jo
        M::MulChk(Sign::S) => {
            a.bytes(&IMUL_RAX_RCX);
            jcc(a, JO, ovf);
        }
        M::MulChk(Sign::U) => {
            a.bytes(&MUL_RCX);
            jcc(a, JO, ovf);
        }
        M::Wrap(WrapOp::Add) => a.bytes(&ADD_RAX_RCX),
        M::Wrap(WrapOp::Sub) => a.bytes(&SUB_RAX_RCX),
        M::Wrap(WrapOp::Mul) => a.bytes(&IMUL_RAX_RCX),
        // and rax, rdx
        M::AndR2 => a.bytes(&AND_RAX_RDX),
        // cmp rax, rdx; ja
        M::TrapGtU => {
            a.bytes(&CMP_RAX_RDX);
            jcc(a, JA, ovf);
        }
        // test rcx, rcx; je
        M::TrapZero => {
            a.bytes(&TEST_RCX_RCX);
            let l = a.trap_label(TrapKind::DivisionByZero);
            jcc(a, JE, l);
        }
        // movabs rdx, MIN; cmp rax, rdx; jne +10; cmp rcx, -1; je ovf
        M::TrapDivOvf => {
            a.bytes(&[REX_W, OPCODE_MOV_IMM + RDX]);
            a.bytes(&(i64::MIN as u64).to_le_bytes());
            a.bytes(&DIV_OVF_CMPS);
            jcc(a, JE, ovf);
        }
        // cqo; idiv rcx  |  xor edx, edx; div rcx
        M::Div(Sign::S) => a.bytes(&IDIV_RCX),
        M::Div(Sign::U) => a.bytes(&DIV_RCX),
        M::Rem(Sign::S) => a.bytes(&IREM_RCX),
        M::Rem(Sign::U) => a.bytes(&REM_RCX),
        // neg rax; jo  |  test rax, rax; jne
        M::NegChk(Sign::S) => {
            a.bytes(&NEG_RAX);
            jcc(a, JO, ovf);
        }
        M::NegChk(Sign::U) => {
            a.bytes(&TEST_RAX_RAX);
            jcc(a, JNE, ovf);
        }
        // xor rax, 1
        M::XorOne => a.bytes(&XOR_RAX_ONE),
        // cmp rax, rcx; setcc al; movzx eax, al
        M::Cmp(c, s) => {
            let cc = match (c, s) {
                (Cond::Eq, _) => SETE,
                (Cond::Ne, _) => SETNE,
                (Cond::Lt, Sign::S) => SETL,
                (Cond::Le, Sign::S) => SETLE,
                (Cond::Gt, Sign::S) => SETG,
                (Cond::Ge, Sign::S) => SETGE,
                (Cond::Lt, Sign::U) => SETB,
                (Cond::Le, Sign::U) => SETBE,
                (Cond::Gt, Sign::U) => SETA,
                (Cond::Ge, Sign::U) => SETAE,
            };
            a.bytes(&CMP_RAX_RCX);
            a.bytes(&[ESCAPE_0F, cc, MODRM_DIRECT_RAX]);
            a.bytes(&MOVZX_EAX_AL);
        }
        // movq xmm0, rax; movq xmm1, rcx; <op>sd xmm0, xmm1; movq rax, xmm0
        M::FOp(op) => {
            let opcode = match op {
                FloatOp::Add => OPCODE_ADDSD,
                FloatOp::Sub => OPCODE_SUBSD,
                FloatOp::Mul => OPCODE_MULSD,
                FloatOp::Div => OPCODE_DIVSD,
            };
            a.bytes(&MOVQ_XMM0_RAX_XMM1_RCX);
            a.bytes(&[PREFIX_SD, ESCAPE_0F, opcode, MODRM_XMM0_XMM1]);
            a.bytes(&MOVQ_RAX_XMM0);
        }
        // btc rax, 63
        M::FNeg => a.bytes(&BTC_RAX_63),
        // movq xmm0, rax; movq xmm1, rcx; ucomisd; then a NaN-aware setcc.
        // `ucomisd` sets ZF, PF and CF on an unordered pair.
        M::FCmp(c) => {
            a.bytes(&MOVQ_XMM0_RAX_XMM1_RCX);
            // ucomisd xmm0, xmm1 / ucomisd xmm1, xmm0
            const AB: [u8; 4] = [0x66, 0x0f, 0x2e, 0xc1];
            const BA: [u8; 4] = [0x66, 0x0f, 0x2e, 0xc8];
            match c {
                // ucomisd; mov eax, 0|1; jp +3; sete/setne al
                Cond::Eq | Cond::Ne => {
                    let (init, cc) = if c == Cond::Eq { (0, SETE) } else { (1, SETNE) };
                    a.bytes(&AB);
                    a.bytes(&[
                        OPCODE_MOV_IMM + RAX,
                        init,
                        0,
                        0,
                        0,
                        OPCODE_JP_REL8,
                        0x03,
                        ESCAPE_0F,
                        cc,
                        MODRM_DIRECT_RAX,
                    ]);
                }
                // a > b: seta (CF = 0 and ZF = 0); a >= b: setae (CF = 0).
                // a < b and a <= b swap the operands.
                _ => {
                    let (order, cc) = match c {
                        Cond::Gt => (AB, SETA),
                        Cond::Ge => (AB, SETAE),
                        Cond::Lt => (BA, SETA),
                        _ => (BA, SETAE),
                    };
                    a.bytes(&order);
                    a.bytes(&[ESCAPE_0F, cc, MODRM_DIRECT_RAX]);
                    a.bytes(&MOVZX_EAX_AL);
                }
            }
        }
        // mov r11d, HEAP_BASE; mov rax, [r11 + rcx*8 + 8*off]
        // (mov [r11 + rcx*8 + 8*off], rax).
        M::LdH(off) | M::StH(off) => {
            a.bytes(&MOV_R11D_IMM32);
            a.bytes(&(super::HEAP_BASE as u32).to_le_bytes());
            let opcode = if matches!(m, M::LdH(_)) {
                OPCODE_MOV_LOAD
            } else {
                OPCODE_MOV_STORE
            };
            a.bytes(&[REX_WB, opcode, MODRM_SIB_DISP32, SIB_R11_RCX_8]);
            a.bytes(&(off * WORD_BYTES).to_le_bytes());
        }
        M::Label(l) => a.bind(l)?,
        M::Jmp(l) => rel(a, OPCODE_JMP_REL32, l),
        // test rax, rax; je
        M::Bz(l) => {
            a.bytes(&TEST_RAX_RAX);
            jcc(a, JE, l);
        }
        M::Call(l) => rel(a, OPCODE_CALL_REL32, l),
        // push rbp; mov rbp, rsp; sub rsp, imm32
        M::Enter(words) => prologue(a, words * WORD_BYTES),
        // mov rsp, rbp; pop rbp; ret  |  Windows: add rsp, frame; pop rbp; ret
        // (the only epilogue shape the Windows x64 unwinder recognises)
        M::Leave if a.windows => {
            let frame = a.frame;
            a.bytes(&ADD_RSP_IMM32);
            a.bytes(&frame.to_le_bytes());
            a.bytes(&POP_RBP_RET);
        }
        M::Leave => a.bytes(&MOV_RSP_RBP_POP_RBP_RET),
        M::Trap(k) => {
            let l = a.trap_label(k);
            rel(a, OPCODE_JMP_REL32, l)
        }
        M::Scope(ovf, div0) => a.set_scope(ovf, div0),
    }
    Ok(())
}

/// The bytes `emit` writes for each sample instruction (`<instruction>:
/// <byte> ...`), with the abort exits bound before the instruction and labels
/// `FIRST_FREE_LABEL` and 9 after it; then the Windows forms of `enter` (stack probes) and `leave`.
pub fn encoding_listing() -> String {
    use std::fmt::Write;
    fn line(out: &mut String, m: &M, windows: bool, frame: u32, tag: &str) {
        let mut a = Asm::default();
        a.windows = windows;
        a.frame = frame;
        for k in TrapKind::ALL {
            a.bind(k.label()).expect("fresh label");
        }
        emit(&mut a, m).expect("sample instructions encode");
        a.bind(super::machine::FIRST_FREE_LABEL)
            .expect("fresh label");
        a.bind(9).expect("fresh label");
        let code = a.finish().expect("labels resolve");
        let _ = write!(out, "{m}{tag}:");
        for b in code {
            let _ = write!(out, " {b:02x}");
        }
        out.push('\n');
    }
    let mut out = String::new();
    for m in super::asm::encoding_samples() {
        line(&mut out, &m, false, 0, "");
    }
    for words in [4, 1000, 2048] {
        line(&mut out, &M::Enter(words), true, 0, " (windows)");
    }
    line(&mut out, &M::Leave, true, 8000, " (windows, frame 8000)");
    out
}

/// `push rbp; mov rbp, rsp; sub rsp, bytes`. On Windows, frames of a page
/// or more first touch each page below `rsp` in order (`test [rsp - 4096k],
/// esp`), as `__chkstk` would, so the guard page grows the stack, and the
/// prologue is recorded for the unwind tables.
fn prologue(a: &mut Asm, bytes: u32) {
    let start = a.here();
    a.bytes(&PUSH_RBP);
    let push_end = (a.here() - start) as u8;
    a.bytes(&MOV_RBP_RSP);
    if a.windows {
        for page in 1..=bytes / PAGE_BYTES {
            a.bytes(&TEST_RSP_DISP32_ESP);
            a.bytes(&(-((page * PAGE_BYTES) as i32)).to_le_bytes());
        }
    }
    a.bytes(&SUB_RSP_IMM32);
    a.bytes(&bytes.to_le_bytes());
    a.frame = bytes;
    if a.windows {
        a.prologues.push(super::asm::Prologue {
            start,
            push_end,
            alloc_end: (a.here() - start) as u8,
            frame: bytes,
            end: None,
        });
    }
}

/// `call` an external symbol (resolved by the linker).
fn call_external(a: &mut Asm, name: &'static str) {
    a.bytes(&[OPCODE_CALL_REL32]);
    let at = a.here();
    a.bytes(&[0; 4]);
    a.externals.push((at, name));
}

/// The Windows C `main`: reserve the result area, call the entry, optionally
/// write the result words with `WriteFile`, and return the status to the
/// C runtime. The abort exits call `ExitProcess`. Returns the offset of
/// `main` in the code.
pub fn start_windows(
    a: &mut Asm,
    protocol: ResultProtocol,
    result_words: u32,
    entry: Label,
) -> Result<usize, VerifiedError> {
    let main = a.here();
    let area = ((result_words.max(1) + 1) & !1) * WORD_BYTES;
    prologue(a, area);
    rel(a, OPCODE_CALL_REL32, entry);
    if protocol == ResultProtocol::Words && result_words > 0 {
        // sub rsp, 48 (shadow space, the fifth argument, `written`)
        a.bytes(&SUB_RSP_48);
        // mov ecx, -11 (STD_OUTPUT_HANDLE); call GetStdHandle
        a.bytes(&[OPCODE_MOV_IMM + RCX]);
        a.bytes(&STD_OUTPUT_HANDLE.to_le_bytes());
        call_external(a, "GetStdHandle");
        // mov rcx, rax; lea rdx, [rsp + 48]; mov r8d, len; lea r9, [rsp + 40];
        // mov qword [rsp + 32], 0; call WriteFile; add rsp, 48
        a.bytes(&WRITE_FILE_ARGS);
        a.bytes(&(result_words * WORD_BYTES).to_le_bytes());
        a.bytes(&LEA_R9_RSP_40);
        a.bytes(&MOV_QWORD_RSP_32_ZERO);
        call_external(a, "WriteFile");
        a.bytes(&ADD_RSP_48);
    }
    if protocol == ResultProtocol::ExitStatus && result_words > 0 {
        // movzx eax, byte [rsp]
        a.bytes(&MOVZX_EAX_BYTE_RSP);
    } else {
        // xor eax, eax
        a.bytes(&XOR_EAX_EAX);
    }
    emit(a, &M::Leave)?;
    let here = a.here();
    if let Some(p) = a.prologues.last_mut() {
        p.end = Some(here);
    }
    for k in TrapKind::ALL {
        a.bind(k.label())?;
        // Function bodies keep rsp 16-byte aligned.
        // sub rsp, 32; mov ecx, status; call ExitProcess
        a.bytes(&SUB_RSP_32);
        a.bytes(&[OPCODE_MOV_IMM + RCX, k.exit_status(), 0, 0, 0]);
        call_external(a, "ExitProcess");
        a.bytes(&[INT3]);
    }
    Ok(main)
}

/// `_start`: reserve the entry's result area, call it, optionally write the
/// result words to stdout, and exit.
pub fn start(
    a: &mut Asm,
    os: Os,
    protocol: ResultProtocol,
    result_words: u32,
    entry: Label,
) -> Result<(), VerifiedError> {
    if os != Os::Linux {
        return Err(VerifiedError::Target(
            "x86-64 output is a Linux ELF executable only".into(),
        ));
    }
    let area = (result_words.max(1) + 1) & !1;
    // sub rsp, imm32; call entry
    a.bytes(&SUB_RSP_IMM32);
    a.bytes(&(area * WORD_BYTES).to_le_bytes());
    rel(a, OPCODE_CALL_REL32, entry);
    if protocol == ResultProtocol::Words && result_words > 0 {
        // mov eax, 1 (write); mov edi, 1; mov rsi, rsp; mov edx, len; syscall
        a.bytes(&[OPCODE_MOV_IMM + RAX, SYS_WRITE, 0, 0, 0]);
        a.bytes(&[OPCODE_MOV_IMM + RDI, STDOUT, 0, 0, 0]);
        a.bytes(&MOV_RSI_RSP);
        a.bytes(&[OPCODE_MOV_IMM + RDX]);
        a.bytes(&(result_words * WORD_BYTES).to_le_bytes());
        a.bytes(&SYSCALL);
    }
    if protocol == ResultProtocol::ExitStatus && result_words > 0 {
        // mov rdi, [rsp]
        a.bytes(&MOV_RDI_RSP_WORD);
    } else {
        // xor edi, edi
        a.bytes(&XOR_EDI_EDI);
    }
    // mov eax, 60 (exit); syscall
    a.bytes(&[OPCODE_MOV_IMM + RAX, SYS_EXIT, 0, 0, 0]);
    a.bytes(&SYSCALL);
    for k in TrapKind::ALL {
        a.bind(k.label())?;
        // mov edi, status; mov eax, 60; syscall
        a.bytes(&[OPCODE_MOV_IMM + RDI, k.exit_status(), 0, 0, 0]);
        a.bytes(&[OPCODE_MOV_IMM + RAX, SYS_EXIT, 0, 0, 0]);
        a.bytes(&SYSCALL);
    }
    Ok(())
}
