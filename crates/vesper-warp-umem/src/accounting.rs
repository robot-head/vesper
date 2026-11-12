//! `XdpUmemAccounting::check_conservation` (§35.5), generalised from
//! fill + rx + tx to every location a frame can be, plus an auditor that
//! cross-checks the driver's table against the device's rings chunk by chunk.

use crate::driver::{DriverLoc, LeaseMark, Xsk, XskBackend};
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
        let actual = self.sum();
        if actual != u64::from(self.total_frames) {
            return Err(ConservationError::TotalMismatch {
                expected: self.total_frames,
                actual,
            });
        }
        match self.get(Location::Leaked) {
            0 => Ok(()),
            count => Err(ConservationError::Leaked {
                count,
                first_chunk: None,
            }),
        }
    }

    /// The count model of one transition (mirrors `apply` in `lean/WarpUmem.lean`).
    pub fn apply(&mut self, kind: TransitionKind) -> Result<(), ConservationError> {
        let (from, to) = kind.endpoints();
        let source = &mut self.counts[from.index()];
        *source = source
            .checked_sub(1)
            .ok_or(ConservationError::Underflow { transition: kind })?;
        let dest = &mut self.counts[to.index()];
        // The source was nonzero, so the sum (<= total <= 2^20) is unchanged.
        *dest += 1;
        Ok(())
    }
}

/// Coarse audit for any backend: the driver's own counts add up and nothing
/// leaked. Used for the Linux backend, whose kernel side is opaque.
pub fn audit_driver<B: XskBackend>(xsk: &mut Xsk<B>) -> Result<(), ConservationError> {
    let total = xsk.config().frame_count;
    let c = xsk.census();
    let actual = [
        c.free,
        c.fill_posted,
        c.app_rx,
        c.app_tx,
        c.tx_posted,
        c.in_flight,
        c.leaked,
    ]
    .iter()
    .map(|&n| u64::from(n))
    .sum::<u64>();
    if actual != u64::from(total) {
        return Err(ConservationError::TotalMismatch {
            expected: total,
            actual,
        });
    }
    if c.free as usize != xsk.free_list().len() {
        return Err(ConservationError::SideMismatch {
            side: "free list",
            driver: c.free,
            device: xsk.free_list().len() as u32,
        });
    }
    if c.leaked > 0 {
        return Err(leaked(xsk, c.leaked));
    }
    Ok(())
}

fn leaked<B: XskBackend>(xsk: &Xsk<B>, count: u32) -> ConservationError {
    let first_chunk = (0..xsk.config().frame_count).find(|&c| {
        matches!(
            xsk.location(c),
            Some(DriverLoc::Leaked | DriverLoc::TxPosted(LeaseMark::Leaked))
        )
    });
    ConservationError::Leaked { count, first_chunk }
}

