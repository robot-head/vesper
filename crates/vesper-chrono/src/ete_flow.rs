//! AArch64 control-flow reconstruction from ETE / ETMv4 atoms over a code
//! image (§31.1 item 3).
//!
//! The ETE instruction trace only reports *P0 elements* (branches) as atoms
//! and only reports addresses where the code cannot predict them, so the
//! instruction-level path is rebuilt by walking the traced code: from the
//! current address, A64 instructions are decoded one at a time until the
//! next branch, which an atom then resolves.
//!
//! * `B`, `BL`, `B.cond`/`BC.cond`, `CBZ`/`CBNZ`, `TBZ`/`TBNZ`: the target is
//!   computed from the encoding. E is taken, N is not taken (an N atom for an
//!   unconditional `B`/`BL` is a mismatch).
//! * `BR`, `BLR`, `RET`, `ERET` and their pointer-authenticating forms: an E
//!   atom is followed by an address element naming the target (return-stack
//!   compression is assumed off, as the trace-info element of a stream
//!   without it reports). An N atom is a mismatch.
//! * An exception element (preferred return address) ends the straight-line
//!   walk there; the next address element is the vector it went to.
//! * An address element that no branch is waiting for (after sync, trace-on
//!   or overflow) restarts the walk there.
//! * Overflow and discard drop the current position until the next address.
//!
//! Each branch is stamped with the timestamp interval it lies in, as
//! [`crate::ipt_flow`] does with TSC packets, and is reported with
//! [`crate::ipt_flow`]'s `Branch` and `BranchKind`, so
//! [`crate::BranchTimeline::steps_from_pt`] places either trace kind on the
//! effect timeline. `ISB`, `WFI`/`WFE` and other instructions the trace may
//! optionally report as P0 elements are treated as ordinary instructions;
//! cycle-count, conditional-instruction and speculation (commit/cancel)
//! tracing are rejected by the element decoder.
//!
//! Tested with constructed traces over constructed A64 code, not with a
//! trace captured on Arm hardware.

use std::fmt;

use crate::ete::{EteElement, EteElementAt, InstructionSet};
use crate::ipt_flow::{Branch, BranchKind, CodeImage, FlowLimits, FlowTrace};

/// How an A64 instruction changes control flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum A64Flow {
    /// Not a branch.
    Sequential,
    /// `B` (`call` false) or `BL` (`call` true).
    Direct { target: u64, call: bool },
    /// `B.cond`, `BC.cond`, `CBZ`, `CBNZ`, `TBZ`, `TBNZ`.
    Conditional { target: u64 },
    /// `BR`, `BLR` and the `BRA*`/`BLRA*` forms.
    Indirect { call: bool },
    /// `RET`, `RETAA`, `RETAB`.
    Return,
    /// `ERET`, `ERETAA`, `ERETAB`.
    ExceptionReturn,
}

fn sext(value: u32, bits: u32) -> i64 {
    let shift = 64 - bits;
    ((u64::from(value) << shift) as i64) >> shift
}

