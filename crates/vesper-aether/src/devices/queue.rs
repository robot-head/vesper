//! The virtqueue engine behind the MMIO transport: split (virtio-v1.2 §2.7)
//! and packed (§2.8) rings over [`GuestMemory`], with the ring features of
//! §6: indirect descriptors (`VIRTIO_F_INDIRECT_DESC`, §2.7.5.3 / §2.8.19),
//! used-buffer notification suppression with and without
//! `VIRTIO_F_EVENT_IDX` (§2.7.7, §2.7.10, §2.8.10), in-order batched
//! completion (`VIRTIO_F_IN_ORDER`, §2.7.9 / §2.8.20) and the bounded capture
//! that `VIRTIO_F_NOTIFICATION_DATA` (§2.9) permits.
//!
//! Every descriptor, available-ring entry and indirect table is read from
//! guest memory exactly once, and the device-readable bytes of a chain are
//! copied out once at capture, so a racing guest cannot change a chain
//! between validation and use (no double fetch). Nothing is committed to the
//! queue state until the whole chain has been validated. Chains complete in
//! the order they were captured.
//!
//! Policy beyond the specification, stated so it is not mistaken for it:
//! zero-length buffers are rejected; an indirect descriptor must be the only
//! descriptor of its chain (which is how drivers use it); packed buffer ids
//! must be below the queue size. With `IN_ORDER`, completions are batched
//! into one used entry only while every earlier buffer of the batch was
//! written to its full device-writable length, because a driver infers the
//! lengths of the skipped entries (Linux uses the full length); a partially
//! written buffer always gets its own entry.
#![forbid(unsafe_code)]

use super::mmio::DeviceError;
use crate::guest_mem::GuestMemory;
use crate::virtio::{
    DESCRIPTOR_BYTES, FLAG_AVAIL, FLAG_INDIRECT, FLAG_NEXT, FLAG_USED, FLAG_WRITE, QueueError,
    QueueLimits,
};
use std::collections::VecDeque;

pub const VIRTIO_F_INDIRECT_DESC: u64 = 1 << 28;
pub const VIRTIO_F_EVENT_IDX: u64 = 1 << 29;
pub const VIRTIO_F_IN_ORDER: u64 = 1 << 35;
pub const VIRTIO_F_NOTIFICATION_DATA: u64 = 1 << 38;
/// Split `avail.flags`: the driver does not want used-buffer notifications.
pub const VIRTQ_AVAIL_F_NO_INTERRUPT: u16 = 1;
/// Packed event suppression `flags` values (§2.8.10).
pub const RING_EVENT_FLAGS_ENABLE: u16 = 0;
pub const RING_EVENT_FLAGS_DISABLE: u16 = 1;
pub const RING_EVENT_FLAGS_DESC: u16 = 2;
/// Largest queue size either format allows.
pub const MAX_QUEUE_SIZE: u16 = 32768;
const DESC: u64 = DESCRIPTOR_BYTES as u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingFormat {
    Split,
    Packed,
}

/// The ring features a queue runs with (negotiated by the transport).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RingFeatures {
    pub indirect: bool,
    pub event_idx: bool,
    pub in_order: bool,
}
impl RingFeatures {
    pub fn from_bits(features: u64) -> Self {
        Self {
            indirect: features & VIRTIO_F_INDIRECT_DESC != 0,
            event_idx: features & VIRTIO_F_EVENT_IDX != 0,
            in_order: features & VIRTIO_F_IN_ORDER != 0,
        }
    }
}

