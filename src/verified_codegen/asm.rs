//! A tiny label-resolving assembler buffer shared by the encoders.
use super::VerifiedError;
use super::machine::{Cond, FIRST_FREE_LABEL, FloatOp, Label, Loc, M, Reg, Sign, TrapKind, WrapOp};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fix {
    /// x86-64 rel32 at `at`, relative to the end of the field.
    Rel32,
    /// AArch64 `b`/`bl` imm26 in the instruction word at `at`.
    Imm26,
    /// AArch64 `b.cond`/`cbz` imm19 in the instruction word at `at`.
    Imm19,
    /// RISC-V `jal` imm20 (±1 MiB) in the instruction word at `at`.
    Jal,
}

/// A function's prologue, for Windows unwind tables: where it starts, how
/// many bytes it is, and the frame it allocates below the saved `rbp`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prologue {
    pub start: usize,
    /// Offset of the end of `push rbp` from `start`.
    pub push_end: u8,
    /// Offset of the end of `sub rsp, frame` from `start`.
    pub alloc_end: u8,
    pub frame: u32,
    /// Where the function's unwind range ends, when it is not the next
    /// prologue's start.
    pub end: Option<usize>,
}

#[derive(Default)]
pub struct Asm {
    pub code: Vec<u8>,
    labels: BTreeMap<Label, usize>,
    fixups: Vec<(usize, Label, Fix)>,
    /// Emit for Windows x64 (stack probes, canonical epilogues, unwind
    /// prologues).
    pub windows: bool,
    /// The frame of the function being emitted (for Windows epilogues).
    pub frame: u32,
    pub prologues: Vec<Prologue>,
    /// `rel32` fields that call an external symbol.
    pub externals: Vec<(usize, &'static str)>,
}
impl Asm {
    pub fn bytes(&mut self, b: &[u8]) {
        self.code.extend_from_slice(b);
    }
    pub fn u32(&mut self, w: u32) {
        self.code.extend_from_slice(&w.to_le_bytes());
    }
    pub fn here(&self) -> usize {
        self.code.len()
    }
    pub fn bind(&mut self, l: Label) -> Result<(), VerifiedError> {
        if self.labels.insert(l, self.code.len()).is_some() {
            return Err(VerifiedError::Invalid(format!("label L{l} bound twice")));
        }
        Ok(())
    }
    pub fn fixup(&mut self, at: usize, l: Label, kind: Fix) {
        self.fixups.push((at, l, kind));
    }
    /// Resolve every label reference; the code is final afterwards.
    pub fn finish(mut self) -> Result<Vec<u8>, VerifiedError> {
        for (at, l, kind) in std::mem::take(&mut self.fixups) {
            let target = *self
                .labels
                .get(&l)
                .ok_or_else(|| VerifiedError::Invalid(format!("unbound label L{l}")))?
                as i64;
            match kind {
                Fix::Rel32 => {
                    let rel = target - (at as i64 + 4);
                    let rel = i32::try_from(rel)
                        .map_err(|_| VerifiedError::Limit("x86-64 branch out of range".into()))?;
                    self.code[at..at + 4].copy_from_slice(&rel.to_le_bytes());
                }
                Fix::Jal => {
                    let rel = target - at as i64;
                    if !(-(1i64 << 20)..(1i64 << 20)).contains(&rel) {
                        return Err(VerifiedError::Limit("RISC-V jump out of range".into()));
                    }
                    let imm = rel as u32;
                    let field = ((imm >> 20) & 1) << 31
                        | ((imm >> 1) & 0x3ff) << 21
                        | ((imm >> 11) & 1) << 20
                        | ((imm >> 12) & 0xff) << 12;
                    let mut w = u32::from_le_bytes(self.code[at..at + 4].try_into().unwrap());
                    w |= field;
                    self.code[at..at + 4].copy_from_slice(&w.to_le_bytes());
                }
                Fix::Imm26 | Fix::Imm19 => {
                    let words = (target - at as i64) / 4;
                    let (bits, shift) = if kind == Fix::Imm26 { (26, 0) } else { (19, 5) };
                    let limit = 1i64 << (bits - 1);
                    if !(-limit..limit).contains(&words) {
                        return Err(VerifiedError::Limit("AArch64 branch out of range".into()));
                    }
                    let field = ((words as u64 & ((1u64 << bits) - 1)) as u32) << shift;
                    let mut w = u32::from_le_bytes(self.code[at..at + 4].try_into().unwrap());
                    w |= field;
                    self.code[at..at + 4].copy_from_slice(&w.to_le_bytes());
                }
            }
        }
        Ok(self.code)
    }
}

/// Sample machine instructions covering every instruction kind (and both
/// frame-offset ranges of each target), for the encoding listings the Lean
/// encoding models print.
pub fn encoding_samples() -> Vec<M> {
    use Loc::{In, Out, Slot};
    use Reg::{R0, R1, R2};
    const CONDS: [Cond; 6] = [Cond::Eq, Cond::Ne, Cond::Lt, Cond::Le, Cond::Gt, Cond::Ge];
    let mut samples = vec![
        M::Ld(R0, Slot(1)),
        M::Ld(R1, Slot(300)),
        M::St(Out(2), R0),
        M::Ld(R2, In(3)),
        M::St(In(400), R1),
        M::Imm(R0, 0x0123_4567_89ab_cdef),
        M::Imm(R2, u64::MAX),
    ];
    for s in [Sign::S, Sign::U] {
        samples.extend([M::AddChk(s), M::SubChk(s), M::MulChk(s)]);
    }
    samples.extend([
        M::Wrap(WrapOp::Add),
        M::Wrap(WrapOp::Sub),
        M::Wrap(WrapOp::Mul),
        M::AndR2,
        M::TrapGtU,
        M::TrapZero,
        M::TrapDivOvf,
        M::Div(Sign::S),
        M::Div(Sign::U),
        M::Rem(Sign::S),
        M::Rem(Sign::U),
        M::NegChk(Sign::S),
        M::NegChk(Sign::U),
        M::XorOne,
    ]);
    for c in CONDS {
        samples.extend([M::Cmp(c, Sign::S), M::Cmp(c, Sign::U)]);
    }
    samples.extend([FloatOp::Add, FloatOp::Sub, FloatOp::Mul, FloatOp::Div].map(M::FOp));
    samples.push(M::FNeg);
    samples.extend(CONDS.map(M::FCmp));
    samples.extend([
        M::Jmp(9),
        M::Jmp(FIRST_FREE_LABEL),
        M::Bz(9),
        M::Call(9),
        M::Enter(4),
        M::Enter(300),
        M::Leave,
    ]);
    samples.extend(TrapKind::ALL.map(M::Trap));

    samples
}

/// The words `emit` writes for each sample, one line per instruction
/// (`<instruction>: <word> ...`, words as eight hex digits). The abort exits
/// are bound before the instruction, and labels `FIRST_FREE_LABEL` and 9 after
/// it.
pub fn encoding_listing(
    samples: &[M],
    emit: fn(&mut Asm, &M) -> Result<(), VerifiedError>,
) -> String {
    let mut out = String::new();
    for m in samples {
        let mut a = Asm::default();
        for k in TrapKind::ALL {
            a.bind(k.label()).expect("fresh label");
        }
        emit(&mut a, m).expect("sample instructions encode");
        a.bind(FIRST_FREE_LABEL).expect("fresh label");
        a.bind(9).expect("fresh label");
        let code = a.finish().expect("labels resolve");
        out.push_str(&format!("{m}:"));
        for word in code.as_chunks::<4>().0 {
            out.push_str(&format!(" {:08x}", u32::from_le_bytes(*word)));
        }
        out.push('\n');
    }
    out
}