/// Classify the A64 instruction `insn` at `pc`. `None` for encodings in
/// the unconditional-branch-(register) class that this module does not
/// model (`DRPS` and reserved forms).
pub fn classify(pc: u64, insn: u32) -> Option<A64Flow> {
    let rel = |imm: u32, bits: u32| pc.wrapping_add((sext(imm, bits) * 4) as u64);
    Some(match insn {
        // B / BL: op 0x14 / 0x94, imm26.
        i if i & 0x7C00_0000 == 0x1400_0000 => A64Flow::Direct {
            target: rel(i & 0x03FF_FFFF, 26),
            call: i >> 31 == 1,
        },
        // B.cond / BC.cond: 0101_0100, imm19 at [23:5].
        i if i & 0xFF00_0000 == 0x5400_0000 => A64Flow::Conditional {
            target: rel((i >> 5) & 0x7FFFF, 19),
        },
        // CBZ / CBNZ: x011_010x, imm19.
        i if i & 0x7E00_0000 == 0x3400_0000 => A64Flow::Conditional {
            target: rel((i >> 5) & 0x7FFFF, 19),
        },
        // TBZ / TBNZ: x011_011x, imm14 at [18:5].
        i if i & 0x7E00_0000 == 0x3600_0000 => A64Flow::Conditional {
            target: rel((i >> 5) & 0x3FFF, 14),
        },
        // Unconditional branch (register): 1101_011 opc op2=11111.
        i if i & 0xFE1F_0000 == 0xD61F_0000 => match (i >> 21) & 0xF {
            0 | 8 => A64Flow::Indirect { call: false },
            1 | 9 => A64Flow::Indirect { call: true },
            2 => A64Flow::Return,
            4 => A64Flow::ExceptionReturn,
            _ => return None,
        },
        _ => A64Flow::Sequential,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EteFlowError {
    /// An atom or exception while no address is known (before sync, or
    /// after an overflow or an indirect branch whose target never came).
    NoAddress {
        element_index: usize,
    },
    /// Execution reached an address outside the image (or not 4-aligned).
    IpOutsideImage {
        ip: u64,
        element_index: usize,
    },
    /// An N atom for a branch that cannot fall through.
    UnexpectedNotTaken {
        ip: u64,
        element_index: usize,
    },
    /// An instruction encoding this module does not model.
    Unsupported {
        ip: u64,
        insn: u32,
    },
    /// The walk to an exception's preferred return address met a branch.
    ExceptionPastBranch {
        ip: u64,
        preferred_return: u64,
        element_index: usize,
    },
    /// A T32 (IS1) address in an A64 trace.
    NotA64 {
        element_index: usize,
    },
    InstructionBudget {
        limit: u64,
    },
}

impl fmt::Display for EteFlowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ete flow: {self:?}")
    }
}
impl std::error::Error for EteFlowError {}

enum Waiting {
    Nothing,
    /// An indirect branch at `from` was taken; its target is the next
    /// address element.
    Indirect {
        from: u64,
        kind: BranchKind,
    },
    /// An exception at this preferred return address; the next address is
    /// the vector.
    Exception {
        from: u64,
    },
}

struct Walker<'i> {
    image: &'i CodeImage<'i>,
    limits: FlowLimits,
    pc: Option<u64>,
    waiting: Waiting,
    ts: Option<u64>,
    open_from: usize,
    trace: FlowTrace,
}

