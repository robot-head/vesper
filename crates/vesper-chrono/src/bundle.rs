//! `.vchrono` crash bundles: serialized, redacted and authenticated recordings.
//!
//! Layout, version 1, all integers little-endian:
//!
//! ```text
//! magic "VCHRONO1" | version u32 = 1 | flags u32 = 0
//! metadata_len u32 | metadata
//! first_seq u64 | next_seq u64 | evicted_records u64
//! record_count u32 | records: ChronoEventWireHeader (32 bytes) + payload
//! snapshot_count u32 | snapshots: epoch u32, start_seq u64, time_ns u64, len u32, image
//! redaction_count u32 | entries: target_tag u8, target u64, offset u32, len u32
//! mac [32]: keyed BLAKE3 of every preceding byte
//! ```
//!
//! The MAC is checked before anything else is parsed, so a bundle modified by
//! anyone without the key is rejected. Parsing is bounded by [`BundleLimits`]
//! and every length is checked; malformed input is a typed error, never a panic.

use std::fmt;

use crate::redact::{RedactionEntry, RedactionError, RedactionPolicy, RedactionTarget};
use crate::ring::{FlightEvent, FlightRecorder};
use crate::wire::{
    ChronoLayouts, DIGEST_BYTES, EFFECT_INOUT_WRITE, EFFECT_OBLIGATION, EFFECT_SCHEDULER,
    EFFECT_SNAPSHOT, EventHeader, HEADER_BYTES, InoutWriteRecord, OP_TAG_DIGEST, ObligationRecord,
    RESERVED_EFFECT_BASE,
};

pub const MAGIC: &[u8; 8] = b"VCHRONO1";
pub const VERSION: u32 = 1;
const MAC_BYTES: usize = 32;

/// 32-byte key for the bundle MAC. Generated and held by the caller.
#[derive(Clone)]
pub struct BundleKey([u8; 32]);

impl BundleKey {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl fmt::Debug for BundleKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BundleKey(..)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BundleLimits {
    pub max_bytes: usize,
    pub max_records: usize,
    pub max_payload: usize,
    pub max_snapshots: usize,
    pub max_image: usize,
    pub max_metadata: usize,
    pub max_redactions: usize,
}

impl Default for BundleLimits {
    fn default() -> Self {
        Self {
            max_bytes: 256 << 20,
            max_records: 1 << 22,
            max_payload: 1 << 20,
            max_snapshots: 4096,
            max_image: 64 << 20,
            max_metadata: 1 << 20,
            max_redactions: 1 << 22,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BundleError {
    TooLarge {
        len: usize,
        max: usize,
    },
    Truncated,
    BadMagic,
    UnsupportedVersion(u32),
    UnsupportedFlags(u32),
    /// The MAC does not match: tampered, corrupted or the wrong key.
    Authentication,
    LimitExceeded(&'static str),
    TrailingBytes,
    Malformed(&'static str),
    Redaction(RedactionError),
}

impl fmt::Display for BundleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, ".vchrono bundle: {self:?}")
    }
}
impl std::error::Error for BundleError {}

impl From<RedactionError> for BundleError {
    fn from(e: RedactionError) -> Self {
        Self::Redaction(e)
    }
}

/// An owned record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedEvent {
    pub lamport_seq: u64,
    pub fiber_id: u64,
    pub time_ns: u64,
    pub effect_id: u16,
    pub op_tag: u16,
    pub digest: bool,
    pub payload: Vec<u8>,
}

impl RecordedEvent {
    pub fn from_event(ev: &FlightEvent<'_>) -> Self {
        Self {
            lamport_seq: ev.lamport_seq,
            fiber_id: ev.fiber_id,
            time_ns: ev.time_ns,
            effect_id: ev.effect_id,
            op_tag: ev.op_tag,
            digest: ev.digest,
            payload: ev.payload.to_vec(),
        }
    }
    pub fn as_event(&self) -> FlightEvent<'_> {
        FlightEvent {
            lamport_seq: self.lamport_seq,
            fiber_id: self.fiber_id,
            time_ns: self.time_ns,
            effect_id: self.effect_id,
            op_tag: self.op_tag,
            digest: self.digest,
            payload: &self.payload,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedSnapshot {
    pub epoch_id: u32,
    pub start_lamport_seq: u64,
    pub time_ns: u64,
    pub image: Vec<u8>,
}

/// A parsed, authenticated and structurally validated bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    pub metadata: Vec<u8>,
    pub first_seq: u64,
    pub next_seq: u64,
    pub evicted_records: u64,
    pub records: Vec<RecordedEvent>,
    pub snapshots: Vec<RecordedSnapshot>,
    pub redactions: Vec<RedactionEntry>,
}

impl Bundle {
    /// Copy the recorder's retained state and apply the redaction policy.
    pub fn capture(recorder: &FlightRecorder, policy: &RedactionPolicy, metadata: &[u8]) -> Self {
        let mut redactions = Vec::new();
        let mut metadata = metadata.to_vec();
        policy.scrub_metadata(&mut metadata, &mut redactions);
        let records = recorder
            .events()
            .map(|ev| {
                let mut record = RecordedEvent::from_event(&ev);
                policy.scrub_record(
                    record.lamport_seq,
                    record.effect_id,
                    record.op_tag,
                    record.digest,
                    &mut record.payload,
                    &mut redactions,
                );
                record
            })
            .collect();
        let snapshots = recorder
            .snapshots()
            .into_iter()
            .map(|s| {
                let mut image = s.image.to_vec();
                policy.scrub_image(s.epoch_id, &mut image, &mut redactions);
                RecordedSnapshot {
                    epoch_id: s.epoch_id,
                    start_lamport_seq: s.start_lamport_seq,
                    time_ns: s.time_ns,
                    image,
                }
            })
            .collect();
        Self {
            metadata,
            first_seq: recorder.first_retained_seq(),
            next_seq: recorder.next_seq(),
            evicted_records: recorder.evicted_records(),
            records,
            snapshots,
            redactions,
        }
    }