/// Where a queue's three areas live in guest memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingLayout {
    pub format: RingFormat,
    pub size: u16,
    pub desc: u64,
    pub driver: u64,
    pub device: u64,
}
impl RingLayout {
    /// Bytes of the descriptor, driver and device areas (§2.7, §2.8).
    pub fn area_bytes(format: RingFormat, size: u16) -> (u64, u64, u64) {
        let n = u64::from(size);
        match format {
            // used_event / avail_event are always reserved (6 = flags, idx, event).
            RingFormat::Split => (DESC * n, 6 + 2 * n, 6 + 8 * n),
            RingFormat::Packed => (DESC * n, 4, 4),
        }
    }
    /// Size, alignment (§2.7 table, §2.8.10) and placement in guest RAM.
    pub fn validate(&self, memory: &GuestMemory) -> Result<(), QueueError> {
        if self.size == 0
            || self.size > MAX_QUEUE_SIZE
            || (self.format == RingFormat::Split && !self.size.is_power_of_two())
        {
            return Err(QueueError::InvalidQueueSize);
        }
        let (d, dr, dv) = Self::area_bytes(self.format, self.size);
        let (driver_align, device_align) = match self.format {
            RingFormat::Split => (2, 4),
            RingFormat::Packed => (4, 4),
        };
        if !self.desc.is_multiple_of(16)
            || !self.driver.is_multiple_of(driver_align)
            || !self.device.is_multiple_of(device_align)
        {
            return Err(QueueError::RingLength);
        }
        if !memory.contains(self.desc, d)
            || !memory.contains(self.driver, dr)
            || !memory.contains(self.device, dv)
        {
            return Err(QueueError::UnauthorizedRange);
        }
        Ok(())
    }
}

/// `NOTIFICATION_DATA`: where the driver says its next available descriptor
/// is. The device captures no chain at or beyond it in this pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AvailHint {
    Split { next_idx: u16 },
    Packed { next_off: u16, next_wrap: bool },
}

/// One guest buffer of a captured chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Buffer {
    pub gpa: u64,
    pub len: u32,
    pub writable: bool,
}

/// A captured, validated chain with its driver-readable bytes copied out
/// once. A non-Clone owner: its only exits are [`super::mmio::QueueIo::complete`]
/// and handing it back with `unpop`.
///
/// ```compile_fail
/// use vesper_aether::devices::queue::Chain;
/// fn duplicate(c: &Chain) -> Chain { c.clone() }
/// ```
#[must_use = "complete the chain or hand it back with QueueIo::unpop"]
#[derive(Debug)]
pub struct Chain {
    origin: u64,
    seq: u64,
    id: u16,
    slots: u16,
    indirect: bool,
    buffers: Vec<Buffer>,
    readable: Vec<u8>,
    writable_len: u64,
}
impl Chain {
    /// The driver-readable part of the chain, concatenated.
    pub fn readable(&self) -> &[u8] {
        &self.readable
    }
    /// Total bytes of the device-writable part.
    pub fn writable_len(&self) -> u64 {
        self.writable_len
    }
    /// Split: the head descriptor index. Packed: the buffer id.
    pub fn buffer_id(&self) -> u16 {
        self.id
    }
    /// The buffers, in chain order (from the indirect table if indirect).
    pub fn buffers(&self) -> &[Buffer] {
        &self.buffers
    }
    pub fn is_indirect(&self) -> bool {
        self.indirect
    }
}

/// Why [`VirtQueue::complete`] failed.
#[derive(Debug)]
pub enum CompleteError {
    /// The chain is not this queue's oldest, or `written` is too long; the
    /// chain is handed back and nothing changed.
    Rejected(QueueError, Chain),
    /// Writing the used ring failed after the chain was retired.
    Device(DeviceError),
}

/// Per-queue counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QueueStats {
    pub chains: u64,
    pub indirect_chains: u64,
    /// Used entries (split) or used descriptors (packed) written.
    pub used_entries: u64,
    /// Buffers completed without an entry of their own (IN_ORDER batching).
    pub batched: u64,
    pub interrupts_suppressed: u64,
    /// Split: the available ring held more than a `NOTIFICATION_DATA` value
    /// announced (counted once per notification).
    pub hint_mismatches: u64,
    /// Packed driver event flags that are reserved (or DESC without
    /// EVENT_IDX); treated as ENABLE.
    pub invalid_event_flags: u64,
}

