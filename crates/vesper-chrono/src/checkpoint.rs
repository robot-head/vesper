//! Native activation checkpoints as Chrono snapshot epochs (§31.1 item 2).
//!
//! An [`ActivationCheckpoint`] describes a parked `vesper-fiber-abi` fiber at
//! one park: its identity, generation, pending park request (with the
//! evidence slot named by depth, since evidence identities are never reused
//! across vectors), its data frame both as the fiber left it at the park and
//! as the resumer has since filled it, the shape of the stack words that point
//! into its own stack, and the journal of every resumption it received so far
//! together with the data-frame image each resumer left before resuming.
//!
//! **What is and is not captured.** The public `vesper-fiber-abi` API exposes
//! the park request, the data frame, the evidence vector and
//! `Parked::stack_self_pointers`, but no accessor for the raw stack bytes
//! between the saved stack pointer and the stack top, nor for the saved
//! register frame, nor a way to map a fiber stack at a chosen address or to
//! write a context. This crate may not use `unsafe` outside
//! [`crate::capture`] and may not edit `vesper-fiber-abi`, so a byte image of
//! the stack and registers is not taken. Instead a checkpoint is restored by
//! **deterministic re-execution**: [`ActivationCheckpoint::restore`] starts a
//! fresh fiber of the same code and argument and replays the journal, checking
//! at every park that the request and park count match. It then checks that
//! the rebuilt activation equals the checkpoint: request, generation, data
//! frame at the park and stack shape (every self-pointing stack word at the same offset,
//! pointing at the same offset). It then re-applies the deliveries made at
//! the captured park, and the rebuilt activation resumes to the same result as
//! the live one.
//!
//! **Relocation.** A fresh stack is a fresh mapping, so the rebuilt
//! activation normally sits at a different address. Saved raw stack pointers
//! do not establish that a byte copy may move, so [`AddressPolicy::SameAddress`]
//! rejects any restore whose stack anchor differs with
//! [`CheckpointError::Relocated`]. [`AddressPolicy::Reexecuted`] accepts a
//! different base because re-execution rebuilt every stack pointer, and
//! reports the displacement. Direct (tail-resumptive) handlers run again
//! during re-execution: they must be deterministic, or their outcomes must be
//! recorded through Chrono; a divergence is reported, not hidden.
//!
//! Checkpoints serialise to a versioned, BLAKE3-checked image that
//! [`capture_epoch`] stores as a `FlightRecorder` snapshot epoch and
//! [`checkpoint_from_bundle`] reads back from a `.vchrono` bundle.

use std::fmt;

use crate::bundle::Bundle;
use crate::ring::{FlightRecorder, RecordError};

/// Image magic and format version.
pub const CHECKPOINT_MAGIC: [u8; 8] = *b"VCHKPT\0\x01";
pub const CHECKPOINT_VERSION: u16 = 1;
const DIGEST_LEN: usize = 32;

/// A park request with its evidence slot named by depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ParkPoint {
    pub evidence_depth: u32,
    pub op: u32,
    pub flags: u32,
    pub arg0: u64,
    pub arg1: u64,
}

/// How a park was answered (`vesper_fiber_abi::abi::ResumeKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResumeTag {
    Value,
    Error,
    Cancelled,
}

impl ResumeTag {
    fn to_u8(self) -> u8 {
        match self {
            Self::Value => 0,
            Self::Error => 1,
            Self::Cancelled => 2,
        }
    }
    fn from_u8(b: u8) -> Option<Self> {
        Some(match b {
            0 => Self::Value,
            1 => Self::Error,
            2 => Self::Cancelled,
            _ => return None,
        })
    }
}

/// One answered park.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JournalEntry {
    pub park: ParkPoint,
    /// The data frame as the resumer left it immediately before resuming
    /// (completions deliver into mutable places this way).
    pub data: Vec<u8>,
    pub tag: ResumeTag,
    pub value: u64,
}

/// Stack words that point into the fiber's own stack, relative to the
/// lowest such word (`anchor`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StackShape {
    /// Absolute address of the lowest self-pointing word.
    pub anchor: u64,
    /// `(slot - anchor, target - anchor)` for every self-pointing word.
    pub words: Vec<(u64, i64)>,
}

