//! The Vesper build of [`SoftNic`]: the device runs in `vesper/src/soft.ves`
//! (inside the `Device` handle); this wrapper keeps the UMEM bytes, lent to
//! every Vesper call that touches them, the batch sizes and the decoded
//! trace.

use crate::binding::{self, Device};
use crate::config::UmemConfig;
use crate::driver::sealed::BackendAccess;
use crate::driver::{Xsk, XskBackend};
use crate::error::UmemError;
use crate::trace::{LOCATIONS, Location, Transition, TransitionKind};

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

/// `TransitionKind` by its declaration index (the trace codes of
/// `vesper/src/soft.ves`).
const KINDS: [TransitionKind; 21] = [
    TransitionKind::NicPullFill,
    TransitionKind::NicDeliver,
    TransitionKind::Harvest,
    TransitionKind::HarvestReject,
    TransitionKind::Recycle,
    TransitionKind::RejectToFree,
    TransitionKind::RecycleToFree,
    TransitionKind::TurnAround,
    TransitionKind::AllocTx,
    TransitionKind::AbandonTx,
    TransitionKind::Submit,
    TransitionKind::NicConsumeTx,
    TransitionKind::NicComplete,
    TransitionKind::CompleteLeased,
    TransitionKind::CompleteRetired,
    TransitionKind::CompleteLeaked,
    TransitionKind::Retire,
    TransitionKind::Replenish,
    TransitionKind::LeakRx,
    TransitionKind::LeakTx,
    TransitionKind::LeakInFlight,
];

fn transition(word: u64) -> Transition {
    Transition {
        kind: KINDS[(word >> 32) as usize % KINDS.len()],
        chunk: word as u32,
    }
}

pub struct SoftNic {
    dev: Device,
    mem: Box<[u8]>,
    /// Fill entries the NIC pulls per step.
    pub fill_batch: u32,
    /// Tx descriptors the NIC consumes per step.
    pub tx_batch: u32,
    /// Scripted packets delivered per step.
    pub rx_batch: u32,
    trace: Option<Vec<Transition>>,
}

/// A driver over the software NIC.
pub type SoftXsk = Xsk<SoftNic>;

impl SoftNic {
    pub fn new(config: UmemConfig, mode: SoftNicMode) -> Result<Self, UmemError> {
        config.validate()?;
        let len = config.umem_len()?;
        Ok(Self {
            dev: binding::soft_nic_new(config, mode)?,
            mem: vec![0u8; len].into_boxed_slice(),
            fill_batch: 64,
            tx_batch: 64,
            rx_batch: 64,
            trace: None,
        })
    }

    /// Convenience: a driver over a fresh software NIC.
    pub fn xsk(config: UmemConfig, mode: SoftNicMode) -> Result<SoftXsk, UmemError> {
        Xsk::new(config, Self::new(config, mode)?)
    }

    /// Starts recording every transition (driver and NIC) in order.
    pub fn enable_trace(&mut self) {
        binding::device_enable_trace(&mut self.dev);
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
        binding::device_soft_stats(&self.dev)
    }
    pub fn mode(&self) -> SoftNicMode {
        binding::device_mode(&self.dev)
    }
    pub fn queue_ingress(&mut self, packet: Vec<u8>) {
        binding::device_queue_ingress(&mut self.dev, &packet);
    }
    pub fn inject(&mut self, fault: Fault) {
        binding::device_inject(&mut self.dev, fault);
    }
    pub fn pending_faults(&self) -> usize {
        binding::device_pending_faults(&self.dev) as usize
    }

    /// Delivers one ingress packet now.
    pub fn deliver(&mut self, packet: &[u8]) -> DeliverOutcome {
        let outcome = binding::device_deliver(&mut self.dev, &mut self.mem, packet);
        self.sync_trace();
        outcome
    }

    /// One device step: complete, pull fill, consume tx (and loop back),
    /// deliver scripted ingress, then apply pending faults.
    pub fn step(&mut self) -> SoftStepReport {
        let report = binding::device_step(
            &mut self.dev,
            &mut self.mem,
            self.fill_batch,
            self.tx_batch,
            self.rx_batch,
        );
        self.sync_trace();
        report
    }

    /// Device-side counts, for the auditor.
    pub fn kernel_census(&self) -> KernelCensus {
        binding::device_kernel_census(&self.dev)
    }

    /// Every chunk the device side holds, with its exact location.
    pub fn for_each_kernel_chunk(&self, mut f: impl FnMut(u32, Location)) {
        for word in binding::device_kernel_chunks(&self.dev) {
            f(
                word as u32,
                LOCATIONS[(word >> 32) as usize % LOCATIONS.len()],
            );
        }
    }
}

impl XskBackend for SoftNic {}

impl BackendAccess for SoftNic {
    fn device(&self) -> &Device {
        &self.dev
    }
    fn parts(&mut self) -> (&mut Device, &mut [u8]) {
        (&mut self.dev, &mut self.mem)
    }
    fn umem(&self) -> &[u8] {
        &self.mem
    }
    fn sync_trace(&mut self) {
        if let Some(trace) = &mut self.trace {
            trace.extend(
                binding::device_take_trace(&mut self.dev)
                    .into_iter()
                    .map(transition),
            );
        }
    }
}
