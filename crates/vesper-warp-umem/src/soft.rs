//! A deterministic software NIC: the kernel/device half of an AF_XDP socket.
//!
//! It consumes the fill and tx rings and produces the rx and completion rings
//! with the Linux semantics this crate models, in two modes:
//! * [`SoftNicMode::Loopback`]: every transmitted packet is delivered back
//!   into a fill buffer and published on the rx ring;
//! * [`SoftNicMode::Scripted`]: received packets come from a queued script or
//!   from [`SoftNic::deliver`].
//!
//! Faults can be injected: malformed rx descriptors (outside the UMEM, into
//! the headroom, past the chunk end, zero length), spurious completions, and
//! tx/completion stalls that create ring-full backpressure.
//!
//! Divergence from Linux, documented: the kernel drops an invalid tx
//! descriptor without completing it (its frame is then lost to user space).
//! The driver validates every descriptor before publishing it, so that path is
//! unreachable through this crate's API; the software NIC still completes such
//! a descriptor and counts it in `tx_invalid`, so a bug shows up in the stats
//! rather than as a lost frame. EDT pacing is not enforced here: the launch
//! time travels with the lease, and NIC launch-time offload is not modelled.

use std::collections::VecDeque;

use crate::config::UmemConfig;
use crate::driver::sealed::BackendAccess;
use crate::driver::{Xsk, XskBackend};
use crate::error::UmemError;
use crate::ring::{ProducerRing, Ring, pop_one, push_one};
use crate::trace::{Transition, TransitionKind};
use crate::wire::XdpDesc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoftNicMode {
    Loopback,
    Scripted,
}

/// One-shot faults, applied at the end of the next `step` that can apply them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// A spurious rx descriptor whose address is outside the UMEM. No chunk
    /// is consumed (a real kernel bug of this kind would also lose a buffer;
    /// the model cannot know which).
    RxOutOfRange,
    /// A real fill buffer delivered with its data offset inside the headroom
    /// (or, with zero headroom, crossing the chunk end).
    RxHeadroomOverlap,
    /// A real fill buffer delivered with a length past the chunk end.
    RxOverLength,
    /// A real fill buffer delivered with length zero.
    RxZeroLength,
    /// A spurious completion entry outside the UMEM.
    CompletionOutOfRange,
    /// The NIC ignores the tx ring for this many steps.
    StallTx(u32),
    /// The NIC posts no completions for this many steps.
    StallCompletion(u32),
}

/// Outcome of delivering one ingress packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliverOutcome {
    Delivered {
        chunk: u32,
    },
    /// No fill buffer was available; the packet was dropped.
    NoBuffer,
    /// The rx ring was full; the packet was dropped, the buffer kept.
    RxRingFull,
    /// The packet does not fit after the headroom; dropped.
    Oversize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SoftNicStats {
    pub rx_delivered: u64,
    pub rx_dropped_no_buffer: u64,
    pub rx_dropped_ring_full: u64,
    pub rx_dropped_oversize: u64,
    pub tx_consumed: u64,
    pub tx_invalid: u64,
    pub completions_posted: u64,
    pub faults_applied: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SoftStepReport {
    pub completed: u32,
    pub fill_pulled: u32,
    pub tx_consumed: u32,
    pub rx_delivered: u32,
    pub faults_applied: u32,
}

/// The device-side view of the chunks, by location.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KernelCensus {
    pub fill_ring: u32,
    pub nic_rx: u32,
    /// Rx-ring entries that name a chunk.
    pub rx_ring: u32,
    pub tx_ring: u32,
    pub nic_tx: u32,
    /// Completion-ring entries that name a chunk.
    pub completion_ring: u32,
    /// Injected rx entries outside the UMEM.
    pub spurious_rx: u32,
    /// Injected completion entries outside the UMEM.
    pub spurious_completion: u32,
}

pub struct SoftNic {
    config: UmemConfig,
    umem_len: u64,
    mem: Box<[u8]>,
    fill: Ring<u64>,
    rx: Ring<XdpDesc>,
    tx: Ring<XdpDesc>,
    completion: Ring<u64>,
    mode: SoftNicMode,
    rx_pool: Vec<u64>,
    nic_tx: VecDeque<u64>,
    ingress: VecDeque<Vec<u8>>,
    faults: VecDeque<Fault>,
    stall_tx: u32,
    stall_completion: u32,
    /// Fill entries the NIC pulls per step.
    pub fill_batch: u32,
    /// Tx descriptors the NIC consumes per step.
    pub tx_batch: u32,
    /// Scripted packets delivered per step.
    pub rx_batch: u32,
    stats: SoftNicStats,
    trace: Option<Vec<Transition>>,
}

