#![doc = include_str!("../README.md")]
#![deny(unsafe_code)]

pub mod arena;
pub mod bundle;
pub mod capture;
pub mod checkpoint;
pub mod ete;
pub mod ete_flow;
pub mod formatter;
pub mod hwpath;
pub mod ipt;
pub mod ipt_flow;
pub mod lineage;
pub mod redact;
pub mod replay;
pub mod ring;
pub mod simpulse;
pub mod timeline;
pub mod wire;

pub use arena::{ArenaCheckpoint, ArenaDelta, ArenaError, ShardArena};
pub use bundle::{Bundle, BundleError, BundleKey, BundleLimits, RecordedEvent, RecordedSnapshot};
pub use checkpoint::{
    ActivationCheckpoint, AddressPolicy, CheckpointError, CheckpointLimits, JournalEntry,
    ParkPoint, ResumeTag, StackShape,
};
pub use hwpath::{BranchTimeline, EffectAnchor, InstructionPath, PathError, TraceStep};
pub use lineage::{
    HopKind, LineageError, ObligationHop, ObligationId, ObligationLineage, ObligationStatus,
    Provenance,
};
pub use redact::{
    FieldRule, ImageRegion, RedactionEntry, RedactionError, RedactionPolicy, RedactionTarget,
};
pub use replay::{ReplayError, ReplaySession, Replayer, leaked_sessions};
pub use ring::{FlightEvent, FlightRecorder, RecordError, RecorderConfig, SnapshotImage};
pub use timeline::{
    ChronoError, ChronoTimelineIndex, InOutWriteEvent, IngestOrder, OrderingNote, SnapshotEpoch,
};
pub use wire::{ChronoLayouts, EventHeader, TraceCursor};

/// Parts of §31 this crate does not implement or has not exercised; each
/// reports why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unavailable {
    /// Intel PT / CoreSight capture on real hardware (§31.1 item 3). The
    /// decoders (`ipt`, `ipt_flow`, `ete`) and the gated `capture` module
    /// exist; no hardware trace has been captured or decoded.
    HardwareBranchTrace,
    /// Kernel-trapped write tracking for VirtualArena shard checkpoints
    /// (§31.1 item 2): userfaultfd write-protect, `mprotect` fault tracking,
    /// Mach `vm_copy` and Windows `MEM_RESET` / write watches. `arena`
    /// tracks writes at its API instead; see [`arena::probe_write_tracking`].
    KernelWriteTracking,
    /// `vesper replay` CLI and Lens DAP surfaces (integration B, #136).
    ReplayCli,
    /// Interposition on the native EvidenceVector (r15/x28) by compiled code.
    EvidenceVectorInterposition,
}

impl Unavailable {
    pub const ALL: [Self; 4] = [
        Self::HardwareBranchTrace,
        Self::KernelWriteTracking,
        Self::ReplayCli,
        Self::EvidenceVectorInterposition,
    ];
    pub fn reason(self) -> &'static str {
        match self {
            Self::HardwareBranchTrace => {
                "Intel PT / CoreSight decoding is tested only on independently encoded streams; capture needs an intel_pt, cs_etm or arm_spe PMU, which the development and CI hosts lack"
            }
            Self::KernelWriteTracking => {
                "byte-image activation checkpoints are implemented for fixed-address fibers; VirtualArena shard checkpoints are copy-on-write at shard granularity with writes tracked by the arena API, because kernel write trapping (userfaultfd, mprotect + SIGSEGV, vm_copy, write watches) needs unsafe code this crate may not contain"
            }
            Self::ReplayCli => "the vesper replay CLI is integration B (#136)",
            Self::EvidenceVectorInterposition => {
                "compiled code does not call the recorder; embedders record effect outcomes explicitly"
            }
        }
    }
}
