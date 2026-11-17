//! The types of the export ABI (`--rust-types crate::ported::types`): the
//! crate's own public types where their layout is plain words, and
//! word-only mirrors of the ones that hold arrays, borrowed slices or
//! `&'static str` (see `vesper/*.ves` for each mirror's counterpart).

pub use crate::cc::{AckedPacket, CongestionEvent, LostPacket, NewReno, RttSample};
pub use crate::conn::{CloseReason, Config, Event, State, Stats};
pub use crate::rtt::{RtoConfig, RttEstimator};
pub use crate::wire::{Timestamps, WireError};

/// `wire::SackBlock` as words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Blk {
    pub left: u32,
    pub right: u32,
}

/// `wire::TcpOptions` with its four SACK blocks as fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Opts {
    pub mss: Option<u16>,
    pub window_scale: Option<u8>,
    pub sack_permitted: bool,
    pub b0: Blk,
    pub b1: Blk,
    pub b2: Blk,
    pub b3: Blk,
    pub sack_len: u8,
    pub timestamps: Option<Timestamps>,
}

/// `wire::HeaderFields` as words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hdr {
    pub src_port: u16,
    pub dst_port: u16,
    pub seq: u32,
    pub ack: u32,
    pub flags: u8,
    pub window: u16,
    pub urgent: u16,
}

/// A parsed header: `wire::Segment` without its borrowed payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parsed {
    pub src_port: u16,
    pub dst_port: u16,
    pub seq: u32,
    pub ack: u32,
    pub flags: u8,
    pub window: u16,
    pub checksum: u16,
    pub urgent: u16,
    pub header_len: u64,
    pub options: Opts,
}

/// `wire::IpPair` with big-endian address words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ips {
    V4 { src: u32, dst: u32 },
    V6 { s0: u64, s1: u64, d0: u64, d1: u64 },
}

/// `conn::TcpError`; `InvalidConfig` holds an index into
/// [`super::conn::CONFIG_ERRORS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TcpFail {
    Wire(WireError),
    NotForThisConnection,
    RingSizeMismatch,
    InvalidState(State),
    ConnectionClosing,
    SendViewMissing { start: u64, end: u64 },
    BufferTooSmall,
    InvalidConfig(u8),
    ReleaseExceedsUnread,
    Overflow,
}

/// A sent-queue entry (private to the connection; read only through the
/// handle's view).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SentSeg {
    pub start: u64,
    pub end: u64,
    pub syn: bool,
    pub fin: bool,
    pub pn: u64,
    pub sent_ns: u64,
    pub retransmits: u32,
    pub sacked: bool,
    pub lost: bool,
    pub rexmit_in_recovery: bool,
}

/// An out-of-order range (private to the connection).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OooRange {
    pub start: u64,
    pub end: u64,
    pub stamp: u64,
}

/// A queued RST (private to the connection).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RstSpec {
    pub ips: Ips,
    pub src_port: u16,
    pub dst_port: u16,
    pub seq: u32,
    pub ack: Option<u32>,
}

/// A congestion-controller call the connection made, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CcCmd {
    Mss(u32),
    Rtt(RttSample),
    Event { event: CongestionEvent, now: u64 },
}

/// In-order payload of one segment, as offsets into the segment and the
/// receive ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecvOut {
    pub seg_start: u64,
    pub seg_len: u64,
    pub ring_start: u64,
    pub ring_len1: u64,
    pub ring_len2: u64,
    pub fin: bool,
}

/// One segment written by `tcb_poll_transmit`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tx {
    pub len: u64,
    pub edt_ns: u64,
    pub ips: Ips,
    pub entry: bool,
    pub pn: u64,
    pub bytes: u32,
}
