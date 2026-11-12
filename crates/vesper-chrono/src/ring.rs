//! Bounded always-on flight recorder (§31.3) with overwrite-oldest semantics.
//!
//! All storage is allocated by [`FlightRecorder::new`]. Recording effect
//! outcomes, `inout` writes, obligation hops and snapshot images afterwards
//! performs no heap allocation (checked by `tests/zero_alloc.rs` with a
//! counting global allocator). A record never wraps around the end of the
//! byte ring: when it does not fit before the end, the tail skips to the start
//! and the skipped bytes count as used until the oldest records are evicted.

use std::collections::VecDeque;
use std::fmt;

use crate::lineage::{HopKind, ObligationId};
use crate::wire::{
    ChronoLayouts, DIGEST_BYTES, EFFECT_INOUT_WRITE, EFFECT_OBLIGATION, EFFECT_SNAPSHOT,
    EventHeader, HEADER_BYTES, INOUT_WRITE_BYTES, InoutWriteRecord, OBLIGATION_RECORD_BYTES,
    OP_TAG_DIGEST, RESERVED_EFFECT_BASE,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecorderConfig {
    /// Byte capacity of the record ring (the design proposes 16 MiB).
    pub ring_bytes: usize,
    /// Maximum number of retained records.
    pub max_records: usize,
    /// Maximum payload of one record.
    pub max_payload: usize,
    /// Number of retained snapshot images (oldest overwritten).
    pub snapshot_slots: usize,
    /// Maximum bytes of one snapshot image.
    pub snapshot_image_bytes: usize,
}

impl RecorderConfig {
    pub const fn small() -> Self {
        Self {
            ring_bytes: 64 * 1024,
            max_records: 4096,
            max_payload: 1024,
            snapshot_slots: 8,
            snapshot_image_bytes: 4096,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordError {
    InvalidConfig(&'static str),
    PayloadTooLarge {
        len: usize,
        max: usize,
    },
    ReservedEffectId(u16),
    /// Caller op tags must fit in 15 bits; bit 15 is [`OP_TAG_DIGEST`].
    OpTagOutOfRange(u16),
    TimeWentBackwards {
        last_ns: u64,
        got_ns: u64,
    },
    SequenceExhausted,
    ObligationIdExhausted,
    EpochExhausted,
    ImageTooLarge {
        len: usize,
        max: usize,
    },
}

impl fmt::Display for RecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "chrono recorder: {self:?}")
    }
}
impl std::error::Error for RecordError {}

#[derive(Debug, Clone, Copy)]
struct Slot {
    /// Absolute byte position (physical offset is `pos % ring_bytes`).
    pos: u64,
    len: u32,
}

/// A borrowed decoded record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlightEvent<'a> {
    pub lamport_seq: u64,
    pub fiber_id: u64,
    pub time_ns: u64,
    pub effect_id: u16,
    /// Caller op tag without the digest bit.
    pub op_tag: u16,
    /// True when `payload` is a BLAKE3 digest of the outcome, not its bytes.
    pub digest: bool,
    pub payload: &'a [u8],
}

impl FlightEvent<'_> {
    pub fn header(&self) -> EventHeader {
        EventHeader {
            lamport_seq: self.lamport_seq,
            fiber_id: self.fiber_id,
            ipt_tsc_anchor: self.time_ns,
            effect_id: self.effect_id,
            op_tag: self.op_tag | if self.digest { OP_TAG_DIGEST } else { 0 },
            payload_byte_len: self.payload.len() as u32,
        }
    }
}

struct SnapshotSlot {
    epoch_id: u32,
    start_seq: u64,
    time_ns: u64,
    len: usize,
    image: Box<[u8]>,
}

/// A borrowed retained snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotImage<'a> {
    pub epoch_id: u32,
    pub start_lamport_seq: u64,
    pub time_ns: u64,
    pub image: &'a [u8],
}

pub struct FlightRecorder {
    layouts: ChronoLayouts,
    config: RecorderConfig,
    buf: Box<[u8]>,
    head: u64,
    tail: u64,
    slots: VecDeque<Slot>,
    next_seq: u64,
    last_time_ns: u64,
    evicted_records: u64,
    next_obligation: ObligationId,
    snapshots: Vec<SnapshotSlot>,
    next_snapshot_slot: usize,
    next_epoch: u32,
}

