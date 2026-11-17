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
    /// RISC-V `auipc rd, hi` at `at` and `jalr rd, lo(rd)` after it
    /// (±2 GiB), relative to the `auipc`.
    AuipcJalr,
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
    /// Further copies of the abort exits (labels below `FIRST_FREE_LABEL`):
    /// a branch to one resolves to the nearest copy.
    copies: BTreeMap<Label, Vec<usize>>,
    fixups: Vec<(usize, Label, Fix)>,
    /// Emit for Windows x64 (stack probes, canonical epilogues, unwind
    /// prologues).
    pub windows: bool,
    /// The frame of the function being emitted (for Windows epilogues).
    pub frame: u32,
    pub prologues: Vec<Prologue>,
    /// `rel32` fields that call an external symbol.
    pub externals: Vec<(usize, &'static str)>,
    /// The trap scope set by the last `M::Scope` (`None`: the program-wide
    /// abort exits).
    pub scope: Option<(Label, Label)>,
}
impl Asm {
    pub fn bytes(&mut self, b: &[u8]) {
        self.code.extend_from_slice(b);
    }
    pub fn u32(&mut self, w: u32) {
        self.code.extend_from_slice(&w.to_le_bytes());
    }
    /// Where an abort of kind `k` jumps in the current trap scope.
    pub fn trap_label(&self, k: TrapKind) -> Label {
        match (self.scope, k) {
            (Some((ovf, _)), TrapKind::Overflow) => ovf,
            (Some((_, div0)), TrapKind::DivisionByZero) => div0,
            _ => k.label(),
        }
    }
    /// Set the trap scope (`M::Scope`); the default labels clear it.
    pub fn set_scope(&mut self, ovf: Label, div0: Label) {
        self.scope = (ovf != TrapKind::Overflow.label()
            || div0 != TrapKind::DivisionByZero.label())
        .then_some((ovf, div0));
    }
    pub fn here(&self) -> usize {
        self.code.len()
    }
    /// Bind another copy of the abort exit `l` here.
    pub fn bind_copy(&mut self, l: Label) {
        self.copies.entry(l).or_default().push(self.code.len());
    }
    /// Where the last copy of the abort exits was bound.
    pub fn last_exits(&self) -> usize {
        self.copies
            .values()
            .flatten()
            .chain(
                self.labels
                    .iter()
                    .filter(|(l, _)| **l < FIRST_FREE_LABEL)
                    .map(|(_, at)| at),
            )
            .copied()
            .max()
            .unwrap_or(0)
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
            let mut target = *self
                .labels
                .get(&l)
                .ok_or_else(|| VerifiedError::Invalid(format!("unbound label L{l}")))?
                as i64;
            // The nearest copy of an abort exit.
            for &c in self.copies.get(&l).into_iter().flatten() {
                if (c as i64 - at as i64).abs() < (target - at as i64).abs() {
                    target = c as i64;
                }
            }
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
                Fix::AuipcJalr => {
                    let rel = target - at as i64;
                    let hi = (rel + 0x800) >> 12;
                    let lo = rel - (hi << 12);
                    if !(-(1i64 << 19)..(1i64 << 19)).contains(&hi) {
                        return Err(VerifiedError::Limit("RISC-V call out of range".into()));
                    }
                    let patch = |code: &mut Vec<u8>, at: usize, field: u32| {
                        let w = u32::from_le_bytes(code[at..at + 4].try_into().unwrap()) | field;
                        code[at..at + 4].copy_from_slice(&w.to_le_bytes());
                    };
                    patch(&mut self.code, at, ((hi as u32) & 0xf_ffff) << 12);
                    patch(&mut self.code, at + 4, ((lo as u32) & 0xfff) << 20);
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
    // Milestone 8: heap words.
    samples.extend([M::LdH(3), M::StH(3), M::LdH(300), M::StH(300)]);

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

#[cfg(test)]
mod tests {
    use super::*;

    /// A branch to an abort exit resolves to its nearest copy.
    #[test]
    fn fixups_pick_the_nearest_exit_copy() {
        let l = TrapKind::Overflow.label();
        let mut a = Asm::default();
        a.bind(l).unwrap();
        a.u32(0);
        a.code.resize(0x10_0000, 0);
        a.bind_copy(l);
        a.u32(0);
        let at = a.here();
        a.u32(0x0000_006f); // jal x0, 0
        a.fixup(at, l, Fix::Jal);
        assert_eq!(a.last_exits(), 0x10_0000);
        let code = a.finish().unwrap();
        // Back 4 bytes to the copy: imm = -4.
        let w = u32::from_le_bytes(code[at..at + 4].try_into().unwrap());
        assert_eq!(w, 0xffdf_f06f);
    }

    /// `auipc` + `jalr` reaches a target 3 MiB away, past `jal`'s range.
    #[test]
    fn auipc_jalr_reaches_far_targets() {
        let target = 3 << 20;
        let mut a = Asm::default();
        a.u32(0x0000_0097); // auipc ra, 0
        a.u32(0x0000_80e7); // jalr ra, 0(ra)
        a.fixup(0, FIRST_FREE_LABEL, Fix::AuipcJalr);
        a.code.resize(target + 0x7f4, 0);
        a.bind(FIRST_FREE_LABEL).unwrap();
        let code = a.finish().unwrap();
        let hi = u32::from_le_bytes(code[0..4].try_into().unwrap()) as i32 >> 12;
        let lo = u32::from_le_bytes(code[4..8].try_into().unwrap()) as i32 >> 20;
        assert_eq!(((hi as i64) << 12) + lo as i64, (target + 0x7f4) as i64);
        // A rounding case: the low part is negative.
        let mut a = Asm::default();
        a.u32(0x0000_0097);
        a.u32(0x0000_80e7);
        a.fixup(0, FIRST_FREE_LABEL, Fix::AuipcJalr);
        a.code.resize(target + 0x900, 0);
        a.bind(FIRST_FREE_LABEL).unwrap();
        let code = a.finish().unwrap();
        let hi = u32::from_le_bytes(code[0..4].try_into().unwrap()) as i32 >> 12;
        let lo = u32::from_le_bytes(code[4..8].try_into().unwrap()) as i32 >> 20;
        assert!(lo < 0);
        assert_eq!(((hi as i64) << 12) + lo as i64, (target + 0x900) as i64);
    }
}
