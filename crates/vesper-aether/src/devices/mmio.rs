//! VirtIO 1.2 MMIO transport, version 2 (virtio-v1.2 §4.2.2), over split
//! (§2.7) or packed (§2.8) virtqueues.
//!
//! The transport implements the register file a driver programs: magic,
//! version, device and vendor ids, 64-bit feature negotiation through the
//! selector registers, per-queue size/address/ready registers, notification,
//! interrupt status and acknowledge, the device status state machine,
//! shared-memory selectors (no regions) and the configuration generation.
//!
//! The device offers `VIRTIO_F_VERSION_1` (required: legacy drivers are
//! refused at `FEATURES_OK`, §3.1.1) and, unless the embedder narrows them
//! with [`MmioTransport::with_ring_features`], the ring features
//! `RING_PACKED`, `INDIRECT_DESC`, `EVENT_IDX`, `IN_ORDER` and
//! `NOTIFICATION_DATA`. A driver that accepts `RING_PACKED` gets packed
//! queues, otherwise split queues. Rings run on the engine in
//! [`super::queue`]: chains are captured with a single fetch of every
//! descriptor, indirect table and available entry, used-buffer interrupts
//! honour the driver's suppression (`avail.flags`, `used_event` or the packed
//! driver event structure), `IN_ORDER` completions are batched, and with
//! `NOTIFICATION_DATA` the `QueueNotify` value's next-available position
//! bounds what a notification captures. A malformed chain puts the device
//! into `DEVICE_NEEDS_RESET` (§2.1.2) and raises a configuration interrupt; it
//! is never processed. `VIRTIO_F_RING_RESET` is not offered.
//!
//! The register file itself (every register's decode, the status state
//! machine, feature negotiation and the counters) is Vesper
//! (vesper/src/mmio.ves, SH6 lane 14, #1409); this side keeps the device
//! model, the live rings and guest memory. The Rust register file it
//! replaced is kept only as the unit tests' reference
//! (src/devices/mmio/reference.rs).
//!
//! This is a host model on ordinary bytes ([`GuestMemory`]). There is no
//! memory ordering with a concurrently running guest, no ioeventfd/irqfd and no
//! KVM wiring; it validates the modelled register and ring protocol only.
#![forbid(unsafe_code)]

use super::queue::{
    AvailHint, CompleteError, RingFeatures, RingFormat, RingLayout, VIRTIO_F_EVENT_IDX,
    VIRTIO_F_IN_ORDER, VIRTIO_F_INDIRECT_DESC, VIRTIO_F_NOTIFICATION_DATA, VirtQueue,
};
use crate::guest_mem::{GuestMemory, MemError};
use crate::virtio::{QueueError, QueueLimits};
use std::collections::VecDeque;

pub use super::queue::Chain;

pub const MMIO_MAGIC: u32 = 0x7472_6976;
pub const MMIO_VERSION: u32 = 2;
/// Vendor id reported by Aether devices ("VSPR").
pub const AETHER_VENDOR_ID: u32 = 0x5250_5356;
/// Size of one device's register window.
pub const MMIO_WINDOW_BYTES: u64 = 0x200;