impl StackShape {
    /// From `(address, value)` pairs as `Parked::stack_self_pointers` returns.
    pub fn from_pairs(pairs: &[(u64, u64)]) -> Option<Self> {
        let anchor = pairs.iter().map(|(a, _)| *a).min()?;
        let words = pairs
            .iter()
            .map(|(a, v)| (a - anchor, v.wrapping_sub(anchor) as i64))
            .collect();
        Some(Self { anchor, words })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ActivationCheckpoint {
    /// Fiber id of the live activation (the restored one has a new id).
    pub fiber_id: u64,
    /// Continuation generation at the park.
    pub generation: u64,
    pub park: ParkPoint,
    /// The data frame as the fiber left it when it parked.
    pub data_at_park: Vec<u8>,
    /// The data frame at capture, including completions the resumer has
    /// already delivered into it at this park.
    pub data: Vec<u8>,
    /// `None` when no stack word points into the stack.
    pub stack: Option<StackShape>,
    /// Every park answered before this one, oldest first.
    pub journal: Vec<JournalEntry>,
}

/// Decoding bounds for untrusted images.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointLimits {
    pub max_data_bytes: usize,
    pub max_journal: usize,
    pub max_stack_words: usize,
}

impl Default for CheckpointLimits {
    fn default() -> Self {
        Self {
            max_data_bytes: 64 * 1024,
            max_journal: 4096,
            max_stack_words: 64 * 1024,
        }
    }
}

/// How a restore may place the rebuilt activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressPolicy {
    /// The rebuilt stack must be at the captured address.
    SameAddress,
    /// Accept a different address: re-execution rebuilt every stack
    /// pointer. The displacement is reported.
    Reexecuted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckpointError {
    Truncated {
        needed: usize,
        available: usize,
    },
    BadMagic,
    UnsupportedVersion(u16),
    /// The BLAKE3 trailer does not match (corrupted or redacted image).
    Corrupt,
    TrailingBytes(usize),
    TooLarge {
        what: &'static str,
        len: usize,
        max: usize,
    },
    BadResumeTag(u8),
    BadFlag(u8),
    /// The fiber to restore into has already started.
    NotFresh,
    /// The parked request names an evidence identity the fiber's vector does
    /// not hold.
    UnknownEvidence,
    /// The journal names an evidence depth the fresh fiber's vector lacks.
    UnknownEvidenceDepth(u32),
    /// Re-execution exited before reaching park `park_index`.
    ExitedEarly {
        park_index: usize,
        status: u64,
    },
    /// Re-execution parked with a different request than recorded.
    Diverged {
        park_index: usize,
        expected: ParkPoint,
        found: ParkPoint,
    },
    /// A journal data image does not fit the fresh data frame.
    DataSize {
        expected: usize,
        found: usize,
    },
    /// The rebuilt activation differs from the checkpoint.
    Mismatch(&'static str),
    /// The rebuilt stack is at a different address than captured.
    Relocated {
        captured: u64,
        restored: u64,
    },
    /// [`AddressPolicy::SameAddress`] with no self-pointing stack word to
    /// locate the stack by.
    AddressUnknown,
    /// The fiber rejected a journal resumption.
    Rejected(String),
    Record(RecordError),
    NoSuchEpoch(u32),
}

impl fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "chrono checkpoint: {self:?}")
    }
}
impl std::error::Error for CheckpointError {}