#[derive(Debug)]
struct InFlight {
    seq: u64,
    id: u16,
    slots: u16,
    writable_len: u64,
    descriptors: Vec<u16>,
}
#[derive(Clone, Copy, Debug)]
struct Done {
    id: u16,
    slots: u16,
    written: u32,
    full: bool,
}

/// Device side of one virtqueue.
#[derive(Debug)]
pub struct VirtQueue {
    origin: u64,
    layout: RingLayout,
    features: RingFeatures,
    limits: QueueLimits,
    next_seq: u64,
    in_flight: VecDeque<InFlight>,
    id_busy: Vec<bool>,
    batch: Vec<Done>,
    stats: QueueStats,
    // Split state.
    last_avail: u16,
    used_idx: u16,
    signalled_used: u16,
    desc_busy: Vec<bool>,
    // Packed state: (index, wrap counter).
    avail: (u16, bool),
    used: (u16, bool),
    signalled: (u16, bool),
    occupied: usize,
}

fn origin() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

struct Work(usize);
impl Work {
    fn charge(&mut self) -> Result<(), QueueError> {
        self.0 = self.0.checked_sub(1).ok_or(QueueError::Limit)?;
        Ok(())
    }
}

/// Builds a chain's buffer list with the checks every format shares.
struct ChainBuilder<'m> {
    memory: &'m GuestMemory,
    limits: QueueLimits,
    buffers: Vec<Buffer>,
    readable: Vec<u8>,
    writable_len: u64,
    total: u64,
    saw_write: bool,
}
impl<'m> ChainBuilder<'m> {
    fn new(memory: &'m GuestMemory, limits: QueueLimits) -> Self {
        Self {
            memory,
            limits,
            buffers: Vec::new(),
            readable: Vec::new(),
            writable_len: 0,
            total: 0,
            saw_write: false,
        }
    }
    fn push(&mut self, gpa: u64, len: u32, writable: bool) -> Result<(), QueueError> {
        if self.buffers.len() >= self.limits.max_chain_descriptors {
            return Err(QueueError::Limit);
        }
        if len == 0 {
            return Err(QueueError::EmptyBuffer);
        }
        gpa.checked_add(u64::from(len))
            .ok_or(QueueError::Overflow)?;
        if self.saw_write && !writable {
            return Err(QueueError::ReadAfterWrite);
        }
        self.saw_write |= writable;
        if !self.memory.contains(gpa, u64::from(len)) {
            return Err(QueueError::UnauthorizedRange);
        }
        self.total = self
            .total
            .checked_add(u64::from(len))
            .ok_or(QueueError::Overflow)?;
        if self.total > self.limits.max_chain_bytes {
            return Err(QueueError::Limit);
        }
        if writable {
            self.writable_len += u64::from(len);
        } else {
            // Single fetch: the readable bytes are copied out now, once.
            let at = self.readable.len();
            self.readable.resize(at + len as usize, 0);
            self.memory
                .read(gpa, &mut self.readable[at..])
                .map_err(|_| QueueError::UnauthorizedRange)?;
        }
        self.buffers.push(Buffer { gpa, len, writable });
        Ok(())
    }
}

/// A decoded 16-byte descriptor (either format's field order).
#[derive(Clone, Copy, Debug)]
struct RawDesc {
    addr: u64,
    len: u32,
    /// Split: `flags`. Packed: `flags`.
    flags: u16,
    /// Split: `next`. Packed: buffer id.
    tail: u16,
}
fn decode_split(b: &[u8]) -> RawDesc {
    RawDesc {
        addr: u64::from_le_bytes(b[0..8].try_into().expect("8")),
        len: u32::from_le_bytes(b[8..12].try_into().expect("4")),
        flags: u16::from_le_bytes(b[12..14].try_into().expect("2")),
        tail: u16::from_le_bytes(b[14..16].try_into().expect("2")),
    }
}
fn decode_packed(b: &[u8]) -> RawDesc {
    RawDesc {
        addr: u64::from_le_bytes(b[0..8].try_into().expect("8")),
        len: u32::from_le_bytes(b[8..12].try_into().expect("4")),
        tail: u16::from_le_bytes(b[12..14].try_into().expect("2")),
        flags: u16::from_le_bytes(b[14..16].try_into().expect("2")),
    }
}

