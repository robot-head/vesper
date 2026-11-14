//! The x86-64 encoding of each abstract machine instruction.
//!
//! `R0`, `R1`, `R2` are `rax`, `rcx`, `rdx`; frame words are `[rsp + 8n]`
//! (slots and the outgoing area) and `[rbp + 16 + 8n]` (the incoming area).
//! These encodings are tested by running the emitted programs; they are not
//! covered by the Lean theorem.
use super::asm::{Asm, Fix};
use super::machine::{Cond, Label, Loc, M, Reg, Sign, TrapKind, WrapOp};
use super::{Os, ResultProtocol, VerifiedError};

fn reg(r: Reg) -> u8 {
    match r {
        Reg::R0 => 0,
        Reg::R1 => 1,
        Reg::R2 => 2,
    }
}

fn mem(a: &mut Asm, opcode: u8, r: Reg, loc: Loc) {
    let r = reg(r) << 3;
    match loc {
        Loc::Slot(n) | Loc::Out(n) => {
            a.bytes(&[0x48, opcode, 0x84 | r, 0x24]);
            a.bytes(&(n * 8).to_le_bytes());
        }
        Loc::In(n) => {
            a.bytes(&[0x48, opcode, 0x85 | r]);
            a.bytes(&(16 + n * 8).to_le_bytes());
        }
    }
}

