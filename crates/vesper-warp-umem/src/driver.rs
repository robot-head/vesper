//! The application-side XSK driver and the §35.2 `XdpNic` operations.

use std::sync::Arc;

use crate::config::UmemConfig;
use crate::error::{DescError, Refused, RxRejected, UmemError};
use crate::frame::{InFlightQuicPkt, Ledger, RxHarvested, Submitted, TxStaged, UmemFrame};
use crate::ring::{ConsumerRing, ProducerRing, push_one};
use crate::trace::{Transition, TransitionKind};
use crate::wire::XdpDesc;

/// Packet numbers are below 2^62 (shared wave-four vocabulary, RFC 9000 §17.1).
pub const MAX_PACKET_NUMBER_EXCLUSIVE: u64 = 1 << 62;

/// Device-side access the driver needs. Sealed: the trait is not nameable
/// outside this crate, so its methods (raw UMEM bytes and the rings) cannot
/// be called on a backend from safe external code. With the AF_XDP backend
/// that matters for soundness: a slice of a kernel-owned chunk would alias
/// memory the kernel writes. Only the driver calls these, and only for
/// ranges it has checked belong to application-owned chunks.
pub(crate) mod sealed {
    use crate::error::UmemError;
    use crate::ring::{ConsumerRing, ProducerRing};
    use crate::trace::Transition;
    use crate::wire::XdpDesc;

    pub trait BackendAccess {
        type Fill: ProducerRing<u64>;
        type Rx: ConsumerRing<XdpDesc>;
        type Tx: ProducerRing<XdpDesc>;
        type Completion: ConsumerRing<u64>;
        fn fill(&mut self) -> &mut Self::Fill;
        fn rx(&mut self) -> &mut Self::Rx;
        fn tx(&mut self) -> &mut Self::Tx;
        fn completion(&mut self) -> &mut Self::Completion;
        /// Size of the UMEM the device registered, in bytes.
        fn umem_len(&self) -> usize;
        /// `(frame_size, headroom)` the device was set up with.
        fn frame_geometry(&self) -> (u32, u32);
        /// `len` bytes of UMEM at byte offset `addr`, or `None` if out of range.
        fn frame(&self, addr: u64, len: usize) -> Option<&[u8]>;
        fn frame_mut(&mut self, addr: u64, len: usize) -> Option<&mut [u8]>;
        /// Tells the device there is transmit work (`sendto` on a real socket).
        fn kick_tx(&mut self) -> Result<(), UmemError> {
            Ok(())
        }
        /// Observes a driver-side transition (the software NIC traces them).
        fn record(&mut self, _transition: Transition) {}
    }
}

