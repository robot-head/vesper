//! §31.2 flight-log wire records, declared as `vesper-ffi` wire layouts.
//!
//! Every integer is little-endian and byte-aligned, so no layout has implicit
//! padding and records can sit at any byte offset of the ring.
//!
//! Sizes:
//! * `ChronoEventWireHeader` is 8+8+8+2+2+4 = 32 bytes, as the design states.
//! * `TraceCursorWire` is 4+8 = 12 bytes. The design's `copy struct TraceCursor`
//!   has no stated size; natively it would be 16 bytes with 4 implicit padding
//!   bytes, which wire layouts reject.
//! * `ObligationHopWire` is 8+8+4+1 = 21 field bytes. The design's
//!   `copy struct ObligationHop` would natively be 24 bytes with 3 bytes of
//!   implicit tail padding; the wire form makes them an explicit `ZeroPad[3]`,
//!   giving 24 bytes.
//! * `InoutWriteWire` (16 bytes) and `ObligationRecordWire` (24 bytes) are the
//!   payloads of the reserved effect ids below; the design has no layout for
//!   them.

use vesper_ffi::{
    Declaration, Endian, Field, Integer, Registry, ViewError, WireLayout, WireType,
    initialize_wire, view_wire,
};

/// First effect id reserved for Chrono's own structural records.
pub const RESERVED_EFFECT_BASE: u16 = 0xFFF0;
/// Payload: [`InoutWriteWire`].
pub const EFFECT_INOUT_WRITE: u16 = 0xFFF0;
/// Payload: [`ObligationRecordWire`].
pub const EFFECT_OBLIGATION: u16 = 0xFFF1;
/// Payload: epoch id `u32le`. Marks where a snapshot image was captured.
pub const EFFECT_SNAPSHOT: u16 = 0xFFF2;
/// Payload: one SimPulse `TraceEvent` as JSON (see [`crate::simpulse`]).
pub const EFFECT_SCHEDULER: u16 = 0xFFF3;
/// Bit 15 of `op_tag` marks a record whose payload is a 32-byte BLAKE3
/// digest of the effect outcome instead of its bytes. Callers' op tags are
/// therefore limited to 15 bits.
pub const OP_TAG_DIGEST: u16 = 0x8000;

pub const HEADER_BYTES: usize = 32;
pub const TRACE_CURSOR_BYTES: usize = 12;
pub const OBLIGATION_HOP_BYTES: usize = 24;
pub const INOUT_WRITE_BYTES: usize = 16;
pub const OBLIGATION_RECORD_BYTES: usize = 24;
pub const DIGEST_BYTES: usize = 32;

fn u(bits: u16) -> WireType {
    WireType::Integer(Integer::unsigned(bits, Endian::Little))
}

/// The wire declarations, compiled once per [`ChronoLayouts`].
pub fn declarations() -> Vec<Declaration> {
    vec![
        Declaration::new(
            "ChronoEventWireHeader",
            vec![
                Field::new("lamport_seq", u(64)),
                Field::new("fiber_id", u(64)),
                Field::new("ipt_tsc_anchor", u(64)),
                Field::new("effect_id", u(16)),
                Field::new("op_tag", u(16)),
                Field::new("payload_byte_len", u(32)),
            ],
        ),
        Declaration::new(
            "TraceCursorWire",
            vec![
                Field::new("epoch_id", u(32)),
                Field::new("lamport_seq", u(64)),
            ],
        ),
        Declaration::new(
            "ObligationHopWire",
            vec![
                Field::new("lamport_seq", u(64)),
                Field::new("fiber_id", u(64)),
                Field::new("source_span_id", u(32)),
                Field::new("hop_kind_tag", u(8)),
                Field::new("pad", WireType::ZeroPad(3)),
            ],
        ),
        Declaration::new(
            "InoutWriteWire",
            vec![
                Field::new("place_hash", u(64)),
                Field::new("source_span_id", u(32)),
                Field::new("pad", WireType::ZeroPad(4)),
            ],
        ),
        Declaration::new(
            "ObligationRecordWire",
            vec![
                Field::new("obligation_id", u(64)),
                Field::new("aux", u(64)),
                Field::new("source_span_id", u(32)),
                Field::new("hop_kind_tag", u(8)),
                Field::new("pad", WireType::ZeroPad(3)),
            ],
        ),
    ]
}