    /// Capture, redact, serialize, verify no registered secret survived, and seal.
    pub fn export(
        recorder: &FlightRecorder,
        policy: &RedactionPolicy,
        metadata: &[u8],
        key: &BundleKey,
    ) -> Result<Vec<u8>, BundleError> {
        let bundle = Self::capture(recorder, policy, metadata);
        let bytes = bundle.seal(key)?;
        policy.verify_absent(&bytes[..bytes.len() - MAC_BYTES])?;
        policy.verify_absent(&bytes)?;
        Ok(bytes)
    }

    /// Serialize and append the MAC. Does not redact.
    pub fn seal(&self, key: &BundleKey) -> Result<Vec<u8>, BundleError> {
        let layouts = ChronoLayouts::new();
        let len32 = |len: usize, what: &'static str| {
            u32::try_from(len).map_err(|_| BundleError::LimitExceeded(what))
        };
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&len32(self.metadata.len(), "metadata")?.to_le_bytes());
        out.extend_from_slice(&self.metadata);
        out.extend_from_slice(&self.first_seq.to_le_bytes());
        out.extend_from_slice(&self.next_seq.to_le_bytes());
        out.extend_from_slice(&self.evicted_records.to_le_bytes());
        out.extend_from_slice(&len32(self.records.len(), "records")?.to_le_bytes());
        for record in &self.records {
            let mut header = [0u8; HEADER_BYTES];
            let mut ev = record.as_event().header();
            ev.payload_byte_len = len32(record.payload.len(), "payload")?;
            ev.encode(&layouts, &mut header)
                .map_err(|_| BundleError::Malformed("header encode"))?;
            out.extend_from_slice(&header);
            out.extend_from_slice(&record.payload);
        }
        out.extend_from_slice(&len32(self.snapshots.len(), "snapshots")?.to_le_bytes());
        for snapshot in &self.snapshots {
            out.extend_from_slice(&snapshot.epoch_id.to_le_bytes());
            out.extend_from_slice(&snapshot.start_lamport_seq.to_le_bytes());
            out.extend_from_slice(&snapshot.time_ns.to_le_bytes());
            out.extend_from_slice(&len32(snapshot.image.len(), "image")?.to_le_bytes());
            out.extend_from_slice(&snapshot.image);
        }
        out.extend_from_slice(&len32(self.redactions.len(), "redactions")?.to_le_bytes());
        for entry in &self.redactions {
            let (tag, value) = entry.target.tag();
            out.push(tag);
            out.extend_from_slice(&value.to_le_bytes());
            out.extend_from_slice(&entry.offset.to_le_bytes());
            out.extend_from_slice(&entry.len.to_le_bytes());
        }
        let mac = blake3::keyed_hash(&key.0, &out);
        out.extend_from_slice(mac.as_bytes());
        Ok(out)
    }