/// Register offsets (§4.2.2).
pub mod reg {
    pub const MAGIC_VALUE: u64 = 0x000;
    pub const VERSION: u64 = 0x004;
    pub const DEVICE_ID: u64 = 0x008;
    pub const VENDOR_ID: u64 = 0x00c;
    pub const DEVICE_FEATURES: u64 = 0x010;
    pub const DEVICE_FEATURES_SEL: u64 = 0x014;
    pub const DRIVER_FEATURES: u64 = 0x020;
    pub const DRIVER_FEATURES_SEL: u64 = 0x024;
    pub const QUEUE_SEL: u64 = 0x030;
    pub const QUEUE_NUM_MAX: u64 = 0x034;
    pub const QUEUE_NUM: u64 = 0x038;
    pub const QUEUE_READY: u64 = 0x044;
    pub const QUEUE_NOTIFY: u64 = 0x050;
    pub const INTERRUPT_STATUS: u64 = 0x060;
    pub const INTERRUPT_ACK: u64 = 0x064;
    pub const STATUS: u64 = 0x070;
    pub const QUEUE_DESC_LOW: u64 = 0x080;
    pub const QUEUE_DESC_HIGH: u64 = 0x084;
    pub const QUEUE_DRIVER_LOW: u64 = 0x090;
    pub const QUEUE_DRIVER_HIGH: u64 = 0x094;
    pub const QUEUE_DEVICE_LOW: u64 = 0x0a0;
    pub const QUEUE_DEVICE_HIGH: u64 = 0x0a4;
    pub const SHM_SEL: u64 = 0x0ac;
    pub const SHM_LEN_LOW: u64 = 0x0b0;
    pub const SHM_LEN_HIGH: u64 = 0x0b4;
    pub const SHM_BASE_LOW: u64 = 0x0b8;
    pub const SHM_BASE_HIGH: u64 = 0x0bc;
    pub const QUEUE_RESET: u64 = 0x0c0;
    pub const CONFIG_GENERATION: u64 = 0x0fc;
    pub const CONFIG: u64 = 0x100;
}

/// Device status bits (§2.1).
pub mod status {
    pub const ACKNOWLEDGE: u32 = 1;
    pub const DRIVER: u32 = 2;
    pub const DRIVER_OK: u32 = 4;
    pub const FEATURES_OK: u32 = 8;
    pub const DEVICE_NEEDS_RESET: u32 = 64;
    pub const FAILED: u32 = 128;
}

pub const VIRTIO_F_VERSION_1: u64 = 1 << 32;
pub const VIRTIO_F_RING_PACKED: u64 = 1 << 34;
/// Every ring feature the transport implements (offered by default).
pub const RING_FEATURES: u64 = VIRTIO_F_RING_PACKED
    | VIRTIO_F_INDIRECT_DESC
    | VIRTIO_F_EVENT_IDX
    | VIRTIO_F_IN_ORDER
    | VIRTIO_F_NOTIFICATION_DATA;
/// InterruptStatus bit: the device used a buffer.
pub const INT_USED_BUFFER: u32 = 1;
/// InterruptStatus bit: the configuration (or `DEVICE_NEEDS_RESET`) changed.
pub const INT_CONFIG_CHANGE: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MmioError {
    /// Registers below 0x100 take aligned 32-bit accesses only; configuration
    /// space takes 1, 2, 4 or 8 bytes.
    Width {
        offset: u64,
        len: usize,
    },
    OutOfWindow {
        offset: u64,
    },
    ReadOnlyRegister {
        offset: u64,
    },
    WriteOnlyRegister {
        offset: u64,
    },
    /// A register of a feature this device does not offer (e.g. QueueReset).
    UnsupportedRegister {
        offset: u64,
    },
    InvalidQueue {
        index: u32,
    },
    InvalidQueueSize {
        size: u32,
        max: u16,
    },
    /// Queue registers are only writable while the queue is not ready.
    QueueConfiguredWhileReady,
    /// QueueReady=1 with descriptor, driver or device areas outside RAM or
    /// misaligned.
    RingNotInRam,
    /// QueueReady=1 before `FEATURES_OK`: the ring format is not known yet.
    QueueReadyBeforeFeaturesOk,
    /// A `NOTIFICATION_DATA` value outside the ring, or bits above the queue
    /// index without the feature.
    InvalidNotification {
        value: u32,
    },
    /// Only a reset (status 0) may clear status bits.
    StatusRegression {
        from: u32,
        to: u32,
    },
    DriverOkWithoutFeaturesOk,
    /// The driver set `DEVICE_NEEDS_RESET`, which only the device may set.
    DriverSetNeedsReset,
    FeaturesAfterFeaturesOk,
    NotifyBeforeDriverOk,
    QueueNotReady {
        index: u32,
    },
    ConfigOutOfRange {
        offset: u64,
        len: usize,
    },
    ConfigReadOnly,
    /// The device hit a malformed chain or failed; it waits for a reset.
    DeviceNeedsReset,
    /// The driver set FAILED; the device does no more work until reset.
    DriverFailed,
    Queue(QueueError),
    Memory(MemError),
}
impl std::fmt::Display for MmioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for MmioError {}
impl From<QueueError> for MmioError {
    fn from(value: QueueError) -> Self {
        Self::Queue(value)
    }
}
impl From<MemError> for MmioError {
    fn from(value: MemError) -> Self {
        Self::Memory(value)
    }
}