impl FlightRecorder {
    pub fn new(config: RecorderConfig) -> Result<Self, RecordError> {
        let record_max = HEADER_BYTES
            .checked_add(config.max_payload)
            .ok_or(RecordError::InvalidConfig("max_payload overflows"))?;
        if config.max_records == 0 {
            return Err(RecordError::InvalidConfig("max_records must be positive"));
        }
        if config.max_payload < OBLIGATION_RECORD_BYTES.max(DIGEST_BYTES) {
            return Err(RecordError::InvalidConfig(
                "max_payload must hold Chrono's own 32-byte records",
            ));
        }
        if config.max_payload > u32::MAX as usize {
            return Err(RecordError::InvalidConfig("max_payload exceeds u32"));
        }
        if config.ring_bytes < record_max {
            return Err(RecordError::InvalidConfig(
                "ring_bytes must hold one maximal record",
            ));
        }
        let snapshots = (0..config.snapshot_slots)
            .map(|_| SnapshotSlot {
                epoch_id: 0,
                start_seq: 0,
                time_ns: 0,
                len: 0,
                image: vec![0; config.snapshot_image_bytes].into_boxed_slice(),
            })
            .collect();
        Ok(Self {
            layouts: ChronoLayouts::new(),
            config,
            buf: vec![0; config.ring_bytes].into_boxed_slice(),
            head: 0,
            tail: 0,
            slots: VecDeque::with_capacity(config.max_records),
            next_seq: 1,
            last_time_ns: 0,
            evicted_records: 0,
            next_obligation: 1,
            snapshots,
            next_snapshot_slot: 0,
            next_epoch: 1,
        })
    }

    pub fn config(&self) -> RecorderConfig {
        self.config
    }
    pub fn layouts(&self) -> &ChronoLayouts {
        &self.layouts
    }
    /// Sequence the next record will get. Sequences start at 1.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }
    pub fn len(&self) -> usize {
        self.slots.len()
    }
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
    pub fn evicted_records(&self) -> u64 {
        self.evicted_records
    }
    /// Sequence of the oldest retained record (`next_seq` when empty).
    pub fn first_retained_seq(&self) -> u64 {
        self.next_seq - self.slots.len() as u64
    }
    pub fn bytes_used(&self) -> u64 {
        self.tail - self.head
    }

    fn check_effect(&self, effect_id: u16, op_tag: u16) -> Result<(), RecordError> {
        if effect_id >= RESERVED_EFFECT_BASE {
            return Err(RecordError::ReservedEffectId(effect_id));
        }
        if op_tag & OP_TAG_DIGEST != 0 {
            return Err(RecordError::OpTagOutOfRange(op_tag));
        }
        Ok(())
    }

    /// Record an effect completion with its outcome bytes.
    pub fn record_effect(
        &mut self,
        fiber_id: u64,
        time_ns: u64,
        effect_id: u16,
        op_tag: u16,
        payload: &[u8],
    ) -> Result<u64, RecordError> {
        self.check_effect(effect_id, op_tag)?;
        self.append(fiber_id, time_ns, effect_id, op_tag, payload)
    }

    /// Record an effect completion by the BLAKE3 digest of its outcome. A
    /// digest proves which outcome occurred but cannot be replayed; a
    /// low-entropy outcome can be recovered from its digest by guessing.
    pub fn record_effect_digest(
        &mut self,
        fiber_id: u64,
        time_ns: u64,
        effect_id: u16,
        op_tag: u16,
        outcome: &[u8],
    ) -> Result<u64, RecordError> {
        self.check_effect(effect_id, op_tag)?;
        let digest = blake3::hash(outcome);
        self.append(
            fiber_id,
            time_ns,
            effect_id,
            op_tag | OP_TAG_DIGEST,
            digest.as_bytes(),
        )
    }

    /// Chrono's own reserved records (the SimPulse scheduler adapter).
    pub(crate) fn record_effect_internal(
        &mut self,
        fiber_id: u64,
        time_ns: u64,
        effect_id: u16,
        payload: &[u8],
    ) -> Result<u64, RecordError> {
        debug_assert!(effect_id >= RESERVED_EFFECT_BASE);
        self.append(fiber_id, time_ns, effect_id, 0, payload)
    }