/// `vring_need_event` (§2.7.10): true if `event` lies in `[old, new)`
/// modulo 2^16, i.e. the used index moved past the driver's event index.
pub fn vring_need_event(event: u16, new: u16, old: u16) -> bool {
    new.wrapping_sub(event).wrapping_sub(1) < new.wrapping_sub(old)
}

impl VirtQueue {
    pub fn new(
        layout: RingLayout,
        features: RingFeatures,
        limits: QueueLimits,
        memory: &GuestMemory,
    ) -> Result<Self, QueueError> {
        layout.validate(memory)?;
        let size = usize::from(layout.size);
        Ok(Self {
            origin: origin(),
            layout,
            features,
            limits,
            next_seq: 0,
            in_flight: VecDeque::new(),
            id_busy: vec![false; size],
            batch: Vec::new(),
            stats: QueueStats::default(),
            last_avail: 0,
            used_idx: 0,
            signalled_used: 0,
            desc_busy: vec![false; size],
            avail: (0, true),
            used: (0, true),
            signalled: (0, true),
            occupied: 0,
        })
    }
    pub fn layout(&self) -> RingLayout {
        self.layout
    }
    pub fn features(&self) -> RingFeatures {
        self.features
    }
    pub fn stats(&self) -> QueueStats {
        self.stats
    }
    pub fn in_flight(&self) -> usize {
        self.in_flight.len()
    }
    /// Validates a `NOTIFICATION_DATA` value for this queue's format.
    pub fn hint_from_data(&self, data: u16) -> Result<AvailHint, QueueError> {
        Ok(match self.layout.format {
            RingFormat::Split => AvailHint::Split { next_idx: data },
            RingFormat::Packed => {
                let next_off = data & 0x7fff;
                if next_off >= self.layout.size {
                    return Err(QueueError::InvalidNotification);
                }
                AvailHint::Packed {
                    next_off,
                    next_wrap: data & 0x8000 != 0,
                }
            }
        })
    }

    /// Captures the next available chain, if any. `Ok(None)`: nothing (more)
    /// is available, the ring is full, or the hint bound was reached. On an
    /// error nothing was committed and the queue is unchanged.
    pub fn capture(
        &mut self,
        memory: &GuestMemory,
        hint: Option<AvailHint>,
    ) -> Result<Option<Chain>, QueueError> {
        match self.layout.format {
            RingFormat::Split => self.capture_split(memory, hint),
            RingFormat::Packed => self.capture_packed(memory, hint),
        }
    }