impl Walker<'_> {
    fn insn(&self, ip: u64, element_index: usize) -> Result<u32, EteFlowError> {
        let outside = EteFlowError::IpOutsideImage { ip, element_index };
        if !ip.is_multiple_of(4) {
            return Err(outside);
        }
        let bytes = self.image.at(ip).ok_or(outside.clone())?;
        let word = bytes.get(..4).ok_or(outside)?;
        Ok(u32::from_le_bytes(word.try_into().unwrap()))
    }
    fn step(&mut self) -> Result<(), EteFlowError> {
        self.trace.instructions += 1;
        if self.trace.instructions > self.limits.max_instructions {
            return Err(EteFlowError::InstructionBudget {
                limit: self.limits.max_instructions,
            });
        }
        Ok(())
    }
    fn push(&mut self, from: u64, to: Option<u64>, kind: BranchKind, element_index: usize) {
        self.trace.branches.push(Branch {
            from,
            to,
            kind,
            tsc_lo: self.ts,
            tsc_hi: None,
            packet_index: element_index,
        });
    }
    /// Walk straight-line code to the next branch and return it.
    fn walk_to_branch(&mut self, element_index: usize) -> Result<(u64, A64Flow), EteFlowError> {
        let mut ip = self.pc.ok_or(EteFlowError::NoAddress { element_index })?;
        loop {
            let insn = self.insn(ip, element_index)?;
            self.step()?;
            match classify(ip, insn).ok_or(EteFlowError::Unsupported { ip, insn })? {
                A64Flow::Sequential => ip = ip.wrapping_add(4),
                flow => return Ok((ip, flow)),
            }
        }
    }
    fn atom(&mut self, executed: bool, i: usize) -> Result<(), EteFlowError> {
        let (ip, flow) = self.walk_to_branch(i)?;
        let not_taken = EteFlowError::UnexpectedNotTaken {
            ip,
            element_index: i,
        };
        match flow {
            A64Flow::Sequential => unreachable!("walk_to_branch stops at branches"),
            A64Flow::Conditional { target } => {
                let to = if executed { target } else { ip.wrapping_add(4) };
                self.push(ip, Some(to), BranchKind::Conditional { taken: executed }, i);
                self.pc = Some(to);
            }
            A64Flow::Direct { target, call } => {
                if !executed {
                    return Err(not_taken);
                }
                let kind = if call {
                    BranchKind::DirectCall
                } else {
                    BranchKind::DirectJump
                };
                self.push(ip, Some(target), kind, i);
                self.pc = Some(target);
            }
            A64Flow::Indirect { .. } | A64Flow::Return | A64Flow::ExceptionReturn => {
                if !executed {
                    return Err(not_taken);
                }
                let kind = match flow {
                    A64Flow::Indirect { call: true } => BranchKind::IndirectCall,
                    A64Flow::Indirect { call: false } => BranchKind::IndirectJump,
                    A64Flow::Return => BranchKind::Return { compressed: false },
                    _ => BranchKind::Far,
                };
                self.waiting = Waiting::Indirect { from: ip, kind };
                self.pc = None;
            }
        }
        Ok(())
    }
    fn exception(&mut self, preferred_return: u64, i: usize) -> Result<(), EteFlowError> {
        // Instructions from the current position up to (not including) the
        // preferred return address ran before the exception was taken. With
        // no known position (an exception right after sync) nothing is
        // walked.
        if let Some(mut ip) = self.pc {
            while ip != preferred_return {
                let insn = self.insn(ip, i)?;
                match classify(ip, insn).ok_or(EteFlowError::Unsupported { ip, insn })? {
                    A64Flow::Sequential => {
                        self.step()?;
                        ip = ip.wrapping_add(4);
                    }
                    _ => {
                        return Err(EteFlowError::ExceptionPastBranch {
                            ip,
                            preferred_return,
                            element_index: i,
                        });
                    }
                }
            }
        }
        self.waiting = Waiting::Exception {
            from: preferred_return,
        };
        self.pc = None;
        Ok(())
    }
    fn address(&mut self, address: u64, isa: InstructionSet, i: usize) -> Result<(), EteFlowError> {
        if isa != InstructionSet::Is0 {
            return Err(EteFlowError::NotA64 { element_index: i });
        }
        match std::mem::replace(&mut self.waiting, Waiting::Nothing) {
            Waiting::Indirect { from, kind } => self.push(from, Some(address), kind, i),
            Waiting::Exception { from } => self.push(from, Some(address), BranchKind::Far, i),
            // A position the walk would have reached anyway is only a
            // confirmation; anything else restarts the walk there.
            Waiting::Nothing if self.pc == Some(address) => {}
            Waiting::Nothing => self.push(address, Some(address), BranchKind::Enable, i),
        }
        self.pc = Some(address);
        Ok(())
    }
    fn lost(&mut self) {
        self.pc = None;
        self.waiting = Waiting::Nothing;
    }
}

/// Rebuild the A64 branch trace of decoded ETE / ETMv4 `elements` over
/// `image`. Address and exception elements are resolved against the code;
/// trace-info, context and event elements carry no control flow and are
/// skipped. A trailing indirect branch whose target never arrived is left
/// out and reported through `stopped_at` (its address).
pub fn reconstruct(
    elements: &[EteElementAt],
    image: &CodeImage<'_>,
    limits: &FlowLimits,
) -> Result<FlowTrace, EteFlowError> {
    let mut w = Walker {
        image,
        limits: *limits,
        pc: None,
        waiting: Waiting::Nothing,
        ts: None,
        open_from: 0,
        trace: FlowTrace::default(),
    };
    for (i, e) in elements.iter().enumerate() {
        match e.element {
            EteElement::Atom { executed } => w.atom(executed, i)?,
            EteElement::Address { address, isa } => w.address(address, isa, i)?,
            EteElement::Exception {
                preferred_return, ..
            } => w.exception(preferred_return, i)?,
            EteElement::Timestamp { timestamp, .. } => {
                for b in &mut w.trace.branches[w.open_from..] {
                    b.tsc_hi = Some(timestamp);
                }
                w.open_from = w.trace.branches.len();
                w.ts = Some(timestamp);
            }
            EteElement::Overflow => {
                w.trace.overflows += 1;
                w.lost();
            }
            EteElement::Discard | EteElement::TraceOn | EteElement::Sync => w.lost(),
            EteElement::TraceInfo { .. }
            | EteElement::Context(_)
            | EteElement::Event { .. }
            | EteElement::ExceptionReturn => {}
        }
    }
    w.trace.stopped_at = match w.waiting {
        Waiting::Indirect { from, .. } => Some(from),
        Waiting::Exception { from } => Some(from),
        Waiting::Nothing => w.pc,
    };
    Ok(w.trace)
}
