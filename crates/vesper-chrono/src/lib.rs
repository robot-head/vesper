#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

pub mod bundle;
pub mod lineage;
pub mod redact;
pub mod replay;
pub mod ring;
pub mod simpulse;
pub mod timeline;
pub mod wire;

pub use bundle::{Bundle, BundleError, BundleKey, BundleLimits, RecordedEvent, RecordedSnapshot};
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

/// Features of §31 this crate does not implement; each reports unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unavailable {
    /// Intel Processor Trace / Arm CoreSight ETM branch tracing (§31.1 item 3).
    HardwareBranchTrace,
    /// Native fiber activation and VirtualArena copy-on-write checkpoints
    /// (§31.1 item 2); snapshots here are caller-provided state images.
    NativeActivationCheckpoint,
    /// `vesper replay` CLI and Lens DAP surfaces (integration B, #136).
    ReplayCli,
    /// Interposition on the native EvidenceVector (r15/x28) by compiled code.
    EvidenceVectorInterposition,
}

impl Unavailable {
    pub const ALL: [Self; 4] = [
        Self::HardwareBranchTrace,
        Self::NativeActivationCheckpoint,
        Self::ReplayCli,
        Self::EvidenceVectorInterposition,
    ];
    pub fn reason(self) -> &'static str {
        match self {
            Self::HardwareBranchTrace => {
                "Intel PT / CoreSight ETM tracing is not implemented or exercised; ipt_tsc_anchor holds caller-supplied logical time"
            }
            Self::NativeActivationCheckpoint => {
                "native activation and VirtualArena CoW checkpoints are not implemented; snapshots are caller-provided images"
            }
            Self::ReplayCli => "the vesper replay CLI is integration B (#136)",
            Self::EvidenceVectorInterposition => {
                "compiled code does not call the recorder; embedders record effect outcomes explicitly"
            }
        }
    }
}