    pub fn record_inout_write(
        &mut self,
        fiber_id: u64,
        time_ns: u64,
        place_hash: u64,
        source_span_id: u32,
    ) -> Result<u64, RecordError> {
        let mut payload = [0u8; INOUT_WRITE_BYTES];
        InoutWriteRecord {
            place_hash,
            source_span_id,
        }
        .encode(&self.layouts, &mut payload)
        .expect("fixed-size buffer");
        self.append(fiber_id, time_ns, EFFECT_INOUT_WRITE, 0, &payload)
    }

    /// Mint a new obligation id and record its creation.
    pub fn mint_obligation(
        &mut self,
        fiber_id: u64,
        time_ns: u64,
        source_span_id: u32,
        parent: Option<ObligationId>,
    ) -> Result<ObligationId, RecordError> {
        let id = self.next_obligation;
        let next = id
            .checked_add(1)
            .ok_or(RecordError::ObligationIdExhausted)?;
        if let Some(parent) = parent
            && parent >= id
        {
            return Err(RecordError::InvalidConfig(
                "parent obligation not yet minted",
            ));
        }
        self.write_hop(
            fiber_id,
            time_ns,
            id,
            HopKind::Mint { parent },
            source_span_id,
        )?;
        self.next_obligation = next;
        Ok(id)
    }

    /// Record a non-creation hop. Use [`Self::mint_obligation`] for creation.
    pub fn record_obligation_hop(
        &mut self,
        fiber_id: u64,
        time_ns: u64,
        obligation_id: ObligationId,
        kind: HopKind,
        source_span_id: u32,
    ) -> Result<u64, RecordError> {
        if matches!(kind, HopKind::Mint { .. })
            || obligation_id == 0
            || obligation_id >= self.next_obligation
        {
            return Err(RecordError::InvalidConfig(
                "hop must name an already minted obligation and must not be a mint",
            ));
        }
        self.write_hop(fiber_id, time_ns, obligation_id, kind, source_span_id)
    }

    fn write_hop(
        &mut self,
        fiber_id: u64,
        time_ns: u64,
        id: ObligationId,
        kind: HopKind,
        span: u32,
    ) -> Result<u64, RecordError> {
        let mut payload = [0u8; OBLIGATION_RECORD_BYTES];
        kind.to_record(id, span)
            .encode(&self.layouts, &mut payload)
            .expect("fixed-size buffer");
        self.append(fiber_id, time_ns, EFFECT_OBLIGATION, 0, &payload)
    }

    /// Copy a caller-provided state image into the oldest snapshot slot. The
    /// image must be the state after every record before the returned epoch's
    /// `start_lamport_seq`; a marker record is written at that sequence.
    pub fn capture_snapshot(&mut self, time_ns: u64, image: &[u8]) -> Result<u32, RecordError> {
        if self.snapshots.is_empty() || image.len() > self.config.snapshot_image_bytes {
            return Err(RecordError::ImageTooLarge {
                len: image.len(),
                max: if self.snapshots.is_empty() {
                    0
                } else {
                    self.config.snapshot_image_bytes
                },
            });
        }
        let epoch = self.next_epoch;
        let next_epoch = epoch.checked_add(1).ok_or(RecordError::EpochExhausted)?;
        let seq = self.append(0, time_ns, EFFECT_SNAPSHOT, 0, &epoch.to_le_bytes())?;
        let slot = &mut self.snapshots[self.next_snapshot_slot];
        slot.epoch_id = epoch;
        slot.start_seq = seq;
        slot.time_ns = time_ns;
        slot.len = image.len();
        slot.image[..image.len()].copy_from_slice(image);
        self.next_snapshot_slot = (self.next_snapshot_slot + 1) % self.snapshots.len();
        self.next_epoch = next_epoch;
        Ok(epoch)
    }