    fn capture_split(
        &mut self,
        memory: &GuestMemory,
        hint: Option<AvailHint>,
    ) -> Result<Option<Chain>, QueueError> {
        let size = self.layout.size;
        let read16 = |gpa| {
            memory
                .read_u16(gpa)
                .map_err(|_| QueueError::UnauthorizedRange)
        };
        let avail_idx = read16(self.layout.driver + 2)?;
        let pending = avail_idx.wrapping_sub(self.last_avail);
        if pending > size {
            return Err(QueueError::AvailOverrun);
        }
        if pending == 0 {
            return Ok(None);
        }
        if let Some(AvailHint::Split { next_idx }) = hint
            && next_idx == self.last_avail
        {
            // The ring holds more than the notification announced.
            self.stats.hint_mismatches += 1;
            return Ok(None);
        }
        let slot = u64::from(self.last_avail % size);
        let head = read16(self.layout.driver + 4 + 2 * slot)?;
        if head >= size {
            return Err(QueueError::InvalidBufferId);
        }
        if self.id_busy[usize::from(head)] {
            return Err(QueueError::BufferIdInUse);
        }
        let mut work = Work(self.limits.max_work);
        let mut builder = ChainBuilder::new(memory, self.limits);
        let mut descriptors: Vec<u16> = Vec::new();
        let mut indirect = false;
        let mut index = head;
        loop {
            work.charge()?;
            if descriptors.len() >= usize::from(size) || descriptors.contains(&index) {
                return Err(QueueError::ChainLoop);
            }
            if self.desc_busy[usize::from(index)] {
                return Err(QueueError::DescriptorInUse);
            }
            let mut raw = [0u8; DESCRIPTOR_BYTES];
            memory
                .read(self.layout.desc + DESC * u64::from(index), &mut raw)
                .map_err(|_| QueueError::UnauthorizedRange)?;
            let d = decode_split(&raw);
            descriptors.push(index);
            if d.flags & !(FLAG_NEXT | FLAG_WRITE | FLAG_INDIRECT) != 0 {
                return Err(QueueError::UnsupportedFlags);
            }
            if d.flags & FLAG_INDIRECT != 0 {
                if !self.features.indirect {
                    return Err(QueueError::IndirectNotNegotiated);
                }
                if index != head || d.flags & FLAG_NEXT != 0 {
                    return Err(QueueError::IndirectPlacement);
                }
                indirect = true;
                self.walk_split_indirect(memory, d, &mut builder, &mut work)?;
                break;
            }
            builder.push(d.addr, d.len, d.flags & FLAG_WRITE != 0)?;
            if d.flags & FLAG_NEXT == 0 {
                break;
            }
            if d.tail >= size {
                return Err(QueueError::InvalidNext);
            }
            index = d.tail;
        }
        // Commit.
        for &i in &descriptors {
            self.desc_busy[usize::from(i)] = true;
        }
        self.id_busy[usize::from(head)] = true;
        self.last_avail = self.last_avail.wrapping_add(1);
        Ok(Some(self.commit(head, 1, indirect, builder, descriptors)))
    }

    /// Reads a split indirect table once and walks it from entry 0 (§2.7.5.3).
    fn walk_split_indirect(
        &self,
        memory: &GuestMemory,
        d: RawDesc,
        builder: &mut ChainBuilder<'_>,
        work: &mut Work,
    ) -> Result<(), QueueError> {
        let table = self.read_table(memory, d)?;
        let entries = table.len() / DESCRIPTOR_BYTES;
        let mut seen = vec![false; entries];
        let mut i = 0usize;
        loop {
            work.charge()?;
            if seen[i] {
                return Err(QueueError::ChainLoop);
            }
            seen[i] = true;
            let e = decode_split(&table[i * DESCRIPTOR_BYTES..(i + 1) * DESCRIPTOR_BYTES]);
            if e.flags & FLAG_INDIRECT != 0 {
                return Err(QueueError::IndirectNested);
            }
            if e.flags & !(FLAG_NEXT | FLAG_WRITE) != 0 {
                return Err(QueueError::UnsupportedFlags);
            }
            builder.push(e.addr, e.len, e.flags & FLAG_WRITE != 0)?;
            if e.flags & FLAG_NEXT == 0 {
                return Ok(());
            }
            i = usize::from(e.tail);
            if i >= entries {
                return Err(QueueError::InvalidNext);
            }
        }
    }

    fn read_table(&self, memory: &GuestMemory, d: RawDesc) -> Result<Vec<u8>, QueueError> {
        let len = d.len as usize;
        if len == 0 || !len.is_multiple_of(DESCRIPTOR_BYTES) {
            return Err(QueueError::IndirectTableLength);
        }
        if len / DESCRIPTOR_BYTES > self.limits.max_chain_descriptors {
            return Err(QueueError::Limit);
        }
        let mut table = vec![0u8; len];
        memory
            .read(d.addr, &mut table)
            .map_err(|_| QueueError::UnauthorizedRange)?;
        Ok(table)
    }