impl From<RecordError> for CheckpointError {
    fn from(e: RecordError) -> Self {
        Self::Record(e)
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], CheckpointError> {
        let available = self.bytes.len() - self.at;
        if available < n {
            return Err(CheckpointError::Truncated {
                needed: n,
                available,
            });
        }
        let s = &self.bytes[self.at..self.at + n];
        self.at += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, CheckpointError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, CheckpointError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, CheckpointError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, CheckpointError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn len(&mut self, what: &'static str, max: usize) -> Result<usize, CheckpointError> {
        let len = self.u32()? as usize;
        if len > max {
            return Err(CheckpointError::TooLarge { what, len, max });
        }
        Ok(len)
    }
    fn park(&mut self) -> Result<ParkPoint, CheckpointError> {
        Ok(ParkPoint {
            evidence_depth: self.u32()?,
            op: self.u32()?,
            flags: self.u32()?,
            arg0: self.u64()?,
            arg1: self.u64()?,
        })
    }
}

fn put_park(out: &mut Vec<u8>, p: &ParkPoint) {
    out.extend_from_slice(&p.evidence_depth.to_le_bytes());
    out.extend_from_slice(&p.op.to_le_bytes());
    out.extend_from_slice(&p.flags.to_le_bytes());
    out.extend_from_slice(&p.arg0.to_le_bytes());
    out.extend_from_slice(&p.arg1.to_le_bytes());
}

impl ActivationCheckpoint {
    /// Serialise to the versioned image (little-endian, BLAKE3 trailer).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&CHECKPOINT_MAGIC);
        out.extend_from_slice(&CHECKPOINT_VERSION.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&self.fiber_id.to_le_bytes());
        out.extend_from_slice(&self.generation.to_le_bytes());
        put_park(&mut out, &self.park);
        for frame in [&self.data_at_park, &self.data] {
            out.extend_from_slice(&(frame.len() as u32).to_le_bytes());
            out.extend_from_slice(frame);
        }
        match &self.stack {
            None => out.push(0),
            Some(shape) => {
                out.push(1);
                out.extend_from_slice(&shape.anchor.to_le_bytes());
                out.extend_from_slice(&(shape.words.len() as u32).to_le_bytes());
                for (slot, target) in &shape.words {
                    out.extend_from_slice(&slot.to_le_bytes());
                    out.extend_from_slice(&target.to_le_bytes());
                }
            }
        }
        out.extend_from_slice(&(self.journal.len() as u32).to_le_bytes());
        for e in &self.journal {
            put_park(&mut out, &e.park);
            out.push(e.tag.to_u8());
            out.extend_from_slice(&e.value.to_le_bytes());
            out.extend_from_slice(&(e.data.len() as u32).to_le_bytes());
            out.extend_from_slice(&e.data);
        }
        let digest = blake3::hash(&out);
        out.extend_from_slice(digest.as_bytes());
        out
    }

    /// Parse an image, checking the trailer first and every length against
    /// `limits`.
    pub fn decode(bytes: &[u8], limits: &CheckpointLimits) -> Result<Self, CheckpointError> {
        let min = CHECKPOINT_MAGIC.len() + DIGEST_LEN;
        if bytes.len() < min {
            return Err(CheckpointError::Truncated {
                needed: min,
                available: bytes.len(),
            });
        }
        if bytes[..CHECKPOINT_MAGIC.len()] != CHECKPOINT_MAGIC {
            return Err(CheckpointError::BadMagic);
        }
        let (body, digest) = bytes.split_at(bytes.len() - DIGEST_LEN);
        if blake3::hash(body).as_bytes() != digest {
            return Err(CheckpointError::Corrupt);
        }
        let mut r = Reader { bytes: body, at: 8 };
        let version = r.u16()?;
        if version != CHECKPOINT_VERSION {
            return Err(CheckpointError::UnsupportedVersion(version));
        }
        if r.u16()? != 0 {
            return Err(CheckpointError::BadFlag(1));
        }
        let fiber_id = r.u64()?;
        let generation = r.u64()?;
        let park = r.park()?;
        let n = r.len("data", limits.max_data_bytes)?;
        let data_at_park = r.take(n)?.to_vec();
        let n = r.len("data", limits.max_data_bytes)?;
        let data = r.take(n)?.to_vec();
        let stack = match r.u8()? {
            0 => None,
            1 => {
                let anchor = r.u64()?;
                let n = r.len("stack words", limits.max_stack_words)?;
                let mut words = Vec::with_capacity(n.min(r.bytes.len() / 16));
                for _ in 0..n {
                    words.push((r.u64()?, r.u64()? as i64));
                }
                Some(StackShape { anchor, words })
            }
            b => return Err(CheckpointError::BadFlag(b)),
        };
        let n = r.len("journal", limits.max_journal)?;
        let mut journal = Vec::with_capacity(n.min(r.bytes.len() / 45));
        for _ in 0..n {
            let park = r.park()?;
            let tag = r.u8()?;
            let tag = ResumeTag::from_u8(tag).ok_or(CheckpointError::BadResumeTag(tag))?;
            let value = r.u64()?;
            let len = r.len("journal data", limits.max_data_bytes)?;
            let data = r.take(len)?.to_vec();
            journal.push(JournalEntry {
                park,
                data,
                tag,
                value,
            });
        }
        if r.at != body.len() {
            return Err(CheckpointError::TrailingBytes(body.len() - r.at));
        }
        Ok(Self {
            fiber_id,
            generation,
            park,
            data_at_park,
            data,
            stack,
            journal,
        })
    }
}