/// A device-level failure; the transport moves to `DEVICE_NEEDS_RESET`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceError {
    Queue(QueueError),
    Memory(MemError),
    /// The guest broke a device-specific rule that cannot be reported in-band.
    Protocol(&'static str),
}
impl From<QueueError> for DeviceError {
    fn from(value: QueueError) -> Self {
        Self::Queue(value)
    }
}
impl From<MemError> for DeviceError {
    fn from(value: MemError) -> Self {
        Self::Memory(value)
    }
}

/// One device type behind the transport.
pub trait VirtioDevice {
    /// Virtio device id (§5): 1 net, 2 block, 19 vsock.
    fn device_type(&self) -> u32;
    /// Device-specific feature bits (the transport adds VERSION_1 and the
    /// ring features).
    fn device_features(&self) -> u64;
    /// Maximum size of each queue, by index.
    fn queue_max_sizes(&self) -> &[u16];
    fn config_len(&self) -> usize;
    /// `offset + out.len() <= config_len()` is checked by the transport.
    fn read_config(&self, offset: usize, out: &mut [u8]);
    fn write_config(&mut self, _offset: usize, _data: &[u8]) -> Result<(), MmioError> {
        Err(MmioError::ConfigReadOnly)
    }
    /// The driver set DRIVER_OK with these negotiated features.
    fn activate(&mut self, features: u64);
    /// Status 0 was written: return to the initial state.
    fn reset(&mut self);
    /// Handle available chains of `queue` (after a notification or a poll).
    fn process(&mut self, queue: u16, io: &mut QueueIo<'_>) -> Result<(), DeviceError>;
    /// True once after the device's configuration changed.
    fn take_config_change(&mut self) -> bool {
        false
    }
}

/// Queue access handed to [`VirtioDevice::process`].
#[derive(Debug)]
pub struct QueueIo<'a> {
    queue: &'a mut VirtQueue,
    pending: &'a mut VecDeque<Chain>,
    memory: &'a mut GuestMemory,
    completed: usize,
}
impl QueueIo<'_> {
    /// The oldest available chain. Chains must be completed in this order.
    pub fn pop(&mut self) -> Option<Chain> {
        self.pending.pop_front()
    }
    /// Returns a chain to the front, for a receive queue with nothing to deliver.
    pub fn unpop(&mut self, chain: Chain) {
        self.pending.push_front(chain);
    }
    pub fn available(&self) -> usize {
        self.pending.len()
    }
    /// The ring features this queue runs with.
    pub fn ring_features(&self) -> RingFeatures {
        self.queue.features()
    }
    /// Writes `response` into the chain's device-writable buffers in order,
    /// then publishes the used entry. A response longer than the writable
    /// part (or a chain completed out of order) hands the chain back.
    pub fn complete(&mut self, chain: Chain, response: &[u8]) -> Result<(), DeviceError> {
        let written = u32::try_from(response.len()).map_err(|_| QueueError::Overflow);
        let check = written.and_then(|w| self.queue.check_completion(&chain, w).map(|()| w));
        let written = match check {
            Ok(w) => w,
            Err(e) => {
                // Hand the chain back so that the queue stays consistent.
                self.pending.push_front(chain);
                return Err(DeviceError::Queue(e));
            }
        };
        let mut rest = response;
        for b in chain.buffers() {
            if rest.is_empty() {
                break;
            }
            if !b.writable {
                continue;
            }
            let n = rest.len().min(b.len as usize);
            self.memory.write(b.gpa, &rest[..n])?;
            rest = &rest[n..];
        }
        match self.queue.complete(self.memory, chain, written) {
            Ok(()) => {}
            Err(CompleteError::Rejected(e, chain)) => {
                self.pending.push_front(chain);
                return Err(DeviceError::Queue(e));
            }
            Err(CompleteError::Device(e)) => return Err(e),
        }
        self.completed += 1;
        Ok(())
    }
}