/// A driver over the software NIC.
pub type SoftXsk = Xsk<SoftNic>;

impl SoftNic {
    pub fn new(config: UmemConfig, mode: SoftNicMode) -> Result<Self, UmemError> {
        config.validate()?;
        let len = config.umem_len()?;
        let ring = |e| UmemError::Ring(e);
        let frames = config.frame_count as usize;
        Ok(Self {
            config,
            umem_len: config.chunk_base(config.frame_count),
            mem: vec![0u8; len].into_boxed_slice(),
            fill: Ring::new(config.fill_size).map_err(ring)?,
            rx: Ring::new(config.rx_size).map_err(ring)?,
            tx: Ring::new(config.tx_size).map_err(ring)?,
            completion: Ring::new(config.completion_size).map_err(ring)?,
            mode,
            rx_pool: Vec::with_capacity(frames),
            nic_tx: VecDeque::with_capacity(frames),
            ingress: VecDeque::new(),
            faults: VecDeque::with_capacity(16),
            stall_tx: 0,
            stall_completion: 0,
            fill_batch: 64,
            tx_batch: 64,
            rx_batch: 64,
            stats: SoftNicStats::default(),
            trace: None,
        })
    }

    /// Convenience: a driver over a fresh software NIC.
    pub fn xsk(config: UmemConfig, mode: SoftNicMode) -> Result<SoftXsk, UmemError> {
        Xsk::new(config, Self::new(config, mode)?)
    }

    /// Starts recording every transition (driver and NIC) in order.
    pub fn enable_trace(&mut self) {
        self.trace.get_or_insert_with(Vec::new);
    }
    /// Transitions recorded so far (empty unless tracing).
    pub fn trace(&self) -> &[Transition] {
        self.trace.as_deref().unwrap_or(&[])
    }
    pub fn take_trace(&mut self) -> Vec<Transition> {
        self.trace.as_mut().map(std::mem::take).unwrap_or_default()
    }

    pub fn stats(&self) -> SoftNicStats {
        self.stats
    }
    pub fn mode(&self) -> SoftNicMode {
        self.mode
    }
    pub fn queue_ingress(&mut self, packet: Vec<u8>) {
        self.ingress.push_back(packet);
    }
    pub fn inject(&mut self, fault: Fault) {
        self.faults.push_back(fault);
    }
    pub fn pending_faults(&self) -> usize {
        self.faults.len()
    }

    fn note(&mut self, kind: TransitionKind, chunk: u32) {
        if let Some(trace) = &mut self.trace {
            trace.push(Transition { kind, chunk });
        }
    }

    fn chunk_of(&self, addr: u64) -> Option<u32> {
        // addr < umem_len = frame_count << shift.
        (addr < self.umem_len).then(|| (addr >> self.config.frame_shift()) as u32)
    }

    fn pull_fill(&mut self, max: u32) -> u32 {
        let mut pulled = 0;
        while pulled < max {
            let Some(addr) = pop_one(&mut self.fill) else {
                break;
            };
            // Aligned-chunk mode: the kernel masks the address to its chunk.
            let base = addr & !u64::from(self.config.frame_size - 1);
            // The kernel ignores fill addresses outside the UMEM.
            if let Some(chunk) = self.chunk_of(base) {
                self.rx_pool.push(base);
                self.note(TransitionKind::NicPullFill, chunk);
                pulled += 1;
            }
        }
        pulled
    }

    fn take_rx_buffer(&mut self) -> Option<u64> {
        if self.rx_pool.is_empty() {
            self.pull_fill(1);
        }
        self.rx_pool.pop()
    }

    /// Delivers one ingress packet now.
    pub fn deliver(&mut self, packet: &[u8]) -> DeliverOutcome {
        let room = (self.config.frame_size - self.config.headroom) as usize;
        if packet.is_empty() || packet.len() > room {
            self.stats.rx_dropped_oversize += 1;
            return DeliverOutcome::Oversize;
        }
        if ProducerRing::free_entries(&mut self.rx, 1) == 0 {
            self.stats.rx_dropped_ring_full += 1;
            return DeliverOutcome::RxRingFull;
        }
        let Some(base) = self.take_rx_buffer() else {
            self.stats.rx_dropped_no_buffer += 1;
            return DeliverOutcome::NoBuffer;
        };
        let start = (base + u64::from(self.config.headroom)) as usize;
        self.mem[start..start + packet.len()].copy_from_slice(packet);
        self.publish_rx(base, self.config.headroom, packet.len() as u32);
        let chunk = self.chunk_of(base).expect("pool holds in-range chunks");
        self.stats.rx_delivered += 1;
        DeliverOutcome::Delivered { chunk }
    }