    /// Authenticate, parse and validate a bundle.
    pub fn open(bytes: &[u8], key: &BundleKey, limits: &BundleLimits) -> Result<Self, BundleError> {
        if bytes.len() > limits.max_bytes {
            return Err(BundleError::TooLarge {
                len: bytes.len(),
                max: limits.max_bytes,
            });
        }
        if bytes.len() < MAGIC.len() + MAC_BYTES {
            return Err(BundleError::Truncated);
        }
        if &bytes[..MAGIC.len()] != MAGIC {
            return Err(BundleError::BadMagic);
        }
        let (body, mac) = bytes.split_at(bytes.len() - MAC_BYTES);
        let expected = blake3::keyed_hash(&key.0, body);
        // blake3::Hash equality is constant time.
        if expected != blake3::Hash::from_bytes(mac.try_into().expect("32 bytes")) {
            return Err(BundleError::Authentication);
        }
        let bundle = Self::parse(&body[MAGIC.len()..], limits)?;
        bundle.validate()?;
        Ok(bundle)
    }

    fn parse(body: &[u8], limits: &BundleLimits) -> Result<Self, BundleError> {
        let layouts = ChronoLayouts::new();
        let mut r = Reader { bytes: body };
        let version = r.u32()?;
        if version != VERSION {
            return Err(BundleError::UnsupportedVersion(version));
        }
        let flags = r.u32()?;
        if flags != 0 {
            return Err(BundleError::UnsupportedFlags(flags));
        }
        let metadata_len = r.len(limits.max_metadata, "metadata")?;
        let metadata = r.take(metadata_len)?.to_vec();
        let first_seq = r.u64()?;
        let next_seq = r.u64()?;
        let evicted_records = r.u64()?;
        let record_count = r.len(limits.max_records, "records")?;
        let mut records = Vec::with_capacity(record_count.min(4096));
        for _ in 0..record_count {
            let header = EventHeader::decode(&layouts, r.peek(HEADER_BYTES)?)
                .map_err(|_| BundleError::Malformed("record header"))?;
            r.take(HEADER_BYTES)?;
            let len = header.payload_byte_len as usize;
            if len > limits.max_payload {
                return Err(BundleError::LimitExceeded("payload"));
            }
            let payload = r.take(len)?.to_vec();
            records.push(RecordedEvent {
                lamport_seq: header.lamport_seq,
                fiber_id: header.fiber_id,
                time_ns: header.ipt_tsc_anchor,
                effect_id: header.effect_id,
                op_tag: header.op_tag & !OP_TAG_DIGEST,
                digest: header.op_tag & OP_TAG_DIGEST != 0,
                payload,
            });
        }
        let snapshot_count = r.len(limits.max_snapshots, "snapshots")?;
        let mut snapshots = Vec::with_capacity(snapshot_count.min(256));
        for _ in 0..snapshot_count {
            let epoch_id = r.u32()?;
            let start_lamport_seq = r.u64()?;
            let time_ns = r.u64()?;
            let len = r.len(limits.max_image, "image")?;
            let image = r.take(len)?.to_vec();
            snapshots.push(RecordedSnapshot {
                epoch_id,
                start_lamport_seq,
                time_ns,
                image,
            });
        }
        let redaction_count = r.len(limits.max_redactions, "redactions")?;
        let mut redactions = Vec::with_capacity(redaction_count.min(4096));
        for _ in 0..redaction_count {
            let tag = r.u8()?;
            let value = r.u64()?;
            let target = RedactionTarget::from_tag(tag, value)
                .ok_or(BundleError::Malformed("redaction target"))?;
            redactions.push(RedactionEntry {
                target,
                offset: r.u32()?,
                len: r.u32()?,
            });
        }
        if !r.bytes.is_empty() {
            return Err(BundleError::TrailingBytes);
        }
        Ok(Self {
            metadata,
            first_seq,
            next_seq,
            evicted_records,
            records,
            snapshots,
            redactions,
        })
    }