/// Compiled layouts. Construction allocates; encoding and decoding do not.
#[derive(Debug, Clone)]
pub struct ChronoLayouts {
    pub header: WireLayout,
    pub cursor: WireLayout,
    pub hop: WireLayout,
    pub inout: WireLayout,
    pub obligation: WireLayout,
}

impl ChronoLayouts {
    pub fn new() -> Self {
        let registry = Registry::new(declarations()).expect("Chrono wire declarations are valid");
        let compile = |name: &str| registry.compile(name).expect("Chrono wire layout compiles");
        Self {
            header: compile("ChronoEventWireHeader"),
            cursor: compile("TraceCursorWire"),
            hop: compile("ObligationHopWire"),
            inout: compile("InoutWriteWire"),
            obligation: compile("ObligationRecordWire"),
        }
    }
}

impl Default for ChronoLayouts {
    fn default() -> Self {
        Self::new()
    }
}

/// Decoded `ChronoEventWireHeader`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventHeader {
    pub lamport_seq: u64,
    pub fiber_id: u64,
    /// Without hardware tracing this holds the caller-supplied logical time in
    /// nanoseconds; no TSC or CNTVCT value is ever read.
    pub ipt_tsc_anchor: u64,
    pub effect_id: u16,
    pub op_tag: u16,
    pub payload_byte_len: u32,
}

impl EventHeader {
    pub fn encode(&self, layouts: &ChronoLayouts, out: &mut [u8]) -> Result<(), ViewError> {
        let (mut view, _) = initialize_wire(&layouts.header, out)?;
        view.write_u64(&["lamport_seq"], self.lamport_seq)?;
        view.write_u64(&["fiber_id"], self.fiber_id)?;
        view.write_u64(&["ipt_tsc_anchor"], self.ipt_tsc_anchor)?;
        view.write_u64(&["effect_id"], self.effect_id.into())?;
        view.write_u64(&["op_tag"], self.op_tag.into())?;
        view.write_u64(&["payload_byte_len"], self.payload_byte_len.into())?;
        Ok(())
    }
    pub fn decode(layouts: &ChronoLayouts, bytes: &[u8]) -> Result<Self, ViewError> {
        let view = view_wire(&layouts.header, bytes)?;
        Ok(Self {
            lamport_seq: view.read_u64(&["lamport_seq"])?,
            fiber_id: view.read_u64(&["fiber_id"])?,
            ipt_tsc_anchor: view.read_u64(&["ipt_tsc_anchor"])?,
            effect_id: view.read_u64(&["effect_id"])? as u16,
            op_tag: view.read_u64(&["op_tag"])? as u16,
            payload_byte_len: view.read_u64(&["payload_byte_len"])? as u32,
        })
    }
}

/// `copy struct TraceCursor` (§31.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TraceCursor {
    /// Epoch of the nearest snapshot at or before `lamport_seq`; 0 when none.
    pub epoch_id: u32,
    pub lamport_seq: u64,
}

impl TraceCursor {
    pub fn encode(&self, layouts: &ChronoLayouts, out: &mut [u8]) -> Result<(), ViewError> {
        let (mut view, _) = initialize_wire(&layouts.cursor, out)?;
        view.write_u64(&["epoch_id"], self.epoch_id.into())?;
        view.write_u64(&["lamport_seq"], self.lamport_seq)
    }
    pub fn decode(layouts: &ChronoLayouts, bytes: &[u8]) -> Result<Self, ViewError> {
        let view = view_wire(&layouts.cursor, bytes)?;
        Ok(Self {
            epoch_id: view.read_u64(&["epoch_id"])? as u32,
            lamport_seq: view.read_u64(&["lamport_seq"])?,
        })
    }
}

