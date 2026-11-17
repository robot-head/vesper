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
    let ovf = a.trap_label(TrapKind::Overflow);
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
            let l = a.trap_label(TrapKind::DivisionByZero);
            jcc(a, JE, l);
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
        // movq xmm0, rax; movq xmm1, rcx; <op>sd xmm0, xmm1; movq rax, xmm0
        M::FOp(op) => {
            let opcode = match op {
                FloatOp::Add => 0x58,
                FloatOp::Sub => 0x5c,
                FloatOp::Mul => 0x59,
                FloatOp::Div => 0x5e,
            };
            a.bytes(&[0x66, 0x48, 0x0f, 0x6e, 0xc0, 0x66, 0x48, 0x0f, 0x6e, 0xc9]);
            a.bytes(&[0xf2, 0x0f, opcode, 0xc1]);
            a.bytes(&[0x66, 0x48, 0x0f, 0x7e, 0xc0]);
        }
        // btc rax, 63
        M::FNeg => a.bytes(&[0x48, 0x0f, 0xba, 0xf8, 0x3f]),
        // movq xmm0, rax; movq xmm1, rcx; ucomisd; then a NaN-aware setcc.
        // `ucomisd` sets ZF, PF and CF on an unordered pair.
        M::FCmp(c) => {
            a.bytes(&[0x66, 0x48, 0x0f, 0x6e, 0xc0, 0x66, 0x48, 0x0f, 0x6e, 0xc9]);
            // ucomisd xmm0, xmm1 / ucomisd xmm1, xmm0
            const AB: [u8; 4] = [0x66, 0x0f, 0x2e, 0xc1];
            const BA: [u8; 4] = [0x66, 0x0f, 0x2e, 0xc8];
            match c {
                // ucomisd; mov eax, 0|1; jp +3; sete/setne al
                Cond::Eq | Cond::Ne => {
                    let (init, cc) = if c == Cond::Eq { (0, 0x94) } else { (1, 0x95) };
                    a.bytes(&AB);
                    a.bytes(&[0xb8, init, 0, 0, 0, 0x7a, 0x03, 0x0f, cc, 0xc0]);
                }
                // a > b: seta (CF = 0 and ZF = 0); a >= b: setae (CF = 0).
                // a < b and a <= b swap the operands.
                _ => {
                    let (order, cc) = match c {
                        Cond::Gt => (AB, 0x97),
                        Cond::Ge => (AB, 0x93),
                        Cond::Lt => (BA, 0x97),
                        _ => (BA, 0x93),
                    };
                    a.bytes(&order);
                    a.bytes(&[0x0f, cc, 0xc0, 0x0f, 0xb6, 0xc0]);
                }
            }
        }
        // mov r11d, HEAP_BASE; mov rax, [r11 + rcx*8 + 8*off]
        // (mov [r11 + rcx*8 + 8*off], rax).
        M::LdH(off) | M::StH(off) => {
            a.bytes(&[0x41, 0xbb]);
            a.bytes(&(super::HEAP_BASE as u32).to_le_bytes());
            let opcode = if matches!(m, M::LdH(_)) { 0x8b } else { 0x89 };
            a.bytes(&[0x49, opcode, 0x84, 0xcb]);
            a.bytes(&(off * 8).to_le_bytes());
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
        M::Enter(words) => prologue(a, words * 8),
        // mov rsp, rbp; pop rbp; ret  |  Windows: add rsp, frame; pop rbp; ret
        // (the only epilogue shape the Windows x64 unwinder recognises)
        M::Leave if a.windows => {
            let frame = a.frame;
            a.bytes(&[0x48, 0x81, 0xc4]);
            a.bytes(&frame.to_le_bytes());
            a.bytes(&[0x5d, 0xc3]);
        }
        M::Leave => a.bytes(&[0x48, 0x89, 0xec, 0x5d, 0xc3]),
        M::Trap(k) => {
            let l = a.trap_label(k);
            rel(a, 0xe9, l)
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
    a.bytes(&[0x55]);
    let push_end = (a.here() - start) as u8;
    a.bytes(&[0x48, 0x89, 0xe5]);
    if a.windows {
        for page in 1..=bytes / 4096 {
            a.bytes(&[0x85, 0xa4, 0x24]);
            a.bytes(&(-((page * 4096) as i32)).to_le_bytes());
        }
    }
    a.bytes(&[0x48, 0x81, 0xec]);
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
    a.bytes(&[0xe8]);
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
    let area = ((result_words.max(1) + 1) & !1) * 8;
    prologue(a, area);
    rel(a, 0xe8, entry);
    if protocol == ResultProtocol::Words && result_words > 0 {
        // sub rsp, 48 (shadow space, the fifth argument, `written`)
        a.bytes(&[0x48, 0x83, 0xec, 0x30]);
        // mov ecx, -11 (STD_OUTPUT_HANDLE); call GetStdHandle
        a.bytes(&[0xb9, 0xf5, 0xff, 0xff, 0xff]);
        call_external(a, "GetStdHandle");
        // mov rcx, rax; lea rdx, [rsp + 48]; mov r8d, len; lea r9, [rsp + 40];
        // mov qword [rsp + 32], 0; call WriteFile; add rsp, 48
        a.bytes(&[0x48, 0x89, 0xc1, 0x48, 0x8d, 0x54, 0x24, 0x30, 0x41, 0xb8]);
        a.bytes(&(result_words * 8).to_le_bytes());
        a.bytes(&[0x4c, 0x8d, 0x4c, 0x24, 0x28]);
        a.bytes(&[0x48, 0xc7, 0x44, 0x24, 0x20, 0, 0, 0, 0]);
        call_external(a, "WriteFile");
        a.bytes(&[0x48, 0x83, 0xc4, 0x30]);
    }
    if protocol == ResultProtocol::ExitStatus && result_words > 0 {
        // movzx eax, byte [rsp]
        a.bytes(&[0x0f, 0xb6, 0x04, 0x24]);
    } else {
        // xor eax, eax
        a.bytes(&[0x31, 0xc0]);
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
        a.bytes(&[0x48, 0x83, 0xec, 0x20, 0xb9, k.exit_status(), 0, 0, 0]);
        call_external(a, "ExitProcess");
        a.bytes(&[0xcc]);
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