    fn validate(&self) -> Result<(), BundleError> {
        let layouts = ChronoLayouts::new();
        let malformed = BundleError::Malformed;
        if self.first_seq == 0 || self.evicted_records != self.first_seq - 1 {
            return Err(malformed("first_seq and evicted_records disagree"));
        }
        let count = self.records.len() as u64;
        if self.first_seq.checked_add(count) != Some(self.next_seq) {
            return Err(malformed("record count and sequence range disagree"));
        }
        let mut last_time = 0;
        for (i, record) in self.records.iter().enumerate() {
            if record.lamport_seq != self.first_seq + i as u64 {
                return Err(malformed("records are not consecutive in lamport_seq"));
            }
            if record.time_ns < last_time {
                return Err(malformed("record time went backwards"));
            }
            last_time = record.time_ns;
            if record.digest && record.payload.len() != DIGEST_BYTES {
                return Err(malformed("digest record payload is not 32 bytes"));
            }
            match record.effect_id {
                EFFECT_INOUT_WRITE => {
                    if record.payload.len() != crate::wire::INOUT_WRITE_BYTES || record.digest {
                        return Err(malformed("inout write payload size"));
                    }
                    InoutWriteRecord::decode(&layouts, &record.payload)
                        .map_err(|_| malformed("inout write payload"))?;
                }
                EFFECT_OBLIGATION => {
                    if record.payload.len() != crate::wire::OBLIGATION_RECORD_BYTES || record.digest
                    {
                        return Err(malformed("obligation payload size"));
                    }
                    let decoded = ObligationRecord::decode(&layouts, &record.payload)
                        .map_err(|_| malformed("obligation payload"))?;
                    crate::lineage::HopKind::from_record(&decoded)
                        .map_err(|_| malformed("obligation hop kind"))?;
                }
                EFFECT_SNAPSHOT if record.payload.len() != 4 || record.digest => {
                    return Err(malformed("snapshot marker payload size"));
                }
                EFFECT_SCHEDULER if record.digest => {
                    return Err(malformed("scheduler record cannot be a digest"));
                }
                EFFECT_SNAPSHOT | EFFECT_SCHEDULER => {}
                id if id >= RESERVED_EFFECT_BASE => {
                    return Err(malformed("unknown reserved effect id"));
                }
                _ => {}
            }
        }
        let mut previous: Option<&RecordedSnapshot> = None;
        for snapshot in &self.snapshots {
            if snapshot.epoch_id == 0 || snapshot.start_lamport_seq == 0 {
                return Err(malformed("snapshot epoch or start is zero"));
            }
            if snapshot.start_lamport_seq >= self.next_seq {
                return Err(malformed("snapshot starts after the recording"));
            }
            if let Some(p) = previous
                && (snapshot.epoch_id <= p.epoch_id
                    || snapshot.start_lamport_seq <= p.start_lamport_seq)
            {
                return Err(malformed("snapshots out of order"));
            }
            if let Some(marker) = self.record(snapshot.start_lamport_seq)
                && (marker.effect_id != EFFECT_SNAPSHOT
                    || marker.payload != snapshot.epoch_id.to_le_bytes())
            {
                return Err(malformed("snapshot marker record mismatch"));
            }
            previous = Some(snapshot);
        }
        for entry in &self.redactions {
            let target_len = match entry.target {
                RedactionTarget::Record { lamport_seq } => self
                    .record(lamport_seq)
                    .map(|r| r.payload.len())
                    .ok_or(malformed("redaction names a missing record"))?,
                RedactionTarget::Snapshot { epoch_id } => self
                    .snapshots
                    .iter()
                    .find(|s| s.epoch_id == epoch_id)
                    .map(|s| s.image.len())
                    .ok_or(malformed("redaction names a missing snapshot"))?,
                RedactionTarget::Metadata => self.metadata.len(),
            };
            let end = (entry.offset as usize)
                .checked_add(entry.len as usize)
                .ok_or(malformed("redaction range overflows"))?;
            if entry.len == 0 || end > target_len {
                return Err(malformed("redaction range outside its target"));
            }
        }
        Ok(())
    }

    /// The record with this sequence, if retained.
    pub fn record(&self, lamport_seq: u64) -> Option<&RecordedEvent> {
        let index = lamport_seq.checked_sub(self.first_seq)?;
        self.records.get(usize::try_from(index).ok()?)
    }

    /// Whether any redaction touched this record.
    pub fn is_redacted(&self, lamport_seq: u64) -> bool {
        self.redactions
            .iter()
            .any(|e| e.target == RedactionTarget::Record { lamport_seq })
    }

    pub fn events(&self) -> impl Iterator<Item = FlightEvent<'_>> + '_ {
        self.records.iter().map(RecordedEvent::as_event)
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    fn peek(&self, n: usize) -> Result<&'a [u8], BundleError> {
        self.bytes.get(..n).ok_or(BundleError::Truncated)
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], BundleError> {
        if self.bytes.len() < n {
            return Err(BundleError::Truncated);
        }
        let (head, rest) = self.bytes.split_at(n);
        self.bytes = rest;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8, BundleError> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, BundleError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().expect("4")))
    }
    fn u64(&mut self) -> Result<u64, BundleError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().expect("8")))
    }
    fn len(&mut self, max: usize, what: &'static str) -> Result<usize, BundleError> {
        let n = self.u32()? as usize;
        if n > max {
            return Err(BundleError::LimitExceeded(what));
        }
        Ok(n)
    }
}
