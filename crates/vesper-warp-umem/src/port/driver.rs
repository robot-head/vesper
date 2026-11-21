//! The Vesper build of the driver: the location table, free list, census
//! and statistics live in the device handle and every operation runs in
//! `vesper/src/driver.ves`. This wrapper keeps the linear owners and their
//! drop ledger (Rust ownership of Rust values), and reports dropped owners
//! to Vesper before each operation.

use std::sync::Arc;

use crate::binding::{self, Device};
use crate::config::UmemConfig;
use crate::error::{Refused, RxRejected, UmemError};
use crate::frame::{InFlightQuicPkt, Ledger, RxHarvested, Submitted, TxStaged, UmemFrame};

/// Packet numbers are below 2^62 (shared wave-four vocabulary, RFC 9000 §17.1).
pub const MAX_PACKET_NUMBER_EXCLUSIVE: u64 = 1 << 62;

/// Device-side access the driver needs. Sealed: only the driver reaches the
/// Vesper device and the UMEM bytes.
pub(crate) mod sealed {
    use crate::binding::Device;

    pub trait BackendAccess {
        /// The Vesper device: the device half and the driver's state.
        fn device(&self) -> &Device;
        /// The device and the UMEM bytes, lent together to one call.
        fn parts(&mut self) -> (&mut Device, &mut [u8]);
        /// The UMEM bytes.
        fn umem(&self) -> &[u8];
        /// Moves transitions recorded in Vesper to the backend's trace.
        fn sync_trace(&mut self) {}
    }
}

/// A device behind a driver. Implemented only inside this crate (see
/// `sealed::BackendAccess`); the Vesper build has the software NIC only.
pub trait XskBackend: sealed::BackendAccess {
    /// Tells the device there is transmit work (`sendto` on a real socket).
    fn wake_tx(&mut self) -> Result<(), UmemError> {
        Ok(())
    }
}

/// What happened to a lease's lease mark while its chunk is on the kernel tx side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseMark {
    Live,
    Retired,
    Leaked,
}

/// The driver's own view of where each chunk is. The kernel-side positions
/// (fill ring vs NIC vs rx ring, and tx ring vs NIC vs completion ring) are not
/// visible to the application, so they are collapsed here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriverLoc {
    Free,
    /// Somewhere on the kernel receive side: fill ring, NIC or rx ring.
    FillPosted,
    AppRx,
    AppTx,
    /// Somewhere on the kernel transmit side: tx ring, NIC or completion ring.
    TxPosted(LeaseMark),
    InFlight,
    Leaked,
}

/// Driver-side counts per [`DriverLoc`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DriverCensus {
    pub free: u32,
    pub fill_posted: u32,
    pub app_rx: u32,
    pub app_tx: u32,
    pub tx_posted: u32,
    /// Of `tx_posted`, those whose lease is still live.
    pub tx_posted_leased: u32,
    pub in_flight: u32,
    pub leaked: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DriverStats {
    pub rx_harvested: u64,
    pub rx_rejected: u64,
    pub rx_rejected_recycled: u64,
    pub tx_submitted: u64,
    pub tx_ring_full: u64,
    pub completions: u64,
    pub completions_invalid: u64,
    pub retired: u64,
    pub wake_errors: u64,
    pub leaks_detected: u64,
}

/// Result of one `harvest_completions` call.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompletionReport {
    /// Completions whose lease is still live (chunk now `InFlight`).
    pub completed_leased: u32,
    /// Completions whose lease had been retired (chunk freed).
    pub freed: u32,
    /// Completions for dropped leases (chunk quarantined).
    pub leaked: u32,
    /// Entries that named no kernel-tx-side chunk; skipped.
    pub invalid: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetireOutcome {
    /// The transmit had completed; the chunk is free again.
    Freed,
    /// The transmit has not completed yet; the chunk is freed by its completion.
    AwaitingCompletion,
}