/// Transport counters, for tests and reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TransportStats {
    pub notifications: u64,
    pub chains_captured: u64,
    pub chains_completed: u64,
    pub interrupts: u64,
    pub rejected_feature_sets: u64,
    /// Passes that completed buffers but whose interrupt the driver suppressed.
    pub interrupts_suppressed: u64,
}

#[derive(Debug)]
struct ActiveQueue {
    queue: VirtQueue,
    pending: VecDeque<Chain>,
}

/// Register-access results of the Vesper register file
/// (vesper/src/mmio.ves): the kind in the low byte, a payload above it.
mod access {
    pub const OK: u8 = 0;
    pub const DO_CONFIG: u8 = 1;
    pub const DO_RESET: u8 = 2;
    pub const DO_ACTIVATE: u8 = 3;
    pub const DO_READY_ON: u8 = 4;
    pub const DO_READY_OFF: u8 = 5;
    pub const DO_NOTIFY: u8 = 6;
    pub const DO_NOTIFY_DATA: u8 = 7;
}

/// The error an access result names (`offset`, `len` and `value` are the
/// access's own).
fn access_error(code: u64, offset: u64, len: usize, value: u32, from: u32) -> MmioError {
    let payload = code >> 8;
    match code as u8 {
        16 => MmioError::Width { offset, len },
        17 => MmioError::OutOfWindow { offset },
        18 => MmioError::ReadOnlyRegister { offset },
        19 => MmioError::WriteOnlyRegister { offset },
        20 => MmioError::UnsupportedRegister { offset },
        21 => MmioError::InvalidQueue {
            index: payload as u32,
        },
        22 => MmioError::InvalidQueueSize {
            size: payload as u32,
            max: (payload >> 32) as u16,
        },
        23 => MmioError::QueueConfiguredWhileReady,
        24 => MmioError::QueueReadyBeforeFeaturesOk,
        25 => MmioError::StatusRegression { from, to: value },
        26 => MmioError::DriverOkWithoutFeaturesOk,
        27 => MmioError::DriverSetNeedsReset,
        28 => MmioError::FeaturesAfterFeaturesOk,
        29 => MmioError::ConfigOutOfRange { offset, len },
        30 => MmioError::DeviceNeedsReset,
        31 => MmioError::DriverFailed,
        32 => MmioError::NotifyBeforeDriverOk,
        33 => MmioError::QueueNotReady {
            index: payload as u32,
        },
        k => unreachable!("unknown MMIO access result {k}"),
    }
}

/// One VirtIO MMIO device: the register file plus the device model.
///
/// The register file (selectors, feature negotiation, the status machine,
/// per-queue registers, interrupt status, configuration generation and the
/// counters) is the Vesper port's state (vesper/src/mmio.ves); this side
/// keeps the device model, the live rings and the guest memory accesses.
#[derive(Debug)]
pub struct MmioTransport<D: VirtioDevice> {
    device: D,
    vm_id: u64,
    limits: QueueLimits,
    ring_features: u64,
    state: crate::binding::MmioState,
    active: Vec<Option<ActiveQueue>>,
}