/// Exact auditor for the software NIC. Reusable scratch space keeps repeated
/// audits allocation-free after the first.
#[derive(Debug, Default)]
pub struct UmemAuditor {
    seen: Vec<u8>,
}

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
        let acc = self.counts(xsk)?;
        acc.check_conservation().map_err(|e| match e {
            ConservationError::Leaked { count, .. } => leaked(xsk, count),
            other => other,
        })?;
        let pending_leaked = (0..acc.total_frames)
            .filter(|&c| xsk.location(c) == Some(DriverLoc::TxPosted(LeaseMark::Leaked)))
            .count() as u32;
        if pending_leaked > 0 {
            return Err(leaked(xsk, pending_leaked));
        }
        Ok(acc)
    }

    /// Fine-grained counts from both halves, checked chunk by chunk: the
    /// sides agree, every chunk is in exactly one place, and the counts add
    /// up to the UMEM. Leaked chunks are counted (in `Location::Leaked`), not
    /// rejected.
    pub fn counts(
        &mut self,
        xsk: &mut Xsk<SoftNic>,
    ) -> Result<XdpUmemAccounting, ConservationError> {
        let total = xsk.config().frame_count;
        let driver = xsk.census();
        let kernel = xsk.backend().kernel_census();
        let mut acc = XdpUmemAccounting {
            total_frames: total,
            counts: [0; LOCATIONS.len()],
        };
        let mut set = |loc: Location, n: u32| acc.counts[loc.index()] = n;
        set(Location::Free, driver.free);
        set(Location::FillRing, kernel.fill_ring);
        set(Location::NicRx, kernel.nic_rx);
        set(Location::RxRing, kernel.rx_ring);
        set(Location::AppRx, driver.app_rx);
        set(Location::AppTx, driver.app_tx);
        set(Location::TxRing, kernel.tx_ring);
        set(Location::NicTx, kernel.nic_tx);
        set(Location::CompletionRing, kernel.completion_ring);
        set(Location::InFlight, driver.in_flight);
        set(Location::Leaked, driver.leaked);

        let rx_side = kernel.fill_ring + kernel.nic_rx + kernel.rx_ring;
        if rx_side != driver.fill_posted {
            return Err(ConservationError::SideMismatch {
                side: "kernel receive side",
                driver: driver.fill_posted,
                device: rx_side,
            });
        }
        let tx_side = kernel.tx_ring + kernel.nic_tx + kernel.completion_ring;
        if tx_side != driver.tx_posted {
            return Err(ConservationError::SideMismatch {
                side: "kernel transmit side",
                driver: driver.tx_posted,
                device: tx_side,
            });
        }

        // Chunk-level multiset check: every chunk exactly once.
        self.seen.clear();
        self.seen.resize(total as usize, 0);
        let mut fault: Option<ConservationError> = None;
        for &chunk in xsk.free_list() {
            let driver_loc = xsk.location(chunk).unwrap_or(DriverLoc::Free);
            if driver_loc != DriverLoc::Free {
                first(
                    &mut fault,
                    ConservationError::Misplaced {
                        chunk,
                        device: Location::Free,
                        driver: driver_loc,
                    },
                );
            }
            mark(&mut self.seen, chunk, &mut fault);
        }
        for chunk in 0..total {
            if matches!(
                xsk.location(chunk),
                Some(DriverLoc::AppRx | DriverLoc::AppTx | DriverLoc::InFlight | DriverLoc::Leaked)
            ) {
                mark(&mut self.seen, chunk, &mut fault);
            }
        }
        let seen = &mut self.seen;
        xsk.backend().for_each_kernel_chunk(|chunk, loc| {
            let driver_loc = xsk.location(chunk).unwrap_or(DriverLoc::Free);
            let ok = match loc {
                Location::FillRing | Location::NicRx | Location::RxRing => {
                    driver_loc == DriverLoc::FillPosted
                }
                _ => matches!(driver_loc, DriverLoc::TxPosted(_)),
            };
            if !ok {
                first(
                    &mut fault,
                    ConservationError::Misplaced {
                        chunk,
                        device: loc,
                        driver: driver_loc,
                    },
                );
            }
            mark(seen, chunk, &mut fault);
        });
        if let Some(e) = fault {
            return Err(e);
        }
        if let Some(chunk) = self.seen.iter().position(|&n| n == 0) {
            return Err(ConservationError::Missing {
                chunk: chunk as u32,
            });
        }

        let actual = acc.sum();
        if actual != u64::from(total) {
            return Err(ConservationError::TotalMismatch {
                expected: total,
                actual,
            });
        }
        Ok(acc)
    }
}

fn first(slot: &mut Option<ConservationError>, e: ConservationError) {
    slot.get_or_insert(e);
}

fn mark(seen: &mut [u8], chunk: u32, fault: &mut Option<ConservationError>) {
    let slot = &mut seen[chunk as usize];
    *slot = slot.saturating_add(1);
    if *slot > 1 {
        first(fault, ConservationError::Duplicate { chunk });
    }
}