/// The §35.2 `linear effect XdpNic`, minus `park_for_ack_or_pto` (lane 4).
pub trait XdpNic {
    /// `harvest_rx_single`: the next received frame, `Ok(None)` if the rx ring
    /// is empty, or a typed rejection of a malformed descriptor (whose chunk,
    /// when identifiable, was already returned to the fill ring).
    fn harvest_rx(&mut self) -> Result<Option<UmemFrame<RxHarvested>>, RxRejected>;
    /// `view_frame_bytes(in frame)`.
    fn view_frame_bytes<'a>(
        &'a self,
        frame: &UmemFrame<RxHarvested>,
    ) -> Result<&'a [u8], UmemError>;
    /// `recycle_to_fill_ring(sink frame)`.
    fn recycle_to_fill_ring(
        &mut self,
        frame: UmemFrame<RxHarvested>,
    ) -> Result<(), Refused<UmemFrame<RxHarvested>>>;
    /// `turn_around_tx_in_place(sink rx_frame, tx_len)`: same chunk, same data offset.
    fn turn_around_tx_in_place(
        &mut self,
        frame: UmemFrame<RxHarvested>,
        tx_len: u16,
    ) -> Result<UmemFrame<TxStaged>, Refused<UmemFrame<RxHarvested>>>;
    /// `submit_paced_tx(sink tx_frame, pkt_num_full, edt_launch_ns)`.
    fn submit_paced_tx(
        &mut self,
        frame: UmemFrame<TxStaged>,
        pkt_num_full: u64,
        edt_launch_ns: u64,
    ) -> Result<InFlightQuicPkt, Refused<UmemFrame<TxStaged>>>;
    /// Drains up to `max` completion-ring entries.
    fn harvest_completions(&mut self, max: u32) -> CompletionReport;
    /// Ends a lease on ack or loss.
    fn retire(&mut self, lease: InFlightQuicPkt)
    -> Result<RetireOutcome, Refused<InFlightQuicPkt>>;
}

/// An AF_XDP socket driver over a UMEM: the Vesper device holds the
/// location table, the free list and the counters; this wrapper holds the
/// leak ledger.
pub struct Xsk<B: XskBackend> {
    backend: B,
    config: UmemConfig,
    ledger: Arc<Ledger>,
}

const APP_RX: u64 = 2;
const APP_TX: u64 = 3;

impl<B: XskBackend> Xsk<B> {
    /// All frames start on the free list; call [`Xsk::replenish_fill`].
    pub fn new(config: UmemConfig, mut backend: B) -> Result<Self, UmemError> {
        config.validate()?;
        if let Some(e) = binding::xsk_attach(backend.parts().0, config) {
            return Err(e.into());
        }
        Ok(Self {
            backend,
            config,
            ledger: Ledger::new(config.frame_count),
        })
    }

