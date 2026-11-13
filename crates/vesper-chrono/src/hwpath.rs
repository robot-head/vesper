//! Mapping hardware branch traces onto Chrono's effect-boundary timeline.
//!
//! A hardware trace stamps control flow with time-stamp-counter values only
//! at TSC packets, so each reconstructed step lies in an interval
//! `[tsc_lo, tsc_hi)`: after the last TSC packet before it and before the
//! first one after it. Effect records carry their own anchor (the
//! recorder's `time_ns` field, which must hold the same TSC / CNTVCT clock
//! when hardware tracing is active). A step lies *definitely* between two
//! effect records `a` and `b` when `tsc_lo > anchor(a)` and
//! `tsc_hi <= anchor(b)`; a step whose interval straddles either anchor is
//! reported as *uncertain* instead of being assigned to a side.
//!
//! [`reverse_watch_path`] combines this with the O(log N) reverse `inout`
//! watchpoint: it names the effect record that last wrote a place and the
//! instruction-level path from the record before it to that write.

use std::fmt;

use crate::ete::{EteElement, EteElementAt, InstructionSet};
use crate::ipt_flow::{BranchKind, FlowTrace};
use crate::replay::{ReplayError, ReplaySession};
use crate::ring::FlightEvent;
use crate::wire::TraceCursor;

/// What a [`TraceStep`] records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StepKind {
    /// A branch reconstructed from Intel PT.
    Branch(BranchKind),
    /// An ETE/ETM address element (a branch target or synchronisation point).
    Address(InstructionSet),
    /// An ETE/ETM atom at the most recent address.
    Atom {
        executed: bool,
    },
    /// An ETE/ETM exception taken at `from`.
    Exception {
        exception_type: u16,
    },
    ExceptionReturn,
}

/// One step of an instruction-level path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TraceStep {
    pub from: u64,
    pub to: Option<u64>,
    pub kind: StepKind,
    pub tsc_lo: Option<u64>,
    pub tsc_hi: Option<u64>,
}

/// One effect record's sequence and clock anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EffectAnchor {
    pub lamport_seq: u64,
    pub tsc: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    /// Anchors must have strictly increasing sequences and non-decreasing
    /// clocks.
    AnchorsOutOfOrder {
        index: usize,
    },
    /// Step intervals must be non-decreasing (the order hardware emits them).
    StepsOutOfOrder {
        index: usize,
    },
    /// A step whose interval is empty or reversed.
    BadInterval {
        index: usize,
    },
    UnknownSeq {
        lamport_seq: u64,
    },
    /// `from` must be before `to`.
    Reversed {
        from: u64,
        to: u64,
    },
    /// There is no effect record before this one.
    NoPreviousRecord {
        lamport_seq: u64,
    },
    Replay(ReplayError),
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "chrono hardware path: {self:?}")
    }
}
impl std::error::Error for PathError {}

impl From<ReplayError> for PathError {
    fn from(e: ReplayError) -> Self {
        Self::Replay(e)
    }
}

/// The instruction-level path between two effect records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstructionPath<'a> {
    pub from: EffectAnchor,
    pub to: EffectAnchor,
    /// Steps that certainly ran after `from` and before `to`, in order.
    pub definite: &'a [TraceStep],
    /// Steps that may have run before `from` or between them.
    pub uncertain_start: &'a [TraceStep],
    /// Steps that may have run between them or after `to`.
    pub uncertain_end: &'a [TraceStep],
}

/// Hardware trace steps indexed against effect anchors.
#[derive(Debug, Clone, Default)]
pub struct BranchTimeline {
    anchors: Vec<EffectAnchor>,
    steps: Vec<TraceStep>,
}

fn lo(s: &TraceStep) -> u64 {
    s.tsc_lo.unwrap_or(0)
}
fn hi(s: &TraceStep) -> u64 {
    s.tsc_hi.unwrap_or(u64::MAX)
}

impl BranchTimeline {
    pub fn new(anchors: Vec<EffectAnchor>, steps: Vec<TraceStep>) -> Result<Self, PathError> {
        for (i, w) in anchors.windows(2).enumerate() {
            if w[1].lamport_seq <= w[0].lamport_seq || w[1].tsc < w[0].tsc {
                return Err(PathError::AnchorsOutOfOrder { index: i + 1 });
            }
        }
        for (i, s) in steps.iter().enumerate() {
            if lo(s) >= hi(s) {
                return Err(PathError::BadInterval { index: i });
            }
        }
        for (i, w) in steps.windows(2).enumerate() {
            if lo(&w[1]) < lo(&w[0]) || hi(&w[1]) < hi(&w[0]) {
                return Err(PathError::StepsOutOfOrder { index: i + 1 });
            }
        }
        Ok(Self { anchors, steps })
    }

