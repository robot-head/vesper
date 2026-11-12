//! Bounded, direct-chain VirtIO 1.2 packed-ring host model (§2.8).
//!
//! Captures descriptor metadata from ordinary borrowed bytes, validates the whole
//! chain, then exposes an immutable owned snapshot. This does not capture payload
//! bytes or authorize live DMA. Completion writes ordinary host bytes, with no
//! device memory ordering, notification, transport, or hardware isolation claim.
//! Indirect descriptors, event suppression and IN_ORDER batching are unsupported.

#![forbid(unsafe_code)]

use crate::{Permission, SnapshotMap};
use std::{collections::VecDeque, fmt};

pub const DESCRIPTOR_BYTES: usize = 16;
pub const FLAG_NEXT: u16 = 1;
pub const FLAG_WRITE: u16 = 2;
pub const FLAG_INDIRECT: u16 = 4;
pub const FLAG_AVAIL: u16 = 1 << 7;
pub const FLAG_USED: u16 = 1 << 15;
const SUPPORTED_FLAGS: u16 = FLAG_NEXT | FLAG_WRITE | FLAG_AVAIL | FLAG_USED;
const MAX_QUEUE_SIZE: usize = 32768;

/// Wire fields are explicitly encoded; no Rust layout or pointer cast is used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackedDescriptor {
    pub guest_address: u64,
    pub byte_len: u32,
    pub buffer_id: u16,
    pub flags: u16,
}
impl PackedDescriptor {
    pub fn encode(self) -> [u8; DESCRIPTOR_BYTES] {
        let mut out = [0; DESCRIPTOR_BYTES];
        out[0..8].copy_from_slice(&self.guest_address.to_le_bytes());
        out[8..12].copy_from_slice(&self.byte_len.to_le_bytes());
        out[12..14].copy_from_slice(&self.buffer_id.to_le_bytes());
        out[14..16].copy_from_slice(&self.flags.to_le_bytes());
        out
    }
    /// Decodes exactly 16 bytes. Flag/chain/access policy is checked by capture,
    /// not by this raw codec (which can also inspect used descriptors).
    pub fn decode(bytes: &[u8]) -> Result<Self, QueueError> {
        let raw: &[u8; DESCRIPTOR_BYTES] = bytes.try_into().map_err(|_| QueueError::RingLength)?;
        Ok(Self {
            guest_address: u64::from_le_bytes(raw[0..8].try_into().expect("fixed slice")),
            byte_len: u32::from_le_bytes(raw[8..12].try_into().expect("fixed slice")),
            buffer_id: u16::from_le_bytes(raw[12..14].try_into().expect("fixed slice")),
            flags: u16::from_le_bytes(raw[14..16].try_into().expect("fixed slice")),
        })
    }
    /// Phase-bit predicate only; queue occupancy and cursor state are also required.
    pub fn is_available(self, wrap: bool) -> bool {
        (self.flags & FLAG_AVAIL != 0) == wrap && (self.flags & FLAG_USED != 0) != wrap
    }
    /// Phase-bit predicate only: initial zero bytes match wrap=false, so this
    /// alone does not prove that any outstanding buffer has completed.
    pub fn is_used(self, wrap: bool) -> bool {
        (self.flags & FLAG_AVAIL != 0) == wrap && (self.flags & FLAG_USED != 0) == wrap
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueError {
    Limit,
    Overflow,
    InvalidQueueSize,
    RingLength,
    NotAvailable,
    IncompleteChain,
    UnsupportedFlags,
    EmptyBuffer,
    ReadAfterWrite,
    UnauthorizedRange,
    WritableReadonlyMemory,
    OverlappingAuthorization,
    InvalidBufferId,
    BufferIdInUse,
    Full,
    ForeignLease,
    StaleLease,
    CompletionOrder,
    InvalidWrittenLength,
    IdentityExhausted,
}
impl fmt::Display for QueueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for QueueError {}

/// Hard bounds supplement caller-selected per-chain and validation budgets.
#[derive(Clone, Copy, Debug)]
pub struct QueueLimits {
    pub max_authorized_ranges: usize,
    pub max_chain_descriptors: usize,
    pub max_chain_bytes: u64,
    pub max_work: usize,
}
impl Default for QueueLimits {
    fn default() -> Self {
        Self {
            max_authorized_ranges: 256,
            max_chain_descriptors: 1024,
            max_chain_bytes: 1024 * 1024,
            max_work: 1_000_000,
        }
    }
}
impl QueueLimits {
    fn validate(self) -> Result<(), QueueError> {
        if self.max_authorized_ranges == 0
            || self.max_authorized_ranges > 4096
            || self.max_chain_descriptors == 0
            || self.max_chain_descriptors > MAX_QUEUE_SIZE
            || self.max_chain_bytes == 0
            || self.max_chain_bytes > 64 * 1024 * 1024
            || self.max_work == 0
            || self.max_work > 16_777_216
        {
            return Err(QueueError::Limit);
        }
        Ok(())
    }
}
struct Work(usize);
impl Work {
    fn charge(&mut self) -> Result<(), QueueError> {
        self.0 = self.0.checked_sub(1).ok_or(QueueError::Limit)?;
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceAccess {
    Read,
    Write,
    ReadWrite,
}
impl DeviceAccess {
    fn reads(self) -> bool {
        matches!(self, Self::Read | Self::ReadWrite)
    }
    fn writes(self) -> bool {
        matches!(self, Self::Write | Self::ReadWrite)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthorizedRange {
    pub guest_start: u64,
    pub byte_len: u64,
    pub access: DeviceAccess,
}
#[derive(Clone, Debug)]
struct Region {
    start: u64,
    end: u64,
    access: DeviceAccess,
}
/// Immutable numeric permission snapshot derived from one already validated
/// SnapshotMap and explicit caller allowlist. It does not own/pin guest memory.
/// Every authorization must fit one slot, even across adjacent equal slots.
#[derive(Clone, Debug)]
pub struct GuestAuthorization {
    vm_id: u64,
    regions: Vec<Region>,
}
impl GuestAuthorization {
    pub fn new(
        map: &SnapshotMap,
        vm_id: u64,
        ranges: &[AuthorizedRange],
        limits: QueueLimits,
    ) -> Result<Self, QueueError> {
        limits.validate()?;
        if ranges.is_empty() || ranges.len() > limits.max_authorized_ranges {
            return Err(QueueError::Limit);
        }
        let mut work = Work(limits.max_work);
        let mut regions = Vec::with_capacity(ranges.len());
        for range in ranges {
            work.charge()?;
            if range.byte_len == 0 {
                return Err(QueueError::EmptyBuffer);
            }
            let end = range
                .guest_start
                .checked_add(range.byte_len)
                .ok_or(QueueError::Overflow)?;
            let mut found = false;
            for slot in &map.slots {
                work.charge()?;
                if slot.slot.vm_id == vm_id
                    && slot.guest.start() <= range.guest_start
                    && end <= slot.guest.end()
                {
                    if range.access.writes() && slot.slot.permission != Permission::Private {
                        return Err(QueueError::WritableReadonlyMemory);
                    }
                    found = true;
                    break;
                }
            }
            if !found {
                return Err(QueueError::UnauthorizedRange);
            }
            regions.push(Region {
                start: range.guest_start,
                end,
                access: range.access,
            });
        }
        regions.sort_unstable_by_key(|r| r.start);
        for pair in regions.windows(2) {
            work.charge()?;
            if pair[0].end > pair[1].start {
                return Err(QueueError::OverlappingAuthorization);
            }
        }
        Ok(Self { vm_id, regions })
    }
    pub fn vm_id(&self) -> u64 {
        self.vm_id
    }
    fn permits(
        &self,
        start: u64,
        end: u64,
        write: bool,
        work: &mut Work,
    ) -> Result<(), QueueError> {
        // Bounded binary search, with each comparison charged.
        let (mut lo, mut hi) = (0usize, self.regions.len());
        while lo < hi {
            work.charge()?;
            let mid = lo + (hi - lo) / 2;
            if self.regions[mid].start <= start {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let region = lo
            .checked_sub(1)
            .and_then(|i| self.regions.get(i))
            .ok_or(QueueError::UnauthorizedRange)?;
        if end > region.end
            || if write {
                !region.access.writes()
            } else {
                !region.access.reads()
            }
        {
            return Err(QueueError::UnauthorizedRange);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingCursor {
    pub index: u16,
    pub wrap: bool,
}
impl RingCursor {
    fn advance(self, count: usize, size: usize) -> Result<Self, QueueError> {
        let next = usize::from(self.index)
            .checked_add(count)
            .ok_or(QueueError::Overflow)?;
        if count > size {
            return Err(QueueError::Limit);
        }
        let (index, wrap) = if next >= size {
            (next - size, !self.wrap)
        } else {
            (next, self.wrap)
        };
        Ok(Self {
            index: u16::try_from(index).map_err(|_| QueueError::Overflow)?,
            wrap,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueueState {
    pub next_available: RingCursor,
    pub next_used: RingCursor,
    pub in_flight_descriptors: usize,
    pub in_flight_buffers: usize,
}
/// Each item is copied exactly once from the supplied byte-ring before exposure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapturedDescriptor {
    pub cursor: RingCursor,
    pub descriptor: PackedDescriptor,
}
#[derive(Debug)]
pub struct CapturedChain {
    vm_id: u64,
    descriptors: Vec<CapturedDescriptor>,
    buffer_id: u16,
    readable_bytes: u64,
    writable_bytes: u64,
}
impl CapturedChain {
    pub fn vm_id(&self) -> u64 {
        self.vm_id
    }
    pub fn descriptors(&self) -> &[CapturedDescriptor] {
        &self.descriptors
    }
    pub fn buffer_id(&self) -> u16 {
        self.buffer_id
    }
    pub fn readable_bytes(&self) -> u64 {
        self.readable_bytes
    }
    pub fn writable_bytes(&self) -> u64 {
        self.writable_bytes
    }
}
/// Non-Clone affine owner. Drop/forget leaves the queue reservation live and
/// stalls progress; this model does not claim static linear discharge/liveness.
///
/// ```compile_fail
/// use vesper_aether::virtio::DescriptorLease;
/// fn duplicate(lease: &DescriptorLease) -> DescriptorLease { (*lease).clone() }
/// ```
#[must_use = "complete the lease; dropping leaves its descriptors reserved"]
#[derive(Debug)]
pub struct DescriptorLease {
    origin: u64,
    generation: u64,
    start: RingCursor,
    chain: CapturedChain,
}
impl DescriptorLease {
    pub fn chain(&self) -> &CapturedChain {
        &self.chain
    }
}
#[derive(Debug)]
pub struct CompletionError {
    pub error: QueueError,
    pub lease: DescriptorLease,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsedRecord {
    pub cursor: RingCursor,
    pub descriptor: PackedDescriptor,
    /// Full cursor advance (chain length), including the used-record slot.
    pub skipped_descriptors: usize,
}
#[derive(Debug)]
struct Reservation {
    generation: u64,
    start: RingCursor,
    count: usize,
    buffer_id: u16,
}

/// Device-side capture and in-order completion policy. No IN_ORDER feature or
/// batch completion is negotiated: each direct chain gets one used descriptor.
/// The externally supplied ring is ordinary host bytes, not mapped device memory.
#[derive(Debug)]
pub struct PackedQueue {
    origin: u64,
    next_generation: u64,
    size: usize,
    authorization: GuestAuthorization,
    limits: QueueLimits,
    available: RingCursor,
    used: RingCursor,
    occupied: usize,
    reservations: VecDeque<Reservation>,
    ids_in_use: Vec<bool>,
}
impl PackedQueue {
    pub fn new(
        size: usize,
        authorization: &GuestAuthorization,
        limits: QueueLimits,
    ) -> Result<Self, QueueError> {
        limits.validate()?;
        if size == 0 || size > MAX_QUEUE_SIZE {
            return Err(QueueError::InvalidQueueSize);
        }
        if authorization.regions.len() > limits.max_authorized_ranges {
            return Err(QueueError::Limit);
        }
        Ok(Self {
            origin: identity()?,
            next_generation: 1,
            size,
            authorization: authorization.clone(),
            limits,
            available: RingCursor {
                index: 0,
                wrap: true,
            },
            used: RingCursor {
                index: 0,
                wrap: true,
            },
            occupied: 0,
            reservations: VecDeque::new(),
            ids_in_use: vec![false; size],
        })
    }
    pub fn state(&self) -> QueueState {
        QueueState {
            next_available: self.available,
            next_used: self.used,
            in_flight_descriptors: self.occupied,
            in_flight_buffers: self.reservations.len(),
        }
    }
    fn validate_ring(&self, ring: &[u8]) -> Result<(), QueueError> {
        if ring.len()
            != self
                .size
                .checked_mul(DESCRIPTOR_BYTES)
                .ok_or(QueueError::Overflow)?
        {
            return Err(QueueError::RingLength);
        }
        Ok(())
    }
    /// No queue state is committed until every descriptor in the chain passes.
    /// Nonterminal IDs are ignored per §2.8.6; the LAST descriptor supplies ID.
    /// This policy limits terminal IDs to queue size and rejects live duplicates.
    /// Zero-length elements are conservatively unsupported, not a spec ban.
    pub fn capture(&mut self, ring: &[u8]) -> Result<DescriptorLease, QueueError> {
        self.validate_ring(ring)?;
        if self.occupied == self.size {
            return Err(QueueError::Full);
        }
        let next_generation = self
            .next_generation
            .checked_add(1)
            .ok_or(QueueError::IdentityExhausted)?;
        let free = self.size - self.occupied;
        let mut work = Work(self.limits.max_work);
        let mut cursor = self.available;
        let mut descriptors = Vec::new();
        let (mut readable_bytes, mut writable_bytes) = (0u64, 0u64);
        let mut saw_write = false;
        let buffer_id;
        loop {
            work.charge()?;
            if descriptors.len() >= free {
                return Err(QueueError::Full);
            }
            if descriptors.len() >= self.limits.max_chain_descriptors {
                return Err(QueueError::Limit);
            }
            let offset = usize::from(cursor.index)
                .checked_mul(DESCRIPTOR_BYTES)
                .ok_or(QueueError::Overflow)?;
            let descriptor = PackedDescriptor::decode(&ring[offset..offset + DESCRIPTOR_BYTES])?;
            if descriptor.flags & !SUPPORTED_FLAGS != 0 {
                return Err(QueueError::UnsupportedFlags);
            }
            if !descriptor.is_available(cursor.wrap) {
                return Err(if descriptors.is_empty() {
                    QueueError::NotAvailable
                } else {
                    QueueError::IncompleteChain
                });
            }
            if descriptor.byte_len == 0 {
                return Err(QueueError::EmptyBuffer);
            }
            let end = descriptor
                .guest_address
                .checked_add(u64::from(descriptor.byte_len))
                .ok_or(QueueError::Overflow)?;
            let write = descriptor.flags & FLAG_WRITE != 0;
            if saw_write && !write {
                return Err(QueueError::ReadAfterWrite);
            }
            saw_write |= write;
            self.authorization
                .permits(descriptor.guest_address, end, write, &mut work)?;
            let total = readable_bytes
                .checked_add(writable_bytes)
                .and_then(|n| n.checked_add(u64::from(descriptor.byte_len)))
                .ok_or(QueueError::Overflow)?;
            if total > self.limits.max_chain_bytes {
                return Err(QueueError::Limit);
            }
            if write {
                writable_bytes = writable_bytes
                    .checked_add(u64::from(descriptor.byte_len))
                    .ok_or(QueueError::Overflow)?;
            } else {
                readable_bytes = readable_bytes
                    .checked_add(u64::from(descriptor.byte_len))
                    .ok_or(QueueError::Overflow)?;
            }
            descriptors.push(CapturedDescriptor { cursor, descriptor });
            cursor = cursor.advance(1, self.size)?;
            if descriptor.flags & FLAG_NEXT == 0 {
                buffer_id = descriptor.buffer_id;
                break;
            }
            if descriptors.len() == self.size {
                return Err(QueueError::IncompleteChain);
            }
        }
        let id = usize::from(buffer_id);
        if id >= self.size {
            return Err(QueueError::InvalidBufferId);
        }
        if self.ids_in_use[id] {
            return Err(QueueError::BufferIdInUse);
        }
        let count = descriptors.len();
        let lease = DescriptorLease {
            origin: self.origin,
            generation: self.next_generation,
            start: self.available,
            chain: CapturedChain {
                vm_id: self.authorization.vm_id,
                descriptors,
                buffer_id,
                readable_bytes,
                writable_bytes,
            },
        };
        self.reservations.push_back(Reservation {
            generation: lease.generation,
            start: lease.start,
            count,
            buffer_id,
        });
        self.available = cursor;
        self.occupied += count; // count <= size - occupied was checked before every read.
        self.ids_in_use[id] = true;
        self.next_generation = next_generation;
        Ok(lease)
    }
    /// Uses only the validated captured chain, never refetching guest metadata.
    /// Checks every fallible condition before either ring bytes or queue state
    /// change. Errors return the exact lease for retry on its originating queue.
    /// `written_bytes` is a caller declaration, not evidence of payload writes.
    pub fn complete(
        &mut self,
        ring: &mut [u8],
        lease: DescriptorLease,
        written_bytes: u32,
    ) -> Result<UsedRecord, CompletionError> {
        let prepared = self.prepare_completion(ring, &lease, written_bytes);
        let (record, next_used) = match prepared {
            Ok(value) => value,
            Err(error) => return Err(CompletionError { error, lease }),
        };
        let offset = usize::from(record.cursor.index) * DESCRIPTOR_BYTES;
        ring[offset..offset + DESCRIPTOR_BYTES].copy_from_slice(&record.descriptor.encode());
        self.reservations.pop_front();
        self.ids_in_use[usize::from(lease.chain.buffer_id)] = false;
        self.occupied -= lease.chain.descriptors.len();
        self.used = next_used;
        Ok(record)
    }
    fn prepare_completion(
        &self,
        ring: &[u8],
        lease: &DescriptorLease,
        written_bytes: u32,
    ) -> Result<(UsedRecord, RingCursor), QueueError> {
        if lease.origin != self.origin {
            return Err(QueueError::ForeignLease);
        }
        let position = self
            .reservations
            .iter()
            .position(|r| r.generation == lease.generation)
            .ok_or(QueueError::StaleLease)?;
        if position != 0 {
            return Err(QueueError::CompletionOrder);
        }
        let reservation = &self.reservations[0];
        if reservation.start != lease.start
            || reservation.count != lease.chain.descriptors.len()
            || reservation.buffer_id != lease.chain.buffer_id
        {
            return Err(QueueError::StaleLease);
        }
        self.validate_ring(ring)?;
        if u64::from(written_bytes) > lease.chain.writable_bytes {
            return Err(QueueError::InvalidWrittenLength);
        }
        let flags = if self.used.wrap {
            FLAG_AVAIL | FLAG_USED
        } else {
            0
        } | if written_bytes != 0 { FLAG_WRITE } else { 0 };
        let record = UsedRecord {
            cursor: self.used,
            skipped_descriptors: reservation.count,
            descriptor: PackedDescriptor {
                guest_address: 0,
                byte_len: written_bytes,
                buffer_id: reservation.buffer_id,
                flags,
            },
        };
        Ok((record, self.used.advance(reservation.count, self.size)?))
    }
}
fn identity() -> Result<u64, QueueError> {
    use std::sync::atomic::AtomicU64;
    static NEXT: AtomicU64 = AtomicU64::new(1);
    allocate_identity(&NEXT)
}
fn allocate_identity(counter: &std::sync::atomic::AtomicU64) -> Result<u64, QueueError> {
    use std::sync::atomic::Ordering;
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        let next = current
            .checked_add(1)
            .ok_or(QueueError::IdentityExhausted)?;
        match counter.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return Ok(current),
            Err(observed) => current = observed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Limits, MemorySlot, PageMapping, SlotSnapshot};
    #[test]
    fn queue_origin_identity_exhaustion_never_wraps() {
        use std::sync::atomic::{AtomicU64, Ordering};
        let counter = AtomicU64::new(u64::MAX - 1);
        assert_eq!(allocate_identity(&counter), Ok(u64::MAX - 1));
        assert_eq!(
            allocate_identity(&counter),
            Err(QueueError::IdentityExhausted)
        );
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    }
    fn setup() -> (PackedQueue, Vec<u8>) {
        let map = SnapshotMap::validate(
            &[SlotSnapshot {
                slot: MemorySlot {
                    vm_id: 1,
                    slot_id: 0,
                    guest_start: 4096,
                    host_virtual_start: 65536,
                    byte_len: 4096,
                    permission: Permission::Private,
                },
                pages: vec![PageMapping::Zero],
            }],
            Limits::default(),
        )
        .unwrap();
        let auth = GuestAuthorization::new(
            &map,
            1,
            &[AuthorizedRange {
                guest_start: 4096,
                byte_len: 4096,
                access: DeviceAccess::ReadWrite,
            }],
            QueueLimits::default(),
        )
        .unwrap();
        let q = PackedQueue::new(1, &auth, QueueLimits::default()).unwrap();
        let ring = PackedDescriptor {
            guest_address: 4096,
            byte_len: 8,
            buffer_id: 0,
            flags: FLAG_AVAIL | FLAG_WRITE,
        }
        .encode()
        .to_vec();
        (q, ring)
    }
    #[test]
    fn stale_duplicate_completion_cannot_consume_later_generation() {
        let (mut q, mut ring) = setup();
        let lease = q.capture(&ring).unwrap();
        // Module-private adversarial handle copy; no public Clone/constructor exists.
        let stale = DescriptorLease {
            origin: lease.origin,
            generation: lease.generation,
            start: lease.start,
            chain: CapturedChain {
                vm_id: lease.chain.vm_id,
                descriptors: lease.chain.descriptors.clone(),
                buffer_id: lease.chain.buffer_id,
                readable_bytes: lease.chain.readable_bytes,
                writable_bytes: lease.chain.writable_bytes,
            },
        };
        q.complete(&mut ring, lease, 8).unwrap();
        ring.copy_from_slice(
            &PackedDescriptor {
                guest_address: 4096,
                byte_len: 8,
                buffer_id: 0,
                flags: FLAG_USED | FLAG_WRITE,
            }
            .encode(),
        );
        let next = q.capture(&ring).unwrap();
        let state = q.state();
        let bytes = ring.clone();
        let err = q.complete(&mut ring, stale, 8).unwrap_err();
        assert_eq!(err.error, QueueError::StaleLease);
        assert_eq!(q.state(), state);
        assert_eq!(ring, bytes);
        q.complete(&mut ring, next, 8).unwrap();
        let state = q.state();
        assert_eq!(
            q.complete(&mut ring, err.lease, 0).unwrap_err().error,
            QueueError::StaleLease
        );
        assert_eq!(q.state(), state);
    }
    #[test]
    fn generation_exhaustion_never_wraps_and_existing_owner_can_complete() {
        let (mut q, mut ring) = setup();
        q.next_generation = u64::MAX - 1;
        let lease = q.capture(&ring).unwrap();
        assert_eq!(lease.generation, u64::MAX - 1);
        q.complete(&mut ring, lease, 0).unwrap();
        ring.copy_from_slice(
            &PackedDescriptor {
                guest_address: 4096,
                byte_len: 8,
                buffer_id: 0,
                flags: FLAG_USED,
            }
            .encode(),
        );
        let state = q.state();
        let bytes = ring.clone();
        assert_eq!(q.capture(&ring).unwrap_err(), QueueError::IdentityExhausted);
        assert_eq!(q.state(), state);
        assert_eq!(ring, bytes);
        assert_eq!(q.next_generation, u64::MAX);
    }
}