    pub fn config(&self) -> &UmemConfig {
        &self.config
    }
    pub fn backend(&self) -> &B {
        &self.backend
    }
    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }
    pub(crate) fn device(&self) -> &Device {
        self.backend.device()
    }
    pub(crate) fn device_mut(&mut self) -> &mut Device {
        self.backend.parts().0
    }
    pub fn stats(&self) -> DriverStats {
        binding::xsk_stats(self.backend.device())
    }
    /// Counts per driver location, after reconciling dropped owners.
    pub fn census(&mut self) -> DriverCensus {
        self.reconcile_leaks();
        binding::xsk_census(self.backend.device())
    }
    pub fn location(&self, chunk: u32) -> Option<DriverLoc> {
        binding::xsk_location(self.backend.device(), chunk)
    }
    /// Chunks currently on the free list, in pop order (last first).
    pub fn free_list(&self) -> &[u32] {
        self.backend.device().as_ref().free().as_slice()
    }
    /// Total dropped owners ever detected.
    pub fn leaked_owners(&self) -> u32 {
        self.ledger.leak_count()
    }

    /// Quarantines chunks whose owner was dropped since the last call.
    fn reconcile_leaks(&mut self) {
        if self.ledger.take_unreconciled() == 0 {
            return;
        }
        let leaked: Vec<u32> = (0..self.config.frame_count)
            .filter(|&chunk| self.ledger.is_leaked(chunk))
            .collect();
        binding::xsk_reconcile(self.backend.parts().0, &leaked);
        self.backend.sync_trace();
    }

    fn owned<S: crate::frame::FrameState>(&self, frame: &UmemFrame<S>) -> Result<(), UmemError> {
        if frame.belongs_to(&self.ledger) {
            Ok(())
        } else {
            Err(UmemError::ForeignOwner)
        }
    }

    /// Moves up to `max` free chunks into the fill ring; returns how many.
    pub fn replenish_fill(&mut self, max: u32) -> u32 {
        self.reconcile_leaks();
        let n = binding::xsk_replenish_fill(self.backend.parts().0, max);
        self.backend.sync_trace();
        n
    }

    /// A fresh transmit frame of `len` bytes at the headroom, zeroed.
    pub fn alloc_tx(&mut self, len: u16) -> Result<UmemFrame<TxStaged>, UmemError> {
        self.reconcile_leaks();
        let (dev, mem) = self.backend.parts();
        let (chunk, offset) = binding::xsk_alloc_tx(dev, mem, u32::from(len))?;
        self.backend.sync_trace();
        // offset is the headroom, below frame_size <= 2^15.
        Ok(UmemFrame::new(&self.ledger, chunk, offset as u16, len))
    }

    /// Gives up a staged frame without sending it.
    pub fn abandon_tx(
        &mut self,
        frame: UmemFrame<TxStaged>,
    ) -> Result<(), Refused<UmemFrame<TxStaged>>> {
        self.reconcile_leaks();
        if let Err(e) = self.owned(&frame) {
            return Err(Refused::new(frame, e));
        }
        if let Some(e) = binding::xsk_abandon_tx(self.backend.parts().0, frame.chunk_idx()) {
            return Err(Refused::new(frame, e.into()));
        }
        let _ = frame.consume();
        self.backend.sync_trace();
        Ok(())
    }

    fn window_mut<S: crate::frame::FrameState>(
        &mut self,
        frame: &UmemFrame<S>,
        expected: u64,
    ) -> Result<&mut [u8], UmemError> {
        self.owned(frame)?;
        let addr = binding::xsk_frame_addr(
            self.backend.device(),
            frame.chunk_idx(),
            u32::from(frame.data_offset()),
            expected,
        )? as usize;
        let (_, mem) = self.backend.parts();
        mem.get_mut(addr..addr + usize::from(frame.pkt_len()))
            .ok_or(UmemError::WrongLocation {
                chunk: frame.chunk_idx(),
            })
    }

    /// Mutable bytes of a received frame (for in-place edits before turn-around).
    pub fn rx_bytes_mut(
        &mut self,
        frame: &mut UmemFrame<RxHarvested>,
    ) -> Result<&mut [u8], UmemError> {
        self.window_mut(frame, APP_RX)
    }

    /// Mutable bytes of a staged transmit frame.
    pub fn tx_bytes_mut(
        &mut self,
        frame: &mut UmemFrame<TxStaged>,
    ) -> Result<&mut [u8], UmemError> {
        self.window_mut(frame, APP_TX)
    }

    /// Turn-around with a new data offset (to prepend or strip headers).
    /// Rejects windows that overlap the headroom or leave the chunk, returning
    /// the received frame. Bytes of the new window that the received packet did
    /// not cover are zeroed, so no stale data from earlier packets is sent.
    pub fn turn_around_tx_at(
        &mut self,
        frame: UmemFrame<RxHarvested>,
        data_offset: u16,
        tx_len: u16,
    ) -> Result<UmemFrame<TxStaged>, Refused<UmemFrame<RxHarvested>>> {
        self.reconcile_leaks();
        if let Err(e) = self.owned(&frame) {
            return Err(Refused::new(frame, e));
        }
        let (dev, mem) = self.backend.parts();
        if let Some(e) = binding::xsk_turn_around(
            dev,
            mem,
            frame.chunk_idx(),
            u32::from(frame.data_offset()),
            u32::from(frame.pkt_len()),
            u32::from(data_offset),
            u32::from(tx_len),
        ) {
            return Err(Refused::new(frame, e.into()));
        }
        let (chunk, _, _) = frame.consume();
        self.backend.sync_trace();
        Ok(UmemFrame::new(&self.ledger, chunk, data_offset, tx_len))
    }
}

