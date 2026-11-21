// The Vesper build of the accounting: every check runs in
// `vesper/src/audit.ves`; this module converts the counts and errors.
//! `XdpUmemAccounting::check_conservation` (§35.5), generalised from
//! fill + rx + tx to every location a frame can be, plus an auditor that
//! cross-checks the driver's table against the device's rings chunk by chunk.

use crate::LocationCounts;
use crate::binding;
use crate::driver::{DriverLoc, Xsk, XskBackend};
use crate::soft::SoftNic;
use crate::trace::{LOCATIONS, Location, TransitionKind};

/// Per-location frame counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct XdpUmemAccounting {
    pub total_frames: u32,
    pub counts: [u32; LOCATIONS.len()],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConservationError {
    /// The locations do not add up to the UMEM's frame count.
    TotalMismatch { expected: u32, actual: u64 },
    /// Owners were dropped instead of consumed.
    Leaked {
        count: u32,
        first_chunk: Option<u32>,
    },
    /// The driver's collapsed location disagrees with the device's rings.
    SideMismatch {
        side: &'static str,
        driver: u32,
        device: u32,
    },
    /// A chunk was found in two places.
    Duplicate { chunk: u32 },
    /// A chunk was found nowhere.
    Missing { chunk: u32 },
    /// The device holds a chunk the driver places elsewhere.
    Misplaced {
        chunk: u32,
        device: Location,
        driver: DriverLoc,
    },
    /// A count-model transition from an empty location.
    Underflow { transition: TransitionKind },
}

fn kind_index(kind: TransitionKind) -> u32 {
    kind as u32
}

impl XdpUmemAccounting {
    /// Every frame starts on the free list.
    pub fn new(total_frames: u32) -> Self {
        let mut counts = [0; LOCATIONS.len()];
        counts[Location::Free.index()] = total_frames;
        Self {
            total_frames,
            counts,
        }
    }
    pub fn get(&self, loc: Location) -> u32 {
        self.counts[loc.index()]
    }
    pub fn sum(&self) -> u64 {
        self.counts.iter().map(|&c| u64::from(c)).sum()
    }

    /// The §35.5 check: the frames in all locations add up to the UMEM, and
    /// none has leaked.
    pub fn check_conservation(&self) -> Result<(), ConservationError> {
        binding::accounting_check(self.total_frames, &self.counts).map_or(Ok(()), |e| Err(e.into()))
    }

    /// The count model of one transition (mirrors `apply` in `lean/WarpUmem.lean`).
    pub fn apply(&mut self, kind: TransitionKind) -> Result<(), ConservationError> {
        let mut counts = self.counts.to_vec();
        if !binding::accounting_apply(&mut counts, kind_index(kind)) {
            return Err(ConservationError::Underflow { transition: kind });
        }
        self.counts.copy_from_slice(&counts);
        Ok(())
    }
}

/// Coarse audit for any backend: the driver's own counts add up and nothing
/// leaked.
pub fn audit_driver<B: XskBackend>(xsk: &mut Xsk<B>) -> Result<(), ConservationError> {
    xsk.census();
    binding::xsk_audit_driver(xsk.device()).map_or(Ok(()), |e| Err(e.into()))
}

fn accounting(total_frames: u32, c: LocationCounts) -> XdpUmemAccounting {
    XdpUmemAccounting {
        total_frames,
        counts: [
            c.free,
            c.fill,
            c.nic_rx,
            c.rx,
            c.app_rx,
            c.app_tx,
            c.tx,
            c.nic_tx,
            c.comp,
            c.in_flight,
            c.leaked,
        ],
    }
}

/// Exact auditor for the software NIC. Its scratch space lives in the
/// Vesper device, so repeated audits are allocation-free after the first.
#[derive(Debug, Default)]
pub struct UmemAuditor {}

impl UmemAuditor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Full audit: [`UmemAuditor::counts`], then no leaks (including dropped
    /// leases whose transmit has not completed yet).
    pub fn audit(
        &mut self,
        xsk: &mut Xsk<SoftNic>,
    ) -> Result<XdpUmemAccounting, ConservationError> {
        xsk.census();
        let counts = binding::xsk_audit(xsk.device_mut())?;
        Ok(accounting(xsk.config().frame_count, counts))
    }

    /// Fine-grained counts from both halves, checked chunk by chunk: the
    /// sides agree, every chunk is in exactly one place, and the counts add
    /// up to the UMEM. Leaked chunks are counted (in `Location::Leaked`), not
    /// rejected.
    pub fn counts(
        &mut self,
        xsk: &mut Xsk<SoftNic>,
    ) -> Result<XdpUmemAccounting, ConservationError> {
        xsk.census();
        let counts = binding::xsk_audit_counts(xsk.device_mut())?;
        Ok(accounting(xsk.config().frame_count, counts))
    }
}