    fn packed_available(flags: u16, wrap: bool) -> bool {
        (flags & FLAG_AVAIL != 0) == wrap && (flags & FLAG_USED != 0) != wrap
    }
    fn advance(&self, (index, wrap): (u16, bool), by: u16) -> (u16, bool) {
        let next = u32::from(index) + u32::from(by);
        let size = u32::from(self.layout.size);
        if next >= size {
            ((next - size) as u16, !wrap)
        } else {
            (next as u16, wrap)
        }
    }

    fn capture_packed(
        &mut self,
        memory: &GuestMemory,
        hint: Option<AvailHint>,
    ) -> Result<Option<Chain>, QueueError> {
        if let Some(AvailHint::Packed {
            next_off,
            next_wrap,
        }) = hint
            && (next_off, next_wrap) == self.avail
        {
            return Ok(None);
        }
        let size = usize::from(self.layout.size);
        if self.occupied == size {
            return Ok(None);
        }
        let mut work = Work(self.limits.max_work);
        let mut builder = ChainBuilder::new(memory, self.limits);
        let mut cursor = self.avail;
        let mut slots = 0u16;
        let mut indirect = false;
        let id;
        loop {
            work.charge()?;
            if self.occupied + usize::from(slots) >= size {
                return Err(if slots == 0 {
                    QueueError::Full
                } else {
                    QueueError::IncompleteChain
                });
            }
            let mut raw = [0u8; DESCRIPTOR_BYTES];
            memory
                .read(self.layout.desc + DESC * u64::from(cursor.0), &mut raw)
                .map_err(|_| QueueError::UnauthorizedRange)?;
            let d = decode_packed(&raw);
            if !Self::packed_available(d.flags, cursor.1) {
                if slots == 0 {
                    return Ok(None);
                }
                return Err(QueueError::IncompleteChain);
            }
            if d.flags & !(FLAG_NEXT | FLAG_WRITE | FLAG_INDIRECT | FLAG_AVAIL | FLAG_USED) != 0 {
                return Err(QueueError::UnsupportedFlags);
            }
            slots += 1;
            cursor = self.advance(cursor, 1);
            if d.flags & FLAG_INDIRECT != 0 {
                if !self.features.indirect {
                    return Err(QueueError::IndirectNotNegotiated);
                }
                if slots != 1 || d.flags & FLAG_NEXT != 0 {
                    return Err(QueueError::IndirectPlacement);
                }
                indirect = true;
                // §2.8.19: the table's entries are used in order; their
                // buffer ids are reserved and ignored.
                let table = self.read_table(memory, d)?;
                for e in table.chunks_exact(DESCRIPTOR_BYTES) {
                    work.charge()?;
                    let e = decode_packed(e);
                    if e.flags & FLAG_INDIRECT != 0 {
                        return Err(QueueError::IndirectNested);
                    }
                    if e.flags & !(FLAG_WRITE | FLAG_NEXT) != 0 {
                        return Err(QueueError::UnsupportedFlags);
                    }
                    builder.push(e.addr, e.len, e.flags & FLAG_WRITE != 0)?;
                }
                id = d.tail;
                break;
            }
            builder.push(d.addr, d.len, d.flags & FLAG_WRITE != 0)?;
            if d.flags & FLAG_NEXT == 0 {
                id = d.tail;
                break;
            }
        }
        if usize::from(id) >= size {
            return Err(QueueError::InvalidBufferId);
        }
        if self.id_busy[usize::from(id)] {
            return Err(QueueError::BufferIdInUse);
        }
        self.id_busy[usize::from(id)] = true;
        self.occupied += usize::from(slots);
        self.avail = cursor;
        Ok(Some(self.commit(id, slots, indirect, builder, Vec::new())))
    }