    /// Anchors from recorded effect events (their `time_ns` is the anchor).
    pub fn anchors_from_events<'e>(
        events: impl IntoIterator<Item = FlightEvent<'e>>,
    ) -> Vec<EffectAnchor> {
        events
            .into_iter()
            .map(|e| EffectAnchor {
                lamport_seq: e.lamport_seq,
                tsc: e.time_ns,
            })
            .collect()
    }

    /// Steps from an Intel PT flow reconstruction.
    pub fn steps_from_pt(trace: &FlowTrace) -> Vec<TraceStep> {
        trace
            .branches
            .iter()
            .map(|b| TraceStep {
                from: b.from,
                to: b.to,
                kind: StepKind::Branch(b.kind),
                tsc_lo: b.tsc_lo,
                tsc_hi: b.tsc_hi,
            })
            .collect()
    }

    /// Steps from decoded ETE/ETM elements; timestamps bound the intervals
    /// as TSC packets do for Intel PT.
    pub fn steps_from_ete(elements: &[EteElementAt]) -> Vec<TraceStep> {
        let mut steps: Vec<TraceStep> = Vec::new();
        let mut open_from = 0usize;
        let mut ts: Option<u64> = None;
        let mut at = 0u64;
        for e in elements {
            let step = match e.element {
                EteElement::Timestamp { timestamp, .. } => {
                    for s in &mut steps[open_from..] {
                        s.tsc_hi = Some(timestamp);
                    }
                    open_from = steps.len();
                    ts = Some(timestamp);
                    None
                }
                EteElement::Address { address, isa } => {
                    let from = at;
                    at = address;
                    Some((from, Some(address), StepKind::Address(isa)))
                }
                EteElement::Atom { executed } => Some((at, None, StepKind::Atom { executed })),
                EteElement::Exception {
                    exception_type,
                    preferred_return,
                    ..
                } => Some((
                    preferred_return,
                    None,
                    StepKind::Exception { exception_type },
                )),
                EteElement::ExceptionReturn => Some((at, None, StepKind::ExceptionReturn)),
                _ => None,
            };
            if let Some((from, to, kind)) = step {
                steps.push(TraceStep {
                    from,
                    to,
                    kind,
                    tsc_lo: ts,
                    tsc_hi: None,
                });
            }
        }
        steps
    }

    pub fn anchors(&self) -> &[EffectAnchor] {
        &self.anchors
    }
    pub fn steps(&self) -> &[TraceStep] {
        &self.steps
    }

    fn anchor_index(&self, lamport_seq: u64) -> Result<usize, PathError> {
        self.anchors
            .binary_search_by_key(&lamport_seq, |a| a.lamport_seq)
            .map_err(|_| PathError::UnknownSeq { lamport_seq })
    }

    /// The path between effect records `from_seq` and `to_seq`. O(log N).
    pub fn path_between(
        &self,
        from_seq: u64,
        to_seq: u64,
    ) -> Result<InstructionPath<'_>, PathError> {
        if from_seq >= to_seq {
            return Err(PathError::Reversed {
                from: from_seq,
                to: to_seq,
            });
        }
        let a = self.anchors[self.anchor_index(from_seq)?];
        let b = self.anchors[self.anchor_index(to_seq)?];
        // Both keys are monotone in step order, so every set is a range.
        let start = self.steps.partition_point(|s| lo(s) <= a.tsc);
        let end = self.steps.partition_point(|s| hi(s) <= b.tsc).max(start);
        let u_start = self.steps.partition_point(|s| hi(s) <= a.tsc).min(start);
        let u_end = self.steps.partition_point(|s| lo(s) < b.tsc).max(end);
        Ok(InstructionPath {
            from: a,
            to: b,
            definite: &self.steps[start..end],
            uncertain_start: &self.steps[u_start..start],
            uncertain_end: &self.steps[end..u_end],
        })
    }

    /// The path from the effect record before `to_seq` to `to_seq`.
    pub fn path_before(&self, to_seq: u64) -> Result<InstructionPath<'_>, PathError> {
        let i = self.anchor_index(to_seq)?;
        if i == 0 {
            return Err(PathError::NoPreviousRecord {
                lamport_seq: to_seq,
            });
        }
        self.path_between(self.anchors[i - 1].lamport_seq, to_seq)
    }
}

/// Result of [`reverse_watch_path`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchPath<'a> {
    /// The cursor at the write, as `ReplaySession::reverse_watch_inout`
    /// returned it.
    pub cursor: TraceCursor,
    pub path: InstructionPath<'a>,
}

/// Reverse-watch `place_hash` and name the instruction-level path from the
/// effect record before the last write to that write.
pub fn reverse_watch_path<'t>(
    session: &mut ReplaySession,
    place_hash: u64,
    timeline: &'t BranchTimeline,
) -> Result<WatchPath<'t>, PathError> {
    let cursor = session.reverse_watch_inout(place_hash)?;
    let path = timeline.path_before(cursor.lamport_seq)?;
    Ok(WatchPath { cursor, path })
}