    /// Publishes an rx descriptor for a pool buffer (space already checked).
    fn publish_rx(&mut self, base: u64, offset: u32, len: u32) {
        let desc = XdpDesc {
            addr: base + u64::from(offset),
            len,
            options: 0,
        };
        let pushed = push_one(&mut self.rx, desc);
        debug_assert!(pushed, "rx space was checked");
        let chunk = self.chunk_of(base).expect("pool holds in-range chunks");
        self.note(TransitionKind::NicDeliver, chunk);
    }

    /// Loopback delivery of `len` bytes at UMEM offset `src`.
    fn loop_back(&mut self, src: u64, len: u32) {
        if len > self.config.frame_size - self.config.headroom {
            self.stats.rx_dropped_oversize += 1;
            return;
        }
        if ProducerRing::free_entries(&mut self.rx, 1) == 0 {
            self.stats.rx_dropped_ring_full += 1;
            return;
        }
        let Some(base) = self.take_rx_buffer() else {
            self.stats.rx_dropped_no_buffer += 1;
            return;
        };
        let dst = (base + u64::from(self.config.headroom)) as usize;
        let src = src as usize;
        // Different chunks: one is on the tx side, the other on the rx side.
        self.mem.copy_within(src..src + len as usize, dst);
        self.publish_rx(base, self.config.headroom, len);
        self.stats.rx_delivered += 1;
    }

    fn tx_desc_valid(&self, desc: XdpDesc) -> bool {
        let offset = desc.addr & u64::from(self.config.frame_size - 1);
        desc.addr < self.umem_len
            && desc.len > 0
            && offset + u64::from(desc.len) <= u64::from(self.config.frame_size)
    }

    fn apply_fault(&mut self, fault: Fault) -> bool {
        let headroom = self.config.headroom;
        let frame = self.config.frame_size;
        let real = |nic: &mut Self, offset: u32, len: u32| {
            if ProducerRing::free_entries(&mut nic.rx, 1) == 0 {
                return false;
            }
            let Some(base) = nic.take_rx_buffer() else {
                return false;
            };
            nic.publish_rx(base, offset, len);
            true
        };
        match fault {
            Fault::RxOutOfRange => push_one(
                &mut self.rx,
                XdpDesc {
                    addr: self.umem_len + u64::from(headroom),
                    len: 64,
                    options: 0,
                },
            ),
            Fault::RxHeadroomOverlap if headroom > 0 => real(self, headroom - 1, 1),
            Fault::RxHeadroomOverlap => real(self, frame - 1, 2),
            Fault::RxOverLength => real(self, headroom, frame - headroom + 1),
            Fault::RxZeroLength => real(self, headroom, 0),
            Fault::CompletionOutOfRange => push_one(&mut self.completion, self.umem_len),
            Fault::StallTx(n) => {
                self.stall_tx = self.stall_tx.saturating_add(n);
                true
            }
            Fault::StallCompletion(n) => {
                self.stall_completion = self.stall_completion.saturating_add(n);
                true
            }
        }
    }

    /// One device step: complete, pull fill, consume tx (and loop back),
    /// deliver scripted ingress, then apply pending faults.
    pub fn step(&mut self) -> SoftStepReport {
        let mut report = SoftStepReport::default();
        // 1. Completions.
        if self.stall_completion > 0 {
            self.stall_completion -= 1;
        } else {
            while let Some(&addr) = self.nic_tx.front() {
                if !push_one(&mut self.completion, addr) {
                    break;
                }
                self.nic_tx.pop_front();
                self.stats.completions_posted += 1;
                report.completed += 1;
                if let Some(chunk) = self.chunk_of(addr) {
                    self.note(TransitionKind::NicComplete, chunk);
                }
            }
        }
        // 2. Fill.
        report.fill_pulled = self.pull_fill(self.fill_batch);
        // 3. Transmit. Like the kernel, only take a descriptor when its
        //    completion slot is guaranteed.
        if self.stall_tx > 0 {
            self.stall_tx -= 1;
        } else {
            for _ in 0..self.tx_batch {
                let reserved = self.completion.len() as usize + self.nic_tx.len();
                if reserved >= self.completion.capacity() as usize {
                    break;
                }
                let Some(desc) = pop_one(&mut self.tx) else {
                    break;
                };
                self.stats.tx_consumed += 1;
                report.tx_consumed += 1;
                let valid = self.tx_desc_valid(desc);
                if !valid {
                    self.stats.tx_invalid += 1;
                }
                if let Some(chunk) = self.chunk_of(desc.addr) {
                    self.note(TransitionKind::NicConsumeTx, chunk);
                }
                if valid && self.mode == SoftNicMode::Loopback {
                    self.loop_back(desc.addr, desc.len);
                }
                self.nic_tx.push_back(desc.addr);
            }
        }
        // 4. Scripted ingress.
        if self.mode == SoftNicMode::Scripted {
            for _ in 0..self.rx_batch {
                let Some(packet) = self.ingress.pop_front() else {
                    break;
                };
                if let DeliverOutcome::Delivered { .. } = self.deliver(&packet) {
                    report.rx_delivered += 1;
                }
            }
        }
        // 5. Faults; one that cannot apply yet waits for a later step.
        let pending = self.faults.len();
        for _ in 0..pending {
            let fault = self.faults.pop_front().expect("counted");
            if self.apply_fault(fault) {
                self.stats.faults_applied += 1;
                report.faults_applied += 1;
            } else {
                self.faults.push_back(fault);
            }
        }
        report
    }