    fn append(
        &mut self,
        fiber_id: u64,
        time_ns: u64,
        effect_id: u16,
        op_tag: u16,
        payload: &[u8],
    ) -> Result<u64, RecordError> {
        if payload.len() > self.config.max_payload {
            return Err(RecordError::PayloadTooLarge {
                len: payload.len(),
                max: self.config.max_payload,
            });
        }
        if time_ns < self.last_time_ns {
            return Err(RecordError::TimeWentBackwards {
                last_ns: self.last_time_ns,
                got_ns: time_ns,
            });
        }
        let seq = self.next_seq;
        let next_seq = seq.checked_add(1).ok_or(RecordError::SequenceExhausted)?;
        let len = HEADER_BYTES + payload.len();
        let cap = self.config.ring_bytes as u64;
        let pos = loop {
            if self.slots.is_empty() {
                // Nothing retained: restart at physical offset zero.
                let aligned = self.tail.div_ceil(cap) * cap;
                self.head = aligned;
                self.tail = aligned;
            }
            let phys = (self.tail % cap) as usize;
            let gap = if self.config.ring_bytes - phys < len {
                self.config.ring_bytes - phys
            } else {
                0
            };
            let need = (gap + len) as u64;
            if self.slots.len() < self.config.max_records && cap - (self.tail - self.head) >= need {
                break self.tail + gap as u64;
            }
            // Nonempty here: an empty ring realigns and always fits `len <= cap`.
            self.slots.pop_front();
            self.evicted_records += 1;
            self.head = self.slots.front().map_or(self.tail, |s| s.pos);
        };
        self.write_at(pos, seq, fiber_id, time_ns, effect_id, op_tag, payload);
        self.slots.push_back(Slot {
            pos,
            len: len as u32,
        });
        self.tail = pos + len as u64;
        self.next_seq = next_seq;
        self.last_time_ns = time_ns;
        Ok(seq)
    }

    #[allow(clippy::too_many_arguments)]
    fn write_at(
        &mut self,
        pos: u64,
        seq: u64,
        fiber_id: u64,
        time_ns: u64,
        effect_id: u16,
        op_tag: u16,
        payload: &[u8],
    ) {
        let phys = (pos % self.config.ring_bytes as u64) as usize;
        let header = EventHeader {
            lamport_seq: seq,
            fiber_id,
            ipt_tsc_anchor: time_ns,
            effect_id,
            op_tag,
            payload_byte_len: payload.len() as u32,
        };
        let end = phys + HEADER_BYTES;
        header
            .encode(&self.layouts, &mut self.buf[phys..end])
            .expect("header slot is 32 bytes");
        self.buf[end..end + payload.len()].copy_from_slice(payload);
    }

    fn decode_slot(&self, slot: Slot) -> FlightEvent<'_> {
        let phys = (slot.pos % self.config.ring_bytes as u64) as usize;
        let bytes = &self.buf[phys..phys + slot.len as usize];
        let header = EventHeader::decode(&self.layouts, bytes).expect("recorder wrote this header");
        FlightEvent {
            lamport_seq: header.lamport_seq,
            fiber_id: header.fiber_id,
            time_ns: header.ipt_tsc_anchor,
            effect_id: header.effect_id,
            op_tag: header.op_tag & !OP_TAG_DIGEST,
            digest: header.op_tag & OP_TAG_DIGEST != 0,
            payload: &bytes[HEADER_BYTES..],
        }
    }

    /// Retained records, oldest first.
    pub fn events(&self) -> impl Iterator<Item = FlightEvent<'_>> + '_ {
        self.slots.iter().map(|slot| self.decode_slot(*slot))
    }

    /// Retained snapshots, oldest epoch first.
    pub fn snapshots(&self) -> Vec<SnapshotImage<'_>> {
        let mut out: Vec<_> = self
            .snapshots
            .iter()
            .filter(|s| s.epoch_id != 0)
            .map(|s| SnapshotImage {
                epoch_id: s.epoch_id,
                start_lamport_seq: s.start_seq,
                time_ns: s.time_ns,
                image: &s.image[..s.len],
            })
            .collect();
        out.sort_by_key(|s| s.epoch_id);
        out
    }

    /// Iterate retained snapshots without allocating.
    pub fn for_each_snapshot(&self, mut f: impl FnMut(SnapshotImage<'_>)) {
        for s in self.snapshots.iter().filter(|s| s.epoch_id != 0) {
            f(SnapshotImage {
                epoch_id: s.epoch_id,
                start_lamport_seq: s.start_seq,
                time_ns: s.time_ns,
                image: &s.image[..s.len],
            });
        }
    }
}