    fn commit(
        &mut self,
        id: u16,
        slots: u16,
        indirect: bool,
        builder: ChainBuilder<'_>,
        descriptors: Vec<u16>,
    ) -> Chain {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.stats.chains += 1;
        if indirect {
            self.stats.indirect_chains += 1;
        }
        self.in_flight.push_back(InFlight {
            seq,
            id,
            slots,
            writable_len: builder.writable_len,
            descriptors,
        });
        Chain {
            origin: self.origin,
            seq,
            id,
            slots,
            indirect,
            buffers: builder.buffers,
            readable: builder.readable,
            writable_len: builder.writable_len,
        }
    }

    /// Checks that `chain` is this queue's oldest in-flight chain and that
    /// `written` fits it. Changes nothing.
    pub fn check_completion(&self, chain: &Chain, written: u32) -> Result<(), QueueError> {
        if chain.origin != self.origin {
            return Err(QueueError::ForeignLease);
        }
        let front = self.in_flight.front().ok_or(QueueError::StaleLease)?;
        if front.seq != chain.seq {
            return Err(if self.in_flight.iter().any(|f| f.seq == chain.seq) {
                QueueError::CompletionOrder
            } else {
                QueueError::StaleLease
            });
        }
        if front.id != chain.id || front.slots != chain.slots {
            return Err(QueueError::StaleLease);
        }
        if u64::from(written) > front.writable_len {
            return Err(QueueError::InvalidWrittenLength);
        }
        Ok(())
    }

    /// Retires the oldest chain with `written` device-written bytes and
    /// publishes its used entry (deferred into a batch under `IN_ORDER`).
    /// The caller has already scattered the bytes into the chain's buffers.
    /// A chain that fails [`VirtQueue::check_completion`] is handed back
    /// with the queue unchanged.
    pub fn complete(
        &mut self,
        memory: &mut GuestMemory,
        chain: Chain,
        written: u32,
    ) -> Result<(), CompleteError> {
        if let Err(e) = self.check_completion(&chain, written) {
            return Err(CompleteError::Rejected(e, chain));
        }
        let f = self.in_flight.pop_front().expect("checked above");
        self.id_busy[usize::from(f.id)] = false;
        for i in f.descriptors {
            self.desc_busy[usize::from(i)] = false;
        }
        let done = Done {
            id: f.id,
            slots: f.slots,
            written,
            full: u64::from(written) == f.writable_len,
        };
        drop(chain);
        if self.features.in_order {
            self.batch.push(done);
            Ok(())
        } else {
            self.publish(memory, &[done]).map_err(CompleteError::Device)
        }
    }

    /// Publishes one used entry for `batch` (all completed in order): the
    /// entry carries the last buffer's id and length and sits where the
    /// batch's first entry would; the used position skips the whole batch.
    fn publish(&mut self, memory: &mut GuestMemory, batch: &[Done]) -> Result<(), DeviceError> {
        let last = *batch.last().expect("non-empty batch");
        match self.layout.format {
            RingFormat::Split => {
                let slot = u64::from(self.used_idx % self.layout.size);
                let at = self.layout.device + 4 + 8 * slot;
                let mut elem = [0u8; 8];
                elem[0..4].copy_from_slice(&u32::from(last.id).to_le_bytes());
                elem[4..8].copy_from_slice(&last.written.to_le_bytes());
                memory.write(at, &elem)?;
                self.used_idx = self.used_idx.wrapping_add(batch.len() as u16);
                // The element is written before the index that publishes it.
                memory.write(self.layout.device + 2, &self.used_idx.to_le_bytes())?;
            }
            RingFormat::Packed => {
                let wrap = self.used.1;
                let flags = if wrap { FLAG_AVAIL | FLAG_USED } else { 0 }
                    | if last.written != 0 { FLAG_WRITE } else { 0 };
                let at = self.layout.desc + DESC * u64::from(self.used.0);
                let mut d = [0u8; DESCRIPTOR_BYTES];
                d[8..12].copy_from_slice(&last.written.to_le_bytes());
                d[12..14].copy_from_slice(&last.id.to_le_bytes());
                d[14..16].copy_from_slice(&flags.to_le_bytes());
                // id and length first, flags (which publish it) last.
                memory.write(at, &d[..14])?;
                memory.write(at + 14, &d[14..])?;
                let total: u32 = batch.iter().map(|b| u32::from(b.slots)).sum();
                self.occupied -= total as usize;
                let mut cursor = self.used;
                let mut left = total;
                while left > 0 {
                    let step = left.min(u32::from(self.layout.size)) as u16;
                    cursor = self.advance(cursor, step);
                    left -= u32::from(step);
                }
                self.used = cursor;
            }
        }
        self.stats.used_entries += 1;
        self.stats.batched += batch.len() as u64 - 1;
        Ok(())
    }