/// `jcc rel32` (`0f 8x`) or `jmp`/`call` to a label.
fn jcc(a: &mut Asm, cc: u8, l: Label) {
    a.bytes(&[0x0f, 0x80 | cc]);
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

pub fn emit(a: &mut Asm, m: &M) -> Result<(), VerifiedError> {
    let ovf = TrapKind::Overflow.label();
    match *m {
        M::Ld(r, l) => mem(a, 0x8b, r, l),
        M::St(l, r) => mem(a, 0x89, r, l),
        M::Imm(r, w) => {
            a.bytes(&[0x48, 0xb8 + reg(r)]);
            a.bytes(&w.to_le_bytes());
        }
        // add rax, rcx; jo/jb
        M::AddChk(s) => {
            a.bytes(&[0x48, 0x01, 0xc8]);
            jcc(a, if s == Sign::S { JO } else { JB }, ovf);
        }
        // sub rax, rcx; jo/jb
        M::SubChk(s) => {
            a.bytes(&[0x48, 0x29, 0xc8]);
            jcc(a, if s == Sign::S { JO } else { JB }, ovf);
        }
        // imul rax, rcx; jo  |  mul rcx (rdx:rax); jo
        M::MulChk(Sign::S) => {
            a.bytes(&[0x48, 0x0f, 0xaf, 0xc1]);
            jcc(a, JO, ovf);
        }
        M::MulChk(Sign::U) => {
            a.bytes(&[0x48, 0xf7, 0xe1]);
            jcc(a, JO, ovf);
        }
        M::Wrap(WrapOp::Add) => a.bytes(&[0x48, 0x01, 0xc8]),
        M::Wrap(WrapOp::Sub) => a.bytes(&[0x48, 0x29, 0xc8]),
        M::Wrap(WrapOp::Mul) => a.bytes(&[0x48, 0x0f, 0xaf, 0xc1]),
        // and rax, rdx
        M::AndR2 => a.bytes(&[0x48, 0x21, 0xd0]),
        // cmp rax, rdx; ja
        M::TrapGtU => {
            a.bytes(&[0x48, 0x39, 0xd0]);
            jcc(a, JA, ovf);
        }
        // test rcx, rcx; je
        M::TrapZero => {
            a.bytes(&[0x48, 0x85, 0xc9]);
            jcc(a, JE, TrapKind::DivisionByZero.label());
        }
        // movabs rdx, MIN; cmp rax, rdx; jne +10; cmp rcx, -1; je ovf
        M::TrapDivOvf => {
            a.bytes(&[0x48, 0xba]);
            a.bytes(&(i64::MIN as u64).to_le_bytes());
            a.bytes(&[0x48, 0x39, 0xd0, 0x75, 0x0a, 0x48, 0x83, 0xf9, 0xff]);
            jcc(a, JE, ovf);
        }
        // cqo; idiv rcx  |  xor edx, edx; div rcx
        M::Div(Sign::S) => a.bytes(&[0x48, 0x99, 0x48, 0xf7, 0xf9]),
        M::Div(Sign::U) => a.bytes(&[0x31, 0xd2, 0x48, 0xf7, 0xf1]),
        // cmp rcx, -1; jne +4; xor eax, eax; jmp +8; cqo; idiv rcx; mov rax, rdx
        // (`idiv` faults on MIN / -1; every remainder by -1 is 0).
        M::Rem(Sign::S) => a.bytes(&[
            0x48, 0x83, 0xf9, 0xff, 0x75, 0x04, 0x31, 0xc0, 0xeb, 0x08, 0x48, 0x99, 0x48, 0xf7,
            0xf9, 0x48, 0x89, 0xd0,
        ]),
        M::Rem(Sign::U) => a.bytes(&[0x31, 0xd2, 0x48, 0xf7, 0xf1, 0x48, 0x89, 0xd0]),
        // neg rax; jo  |  test rax, rax; jne
        M::NegChk(Sign::S) => {
            a.bytes(&[0x48, 0xf7, 0xd8]);
            jcc(a, JO, ovf);
        }
        M::NegChk(Sign::U) => {
            a.bytes(&[0x48, 0x85, 0xc0]);
            jcc(a, JNE, ovf);
        }
        // xor rax, 1
        M::XorOne => a.bytes(&[0x48, 0x83, 0xf0, 0x01]),
        // cmp rax, rcx; setcc al; movzx eax, al
        M::Cmp(c, s) => {
            let cc = match (c, s) {
                (Cond::Eq, _) => 0x94,
                (Cond::Ne, _) => 0x95,
                (Cond::Lt, Sign::S) => 0x9c,
                (Cond::Le, Sign::S) => 0x9e,
                (Cond::Gt, Sign::S) => 0x9f,
                (Cond::Ge, Sign::S) => 0x9d,
                (Cond::Lt, Sign::U) => 0x92,
                (Cond::Le, Sign::U) => 0x96,
                (Cond::Gt, Sign::U) => 0x97,
                (Cond::Ge, Sign::U) => 0x93,
            };
            a.bytes(&[0x48, 0x39, 0xc8, 0x0f, cc, 0xc0, 0x0f, 0xb6, 0xc0]);
        }
        M::Label(l) => a.bind(l)?,
        M::Jmp(l) => rel(a, 0xe9, l),
        // test rax, rax; je
        M::Bz(l) => {
            a.bytes(&[0x48, 0x85, 0xc0]);
            jcc(a, JE, l);
        }
        M::Call(l) => rel(a, 0xe8, l),
        // push rbp; mov rbp, rsp; sub rsp, imm32
        M::Enter(words) => {
            a.bytes(&[0x55, 0x48, 0x89, 0xe5, 0x48, 0x81, 0xec]);
            a.bytes(&(words * 8).to_le_bytes());
        }
        // mov rsp, rbp; pop rbp; ret
        M::Leave => a.bytes(&[0x48, 0x89, 0xec, 0x5d, 0xc3]),
        M::Trap(k) => rel(a, 0xe9, k.label()),
    }
    Ok(())
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
    a.bytes(&[0x48, 0x81, 0xec]);
    a.bytes(&(area * 8).to_le_bytes());
    rel(a, 0xe8, entry);
    if protocol == ResultProtocol::Words && result_words > 0 {
        // mov eax, 1 (write); mov edi, 1; mov rsi, rsp; mov edx, len; syscall
        a.bytes(&[0xb8, 1, 0, 0, 0, 0xbf, 1, 0, 0, 0, 0x48, 0x89, 0xe6, 0xba]);
        a.bytes(&(result_words * 8).to_le_bytes());
        a.bytes(&[0x0f, 0x05]);
    }
    if protocol == ResultProtocol::ExitStatus && result_words > 0 {
        // mov rdi, [rsp]
        a.bytes(&[0x48, 0x8b, 0x3c, 0x24]);
    } else {
        // xor edi, edi
        a.bytes(&[0x31, 0xff]);
    }
    // mov eax, 60 (exit); syscall
    a.bytes(&[0xb8, 60, 0, 0, 0, 0x0f, 0x05]);
    for k in TrapKind::ALL {
        a.bind(k.label())?;
        // mov edi, status; mov eax, 60; syscall
        a.bytes(&[
            0xbf,
            k.exit_status(),
            0,
            0,
            0,
            0xb8,
            60,
            0,
            0,
            0,
            0x0f,
            0x05,
        ]);
    }
    Ok(())
}
