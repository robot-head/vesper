//! Frame locations and the transitions between them.
//!
//! These names are shared with `lean/WarpUmem.lean`; the Lean model's
//! `Trans` constructors and `Loc` buckets are the same list, in the same order.

/// Every place a UMEM chunk can be.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Location {
    /// Driver free list.
    Free,
    /// Posted in the fill ring, not yet taken by the NIC.
    FillRing,
    /// Taken from the fill ring by the NIC, waiting for a packet.
    NicRx,
    /// Described by a published rx-ring entry.
    RxRing,
    /// Held by the application as `UmemFrame<RxHarvested>`.
    AppRx,
    /// Held by the application as `UmemFrame<TxStaged>`.
    AppTx,
    /// Described by a published tx-ring entry.
    TxRing,
    /// Taken from the tx ring by the NIC, transmit not yet completed.
    NicTx,
    /// Described by a published completion-ring entry.
    CompletionRing,
    /// Transmit completed, still owned by a live `InFlightQuicPkt`.
    InFlight,
    /// An owner was dropped; the chunk is quarantined and reported.
    Leaked,
}

pub const LOCATIONS: [Location; 11] = [
    Location::Free,
    Location::FillRing,
    Location::NicRx,
    Location::RxRing,
    Location::AppRx,
    Location::AppTx,
    Location::TxRing,
    Location::NicTx,
    Location::CompletionRing,
    Location::InFlight,
    Location::Leaked,
];

impl Location {
    pub const fn index(self) -> usize {
        self as usize
    }
    pub const fn lean_name(self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::FillRing => "fill",
            Self::NicRx => "nicRx",
            Self::RxRing => "rx",
            Self::AppRx => "appRx",
            Self::AppTx => "appTx",
            Self::TxRing => "tx",
            Self::NicTx => "nicTx",
            Self::CompletionRing => "comp",
            Self::InFlight => "inFlight",
            Self::Leaked => "leaked",
        }
    }
}

/// One modelled move of one chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TransitionKind {
    /// NIC takes a fill-ring entry (fill → nicRx).
    NicPullFill,
    /// NIC writes a packet and publishes an rx descriptor (nicRx → rx).
    NicDeliver,
    /// Application harvests a valid descriptor (rx → appRx).
    Harvest,
    /// A malformed descriptor's chunk goes straight back (rx → fill).
    HarvestReject,
    /// `recycle_to_fill_ring` (appRx → fill).
    Recycle,
    /// Fallback when the fill ring is full (rx → free). Unreachable while
    /// `fill_size >= frame_count`, which `UmemConfig::validate` enforces.
    RejectToFree,
    /// Fallback when the fill ring is full (appRx → free). Unreachable as above.
    RecycleToFree,
    /// `turn_around_tx_in_place` (appRx → appTx).
    TurnAround,
    /// A fresh transmit frame (free → appTx).
    AllocTx,
    /// A staged frame is abandoned (appTx → free).
    AbandonTx,
    /// `submit_paced_tx` (appTx → tx).
    Submit,
    /// NIC takes a tx descriptor (tx → nicTx).
    NicConsumeTx,
    /// NIC publishes a completion (nicTx → comp).
    NicComplete,
    /// Completion harvested while the lease lives (comp → inFlight).
    CompleteLeased,
    /// Completion harvested after the lease was retired (comp → free).
    CompleteRetired,
    /// Completion harvested after the lease was dropped (comp → leaked).
    CompleteLeaked,
    /// Lease retired after completion (inFlight → free).
    Retire,
    /// Free chunk posted to the fill ring (free → fill).
    Replenish,
    /// Dropped `UmemFrame<RxHarvested>` (appRx → leaked).
    LeakRx,
    /// Dropped `UmemFrame<TxStaged>` (appTx → leaked).
    LeakTx,
    /// Dropped completed lease (inFlight → leaked).
    LeakInFlight,
}

impl TransitionKind {
    pub const fn endpoints(self) -> (Location, Location) {
        use Location::*;
        match self {
            Self::NicPullFill => (FillRing, NicRx),
            Self::NicDeliver => (NicRx, RxRing),
            Self::Harvest => (RxRing, AppRx),
            Self::HarvestReject => (RxRing, FillRing),
            Self::Recycle => (AppRx, FillRing),
            Self::RejectToFree => (RxRing, Free),
            Self::RecycleToFree => (AppRx, Free),
            Self::TurnAround => (AppRx, AppTx),
            Self::AllocTx => (Free, AppTx),
            Self::AbandonTx => (AppTx, Free),
            Self::Submit => (AppTx, TxRing),
            Self::NicConsumeTx => (TxRing, NicTx),
            Self::NicComplete => (NicTx, CompletionRing),
            Self::CompleteLeased => (CompletionRing, InFlight),
            Self::CompleteRetired => (CompletionRing, Free),
            Self::CompleteLeaked => (CompletionRing, Leaked),
            Self::Retire => (InFlight, Free),
            Self::Replenish => (Free, FillRing),
            Self::LeakRx => (AppRx, Leaked),
            Self::LeakTx => (AppTx, Leaked),
            Self::LeakInFlight => (InFlight, Leaked),
        }
    }
    pub const fn lean_name(self) -> &'static str {
        match self {
            Self::NicPullFill => "nicPullFill",
            Self::NicDeliver => "nicDeliver",
            Self::Harvest => "harvest",
            Self::HarvestReject => "harvestReject",
            Self::Recycle => "recycle",
            Self::RejectToFree => "rejectToFree",
            Self::RecycleToFree => "recycleToFree",
            Self::TurnAround => "turnAround",
            Self::AllocTx => "allocTx",
            Self::AbandonTx => "abandonTx",
            Self::Submit => "submit",
            Self::NicConsumeTx => "nicConsumeTx",
            Self::NicComplete => "nicComplete",
            Self::CompleteLeased => "completeLeased",
            Self::CompleteRetired => "completeRetired",
            Self::CompleteLeaked => "completeLeaked",
            Self::Retire => "retire",
            Self::Replenish => "replenish",
            Self::LeakRx => "leakRx",
            Self::LeakTx => "leakTx",
            Self::LeakInFlight => "leakInFlight",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Transition {
    pub kind: TransitionKind,
    pub chunk: u32,
}