impl<D: VirtioDevice> MmioTransport<D> {
    pub fn new(device: D, vm_id: u64) -> Self {
        Self::with_limits(device, vm_id, QueueLimits::default())
    }
    /// Offers only the ring features in `ring_features` (a subset of
    /// [`RING_FEATURES`]; other bits are ignored).
    pub fn with_ring_features(device: D, vm_id: u64, ring_features: u64) -> Self {
        let mut t = Self::new(device, vm_id);
        t.ring_features = ring_features & RING_FEATURES;
        t
    }
    pub fn with_limits(device: D, vm_id: u64, limits: QueueLimits) -> Self {
        let maxes: Vec<u8> = device
            .queue_max_sizes()
            .iter()
            .flat_map(|m| m.to_le_bytes())
            .collect();
        let state = crate::binding::mmio_state_new(device.device_type(), &maxes);
        let active = device.queue_max_sizes().iter().map(|_| None).collect();
        Self {
            device,
            vm_id,
            limits,
            ring_features: RING_FEATURES,
            state,
            active,
        }
    }
    pub fn device(&self) -> &D {
        &self.device
    }
    /// Host-side access to the device (e.g. injecting a received frame); a
    /// configuration change the device reports raises a config interrupt.
    pub fn with_device<R>(&mut self, f: impl FnOnce(&mut D) -> R) -> R {
        let r = f(&mut self.device);
        self.check_config_change();
        r
    }
    pub fn status(&self) -> u32 {
        self.state.as_ref().status()
    }
    /// The VM this device belongs to.
    pub fn vm_id(&self) -> u64 {
        self.vm_id
    }
    pub fn negotiated_features(&self) -> u64 {
        self.state.as_ref().negotiated()
    }
    pub fn offered_features(&self) -> u64 {
        self.device.device_features() | VIRTIO_F_VERSION_1 | self.ring_features
    }
    /// The ring format negotiated queues use.
    pub fn ring_format(&self) -> RingFormat {
        if self.negotiated_features() & VIRTIO_F_RING_PACKED != 0 {
            RingFormat::Packed
        } else {
            RingFormat::Split
        }
    }
    /// Counters of queue `index`, once it is ready.
    pub fn queue_stats(&self, index: u32) -> Option<super::queue::QueueStats> {
        self.active
            .get(index as usize)?
            .as_ref()
            .map(|a| a.queue.stats())
    }
    pub fn interrupt_pending(&self) -> bool {
        self.state.as_ref().interrupt_status() != 0
    }
    pub fn stats(&self) -> TransportStats {
        self.state.as_ref().stats()
    }

    fn check_config_change(&mut self) {
        if self.device.take_config_change() {
            crate::binding::mmio_config_change(&mut self.state);
        }
    }
    fn reset(&mut self) {
        self.device.reset();
        for a in &mut self.active {
            *a = None;
        }
    }

    /// A driver read of `data.len()` bytes at `offset` in the register window.
    pub fn read(&mut self, offset: u64, data: &mut [u8]) -> Result<(), MmioError> {
        let r = crate::binding::mmio_read(
            &self.state,
            offset,
            data.len() as u64,
            self.offered_features(),
            self.device.config_len() as u64,
        );
        match r as u8 {
            access::OK => {
                data.copy_from_slice(&((r >> 8) as u32).to_le_bytes());
                Ok(())
            }
            access::DO_CONFIG => {
                self.device
                    .read_config((offset - reg::CONFIG) as usize, data);
                Ok(())
            }
            _ => Err(access_error(r, offset, data.len(), 0, 0)),
        }
    }

    /// A driver write. `memory` is the guest RAM (needed when a queue becomes
    /// ready and on notification).
    pub fn write(
        &mut self,
        memory: &mut GuestMemory,
        offset: u64,
        data: &[u8],
    ) -> Result<(), MmioError> {
        let value = <[u8; 4]>::try_from(data).map_or(0, u32::from_le_bytes);
        let from = self.status();
        let offered = self.offered_features();
        let config_len = self.device.config_len() as u64;
        let r = crate::binding::mmio_write(
            &mut self.state,
            offset,
            data.len() as u64,
            value,
            offered,
            config_len,
        );
        match r as u8 {
            access::OK => Ok(()),
            access::DO_CONFIG => {
                self.device
                    .write_config((offset - reg::CONFIG) as usize, data)?;
                self.check_config_change();
                Ok(())
            }
            access::DO_RESET => {
                self.reset();
                Ok(())
            }
            access::DO_ACTIVATE => {
                self.device.activate(self.negotiated_features());
                Ok(())
            }
            access::DO_READY_ON => self.make_ready(memory, (r >> 8) as u32),
            access::DO_READY_OFF => {
                self.active[(r >> 8) as usize] = None;
                Ok(())
            }
            access::DO_NOTIFY => self.run_queue(memory, value, None),
            access::DO_NOTIFY_DATA => {
                // §2.9: vqn in bits 0..16, then next_idx (split) or next_off
                // and next_wrap (packed).
                let index = value & 0xffff;
                let hint = self
                    .active
                    .get(index as usize)
                    .and_then(Option::as_ref)
                    .map(|a| a.queue.hint_from_data((value >> 16) as u16));
                match hint {
                    Some(Ok(h)) => self.run_queue(memory, index, Some(h)),
                    Some(Err(_)) => Err(MmioError::InvalidNotification { value }),
                    None => self.run_queue(memory, index, None),
                }
            }
            _ => Err(access_error(r, offset, data.len(), value, from)),
        }
    }