    /// Device-side counts, for the auditor.
    pub fn kernel_census(&self) -> KernelCensus {
        let mut census = KernelCensus {
            fill_ring: self.fill.len(),
            nic_rx: self.rx_pool.len() as u32,
            tx_ring: self.tx.len(),
            nic_tx: self.nic_tx.len() as u32,
            ..KernelCensus::default()
        };
        for desc in self.rx.committed() {
            if desc.addr < self.umem_len {
                census.rx_ring += 1;
            } else {
                census.spurious_rx += 1;
            }
        }
        for addr in self.completion.committed() {
            if addr < self.umem_len {
                census.completion_ring += 1;
            } else {
                census.spurious_completion += 1;
            }
        }
        census
    }

    /// Every chunk the device side holds, with its exact location.
    pub fn for_each_kernel_chunk(&self, mut f: impl FnMut(u32, crate::trace::Location)) {
        use crate::trace::Location;
        let base_of = |addr: u64| addr & !u64::from(self.config.frame_size - 1);
        for addr in self.fill.committed() {
            if let Some(c) = self.chunk_of(base_of(addr)) {
                f(c, Location::FillRing);
            }
        }
        for &addr in &self.rx_pool {
            if let Some(c) = self.chunk_of(addr) {
                f(c, Location::NicRx);
            }
        }
        for desc in self.rx.committed() {
            if let Some(c) = self.chunk_of(desc.addr) {
                f(c, Location::RxRing);
            }
        }
        for desc in self.tx.committed() {
            if let Some(c) = self.chunk_of(desc.addr) {
                f(c, Location::TxRing);
            }
        }
        for &addr in &self.nic_tx {
            if let Some(c) = self.chunk_of(addr) {
                f(c, Location::NicTx);
            }
        }
        for addr in self.completion.committed() {
            if let Some(c) = self.chunk_of(addr) {
                f(c, Location::CompletionRing);
            }
        }
    }
}

impl XskBackend for SoftNic {}

impl BackendAccess for SoftNic {
    type Fill = Ring<u64>;
    type Rx = Ring<XdpDesc>;
    type Tx = Ring<XdpDesc>;
    type Completion = Ring<u64>;
    fn fill(&mut self) -> &mut Ring<u64> {
        &mut self.fill
    }
    fn rx(&mut self) -> &mut Ring<XdpDesc> {
        &mut self.rx
    }
    fn tx(&mut self) -> &mut Ring<XdpDesc> {
        &mut self.tx
    }
    fn completion(&mut self) -> &mut Ring<u64> {
        &mut self.completion
    }
    fn umem_len(&self) -> usize {
        self.mem.len()
    }
    fn frame_geometry(&self) -> (u32, u32) {
        (self.config.frame_size, self.config.headroom)
    }
    fn frame(&self, addr: u64, len: usize) -> Option<&[u8]> {
        let start = usize::try_from(addr).ok()?;
        self.mem.get(start..start.checked_add(len)?)
    }
    fn frame_mut(&mut self, addr: u64, len: usize) -> Option<&mut [u8]> {
        let start = usize::try_from(addr).ok()?;
        self.mem.get_mut(start..start.checked_add(len)?)
    }
    fn record(&mut self, transition: Transition) {
        if let Some(trace) = &mut self.trace {
            trace.push(transition);
        }
    }
}