    /// Ends a processing pass: publishes any `IN_ORDER` batch, writes the
    /// device's notification-suppression area, and reports whether the
    /// driver wants a used-buffer interrupt for what was published.
    pub fn flush(&mut self, memory: &mut GuestMemory) -> Result<bool, DeviceError> {
        let batch = std::mem::take(&mut self.batch);
        let mut start = 0;
        for (i, d) in batch.iter().enumerate() {
            // A partially written buffer closes its batch (see module docs).
            if !d.full || i + 1 == batch.len() {
                self.publish(memory, &batch[start..=i])?;
                start = i + 1;
            }
        }
        match self.layout.format {
            RingFormat::Split => {
                if self.features.event_idx {
                    // avail_event: notify us once the driver passes this index.
                    let at = self.layout.device + 4 + 8 * u64::from(self.layout.size);
                    memory.write(at, &self.last_avail.to_le_bytes())?;
                }
                let (old, new) = (self.signalled_used, self.used_idx);
                self.signalled_used = new;
                if old == new {
                    return Ok(false);
                }
                let want = if self.features.event_idx {
                    let at = self.layout.driver + 4 + 2 * u64::from(self.layout.size);
                    vring_need_event(memory.read_u16(at)?, new, old)
                } else {
                    memory.read_u16(self.layout.driver)? & VIRTQ_AVAIL_F_NO_INTERRUPT == 0
                };
                if !want {
                    self.stats.interrupts_suppressed += 1;
                }
                Ok(want)
            }
            RingFormat::Packed => {
                // Device event suppression: notifications always enabled.
                memory.write(self.layout.device, &[0, 0, 0, 0])?;
                let (old, new) = (self.signalled, self.used);
                self.signalled = new;
                if old == new {
                    return Ok(false);
                }
                let off_wrap = memory.read_u16(self.layout.driver)?;
                let flags = memory.read_u16(self.layout.driver + 2)?;
                let want = match flags {
                    RING_EVENT_FLAGS_ENABLE => true,
                    RING_EVENT_FLAGS_DISABLE => false,
                    RING_EVENT_FLAGS_DESC if self.features.event_idx => {
                        let event = (off_wrap & 0x7fff, off_wrap & 0x8000 != 0);
                        if event.0 >= self.layout.size {
                            self.stats.invalid_event_flags += 1;
                            true
                        } else {
                            self.passed(old, new, event)
                        }
                    }
                    _ => {
                        self.stats.invalid_event_flags += 1;
                        true
                    }
                };
                if !want {
                    self.stats.interrupts_suppressed += 1;
                }
                Ok(want)
            }
        }
    }

    /// Packed: the used position moved from `old` to `new` (at most one ring
    /// length) across `event`, i.e. `event` lies in `[old, new)` on the ring
    /// of `2 * size` (index, wrap) positions.
    fn passed(&self, old: (u16, bool), new: (u16, bool), event: (u16, bool)) -> bool {
        let size = u32::from(self.layout.size);
        let linear = |(i, w): (u16, bool)| u32::from(i) + if w { 0 } else { size };
        let dist = |a, b| (linear(b) + 2 * size - linear(a)) % (2 * size);
        dist(old, event) < dist(old, new)
    }
}