    /// QueueReady=1 on queue `index`, which the register file accepted:
    /// builds the live ring over the registers.
    fn make_ready(&mut self, memory: &GuestMemory, index: u32) -> Result<(), MmioError> {
        let q = crate::binding::mmio_queue(&self.state, index);
        let negotiated = self.negotiated_features();
        let format = if negotiated & VIRTIO_F_RING_PACKED != 0 {
            RingFormat::Packed
        } else {
            RingFormat::Split
        };
        let layout = RingLayout {
            format,
            size: q.num,
            desc: q.desc,
            driver: q.driver,
            device: q.device,
        };
        let queue = VirtQueue::new(
            layout,
            RingFeatures::from_bits(negotiated),
            self.limits,
            memory,
        )
        .map_err(|e| match e {
            QueueError::InvalidQueueSize => MmioError::InvalidQueueSize {
                size: u32::from(q.num),
                max: q.max,
            },
            _ => MmioError::RingNotInRam,
        })?;
        self.active[index as usize] = Some(ActiveQueue {
            queue,
            pending: VecDeque::new(),
        });
        crate::binding::mmio_queue_set_ready(&mut self.state, index);
        Ok(())
    }

    /// Host-side poll: process queue `index` again (e.g. after the backend has
    /// data for a receive queue). Captures newly available chains first.
    pub fn poll(&mut self, memory: &mut GuestMemory, index: u32) -> Result<(), MmioError> {
        self.run_queue(memory, index, None)
    }

    fn run_queue(
        &mut self,
        memory: &mut GuestMemory,
        index: u32,
        hint: Option<AvailHint>,
    ) -> Result<(), MmioError> {
        let check = crate::binding::mmio_run_check(&self.state, index);
        if check != 0 {
            return Err(access_error(check, 0, 0, 0, 0));
        }
        let Some(active) = self.active[index as usize].as_mut() else {
            return Err(MmioError::QueueNotReady { index });
        };
        let mut captured = 0u64;
        let mut failure = None;
        loop {
            match active.queue.capture(memory, hint) {
                Ok(Some(chain)) => {
                    active.pending.push_back(chain);
                    captured += 1;
                }
                Ok(None) | Err(QueueError::Full) => break,
                Err(e) => {
                    failure = Some(MmioError::Queue(e));
                    break;
                }
            }
        }
        if let Some(e) = failure {
            crate::binding::mmio_run_done(&mut self.state, captured, true, 0, 0, false, false);
            return Err(e);
        }
        let mut io = QueueIo {
            queue: &mut active.queue,
            pending: &mut active.pending,
            memory,
            completed: 0,
        };
        let result = self.device.process(index as u16, &mut io);
        let completed = io.completed as u64;
        let flushed = active.queue.flush(memory);
        let (flush_code, config_changed) = match flushed {
            Ok(irq) => (u64::from(irq), self.device.take_config_change()),
            Err(_) => (2, false),
        };
        crate::binding::mmio_run_done(
            &mut self.state,
            captured,
            false,
            completed,
            flush_code,
            config_changed,
            result.is_err(),
        );
        if let Err(e) = flushed {
            return Err(device_error(e));
        }
        result.map_err(device_error)
    }
}

fn device_error(e: DeviceError) -> MmioError {
    match e {
        DeviceError::Queue(q) => MmioError::Queue(q),
        DeviceError::Memory(m) => MmioError::Memory(m),
        DeviceError::Protocol(_) => MmioError::DeviceNeedsReset,
    }
}

#[cfg(test)]
mod reference;
#[cfg(test)]
pub(crate) use reference::MmioTransport as ReferenceMmioTransport;