/// Store `checkpoint` as a snapshot epoch; returns the epoch id. The
/// recorder's `snapshot_image_bytes` must fit the encoded image.
pub fn capture_epoch(
    recorder: &mut FlightRecorder,
    time_ns: u64,
    checkpoint: &ActivationCheckpoint,
) -> Result<u32, CheckpointError> {
    Ok(recorder.capture_snapshot(time_ns, &checkpoint.encode())?)
}

/// Read the checkpoint stored as `epoch_id` in an opened bundle.
pub fn checkpoint_from_bundle(
    bundle: &Bundle,
    epoch_id: u32,
    limits: &CheckpointLimits,
) -> Result<(u64, ActivationCheckpoint), CheckpointError> {
    let snapshot = bundle
        .snapshots
        .iter()
        .find(|s| s.epoch_id == epoch_id)
        .ok_or(CheckpointError::NoSuchEpoch(epoch_id))?;
    Ok((
        snapshot.start_lamport_seq,
        ActivationCheckpoint::decode(&snapshot.image, limits)?,
    ))
}

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    all(target_os = "macos", target_arch = "aarch64")
))]
pub use native::*;

#[cfg(any(
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ),
    all(target_os = "macos", target_arch = "aarch64")
))]
mod native {
    use super::*;
    use vesper_fiber_abi::abi::{FiberState, ParkRequest};
    use vesper_fiber_abi::{Exit, Fiber, Outcome, Parked, ResumeError, Resumption};

    fn park_point(parked: &Parked) -> Result<ParkPoint, CheckpointError> {
        let r: ParkRequest = parked.request();
        let depth = parked
            .fiber()
            .evidence()
            .depth_of(r.evidence)
            .ok_or(CheckpointError::UnknownEvidence)?;
        Ok(ParkPoint {
            evidence_depth: depth as u32,
            op: r.op,
            flags: r.flags,
            arg0: r.arg0,
            arg1: r.arg1,
        })
    }

    fn resumption(parked: &Parked, e: &JournalEntry) -> Result<Resumption, CheckpointError> {
        let evidence = parked
            .fiber()
            .evidence()
            .identity(e.park.evidence_depth as usize)
            .ok_or(CheckpointError::UnknownEvidenceDepth(e.park.evidence_depth))?;
        Ok(match e.tag {
            ResumeTag::Value => Resumption::value(evidence, e.value),
            ResumeTag::Error => Resumption::error(evidence, e.value),
            ResumeTag::Cancelled => Resumption::cancelled(evidence),
        })
    }

    /// A parked fiber whose resumptions are journaled so that it can be
    /// checkpointed. Move-only, like the `Parked` it owns; dropping it
    /// drops the continuation (with `Parked`'s own cancellation fallback).
    #[must_use = "a recorded activation owns a parked continuation"]
    #[derive(Debug)]
    pub struct RecordedActivation {
        parked: Parked,
        /// The data frame as the fiber left it at this park.
        data_at_park: Vec<u8>,
        journal: Vec<JournalEntry>,
    }

    /// Result of starting or resuming a [`RecordedActivation`].
    #[derive(Debug)]
    pub enum Step {
        Parked(RecordedActivation),
        Exited(Exit),
    }