/// Payload of an [`EFFECT_INOUT_WRITE`] record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InoutWriteRecord {
    pub place_hash: u64,
    pub source_span_id: u32,
}

impl InoutWriteRecord {
    pub fn encode(&self, layouts: &ChronoLayouts, out: &mut [u8]) -> Result<(), ViewError> {
        let (mut view, _) = initialize_wire(&layouts.inout, out)?;
        view.write_u64(&["place_hash"], self.place_hash)?;
        view.write_u64(&["source_span_id"], self.source_span_id.into())
    }
    pub fn decode(layouts: &ChronoLayouts, bytes: &[u8]) -> Result<Self, ViewError> {
        let view = view_wire(&layouts.inout, bytes)?;
        view.validate_padding()?;
        Ok(Self {
            place_hash: view.read_u64(&["place_hash"])?,
            source_span_id: view.read_u64(&["source_span_id"])? as u32,
        })
    }
}

/// Payload of an [`EFFECT_OBLIGATION`] record. `aux` is the parent id for a
/// mint (0 = none), the destination fiber for a channel handoff, the place
/// hash for a projection, and 0 otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObligationRecord {
    pub obligation_id: u64,
    pub aux: u64,
    pub source_span_id: u32,
    pub hop_kind_tag: u8,
}

impl ObligationRecord {
    pub fn encode(&self, layouts: &ChronoLayouts, out: &mut [u8]) -> Result<(), ViewError> {
        let (mut view, _) = initialize_wire(&layouts.obligation, out)?;
        view.write_u64(&["obligation_id"], self.obligation_id)?;
        view.write_u64(&["aux"], self.aux)?;
        view.write_u64(&["source_span_id"], self.source_span_id.into())?;
        view.write_u64(&["hop_kind_tag"], self.hop_kind_tag.into())
    }
    pub fn decode(layouts: &ChronoLayouts, bytes: &[u8]) -> Result<Self, ViewError> {
        let view = view_wire(&layouts.obligation, bytes)?;
        view.validate_padding()?;
        Ok(Self {
            obligation_id: view.read_u64(&["obligation_id"])?,
            aux: view.read_u64(&["aux"])?,
            source_span_id: view.read_u64(&["source_span_id"])? as u32,
            hop_kind_tag: view.read_u64(&["hop_kind_tag"])? as u8,
        })
    }
}

/// Wire form of the design's `copy struct ObligationHop`, used when a lineage
/// query result is exported as bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObligationHopWire {
    pub lamport_seq: u64,
    pub fiber_id: u64,
    pub source_span_id: u32,
    pub hop_kind_tag: u8,
}

impl ObligationHopWire {
    pub fn encode(&self, layouts: &ChronoLayouts, out: &mut [u8]) -> Result<(), ViewError> {
        let (mut view, _) = initialize_wire(&layouts.hop, out)?;
        view.write_u64(&["lamport_seq"], self.lamport_seq)?;
        view.write_u64(&["fiber_id"], self.fiber_id)?;
        view.write_u64(&["source_span_id"], self.source_span_id.into())?;
        view.write_u64(&["hop_kind_tag"], self.hop_kind_tag.into())
    }
    pub fn decode(layouts: &ChronoLayouts, bytes: &[u8]) -> Result<Self, ViewError> {
        let view = view_wire(&layouts.hop, bytes)?;
        view.validate_padding()?;
        Ok(Self {
            lamport_seq: view.read_u64(&["lamport_seq"])?,
            fiber_id: view.read_u64(&["fiber_id"])?,
            source_span_id: view.read_u64(&["source_span_id"])? as u32,
            hop_kind_tag: view.read_u64(&["hop_kind_tag"])? as u8,
        })
    }
}
