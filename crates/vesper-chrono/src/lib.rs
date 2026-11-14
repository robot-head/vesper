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
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
pub mod tracked_arena;
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
    /// The kernel write-tracking mechanisms not used (§31.1 item 2):
    /// userfaultfd write-protect and Mach `vm_copy`. `tracked_arena` tracks
    /// raw-pointer writes with `mprotect` faults (Linux, macOS) and write
    /// watches (Windows); `arena::ShardArena` still tracks at its API. See
    /// [`arena::probe_write_tracking`].
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
                "TrackedArena checkpoints see raw-pointer writes through mprotect faults (Linux, macOS) or write watches (Windows), implemented in vesper-fiber-abi's write_watch module; userfaultfd write-protect (needs CAP_SYS_PTRACE or unprivileged_userfaultfd) and Mach vm_copy snapshots are not used, and the copy-on-write ShardArena still tracks writes at its API"
            }
            Self::ReplayCli => "the vesper replay CLI is integration B (#136)",
            Self::EvidenceVectorInterposition => {
                "compiled code does not call the recorder; embedders record effect outcomes explicitly"
            }
        }
    }
}