impl<B: XskBackend> XdpNic for Xsk<B> {
    fn harvest_rx(&mut self) -> Result<Option<UmemFrame<RxHarvested>>, RxRejected> {
        self.reconcile_leaks();
        let harvested = binding::xsk_harvest_rx(self.backend.parts().0);
        self.backend.sync_trace();
        // offset and len are below frame_size <= 2^15.
        Ok(harvested?.map(|(chunk, offset, len)| {
            UmemFrame::new(&self.ledger, chunk, offset as u16, len as u16)
        }))
    }

    fn view_frame_bytes<'a>(
        &'a self,
        frame: &UmemFrame<RxHarvested>,
    ) -> Result<&'a [u8], UmemError> {
        self.owned(frame)?;
        let addr = binding::xsk_frame_addr(
            self.backend.device(),
            frame.chunk_idx(),
            u32::from(frame.data_offset()),
            APP_RX,
        )? as usize;
        self.backend
            .umem()
            .get(addr..addr + usize::from(frame.pkt_len()))
            .ok_or(UmemError::WrongLocation {
                chunk: frame.chunk_idx(),
            })
    }

    fn recycle_to_fill_ring(
        &mut self,
        frame: UmemFrame<RxHarvested>,
    ) -> Result<(), Refused<UmemFrame<RxHarvested>>> {
        self.reconcile_leaks();
        if let Err(e) = self.owned(&frame) {
            return Err(Refused::new(frame, e));
        }
        if let Some(e) = binding::xsk_recycle(self.backend.parts().0, frame.chunk_idx()) {
            return Err(Refused::new(frame, e.into()));
        }
        let _ = frame.consume();
        self.backend.sync_trace();
        Ok(())
    }

    fn turn_around_tx_in_place(
        &mut self,
        frame: UmemFrame<RxHarvested>,
        tx_len: u16,
    ) -> Result<UmemFrame<TxStaged>, Refused<UmemFrame<RxHarvested>>> {
        let offset = frame.data_offset();
        self.turn_around_tx_at(frame, offset, tx_len)
    }

    fn submit_paced_tx(
        &mut self,
        frame: UmemFrame<TxStaged>,
        pkt_num_full: u64,
        edt_launch_ns: u64,
    ) -> Result<InFlightQuicPkt, Refused<UmemFrame<TxStaged>>> {
        self.reconcile_leaks();
        if let Err(e) = self.owned(&frame) {
            return Err(Refused::new(frame, e));
        }
        if let Some(e) = binding::xsk_submit(
            self.backend.parts().0,
            frame.chunk_idx(),
            u32::from(frame.data_offset()),
            u32::from(frame.pkt_len()),
            pkt_num_full,
        ) {
            return Err(Refused::new(frame, e.into()));
        }
        let (chunk, offset, len) = frame.consume();
        self.backend.sync_trace();
        // The descriptor is already published; a failed wake is retried by
        // the next one (the software NIC needs none).
        let _ = self.backend.wake_tx();
        let held = UmemFrame::<Submitted>::new(&self.ledger, chunk, offset, len);
        Ok(InFlightQuicPkt::new(pkt_num_full, edt_launch_ns, held))
    }

    fn harvest_completions(&mut self, max: u32) -> CompletionReport {
        self.reconcile_leaks();
        let report = binding::xsk_harvest_completions(self.backend.parts().0, max);
        self.backend.sync_trace();
        report
    }

    fn retire(
        &mut self,
        lease: InFlightQuicPkt,
    ) -> Result<RetireOutcome, Refused<InFlightQuicPkt>> {
        self.reconcile_leaks();
        if !lease.belongs_to(&self.ledger) {
            return Err(Refused::new(lease, UmemError::ForeignOwner));
        }
        match binding::xsk_retire(self.backend.parts().0, lease.chunk_idx()) {
            Ok(outcome) => {
                let _ = lease.consume();
                self.backend.sync_trace();
                Ok(outcome)
            }
            Err(e) => Err(Refused::new(lease, e.into())),
        }
    }
}