/// A device behind a driver: the software NIC or a real kernel socket.
/// Implemented only inside this crate (see `sealed::BackendAccess`).
pub trait XskBackend: sealed::BackendAccess {
    /// Tells the device there is transmit work (`sendto` on a real socket).
    fn wake_tx(&mut self) -> Result<(), UmemError> {
        self.kick_tx()
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

/// An AF_XDP socket driver over a UMEM: owns the location table, the free
/// list and the leak ledger, and talks to the device through [`XskBackend`].
pub struct Xsk<B: XskBackend> {
    backend: B,
    config: UmemConfig,
    umem_len: u64,
    table: Box<[DriverLoc]>,
    census: DriverCensus,
    free: Vec<u32>,
    ledger: Arc<Ledger>,
    stats: DriverStats,
}

impl<B: XskBackend> Xsk<B> {
    /// All frames start on the free list; call [`Xsk::replenish_fill`].
    pub fn new(config: UmemConfig, mut backend: B) -> Result<Self, UmemError> {
        config.validate()?;
        let same_geometry = backend.umem_len() == config.umem_len()?
            && backend.frame_geometry() == (config.frame_size, config.headroom)
            && backend.fill().size() == config.fill_size
            && backend.rx().size() == config.rx_size
            && backend.tx().size() == config.tx_size
            && backend.completion().size() == config.completion_size;
        if !same_geometry {
            return Err(UmemError::InvalidConfig(
                "backend UMEM or ring sizes differ from the config",
            ));
        }
        let umem_len = config.chunk_base(config.frame_count);
        Ok(Self {
            backend,
            config,
            umem_len,
            table: vec![DriverLoc::Free; config.frame_count as usize].into_boxed_slice(),
            census: DriverCensus {
                free: config.frame_count,
                ..DriverCensus::default()
            },
            // Chunk 0 is popped first.
            free: (0..config.frame_count).rev().collect(),
            ledger: Ledger::new(config.frame_count),
            stats: DriverStats::default(),
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
    pub fn stats(&self) -> DriverStats {
        self.stats
    }
    /// Counts per driver location, after reconciling dropped owners.
    pub fn census(&mut self) -> DriverCensus {
        self.reconcile_leaks();
        self.census
    }
    pub fn location(&self, chunk: u32) -> Option<DriverLoc> {
        self.table.get(chunk as usize).copied()
    }
    /// Chunks currently on the free list, in pop order (last first).
    pub fn free_list(&self) -> &[u32] {
        &self.free
    }
    /// Total dropped owners ever detected.
    pub fn leaked_owners(&self) -> u32 {
        self.ledger.leak_count()
    }

    fn base(&self, chunk: u32) -> u64 {
        self.config.chunk_base(chunk)
    }

    fn set_loc(&mut self, chunk: u32, to: DriverLoc) {
        let slot = &mut self.table[chunk as usize];
        let from = *slot;
        *slot = to;
        let c = &mut self.census;
        // Counts are bounded by frame_count <= 2^20, so these never wrap.
        for (loc, delta) in [(from, -1i8), (to, 1)] {
            let field = match loc {
                DriverLoc::Free => &mut c.free,
                DriverLoc::FillPosted => &mut c.fill_posted,
                DriverLoc::AppRx => &mut c.app_rx,
                DriverLoc::AppTx => &mut c.app_tx,
                DriverLoc::TxPosted(_) => &mut c.tx_posted,
                DriverLoc::InFlight => &mut c.in_flight,
                DriverLoc::Leaked => &mut c.leaked,
            };
            *field = if delta < 0 { *field - 1 } else { *field + 1 };
            if loc == DriverLoc::TxPosted(LeaseMark::Live) {
                c.tx_posted_leased = if delta < 0 {
                    c.tx_posted_leased - 1
                } else {
                    c.tx_posted_leased + 1
                };
            }
        }
    }

    fn transition(&mut self, kind: TransitionKind, chunk: u32) {
        self.backend.record(Transition { kind, chunk });
    }

    /// Quarantines chunks whose owner was dropped since the last call.
    fn reconcile_leaks(&mut self) {
        if self.ledger.take_unreconciled() == 0 {
            return;
        }
        for chunk in 0..self.config.frame_count {
            if !self.ledger.is_leaked(chunk) {
                continue;
            }
            let kind = match self.table[chunk as usize] {
                DriverLoc::AppRx => Some(TransitionKind::LeakRx),
                DriverLoc::AppTx => Some(TransitionKind::LeakTx),
                DriverLoc::InFlight => Some(TransitionKind::LeakInFlight),
                DriverLoc::TxPosted(LeaseMark::Live) => {
                    // Still on the kernel side; quarantined when it completes.
                    self.set_loc(chunk, DriverLoc::TxPosted(LeaseMark::Leaked));
                    self.stats.leaks_detected += 1;
                    None
                }
                _ => None,
            };
            if let Some(kind) = kind {
                self.set_loc(chunk, DriverLoc::Leaked);
                self.stats.leaks_detected += 1;
                self.transition(kind, chunk);
            }
        }
    }

    fn check_owner<S: crate::frame::FrameState>(
        &self,
        frame: &UmemFrame<S>,
        expected: DriverLoc,
    ) -> Result<(), UmemError> {
        if !frame.belongs_to(&self.ledger) {
            return Err(UmemError::ForeignOwner);
        }
        if self.location(frame.chunk_idx()) != Some(expected) {
            return Err(UmemError::WrongLocation {
                chunk: frame.chunk_idx(),
            });
        }
        Ok(())
    }

    /// Validates a window `[offset, offset + len)` inside one chunk.
    fn check_window(&self, offset: u32, len: u32) -> Result<(), UmemError> {
        if offset < self.config.headroom {
            return Err(UmemError::HeadroomOverlap {
                offset,
                headroom: self.config.headroom,
            });
        }
        match offset.checked_add(len) {
            Some(end) if len > 0 && end <= self.config.frame_size => Ok(()),
            _ => Err(UmemError::LengthOutOfFrame {
                offset,
                len,
                frame_size: self.config.frame_size,
            }),
        }
    }

    /// Classifies an rx descriptor. `Err((reason, Some(chunk)))` means the
    /// chunk is kernel-owned and must go back to the fill ring.
    fn classify_rx(&self, desc: XdpDesc) -> Result<(u32, u16, u16), (DescError, Option<u32>)> {
        if desc.addr >= self.umem_len {
            return Err((DescError::OutOfUmem { addr: desc.addr }, None));
        }
        // addr < umem_len = frame_count << shift, so the quotient fits u32.
        let chunk = (desc.addr >> self.config.frame_shift()) as u32;
        if self.table[chunk as usize] != DriverLoc::FillPosted {
            return Err((DescError::NotKernelOwned { chunk }, None));
        }
        // Lower bits of a u64 masked by frame_size - 1 < 2^15.
        let offset = (desc.addr & u64::from(self.config.frame_size - 1)) as u32;
        if desc.options != 0 {
            return Err((
                DescError::UnsupportedOptions {
                    options: desc.options,
                },
                Some(chunk),
            ));
        }
        if desc.len == 0 {
            return Err((DescError::ZeroLength, Some(chunk)));
        }
        if offset < self.config.headroom {
            return Err((
                DescError::HeadroomOverlap {
                    offset,
                    headroom: self.config.headroom,
                },
                Some(chunk),
            ));
        }
        match offset.checked_add(desc.len) {
            // frame_size <= 2^15, so both fit u16.
            Some(end) if end <= self.config.frame_size => {
                Ok((chunk, offset as u16, desc.len as u16))
            }
            _ => Err((
                DescError::OverLength {
                    offset,
                    len: desc.len,
                    frame_size: self.config.frame_size,
                },
                Some(chunk),
            )),
        }
    }

    /// Posts a kernel-bound chunk to the fill ring; falls back to the free
    /// list (unreachable while `fill_size >= frame_count`).
    fn post_fill(&mut self, chunk: u32, ok: TransitionKind, fallback: TransitionKind) {
        let base = self.base(chunk);
        if push_one(self.backend.fill(), base) {
            self.set_loc(chunk, DriverLoc::FillPosted);
            self.transition(ok, chunk);
        } else {
            self.set_loc(chunk, DriverLoc::Free);
            self.free.push(chunk);
            self.transition(fallback, chunk);
        }
    }

    fn release_to_free(&mut self, chunk: u32, kind: TransitionKind) {
        self.set_loc(chunk, DriverLoc::Free);
        // Capacity is frame_count and each chunk is on the list at most once.
        self.free.push(chunk);
        self.transition(kind, chunk);
    }

    /// Moves up to `max` free chunks into the fill ring; returns how many.
    pub fn replenish_fill(&mut self, max: u32) -> u32 {
        self.reconcile_leaks();
        let available = u32::try_from(self.free.len()).unwrap_or(u32::MAX);
        let wanted = max.min(available);
        if wanted == 0 {
            return 0;
        }
        let n = wanted.min(self.backend.fill().free_entries(wanted));
        let Some(first) = (n > 0).then(|| self.backend.fill().reserve(n)).flatten() else {
            return 0;
        };
        for i in 0..n {
            let chunk = self.free.pop().expect("n <= free.len()");
            let base = self.base(chunk);
            if self
                .backend
                .fill()
                .write(first.wrapping_add(i), base)
                .is_err()
            {
                unreachable!("index {i} is inside the reserved window");
            }
            self.set_loc(chunk, DriverLoc::FillPosted);
            self.transition(TransitionKind::Replenish, chunk);
        }
        self.backend
            .fill()
            .submit(n)
            .expect("submitting exactly the reserved count");
        n
    }

    /// A fresh transmit frame of `len` bytes at the headroom, zeroed.
    pub fn alloc_tx(&mut self, len: u16) -> Result<UmemFrame<TxStaged>, UmemError> {
        self.reconcile_leaks();
        let headroom = self.config.headroom;
        self.check_window(headroom, u32::from(len))?;
        let chunk = self.free.pop().ok_or(UmemError::NoFreeFrame)?;
        let addr = self.base(chunk) + u64::from(headroom);
        if let Some(bytes) = self.backend.frame_mut(addr, usize::from(len)) {
            bytes.fill(0);
        }
        self.set_loc(chunk, DriverLoc::AppTx);
        self.transition(TransitionKind::AllocTx, chunk);
        // headroom < frame_size <= 2^15.
        Ok(UmemFrame::new(&self.ledger, chunk, headroom as u16, len))
    }

    /// Gives up a staged frame without sending it.
    pub fn abandon_tx(
        &mut self,
        frame: UmemFrame<TxStaged>,
    ) -> Result<(), Refused<UmemFrame<TxStaged>>> {
        self.reconcile_leaks();
        if let Err(e) = self.check_owner(&frame, DriverLoc::AppTx) {
            return Err(Refused::new(frame, e));
        }
        let (chunk, _, _) = frame.consume();
        self.release_to_free(chunk, TransitionKind::AbandonTx);
        Ok(())
    }

    /// Mutable bytes of a received frame (for in-place edits before turn-around).
    pub fn rx_bytes_mut(
        &mut self,
        frame: &mut UmemFrame<RxHarvested>,
    ) -> Result<&mut [u8], UmemError> {
        self.check_owner(frame, DriverLoc::AppRx)?;
        let addr = self.base(frame.chunk_idx()) + u64::from(frame.data_offset());
        self.backend
            .frame_mut(addr, usize::from(frame.pkt_len()))
            .ok_or(UmemError::WrongLocation {
                chunk: frame.chunk_idx(),
            })
    }

    /// Mutable bytes of a staged transmit frame.
    pub fn tx_bytes_mut(
        &mut self,
        frame: &mut UmemFrame<TxStaged>,
    ) -> Result<&mut [u8], UmemError> {
        self.check_owner(frame, DriverLoc::AppTx)?;
        let addr = self.base(frame.chunk_idx()) + u64::from(frame.data_offset());
        self.backend
            .frame_mut(addr, usize::from(frame.pkt_len()))
            .ok_or(UmemError::WrongLocation {
                chunk: frame.chunk_idx(),
            })
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
        if let Err(e) = self
            .check_owner(&frame, DriverLoc::AppRx)
            .and_then(|()| self.check_window(u32::from(data_offset), u32::from(tx_len)))
        {
            return Err(Refused::new(frame, e));
        }
        let (chunk, old_off, old_len) = frame.consume();
        let base = self.base(chunk);
        let (new_start, new_end) = (
            u32::from(data_offset),
            u32::from(data_offset) + u32::from(tx_len),
        );
        let (old_start, old_end) = (u32::from(old_off), u32::from(old_off) + u32::from(old_len));
        let chunk_bytes = self
            .backend
            .frame_mut(base, self.config.frame_size as usize)
            .expect("chunk is inside the UMEM");
        for (from, to) in [
            (new_start, new_end.min(old_start)),
            (new_start.max(old_end), new_end),
        ] {
            if from < to {
                chunk_bytes[from as usize..to as usize].fill(0);
            }
        }
        self.set_loc(chunk, DriverLoc::AppTx);
        self.transition(TransitionKind::TurnAround, chunk);
        Ok(UmemFrame::new(&self.ledger, chunk, data_offset, tx_len))
    }
}

impl<B: XskBackend> XdpNic for Xsk<B> {
    fn harvest_rx(&mut self) -> Result<Option<UmemFrame<RxHarvested>>, RxRejected> {
        self.reconcile_leaks();
        let rx = self.backend.rx();
        let (count, idx) = rx.peek(1);
        if count == 0 {
            return Ok(None);
        }
        let desc = rx.read(idx).expect("peeked index is readable");
        rx.release(1).expect("releasing the peeked entry");
        match self.classify_rx(desc) {
            Ok((chunk, offset, len)) => {
                self.set_loc(chunk, DriverLoc::AppRx);
                self.transition(TransitionKind::Harvest, chunk);
                self.stats.rx_harvested += 1;
                Ok(Some(UmemFrame::new(&self.ledger, chunk, offset, len)))
            }
            Err((reason, chunk)) => {
                self.stats.rx_rejected += 1;
                if let Some(chunk) = chunk {
                    self.stats.rx_rejected_recycled += 1;
                    self.post_fill(
                        chunk,
                        TransitionKind::HarvestReject,
                        TransitionKind::RejectToFree,
                    );
                }
                Err(RxRejected {
                    reason,
                    chunk,
                    recycled: chunk.is_some(),
                })
            }
        }
    }

    fn view_frame_bytes<'a>(
        &'a self,
        frame: &UmemFrame<RxHarvested>,
    ) -> Result<&'a [u8], UmemError> {
        self.check_owner(frame, DriverLoc::AppRx)?;
        let addr = self.base(frame.chunk_idx()) + u64::from(frame.data_offset());
        self.backend
            .frame(addr, usize::from(frame.pkt_len()))
            .ok_or(UmemError::WrongLocation {
                chunk: frame.chunk_idx(),
            })
    }

    fn recycle_to_fill_ring(
        &mut self,
        frame: UmemFrame<RxHarvested>,
    ) -> Result<(), Refused<UmemFrame<RxHarvested>>> {
        self.reconcile_leaks();
        if let Err(e) = self.check_owner(&frame, DriverLoc::AppRx) {
            return Err(Refused::new(frame, e));
        }
        let (chunk, _, _) = frame.consume();
        self.post_fill(
            chunk,
            TransitionKind::Recycle,
            TransitionKind::RecycleToFree,
        );
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
        if let Err(e) = self.check_owner(&frame, DriverLoc::AppTx) {
            return Err(Refused::new(frame, e));
        }
        if pkt_num_full >= MAX_PACKET_NUMBER_EXCLUSIVE {
            return Err(Refused::new(
                frame,
                UmemError::PacketNumberOutOfRange { pn: pkt_num_full },
            ));
        }
        let desc = XdpDesc {
            addr: self.base(frame.chunk_idx()) + u64::from(frame.data_offset()),
            len: u32::from(frame.pkt_len()),
            options: 0,
        };
        if !push_one(self.backend.tx(), desc) {
            self.stats.tx_ring_full += 1;
            return Err(Refused::new(frame, UmemError::TxRingFull));
        }
        let (chunk, offset, len) = frame.consume();
        self.set_loc(chunk, DriverLoc::TxPosted(LeaseMark::Live));
        self.transition(TransitionKind::Submit, chunk);
        self.stats.tx_submitted += 1;
        if self.backend.wake_tx().is_err() {
            // The descriptor is already published; the next wake retries.
            self.stats.wake_errors += 1;
        }
        let held = UmemFrame::<Submitted>::new(&self.ledger, chunk, offset, len);
        Ok(InFlightQuicPkt::new(pkt_num_full, edt_launch_ns, held))
    }

    fn harvest_completions(&mut self, max: u32) -> CompletionReport {
        self.reconcile_leaks();
        let mut report = CompletionReport::default();
        let (count, first) = self.backend.completion().peek(max);
        for i in 0..count {
            let addr = self
                .backend
                .completion()
                .read(first.wrapping_add(i))
                .expect("peeked index is readable");
            let chunk = (addr < self.umem_len).then(|| (addr >> self.config.frame_shift()) as u32);
            match chunk.map(|c| (c, self.table[c as usize])) {
                Some((chunk, DriverLoc::TxPosted(mark))) => {
                    self.stats.completions += 1;
                    match mark {
                        LeaseMark::Live => {
                            self.set_loc(chunk, DriverLoc::InFlight);
                            self.transition(TransitionKind::CompleteLeased, chunk);
                            report.completed_leased += 1;
                        }
                        LeaseMark::Retired => {
                            self.release_to_free(chunk, TransitionKind::CompleteRetired);
                            report.freed += 1;
                        }
                        LeaseMark::Leaked => {
                            self.set_loc(chunk, DriverLoc::Leaked);
                            self.transition(TransitionKind::CompleteLeaked, chunk);
                            report.leaked += 1;
                        }
                    }
                }
                _ => {
                    self.stats.completions_invalid += 1;
                    report.invalid += 1;
                }
            }
        }
        if count > 0 {
            self.backend
                .completion()
                .release(count)
                .expect("releasing the peeked entries");
        }
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
        let chunk = lease.chunk_idx();
        match self.location(chunk) {
            Some(DriverLoc::InFlight) => {
                let _ = lease.consume();
                self.release_to_free(chunk, TransitionKind::Retire);
                self.stats.retired += 1;
                Ok(RetireOutcome::Freed)
            }
            Some(DriverLoc::TxPosted(LeaseMark::Live)) => {
                let _ = lease.consume();
                self.set_loc(chunk, DriverLoc::TxPosted(LeaseMark::Retired));
                self.stats.retired += 1;
                Ok(RetireOutcome::AwaitingCompletion)
            }
            _ => Err(Refused::new(lease, UmemError::WrongLocation { chunk })),
        }
    }
}
