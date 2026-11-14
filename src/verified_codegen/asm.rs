//! A tiny label-resolving assembler buffer shared by both encoders.
use super::VerifiedError;
use super::machine::Label;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fix {
    /// x86-64 rel32 at `at`, relative to the end of the field.
    Rel32,
    /// AArch64 `b`/`bl` imm26 in the instruction word at `at`.
    Imm26,
    /// AArch64 `b.cond`/`cbz` imm19 in the instruction word at `at`.
    Imm19,
}

#[derive(Default)]
pub struct Asm {
    pub code: Vec<u8>,
    labels: BTreeMap<Label, usize>,
    fixups: Vec<(usize, Label, Fix)>,
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
