//! Linear frame owners and the in-flight packet lease.
//!
//! Section 35's `linear struct UmemFrame[State]` is modelled as a non-Clone,
//! non-Copy, `#[must_use]` owner. Its only exits are the consuming calls on
//! [`crate::Xsk`]. Dropping an owner instead does not recycle the chunk: the
//! drop is recorded in a shared [`Ledger`], the driver quarantines the chunk
//! as `Leaked`, and the auditor reports it. This is host-library discipline,
//! not an implementation of the Vesper source typing rules for `linear`.

use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

mod sealed {
    pub trait Sealed {}
}

/// Typestate of a [`UmemFrame`].
pub trait FrameState: sealed::Sealed {
    const NAME: &'static str;
}

/// `copy struct RxHarvested {}`: a received frame the application holds.
#[derive(Clone, Copy, Debug)]
pub struct RxHarvested;
/// `copy struct TxStaged {}`: a frame being prepared for transmit.
#[derive(Clone, Copy, Debug)]
pub struct TxStaged;
/// A frame handed to the NIC and held by an [`InFlightQuicPkt`]; its bytes
/// are no longer accessible to the application.
#[derive(Clone, Copy, Debug)]
pub struct Submitted;

impl sealed::Sealed for RxHarvested {}
impl sealed::Sealed for TxStaged {}
impl sealed::Sealed for Submitted {}
impl FrameState for RxHarvested {
    const NAME: &'static str = "RxHarvested";
}
impl FrameState for TxStaged {
    const NAME: &'static str = "TxStaged";
}
impl FrameState for Submitted {
    const NAME: &'static str = "Submitted";
}

/// Records dropped owners. Shared between a driver and its live owners;
/// cloning the `Arc` per frame is an atomic increment, not an allocation.
#[derive(Debug)]
pub(crate) struct Ledger {
    leaked: Box<[AtomicBool]>,
    leak_count: AtomicU32,
    unreconciled: AtomicU32,
}

impl Ledger {
    pub(crate) fn new(frames: u32) -> Arc<Self> {
        Arc::new(Self {
            leaked: (0..frames).map(|_| AtomicBool::new(false)).collect(),
            leak_count: AtomicU32::new(0),
            unreconciled: AtomicU32::new(0),
        })
    }
    fn record_leak(&self, chunk: u32) {
        let Some(flag) = usize::try_from(chunk).ok().and_then(|i| self.leaked.get(i)) else {
            return;
        };
        if !flag.swap(true, Ordering::AcqRel) {
            self.leak_count.fetch_add(1, Ordering::AcqRel);
            self.unreconciled.fetch_add(1, Ordering::AcqRel);
        }
    }
    pub(crate) fn is_leaked(&self, chunk: u32) -> bool {
        usize::try_from(chunk)
            .ok()
            .and_then(|i| self.leaked.get(i))
            .is_some_and(|f| f.load(Ordering::Acquire))
    }
    pub(crate) fn leak_count(&self) -> u32 {
        self.leak_count.load(Ordering::Acquire)
    }
    /// Takes the "new leaks since last reconcile" counter.
    pub(crate) fn take_unreconciled(&self) -> u32 {
        if self.unreconciled.load(Ordering::Acquire) == 0 {
            return 0;
        }
        self.unreconciled.swap(0, Ordering::AcqRel)
    }
}

/// `linear struct UmemFrame[State] { chunk_idx: u32, data_offset: u16, pkt_len: u16 }`.
#[must_use = "a UmemFrame is linear: recycle, turn around, submit or abandon it"]
#[derive(Debug)]
pub struct UmemFrame<S: FrameState> {
    chunk_idx: u32,
    data_offset: u16,
    pkt_len: u16,
    ledger: Option<Arc<Ledger>>,
    _state: PhantomData<S>,
}

impl<S: FrameState> UmemFrame<S> {
    pub(crate) fn new(
        ledger: &Arc<Ledger>,
        chunk_idx: u32,
        data_offset: u16,
        pkt_len: u16,
    ) -> Self {
        Self {
            chunk_idx,
            data_offset,
            pkt_len,
            ledger: Some(Arc::clone(ledger)),
            _state: PhantomData,
        }
    }
    pub fn chunk_idx(&self) -> u32 {
        self.chunk_idx
    }
    /// Offset of the packet data within its chunk.
    pub fn data_offset(&self) -> u16 {
        self.data_offset
    }
    pub fn pkt_len(&self) -> u16 {
        self.pkt_len
    }
    pub(crate) fn belongs_to(&self, ledger: &Arc<Ledger>) -> bool {
        self.ledger.as_ref().is_some_and(|l| Arc::ptr_eq(l, ledger))
    }
    /// The documented consuming exit: disarms leak detection.
    pub(crate) fn consume(mut self) -> (u32, u16, u16) {
        self.ledger = None;
        (self.chunk_idx, self.data_offset, self.pkt_len)
    }
}

impl<S: FrameState> Drop for UmemFrame<S> {
    fn drop(&mut self) {
        if let Some(ledger) = self.ledger.take() {
            ledger.record_leak(self.chunk_idx);
        }
    }
}

/// `linear struct InFlightQuicPkt { pkt_num_full: u64, edt_departure_ns: u64, held_frame: UmemFrame[TxStaged] }`.
///
/// The lease is the in-flight packet: it owns the submitted frame until the
/// packet is acknowledged or declared lost (`Xsk::retire`). The chunk returns
/// to the free list once the lease is retired *and* the NIC has reported the
/// transmit complete, in either order. Parking it in a timing wheel is lane
/// 4's job. The held frame is in the `Submitted` state rather than
/// `TxStaged` so that it cannot be written or submitted twice.
#[must_use = "an in-flight packet is linear: retire it on ack or loss"]
#[derive(Debug)]
pub struct InFlightQuicPkt {
    pkt_num_full: u64,
    edt_departure_ns: u64,
    held_frame: UmemFrame<Submitted>,
}

impl InFlightQuicPkt {
    pub(crate) fn new(
        pkt_num_full: u64,
        edt_departure_ns: u64,
        held_frame: UmemFrame<Submitted>,
    ) -> Self {
        Self {
            pkt_num_full,
            edt_departure_ns,
            held_frame,
        }
    }
    pub fn pkt_num_full(&self) -> u64 {
        self.pkt_num_full
    }
    pub fn edt_departure_ns(&self) -> u64 {
        self.edt_departure_ns
    }
    pub fn held_frame(&self) -> &UmemFrame<Submitted> {
        &self.held_frame
    }
    pub fn chunk_idx(&self) -> u32 {
        self.held_frame.chunk_idx
    }
    pub(crate) fn belongs_to(&self, ledger: &Arc<Ledger>) -> bool {
        self.held_frame.belongs_to(ledger)
    }
    pub(crate) fn consume(self) -> u32 {
        self.held_frame.consume().0
    }
}