    impl Step {
        pub fn parked(self) -> Option<RecordedActivation> {
            match self {
                Step::Parked(a) => Some(a),
                Step::Exited(_) => None,
            }
        }
        pub fn exited(self) -> Option<Exit> {
            match self {
                Step::Exited(e) => Some(e),
                Step::Parked(_) => None,
            }
        }
    }

    /// A rejected resume hands the activation back unchanged.
    #[derive(Debug)]
    pub struct RejectedResume {
        pub error: ResumeError,
        pub activation: RecordedActivation,
    }

    impl RecordedActivation {
        /// Start a created fiber and journal from its first park.
        pub fn start(fiber: Fiber) -> Result<Step, CheckpointError> {
            if fiber.state() != FiberState::Created {
                return Err(CheckpointError::NotFresh);
            }
            Ok(Self::wrap(fiber.start(), Vec::new()))
        }

        fn wrap(outcome: Outcome, journal: Vec<JournalEntry>) -> Step {
            match outcome {
                Outcome::Parked(parked) => Step::Parked(Self {
                    data_at_park: parked.data().to_vec(),
                    parked,
                    journal,
                }),
                Outcome::Exited(e) => Step::Exited(e),
            }
        }

        pub fn parked(&self) -> &Parked {
            &self.parked
        }
        /// The suspended data frame (deliver completions here).
        pub fn data_mut(&mut self) -> &mut [u8] {
            self.parked.data_mut()
        }
        pub fn journal(&self) -> &[JournalEntry] {
            &self.journal
        }
        /// The current park, with its evidence slot by depth.
        pub fn park_point(&self) -> Result<ParkPoint, CheckpointError> {
            park_point(&self.parked)
        }

        /// Checkpoint the activation at its current park.
        pub fn checkpoint(&self) -> Result<ActivationCheckpoint, CheckpointError> {
            Ok(ActivationCheckpoint {
                fiber_id: self.parked.fiber_id(),
                generation: self.parked.token().generation,
                park: park_point(&self.parked)?,
                data_at_park: self.data_at_park.clone(),
                data: self.parked.data().to_vec(),
                stack: StackShape::from_pairs(&self.parked.stack_self_pointers()),
                journal: self.journal.clone(),
            })
        }

        /// Resume exactly once, journaling the resumption and the data frame
        /// the resumer left.
        pub fn resume(mut self, resumption: Resumption) -> Result<Step, Box<RejectedResume>> {
            let park = match park_point(&self.parked) {
                Ok(p) => p,
                Err(_) => {
                    let parked = self.parked.request().evidence;
                    return Err(Box::new(RejectedResume {
                        error: ResumeError::WrongEvidence {
                            parked,
                            offered: resumption.evidence,
                        },
                        activation: self,
                    }));
                }
            };
            let entry = JournalEntry {
                park,
                data: self.parked.data().to_vec(),
                tag: match resumption.kind {
                    vesper_fiber_abi::abi::ResumeKind::Value => ResumeTag::Value,
                    vesper_fiber_abi::abi::ResumeKind::Error => ResumeTag::Error,
                    vesper_fiber_abi::abi::ResumeKind::Cancelled => ResumeTag::Cancelled,
                },
                value: resumption.value,
            };
            let mut journal = std::mem::take(&mut self.journal);
            match self.parked.resume(resumption) {
                Ok(outcome) => {
                    journal.push(entry);
                    Ok(Self::wrap(outcome, journal))
                }
                Err(rejected) => Err(Box::new(RejectedResume {
                    error: rejected.error,
                    activation: Self {
                        parked: rejected.continuation,
                        data_at_park: std::mem::take(&mut self.data_at_park),
                        journal,
                    },
                })),
            }
        }

        /// Give up journaling and hand back the continuation.
        pub fn into_parts(self) -> (Parked, Vec<JournalEntry>) {
            (self.parked, self.journal)
        }
    }

    /// A rebuilt activation and how far its stack moved.
    #[derive(Debug)]
    pub struct Restored {
        pub activation: RecordedActivation,
        /// Restored anchor minus captured anchor (0 when not locatable).
        pub stack_displacement: i64,
    }

    impl ActivationCheckpoint {
        /// Rebuild this activation on `fresh` (a created fiber of the same
        /// code, argument, data size and evidence shape) by re-executing the
        /// journal, then verify it. On any failure the rebuilt continuation
        /// is cancelled before the error is returned.
        pub fn restore(
            &self,
            fresh: Fiber,
            policy: AddressPolicy,
        ) -> Result<Restored, CheckpointError> {
            let mut step = RecordedActivation::start(fresh)?;
            for (i, entry) in self.journal.iter().enumerate() {
                let mut a = match step {
                    Step::Parked(a) => a,
                    Step::Exited(e) => {
                        return Err(CheckpointError::ExitedEarly {
                            park_index: i,
                            status: e.status,
                        });
                    }
                };
                let found = match a.park_point() {
                    Ok(p) => p,
                    Err(e) => return Err(cancel(a, e)),
                };
                if found != entry.park {
                    return Err(cancel(
                        a,
                        CheckpointError::Diverged {
                            park_index: i,
                            expected: entry.park,
                            found,
                        },
                    ));
                }
                if a.parked.data().len() != entry.data.len() {
                    let found = a.parked.data().len();
                    return Err(cancel(
                        a,
                        CheckpointError::DataSize {
                            expected: entry.data.len(),
                            found,
                        },
                    ));
                }
                a.data_mut().copy_from_slice(&entry.data);
                let r = match resumption(&a.parked, entry) {
                    Ok(r) => r,
                    Err(e) => return Err(cancel(a, e)),
                };
                step = match a.resume(r) {
                    Ok(s) => s,
                    Err(rejected) => {
                        let error = CheckpointError::Rejected(rejected.error.to_string());
                        return Err(cancel(rejected.activation, error));
                    }
                };
            }
            let a = match step {
                Step::Parked(a) => a,
                Step::Exited(e) => {
                    return Err(CheckpointError::ExitedEarly {
                        park_index: self.journal.len(),
                        status: e.status,
                    });
                }
            };
            let rebuilt = match a.checkpoint() {
                Ok(c) => c,
                Err(e) => return Err(cancel(a, e)),
            };
            let check = |what: &'static str, ok: bool| {
                if ok {
                    Ok(())
                } else {
                    Err(CheckpointError::Mismatch(what))
                }
            };
            let verified = check("park", rebuilt.park == self.park)
                .and_then(|_| check("generation", rebuilt.generation == self.generation))
                .and_then(|_| check("data frame", rebuilt.data_at_park == self.data_at_park))
                .and_then(|_| check("data frame size", rebuilt.data.len() == self.data.len()))
                .and_then(|_| {
                    check(
                        "stack shape",
                        rebuilt.stack.as_ref().map(|s| &s.words)
                            == self.stack.as_ref().map(|s| &s.words),
                    )
                });
            if let Err(e) = verified {
                return Err(cancel(a, e));
            }
            // Re-apply what the resumer had delivered at this park.
            let mut a = a;
            a.data_mut().copy_from_slice(&self.data);
            let displacement = match (&self.stack, &rebuilt.stack) {
                (Some(c), Some(r)) => r.anchor.wrapping_sub(c.anchor) as i64,
                _ => 0,
            };
            if policy == AddressPolicy::SameAddress {
                match (&self.stack, &rebuilt.stack) {
                    (Some(c), Some(r)) if c.anchor != r.anchor => {
                        let error = CheckpointError::Relocated {
                            captured: c.anchor,
                            restored: r.anchor,
                        };
                        return Err(cancel(a, error));
                    }
                    (Some(_), Some(_)) => {}
                    _ => return Err(cancel(a, CheckpointError::AddressUnknown)),
                }
            }
            Ok(Restored {
                activation: a,
                stack_displacement: displacement,
            })
        }
    }

    /// Discharge a rebuilt continuation that will not be handed out.
    fn cancel(a: RecordedActivation, error: CheckpointError) -> CheckpointError {
        // A cancellation edge that parks again is dropped, which takes
        // `Parked`'s own fallback (one more cancel, then abandonment).
        drop(a.parked.cancel());
        error
    }
}
