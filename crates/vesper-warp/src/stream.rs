//! RFC 9000 §2-4 streams with §13.3 retransmission (wave five, issue #133).
//!
//! Sans-IO and allocation-free once a connection's streams are warm:
//!
//! * [`SendBuf`] keeps a stream's unacknowledged bytes in a fixed ring,
//!   with per-byte "acknowledged" and "lost" bitmaps. Acknowledged prefixes
//!   free ring space; lost ranges are retransmitted as *ranges*, not
//!   packets, and never re-send bytes already acknowledged.
//! * [`RecvBuf`] reassembles out-of-order and duplicate data into a fixed
//!   ring of exactly the advertised window, with a per-byte "received"
//!   bitmap. Memory is bounded by the window whatever the peer's
//!   fragmentation; data past the window is a flow-control violation.
//! * [`SendStream`] and [`RecvStream`] are the RFC 9000 §3.1 and §3.2
//!   state machines (FIN, RESET_STREAM, STOP_SENDING, final size).
//! * [`StreamSet`] is one connection's stream layer: stream IDs and
//!   MAX_STREAMS limits per direction (§2.1, §4.6), connection and stream
//!   credit (§4.1-4.5), *_BLOCKED signalling, CRYPTO buffers per
//!   packet-number space (§19.6), packet building ([`StreamSet::fill`]),
//!   and the per-packet frame record that turns RFC 9002 acknowledgement
//!   and loss events into §13.3 repairs: lost STREAM and CRYPTO ranges are
//!   requeued, RESET_STREAM and STOP_SENDING are resent while still
//!   relevant, and MAX_DATA, MAX_STREAM_DATA, MAX_STREAMS and *_BLOCKED are
//!   resent with their *current* values.
//!
//! Peer violations are [`TransportError`]s carrying the RFC 9000 §20.1
//! code (FLOW_CONTROL_ERROR, STREAM_LIMIT_ERROR, STREAM_STATE_ERROR,
//! FINAL_SIZE_ERROR, CRYPTO_BUFFER_EXCEEDED) for the CONNECTION_CLOSE the
//! endpoint sends. There is no TLS here: the peer's transport parameters
//! are supplied by the caller ([`PeerParams`]); integration A replaces them
//! with the ones the handshake negotiates.
//!
//! Rust ownership in this module is not evidence that Vesper's source-level
//! typing rules are implemented; it is host code.

use std::collections::VecDeque;

use crate::frame::{self, Frame};
use crate::{PnSpace, TransportErrorCode, varint};

/// A peer protocol violation, for CONNECTION_CLOSE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportError {
    pub code: TransportErrorCode,
    /// The type of the frame that triggered the error.
    pub frame_type: u64,
}

/// Errors from the local stream API (never caused by the peer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamError {
    /// No such stream: never opened, or already closed and forgotten.
    UnknownStream(u64),
    /// The stream has no send half (a peer-initiated unidirectional one).
    NotWritable(u64),
    /// The stream has no receive half (a locally initiated unidirectional one).
    NotReadable(u64),
    /// The peer's MAX_STREAMS limit, or our own concurrency limit, is
    /// reached; a STREAMS_BLOCKED frame was queued when the peer's was.
    StreamLimit { uni: bool },
    /// The send half was already finished.
    Finished(u64),
    /// The stream was reset (by us, or by the peer for a receive half),
    /// with this application error code.
    Reset { stream_id: u64, error_code: u64 },
    /// The error code does not fit a varint.
    BadErrorCode,
    /// An RFC 9218 urgency above 7.
    BadPriority(u8),
}

/// Who opened the connection; decides which stream IDs are ours (§2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Role {
    #[default]
    Client,
    Server,
}

impl Role {
    const fn initiator_bit(self) -> u64 {
        match self {
            Self::Client => 0,
            Self::Server => 1,
        }
    }
}

/// Bit 1 of a stream ID: unidirectional.
pub const fn is_uni(stream_id: u64) -> bool {
    stream_id & 2 != 0
}

/// The stream ID of the `index`-th stream of a kind (§2.1).
pub const fn stream_id(initiator: Role, uni: bool, index: u64) -> u64 {
    index << 2 | (uni as u64) << 1 | initiator.initiator_bit()
}

/// Transport parameters that govern streams (RFC 9000 §18.2). Without a
/// handshake they are supplied by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerParams {
    pub initial_max_data: u64,
    pub initial_max_stream_data_bidi_local: u64,
    pub initial_max_stream_data_bidi_remote: u64,
    pub initial_max_stream_data_uni: u64,
    pub initial_max_streams_bidi: u64,
    pub initial_max_streams_uni: u64,
}

/// One connection's stream-layer limits (what we advertise, and how much
/// we buffer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamConfig {
    /// Concurrent streams of each kind the peer may open, and that we open.
    pub max_streams_bidi: u64,
    pub max_streams_uni: u64,
    /// Per-stream receive window and ring size (initial_max_stream_data_*).
    pub stream_window: u32,
    /// Per-stream send ring: unacknowledged plus unsent bytes.
    pub send_buffer: u32,
    /// Connection receive window (initial_max_data).
    pub conn_window: u64,
    /// CRYPTO buffer per packet-number space, each direction (RFC 9000
    /// §7.5: at least 4096 bytes).
    pub crypto_buffer: u32,
    /// Frame records kept for packets in flight; past this the oldest
    /// packet's frames are treated as lost (a spurious retransmission,
    /// never a lost byte).
    pub tracked_frames: usize,
    /// Stream events queued for the application; past this they are
    /// counted and dropped (reads and writes still work).
    pub event_capacity: usize,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            max_streams_bidi: 8,
            max_streams_uni: 4,
            stream_window: 64 * 1024,
            send_buffer: 64 * 1024,
            conn_window: 256 * 1024,
            crypto_buffer: 4096,
            tracked_frames: 1024,
            event_capacity: 64,
        }
    }
}

impl StreamConfig {
    /// The transport parameters this configuration advertises.
    pub const fn local_params(&self) -> PeerParams {
        let w = self.stream_window as u64;
        PeerParams {
            initial_max_data: self.conn_window,
            initial_max_stream_data_bidi_local: w,
            initial_max_stream_data_bidi_remote: w,
            initial_max_stream_data_uni: w,
            initial_max_streams_bidi: self.max_streams_bidi,
            initial_max_streams_uni: self.max_streams_uni,
        }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.stream_window == 0 || self.send_buffer == 0 {
            return Err("stream buffers must be non-empty");
        }
        if self.crypto_buffer < 4096 {
            return Err("crypto_buffer must be at least 4096 bytes (RFC 9000 §7.5)");
        }
        if self.max_streams_bidi > frame::MAX_STREAM_COUNT
            || self.max_streams_uni > frame::MAX_STREAM_COUNT
            || self.conn_window > varint::MAX
        {
            return Err("stream limits out of range");
        }
        if self.max_streams_bidi.saturating_add(self.max_streams_uni) > 1 << 16 {
            return Err("at most 65536 concurrent streams per direction");
        }
        if self.tracked_frames < MAX_FRAMES_PER_PACKET {
            return Err("tracked_frames must hold at least one packet's frames");
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Ring bitmap.

/// One bit per byte offset of a ring of `cap` bytes (offset `o` maps to
/// bit `o % cap`). Callers keep every live range within one window of
/// `cap` bytes.
#[derive(Debug, Clone)]
struct Bits {
    words: Box<[u64]>,
    cap: u64,
}

impl Bits {
    fn new(cap: usize) -> Self {
        Self {
            words: vec![0; cap.div_ceil(64)].into_boxed_slice(),
            cap: cap as u64,
        }
    }

    /// Calls `f(word, mask, offset_of_lowest_mask_bit)` over `[off, end)`,
    /// which is at most `cap` bytes long.
    fn for_each(&self, off: u64, end: u64, mut f: impl FnMut(usize, u64, u64) -> bool) {
        debug_assert!(end - off <= self.cap);
        let mut o = off;
        while o < end {
            let i = o % self.cap;
            // Bits to the end of the word, the ring and the range.
            let bit = i % 64;
            let take = (64 - bit).min(self.cap - i).min(end - o);
            let mask = if take == 64 {
                u64::MAX
            } else {
                ((1u64 << take) - 1) << bit
            };
            if !f((i / 64) as usize, mask, o - bit) {
                return;
            }
            o += take;
        }
    }

    fn set(&mut self, off: u64, end: u64) {
        let mut words = std::mem::take(&mut self.words);
        self.for_each(off, end, |w, m, _| {
            words[w] |= m;
            true
        });
        self.words = words;
    }

    fn clear(&mut self, off: u64, end: u64) {
        let mut words = std::mem::take(&mut self.words);
        self.for_each(off, end, |w, m, _| {
            words[w] &= !m;
            true
        });
        self.words = words;
    }

    /// Sets the bits of `[off, end)` that are clear in `except`.
    fn set_except(&mut self, off: u64, end: u64, except: &Bits) {
        let mut words = std::mem::take(&mut self.words);
        self.for_each(off, end, |w, m, _| {
            words[w] |= m & !except.words[w];
            true
        });
        self.words = words;
    }

    #[cfg(test)]
    fn get(&self, off: u64) -> bool {
        let i = off % self.cap;
        self.words[(i / 64) as usize] & (1 << (i % 64)) != 0
    }

    /// The first offset in `[off, end)` whose bit equals `want`.
    fn find(&self, off: u64, end: u64, want: bool) -> Option<u64> {
        let mut found = None;
        self.for_each(off, end, |w, m, base| {
            let word = if want { self.words[w] } else { !self.words[w] };
            let hit = word & m;
            if hit != 0 {
                found = Some(base + u64::from(hit.trailing_zeros()));
                false
            } else {
                true
            }
        });
        found
    }
}

/// Copies `out.len()` bytes at stream offset `off` out of a ring.
fn ring_read(ring: &[u8], off: u64, out: &mut [u8]) {
    let cap = ring.len();
    let i = (off % cap as u64) as usize;
    let first = out.len().min(cap - i);
    out[..first].copy_from_slice(&ring[i..i + first]);
    let rest = out.len() - first;
    out[first..].copy_from_slice(&ring[..rest]);
}

fn ring_write(ring: &mut [u8], off: u64, data: &[u8]) {
    let cap = ring.len();
    let i = (off % cap as u64) as usize;
    let first = data.len().min(cap - i);
    ring[i..i + first].copy_from_slice(&data[..first]);
    let rest = data.len() - first;
    ring[..rest].copy_from_slice(&data[first..]);
}

// ---------------------------------------------------------------------------
// Send buffer.

/// The bytes of one stream (or CRYPTO stream) the peer has not
/// acknowledged, in a fixed ring.
///
/// Offsets: `[0, base)` acknowledged and freed; `[base, next)` sent, each
/// byte acknowledged, lost (to resend) or in flight; `[next, end)` written
/// but never sent.
#[derive(Debug, Clone)]
pub struct SendBuf {
    data: Box<[u8]>,
    acked: Bits,
    lost: Bits,
    base: u64,
    next: u64,
    end: u64,
    /// No lost byte below this offset (`u64::MAX`: none at all).
    lost_hint: u64,
}

impl SendBuf {
    pub fn new(capacity: usize) -> Self {
        Self {
            data: vec![0; capacity].into_boxed_slice(),
            acked: Bits::new(capacity),
            lost: Bits::new(capacity),
            base: 0,
            next: 0,
            end: 0,
            lost_hint: u64::MAX,
        }
    }
    pub fn capacity(&self) -> usize {
        self.data.len()
    }
    /// Lowest offset not yet acknowledged.
    pub const fn acked_prefix(&self) -> u64 {
        self.base
    }
    /// Offset one past the highest byte ever sent.
    pub const fn sent_offset(&self) -> u64 {
        self.next
    }
    /// Offset one past the last byte written.
    pub const fn written(&self) -> u64 {
        self.end
    }
    /// Bytes buffered (unacknowledged or unsent).
    pub const fn buffered(&self) -> u64 {
        self.end - self.base
    }
    pub fn free_space(&self) -> usize {
        self.data.len() - self.buffered() as usize
    }
    /// Copies as much of `data` as fits; returns the bytes accepted.
    pub fn write(&mut self, data: &[u8]) -> usize {
        let n = data.len().min(self.free_space());
        ring_write(&mut self.data, self.end, &data[..n]);
        self.end += n as u64;
        n
    }
    pub const fn has_lost(&self) -> bool {
        self.lost_hint < self.next
    }
    /// Takes up to `max` bytes of the lowest lost range for retransmission.
    pub fn next_lost(&mut self, max: usize) -> Option<(u64, usize)> {
        if max == 0 || !self.has_lost() {
            return None;
        }
        let from = self.lost_hint.max(self.base);
        let Some(start) = self.lost.find(from, self.next, true) else {
            self.lost_hint = u64::MAX;
            return None;
        };
        let limit = self.next.min(start.saturating_add(max as u64));
        let stop = self.lost.find(start, limit, false).unwrap_or(limit);
        self.lost.clear(start, stop);
        self.lost_hint = stop;
        Some((start, (stop - start) as usize))
    }
    /// Bytes written but never sent.
    pub const fn unsent(&self) -> u64 {
        self.end - self.next
    }
    /// Takes up to `max` never-sent bytes; returns their offset and length.
    pub fn take_new(&mut self, max: u64) -> (u64, usize) {
        let off = self.next;
        let len = max.min(self.unsent());
        self.next += len;
        (off, len as usize)
    }
    /// Copies buffered bytes at `off` (within `[base, end)`) into `out`.
    pub fn copy_out(&self, off: u64, out: &mut [u8]) {
        debug_assert!(off >= self.base && off + out.len() as u64 <= self.end);
        ring_read(&self.data, off, out);
    }
    /// The peer acknowledged `[off, off + len)`; frees the acknowledged
    /// prefix.
    pub fn on_ack(&mut self, off: u64, len: usize) {
        let start = off.max(self.base);
        let stop = off.saturating_add(len as u64).min(self.next);
        if start >= stop {
            return;
        }
        self.acked.set(start, stop);
        self.lost.clear(start, stop);
        if start == self.base {
            let new_base = self
                .acked
                .find(self.base, self.next, false)
                .unwrap_or(self.next);
            self.acked.clear(self.base, new_base);
            self.base = new_base;
        }
    }
    /// `[off, off + len)` was in a lost packet: its unacknowledged bytes
    /// are queued for retransmission.
    pub fn on_lost(&mut self, off: u64, len: usize) {
        let start = off.max(self.base);
        let stop = off.saturating_add(len as u64).min(self.next);
        if start >= stop {
            return;
        }
        self.lost.set_except(start, stop, &self.acked);
        self.lost_hint = self.lost_hint.min(start);
    }
    /// Drops every buffered byte (after RESET_STREAM); nothing more is sent.
    fn discard(&mut self) {
        let end = self.next;
        self.acked.clear(self.base, end);
        self.lost.clear(self.base, end);
        self.base = end;
        self.end = end;
        self.lost_hint = u64::MAX;
    }
    /// Back to empty at offset 0, for reuse by another stream.
    fn reset_for_reuse(&mut self) {
        self.discard();
        self.base = 0;
        self.next = 0;
        self.end = 0;
        self.acked.words.fill(0);
        self.lost.words.fill(0);
    }
}

// ---------------------------------------------------------------------------
// Receive buffer.

/// Data received past the buffer's window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowExceeded;

/// Reassembles one stream's (or CRYPTO stream's) data. Holds exactly one
/// window: offsets `[read, read + capacity)`.
#[derive(Debug, Clone)]
pub struct RecvBuf {
    data: Box<[u8]>,
    have: Bits,
    read: u64,
    /// First offset at or above `read` not yet received.
    contig: u64,
    highest: u64,
}

impl RecvBuf {
    pub fn new(capacity: usize) -> Self {
        Self {
            data: vec![0; capacity].into_boxed_slice(),
            have: Bits::new(capacity),
            read: 0,
            contig: 0,
            highest: 0,
        }
    }
    pub fn capacity(&self) -> usize {
        self.data.len()
    }
    /// Offset the application has read up to.
    pub const fn read_offset(&self) -> u64 {
        self.read
    }
    /// One past the highest offset received.
    pub const fn highest(&self) -> u64 {
        self.highest
    }
    /// Contiguous bytes ready to read.
    pub const fn readable(&self) -> u64 {
        self.contig - self.read
    }
    /// One past the last contiguously received byte.
    pub const fn contiguous(&self) -> u64 {
        self.contig
    }
    /// Stores `data` at `off`. Bytes already read are ignored, duplicates
    /// overwrite (RFC 9000 §2.2 lets an endpoint assume they are equal).
    pub fn insert(&mut self, off: u64, data: &[u8]) -> Result<(), WindowExceeded> {
        let end = off.checked_add(data.len() as u64).ok_or(WindowExceeded)?;
        if end > self.read + self.data.len() as u64 {
            return Err(WindowExceeded);
        }
        // An empty frame (e.g. FIN only) still moves the highest offset
        // that flow control and final-size checks count.
        self.highest = self.highest.max(end);
        let start = off.max(self.read);
        if start >= end {
            return Ok(());
        }
        ring_write(&mut self.data, start, &data[(start - off) as usize..]);
        self.have.set(start, end);
        if start <= self.contig {
            self.contig = self
                .have
                .find(self.contig, self.highest, false)
                .unwrap_or(self.highest);
        }
        Ok(())
    }
    /// Reads contiguous bytes into `out`; returns the count.
    pub fn read(&mut self, out: &mut [u8]) -> usize {
        let n = out.len().min(self.readable() as usize);
        ring_read(&self.data, self.read, &mut out[..n]);
        self.have.clear(self.read, self.read + n as u64);
        self.read += n as u64;
        n
    }
    fn reset_for_reuse(&mut self) {
        self.have.words.fill(0);
        self.read = 0;
        self.contig = 0;
        self.highest = 0;
    }
}

// ---------------------------------------------------------------------------
// Stream state machines.

/// RFC 9000 §3.1 sending-part states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendState {
    Ready,
    Send,
    DataSent,
    DataRecvd,
    ResetSent,
    ResetRecvd,
}

/// RFC 9000 §3.2 receiving-part states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvState {
    Recv,
    SizeKnown,
    DataRecvd,
    DataRead,
    ResetRecvd,
    ResetRead,
}

/// The sending part of a stream.
#[derive(Debug)]
pub struct SendStream {
    buf: SendBuf,
    /// The peer's MAX_STREAM_DATA.
    max_data: u64,
    fin: Option<u64>,
    fin_sent: bool,
    fin_pending: bool,
    fin_acked: bool,
    reset: Option<(u64, u64)>,
    reset_pending: bool,
    reset_acked: bool,
    /// The limit a STREAM_DATA_BLOCKED was last queued for.
    blocked_at: Option<u64>,
    blocked_pending: bool,
    /// The application wrote less than it offered; tell it when there is room.
    want_writable: bool,
}

impl SendStream {
    fn new(buf: SendBuf, max_data: u64) -> Self {
        Self {
            buf,
            max_data,
            fin: None,
            fin_sent: false,
            fin_pending: false,
            fin_acked: false,
            reset: None,
            reset_pending: false,
            reset_acked: false,
            blocked_at: None,
            blocked_pending: false,
            want_writable: false,
        }
    }
    pub fn state(&self) -> SendState {
        if self.reset.is_some() {
            if self.reset_acked {
                SendState::ResetRecvd
            } else {
                SendState::ResetSent
            }
        } else if self.fin_acked && Some(self.buf.base) == self.fin {
            SendState::DataRecvd
        } else if self.fin_sent && Some(self.buf.next) == self.fin {
            SendState::DataSent
        } else if self.buf.next > 0 || self.fin_sent {
            SendState::Send
        } else {
            SendState::Ready
        }
    }
    pub const fn buffer(&self) -> &SendBuf {
        &self.buf
    }
    /// The peer's current credit for this stream.
    pub const fn max_data(&self) -> u64 {
        self.max_data
    }
    fn done(&self) -> bool {
        matches!(self.state(), SendState::DataRecvd | SendState::ResetRecvd)
    }
    /// New bytes sendable under the stream credit.
    fn credit(&self) -> u64 {
        self.max_data
            .min(self.buf.end)
            .saturating_sub(self.buf.next)
    }
    fn fin_due(&self) -> bool {
        self.reset.is_none()
            && self.fin == Some(self.buf.next)
            && (!self.fin_sent || self.fin_pending)
    }
    fn has_data(&self, conn_credit: u64) -> bool {
        self.reset.is_none()
            && (self.buf.has_lost() || self.fin_due() || (self.credit() > 0 && conn_credit > 0))
    }
    fn reset_now(&mut self, error_code: u64) {
        if self.reset.is_some() || self.state() == SendState::DataRecvd {
            return;
        }
        self.reset = Some((error_code, self.buf.next));
        self.reset_pending = true;
        self.blocked_pending = false;
        self.fin_pending = false;
        self.buf.discard();
    }
}

/// The receiving part of a stream.
#[derive(Debug)]
pub struct RecvStream {
    buf: RecvBuf,
    final_size: Option<u64>,
    /// The MAX_STREAM_DATA we advertised.
    max_data: u64,
    max_data_pending: bool,
    reset: Option<(u64, u64)>,
    reset_read: bool,
    stop: Option<u64>,
    stop_pending: bool,
    all_read: bool,
    /// A Readable event is outstanding.
    readable_signalled: bool,
}

impl RecvStream {
    fn new(buf: RecvBuf) -> Self {
        let max_data = buf.capacity() as u64;
        Self {
            buf,
            final_size: None,
            max_data,
            max_data_pending: false,
            reset: None,
            reset_read: false,
            stop: None,
            stop_pending: false,
            all_read: false,
            readable_signalled: false,
        }
    }
    pub fn state(&self) -> RecvState {
        if self.reset.is_some() {
            if self.reset_read {
                RecvState::ResetRead
            } else {
                RecvState::ResetRecvd
            }
        } else if self.all_read {
            RecvState::DataRead
        } else if let Some(f) = self.final_size {
            if self.buf.contig == f {
                RecvState::DataRecvd
            } else {
                RecvState::SizeKnown
            }
        } else {
            RecvState::Recv
        }
    }
    pub const fn buffer(&self) -> &RecvBuf {
        &self.buf
    }
    /// The credit we advertised for this stream.
    pub const fn max_data(&self) -> u64 {
        self.max_data
    }
    pub const fn final_size(&self) -> Option<u64> {
        self.final_size
    }
    fn done(&self) -> bool {
        matches!(self.state(), RecvState::DataRead | RecvState::ResetRead)
    }
    /// Highest offset counted against flow control.
    fn highest(&self) -> u64 {
        match self.reset {
            Some((_, f)) => f,
            None => self.buf.highest,
        }
    }

    /// A STREAM frame; returns the increase of the highest received offset
    /// (connection flow control counts it).
    fn on_data(&mut self, off: u64, data: &[u8], fin: bool) -> Result<u64, TransportErrorCode> {
        let end = off
            .checked_add(data.len() as u64)
            .ok_or(TransportErrorCode::FrameEncodingError)?;
        if let Some(f) = self.final_size
            && (end > f || (fin && end != f))
        {
            return Err(TransportErrorCode::FinalSizeError);
        }
        let before = self.highest();
        if fin && end < before {
            return Err(TransportErrorCode::FinalSizeError);
        }
        if end > self.max_data {
            return Err(TransportErrorCode::FlowControlError);
        }
        if self.reset.is_some() {
            return Ok(0);
        }
        if fin {
            self.final_size = Some(end);
        }
        // max_data <= read + capacity, so the window always holds the data.
        self.buf
            .insert(off, data)
            .map_err(|_| TransportErrorCode::FlowControlError)?;
        Ok(self.highest() - before)
    }

    /// RESET_STREAM; returns (increase of the highest offset, unread bytes
    /// released to connection flow control).
    fn on_reset(
        &mut self,
        error_code: u64,
        final_size: u64,
    ) -> Result<(u64, u64), TransportErrorCode> {
        let before = self.highest();
        if self.final_size.is_some_and(|f| f != final_size) || final_size < before {
            return Err(TransportErrorCode::FinalSizeError);
        }
        if final_size > self.max_data {
            return Err(TransportErrorCode::FlowControlError);
        }
        // Already have everything (or reset): RFC 9000 §3.2 allows ignoring it.
        if self.reset.is_some()
            || matches!(self.state(), RecvState::DataRecvd | RecvState::DataRead)
        {
            return Ok((0, 0));
        }
        self.final_size = Some(final_size);
        self.reset = Some((error_code, final_size));
        self.max_data_pending = false;
        self.stop_pending = false;
        let released = final_size - self.buf.read;
        Ok((final_size - before, released))
    }
}

// ---------------------------------------------------------------------------
// CRYPTO streams.

/// One packet-number space's CRYPTO data (RFC 9000 §19.6): ordered,
/// reliable, no flow control, bounded buffering.
#[derive(Debug)]
pub struct CryptoStream {
    send: SendBuf,
    recv: RecvBuf,
}

impl CryptoStream {
    fn new(capacity: usize) -> Self {
        Self {
            send: SendBuf::new(capacity),
            recv: RecvBuf::new(capacity),
        }
    }
    /// Queues handshake bytes; returns how many fit.
    pub fn write(&mut self, data: &[u8]) -> usize {
        self.send.write(data)
    }
    /// Reads in-order handshake bytes.
    pub fn read(&mut self, out: &mut [u8]) -> usize {
        self.recv.read(out)
    }
    pub fn readable(&self) -> u64 {
        self.recv.readable()
    }
    /// A received CRYPTO frame.
    pub fn on_frame(&mut self, offset: u64, data: &[u8]) -> Result<(), TransportErrorCode> {
        self.recv
            .insert(offset, data)
            .map_err(|_| TransportErrorCode::CryptoBufferExceeded)
    }
    pub fn has_data(&self) -> bool {
        self.send.has_lost() || self.send.unsent() > 0
    }
    /// Writes one CRYPTO frame (lost bytes first) into `out`; returns its
    /// length and the range it carries.
    pub fn write_frame(&mut self, out: &mut [u8]) -> Option<(usize, u64, usize)> {
        let room = out.len().checked_sub(crypto_header_max())?;
        if room == 0 {
            return None;
        }
        let (off, len) = match self.send.next_lost(room) {
            Some(r) => r,
            None => {
                let r = self.send.take_new(room as u64);
                if r.1 == 0 {
                    return None;
                }
                r
            }
        };
        let h = frame::encode_crypto_header(out, off, len).ok()?;
        self.send.copy_out(off, &mut out[h..h + len]);
        Some((h + len, off, len))
    }
    pub fn on_ack(&mut self, off: u64, len: usize) {
        self.send.on_ack(off, len);
    }
    pub fn on_lost(&mut self, off: u64, len: usize) {
        self.send.on_lost(off, len);
    }
    pub const fn send_buffer(&self) -> &SendBuf {
        &self.send
    }
    pub const fn recv_buffer(&self) -> &RecvBuf {
        &self.recv
    }
}

const fn crypto_header_max() -> usize {
    1 + 8 + 4
}

// ---------------------------------------------------------------------------
// Frame records.

/// An RFC 9218 stream priority. Lower urgency is sent first; streams of one
/// urgency that are not incremental are sent one at a time in stream-ID
/// order, incremental ones are interleaved round-robin after them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Priority {
    /// 0 (most urgent) to 7.
    pub urgency: u8,
    pub incremental: bool,
}

impl Priority {
    /// RFC 9218 §4: `u=3`, not incremental.
    pub const DEFAULT: Priority = Priority {
        urgency: 3,
        incremental: false,
    };
}

impl Default for Priority {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Longest NEW_TOKEN token the stream layer carries.
pub const MAX_NEW_TOKEN_LEN: usize = 128;
/// NEW_CONNECTION_ID and RETIRE_CONNECTION_ID frames queued at once.
pub const MAX_CID_FRAMES: usize = 8;

#[derive(Debug, Clone, Copy)]
struct NewCidFrame {
    seq: u64,
    retire_prior_to: u64,
    cid: [u8; 20],
    len: u8,
    token: [u8; frame::RESET_TOKEN_LEN],
    pending: bool,
}

#[derive(Debug, Clone, Copy)]
struct TokenFrame {
    bytes: [u8; MAX_NEW_TOKEN_LEN],
    len: u8,
    pending: bool,
}

/// A retransmittable frame recorded against the packet that carried it
/// (§13.3). Values that must be sent *current* are not stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SentFrame {
    Stream {
        stream_id: u64,
        offset: u64,
        len: u32,
        fin: bool,
    },
    Crypto {
        space: PnSpace,
        offset: u64,
        len: u32,
    },
    ResetStream {
        stream_id: u64,
    },
    StopSending {
        stream_id: u64,
    },
    MaxData,
    MaxStreamData {
        stream_id: u64,
    },
    MaxStreams {
        uni: bool,
    },
    DataBlocked {
        limit: u64,
    },
    StreamDataBlocked {
        stream_id: u64,
        limit: u64,
    },
    StreamsBlocked {
        uni: bool,
        limit: u64,
    },
    /// HANDSHAKE_DONE (server only, RFC 9001 §4.1.2): resent until
    /// acknowledged.
    HandshakeDone,
    /// NEW_CONNECTION_ID with this sequence number (wave six): resent
    /// until acknowledged.
    NewConnectionId {
        seq: u64,
    },
    /// RETIRE_CONNECTION_ID: resent until acknowledged.
    RetireConnectionId {
        seq: u64,
    },
    /// NEW_TOKEN (server): resent until acknowledged or replaced.
    NewToken,
}

/// Most retransmittable frames one packet carries.
pub const MAX_FRAMES_PER_PACKET: usize = 16;

/// The frames of one packet being built.
#[derive(Debug, Clone, Copy)]
pub struct PacketFrames {
    frames: [SentFrame; MAX_FRAMES_PER_PACKET],
    len: usize,
}

impl Default for PacketFrames {
    fn default() -> Self {
        Self {
            frames: [SentFrame::MaxData; MAX_FRAMES_PER_PACKET],
            len: 0,
        }
    }
}

impl PacketFrames {
    pub fn frames(&self) -> &[SentFrame] {
        &self.frames[..self.len]
    }
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
    const fn is_full(&self) -> bool {
        self.len == MAX_FRAMES_PER_PACKET
    }
    fn push(&mut self, f: SentFrame) {
        self.frames[self.len] = f;
        self.len += 1;
    }
    pub fn clear(&mut self) {
        self.len = 0;
    }
}

// ---------------------------------------------------------------------------
// Connection stream layer.

/// What happened on a stream, for the application to act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamEvent {
    /// The peer opened this stream.
    Opened(u64),
    /// New in-order data (or the end of the stream) can be read.
    Readable(u64),
    /// Buffer space or credit opened up after a short write.
    Writable(u64),
    /// The peer reset its sending part (RESET_STREAM).
    Reset { stream_id: u64, error_code: u64 },
    /// The peer asked us to stop sending (STOP_SENDING); the sending part
    /// was reset with the same code.
    StopSending { stream_id: u64, error_code: u64 },
    /// Every byte we sent, and the FIN, was acknowledged.
    Finished(u64),
}

/// Result of a read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadOutcome {
    pub len: usize,
    /// The final byte was read (the stream's receiving part is done).
    pub fin: bool,
}

/// Stream-layer counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamStats {
    pub streams_opened_local: u64,
    pub streams_opened_remote: u64,
    pub streams_closed: u64,
    pub stream_bytes_sent: u64,
    pub stream_bytes_retransmitted: u64,
    pub stream_bytes_received: u64,
    pub stream_frames_lost: u64,
    pub crypto_bytes_sent: u64,
    pub control_frames_sent: u64,
    pub control_frames_lost: u64,
    pub blocked_frames_sent: u64,
    pub packets_acked: u64,
    pub packets_lost: u64,
    /// Frame records evicted because `tracked_frames` was reached; their
    /// frames were treated as lost.
    pub records_evicted: u64,
    pub events_dropped: u64,
}

struct Stream {
    id: u64,
    send: Option<SendStream>,
    recv: Option<RecvStream>,
    priority: Priority,
}

const BIDI: usize = 0;
const UNI: usize = 1;

/// One connection's streams. See the module docs.
pub struct StreamSet {
    role: Role,
    cfg: StreamConfig,
    peer: PeerParams,
    streams: Vec<Stream>,
    send_pool: Vec<SendBuf>,
    recv_pool: Vec<RecvBuf>,
    /// Streams we opened, per kind (next index), and the peer's MAX_STREAMS.
    local_opened: [u64; 2],
    local_open_now: [u64; 2],
    peer_max_streams: [u64; 2],
    streams_blocked_pending: [bool; 2],
    streams_blocked_at: [Option<u64>; 2],
    /// Streams the peer opened, per kind, and our MAX_STREAMS for them.
    remote_opened: [u64; 2],
    local_max_streams: [u64; 2],
    max_streams_pending: [bool; 2],
    /// Connection flow control, sending: the peer's MAX_DATA and the sum
    /// of our streams' highest sent offsets.
    tx_max_data: u64,
    tx_sent: u64,
    data_blocked_at: Option<u64>,
    data_blocked_pending: bool,
    /// Receiving: our MAX_DATA, the sum of the peer's highest offsets, and
    /// the bytes the application consumed (read or reset away).
    rx_max_data: u64,
    rx_highest: u64,
    rx_consumed: u64,
    max_data_pending: bool,
    /// HANDSHAKE_DONE queued by the handshake layer, and acknowledged.
    handshake_done_pending: bool,
    handshake_done_acked: bool,
    crypto: [CryptoStream; 3],
    /// (packet number, frame) in packet-number order.
    sent: VecDeque<(u64, SentFrame)>,
    events: VecDeque<StreamEvent>,
    /// Round-robin cursor over `streams` for incremental data.
    rr: usize,
    stats: StreamStats,
    /// Connection-level control frames carried by the stream layer's
    /// retransmission machinery (wave six, issue #247).
    new_cids: [Option<NewCidFrame>; MAX_CID_FRAMES],
    retire_cids: [Option<(u64, bool)>; MAX_CID_FRAMES],
    new_token: Option<TokenFrame>,
}

impl std::fmt::Debug for StreamSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamSet")
            .field("role", &self.role)
            .field("streams", &self.streams.len())
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

fn kind(uni: bool) -> usize {
    if uni { UNI } else { BIDI }
}

impl StreamSet {
    pub fn new(role: Role, cfg: StreamConfig, peer: PeerParams) -> Result<Self, &'static str> {
        cfg.validate()?;
        let slots = 2 * (cfg.max_streams_bidi + cfg.max_streams_uni) as usize;
        let crypto = || CryptoStream::new(cfg.crypto_buffer as usize);
        Ok(Self {
            role,
            cfg,
            peer,
            streams: Vec::with_capacity(slots),
            send_pool: Vec::with_capacity(slots),
            recv_pool: Vec::with_capacity(slots),
            local_opened: [0; 2],
            local_open_now: [0; 2],
            peer_max_streams: [peer.initial_max_streams_bidi, peer.initial_max_streams_uni],
            streams_blocked_pending: [false; 2],
            streams_blocked_at: [None; 2],
            remote_opened: [0; 2],
            local_max_streams: [cfg.max_streams_bidi, cfg.max_streams_uni],
            max_streams_pending: [false; 2],
            tx_max_data: peer.initial_max_data,
            tx_sent: 0,
            data_blocked_at: None,
            data_blocked_pending: false,
            rx_max_data: cfg.conn_window,
            rx_highest: 0,
            rx_consumed: 0,
            max_data_pending: false,
            handshake_done_pending: false,
            handshake_done_acked: false,
            crypto: [crypto(), crypto(), crypto()],
            sent: VecDeque::with_capacity(cfg.tracked_frames),
            events: VecDeque::with_capacity(cfg.event_capacity),
            rr: 0,
            stats: StreamStats::default(),
            new_cids: [None; MAX_CID_FRAMES],
            retire_cids: [None; MAX_CID_FRAMES],
            new_token: None,
        })
    }

    pub const fn role(&self) -> Role {
        self.role
    }
    pub const fn config(&self) -> &StreamConfig {
        &self.cfg
    }
    pub const fn stats(&self) -> StreamStats {
        self.stats
    }
    /// Live streams.
    pub fn len(&self) -> usize {
        self.streams.len()
    }
    pub fn is_empty(&self) -> bool {
        self.streams.is_empty()
    }
    /// The peer's MAX_DATA and the bytes we have counted against it.
    pub const fn send_credit(&self) -> (u64, u64) {
        (self.tx_max_data, self.tx_sent)
    }
    /// Our MAX_DATA and the peer's bytes counted against it.
    pub const fn recv_credit(&self) -> (u64, u64) {
        (self.rx_max_data, self.rx_highest)
    }
    /// The peer's MAX_STREAMS for (bidi, uni) and our MAX_STREAMS for it.
    pub const fn stream_limits(&self) -> ([u64; 2], [u64; 2]) {
        (self.peer_max_streams, self.local_max_streams)
    }
    /// Frame records held for packets in flight.
    pub fn tracked_frames(&self) -> usize {
        self.sent.len()
    }
    pub fn crypto(&self, space: PnSpace) -> &CryptoStream {
        &self.crypto[space.index()]
    }
    /// The CRYPTO stream of a packet-number space, for the TLS layer.
    pub fn crypto_mut(&mut self, space: PnSpace) -> &mut CryptoStream {
        &mut self.crypto[space.index()]
    }
    pub fn send_state(&self, id: u64) -> Option<SendState> {
        self.find(id)
            .and_then(|i| self.streams[i].send.as_ref().map(SendStream::state))
    }
    pub fn recv_state(&self, id: u64) -> Option<RecvState> {
        self.find(id)
            .and_then(|i| self.streams[i].recv.as_ref().map(RecvStream::state))
    }
    pub fn send_stream(&self, id: u64) -> Option<&SendStream> {
        self.find(id).and_then(|i| self.streams[i].send.as_ref())
    }
    pub fn recv_stream(&self, id: u64) -> Option<&RecvStream> {
        self.find(id).and_then(|i| self.streams[i].recv.as_ref())
    }
    pub fn poll_event(&mut self) -> Option<StreamEvent> {
        self.events.pop_front()
    }

    fn find(&self, id: u64) -> Option<usize> {
        self.streams.iter().position(|s| s.id == id)
    }

    fn is_local(&self, id: u64) -> bool {
        id & 1 == self.role.initiator_bit()
    }

    fn event(&mut self, ev: StreamEvent) {
        if self.events.len() < self.events.capacity() {
            self.events.push_back(ev);
        } else {
            self.stats.events_dropped += 1;
        }
    }

    fn take_send_buf(&mut self) -> SendBuf {
        self.send_pool
            .pop()
            .unwrap_or_else(|| SendBuf::new(self.cfg.send_buffer as usize))
    }
    fn take_recv_buf(&mut self) -> RecvBuf {
        self.recv_pool
            .pop()
            .unwrap_or_else(|| RecvBuf::new(self.cfg.stream_window as usize))
    }

    fn create(&mut self, id: u64) {
        let local = self.is_local(id);
        let uni = is_uni(id);
        let send = if !uni || local {
            let credit = match (local, uni) {
                (true, false) => self.peer.initial_max_stream_data_bidi_remote,
                (true, true) => self.peer.initial_max_stream_data_uni,
                (false, _) => self.peer.initial_max_stream_data_bidi_local,
            };
            Some(SendStream::new(self.take_send_buf(), credit))
        } else {
            None
        };
        let recv = if !uni || !local {
            Some(RecvStream::new(self.take_recv_buf()))
        } else {
            None
        };
        self.streams.push(Stream {
            id,
            send,
            recv,
            priority: Priority::DEFAULT,
        });
    }

    /// Sets a stream's RFC 9218 priority (any stream with a send half).
    pub fn set_priority(&mut self, id: u64, priority: Priority) -> Result<(), StreamError> {
        if priority.urgency > 7 {
            return Err(StreamError::BadPriority(priority.urgency));
        }
        let i = self.lookup_local_api(id)?;
        if self.streams[i].send.is_none() {
            return Err(StreamError::NotWritable(id));
        }
        self.streams[i].priority = priority;
        Ok(())
    }

    /// A stream's priority.
    pub fn priority(&self, id: u64) -> Option<Priority> {
        self.find(id).map(|i| self.streams[i].priority)
    }

    // -- connection-level control frames (wave six) --------------------------

    /// Queues NEW_CONNECTION_ID (RFC 9000 §19.15); false when
    /// [`MAX_CID_FRAMES`] are already outstanding or the ID is invalid.
    pub fn queue_new_connection_id(
        &mut self,
        seq: u64,
        retire_prior_to: u64,
        cid: &[u8],
        reset_token: [u8; frame::RESET_TOKEN_LEN],
    ) -> bool {
        if cid.is_empty() || cid.len() > 20 || retire_prior_to > seq {
            return false;
        }
        let Some(slot) = self.new_cids.iter_mut().find(|f| f.is_none()) else {
            return false;
        };
        let mut bytes = [0u8; 20];
        bytes[..cid.len()].copy_from_slice(cid);
        *slot = Some(NewCidFrame {
            seq,
            retire_prior_to,
            cid: bytes,
            len: cid.len() as u8,
            token: reset_token,
            pending: true,
        });
        true
    }

    /// Queues RETIRE_CONNECTION_ID (RFC 9000 §19.16); false when full.
    pub fn queue_retire_connection_id(&mut self, seq: u64) -> bool {
        if self.retire_cids.iter().flatten().any(|(s, _)| *s == seq) {
            return true;
        }
        let Some(slot) = self.retire_cids.iter_mut().find(|f| f.is_none()) else {
            return false;
        };
        *slot = Some((seq, true));
        true
    }

    /// Queues NEW_TOKEN (RFC 9000 §19.7), replacing an unacknowledged one;
    /// false for an empty or oversized token.
    pub fn queue_new_token(&mut self, token: &[u8]) -> bool {
        if token.is_empty() || token.len() > MAX_NEW_TOKEN_LEN {
            return false;
        }
        let mut bytes = [0u8; MAX_NEW_TOKEN_LEN];
        bytes[..token.len()].copy_from_slice(token);
        self.new_token = Some(TokenFrame {
            bytes,
            len: token.len() as u8,
            pending: true,
        });
        true
    }

    /// Whether queued connection-ID or token frames are unacknowledged.
    pub fn control_outstanding(&self) -> bool {
        self.new_cids.iter().any(Option::is_some)
            || self.retire_cids.iter().any(Option::is_some)
            || self.new_token.is_some()
    }

    // -- 0-RTT (wave six) ----------------------------------------------------

    /// 0-RTT was rejected (RFC 9000 §7.4.1): every stream is forgotten and
    /// the connection starts its stream state afresh under the server's
    /// real transport parameters. CRYPTO buffers and HANDSHAKE_DONE state
    /// are kept.
    pub fn reset_streams(&mut self, peer: PeerParams) -> Result<(), &'static str> {
        if peer.initial_max_streams_bidi > frame::MAX_STREAM_COUNT
            || peer.initial_max_streams_uni > frame::MAX_STREAM_COUNT
        {
            return Err("peer stream limits out of range");
        }
        for s in self.streams.drain(..) {
            if let Some(mut t) = s.send {
                t.buf.reset_for_reuse();
                self.send_pool.push(t.buf);
            }
            if let Some(mut r) = s.recv {
                r.buf.reset_for_reuse();
                self.recv_pool.push(r.buf);
            }
        }
        self.sent
            .retain(|(_, f)| matches!(f, SentFrame::Crypto { .. } | SentFrame::HandshakeDone));
        self.events.clear();
        self.peer = peer;
        self.local_opened = [0; 2];
        self.local_open_now = [0; 2];
        self.peer_max_streams = [peer.initial_max_streams_bidi, peer.initial_max_streams_uni];
        self.streams_blocked_pending = [false; 2];
        self.streams_blocked_at = [None; 2];
        self.remote_opened = [0; 2];
        self.local_max_streams = [self.cfg.max_streams_bidi, self.cfg.max_streams_uni];
        self.max_streams_pending = [false; 2];
        self.tx_max_data = peer.initial_max_data;
        self.tx_sent = 0;
        self.data_blocked_at = None;
        self.data_blocked_pending = false;
        self.rx_max_data = self.cfg.conn_window;
        self.rx_highest = 0;
        self.rx_consumed = 0;
        self.max_data_pending = false;
        self.rr = 0;
        Ok(())
    }

    /// 0-RTT was accepted: the server's real transport parameters replace
    /// the remembered ones, which they may not reduce (RFC 9000 §7.4.1).
    /// Credit only grows, for open streams too.
    pub fn raise_peer_params(&mut self, peer: PeerParams) -> Result<(), &'static str> {
        let old = self.peer;
        if peer.initial_max_data < old.initial_max_data
            || peer.initial_max_stream_data_bidi_local < old.initial_max_stream_data_bidi_local
            || peer.initial_max_stream_data_bidi_remote < old.initial_max_stream_data_bidi_remote
            || peer.initial_max_stream_data_uni < old.initial_max_stream_data_uni
            || peer.initial_max_streams_bidi < old.initial_max_streams_bidi
            || peer.initial_max_streams_uni < old.initial_max_streams_uni
        {
            return Err("the server reduced a remembered transport parameter");
        }
        if peer.initial_max_streams_bidi > frame::MAX_STREAM_COUNT
            || peer.initial_max_streams_uni > frame::MAX_STREAM_COUNT
        {
            return Err("peer stream limits out of range");
        }
        self.peer = peer;
        self.peer_max_streams[BIDI] =
            self.peer_max_streams[BIDI].max(peer.initial_max_streams_bidi);
        self.peer_max_streams[UNI] = self.peer_max_streams[UNI].max(peer.initial_max_streams_uni);
        self.tx_max_data = self.tx_max_data.max(peer.initial_max_data);
        let role_bit = self.role.initiator_bit();
        for s in &mut self.streams {
            let local = s.id & 1 == role_bit;
            let credit = match (local, is_uni(s.id)) {
                (true, false) => peer.initial_max_stream_data_bidi_remote,
                (true, true) => peer.initial_max_stream_data_uni,
                (false, _) => peer.initial_max_stream_data_bidi_local,
            };
            if let Some(t) = s.send.as_mut() {
                t.max_data = t.max_data.max(credit);
            }
        }
        Ok(())
    }

    /// Opens a stream of our own (RFC 9000 §2.1); returns its ID.
    pub fn open(&mut self, uni: bool) -> Result<u64, StreamError> {
        let k = kind(uni);
        let ours = if uni {
            self.cfg.max_streams_uni
        } else {
            self.cfg.max_streams_bidi
        };
        if self.local_opened[k] >= self.peer_max_streams[k] {
            if self.streams_blocked_at[k] != Some(self.peer_max_streams[k]) {
                self.streams_blocked_at[k] = Some(self.peer_max_streams[k]);
                self.streams_blocked_pending[k] = true;
            }
            return Err(StreamError::StreamLimit { uni });
        }
        if self.local_open_now[k] >= ours {
            return Err(StreamError::StreamLimit { uni });
        }
        let id = stream_id(self.role, uni, self.local_opened[k]);
        self.local_opened[k] += 1;
        self.local_open_now[k] += 1;
        self.stats.streams_opened_local += 1;
        self.create(id);
        Ok(id)
    }

    fn lookup_local_api(&self, id: u64) -> Result<usize, StreamError> {
        self.find(id).ok_or(StreamError::UnknownStream(id))
    }

    /// Writes as much of `data` as the send buffer holds; returns the
    /// count. Flow control applies when the data is sent, not here.
    pub fn write(&mut self, id: u64, data: &[u8]) -> Result<usize, StreamError> {
        let i = self.lookup_local_api(id)?;
        let s = self.streams[i]
            .send
            .as_mut()
            .ok_or(StreamError::NotWritable(id))?;
        if let Some((code, _)) = s.reset {
            return Err(StreamError::Reset {
                stream_id: id,
                error_code: code,
            });
        }
        if s.fin.is_some() {
            return Err(StreamError::Finished(id));
        }
        let n = s.buf.write(data);
        if n < data.len() {
            s.want_writable = true;
        }
        Ok(n)
    }

    /// Bytes `write` would accept now.
    pub fn write_space(&self, id: u64) -> Result<usize, StreamError> {
        let i = self.lookup_local_api(id)?;
        let s = self.streams[i]
            .send
            .as_ref()
            .ok_or(StreamError::NotWritable(id))?;
        Ok(if s.reset.is_some() || s.fin.is_some() {
            0
        } else {
            s.buf.free_space()
        })
    }

    /// Ends the sending part after the bytes written so far (FIN).
    pub fn finish(&mut self, id: u64) -> Result<(), StreamError> {
        let i = self.lookup_local_api(id)?;
        let s = self.streams[i]
            .send
            .as_mut()
            .ok_or(StreamError::NotWritable(id))?;
        if let Some((code, _)) = s.reset {
            return Err(StreamError::Reset {
                stream_id: id,
                error_code: code,
            });
        }
        if s.fin.is_none() {
            s.fin = Some(s.buf.end);
        }
        Ok(())
    }

    /// Abandons the sending part with RESET_STREAM (§3.1).
    pub fn reset(&mut self, id: u64, error_code: u64) -> Result<(), StreamError> {
        if error_code > varint::MAX {
            return Err(StreamError::BadErrorCode);
        }
        let i = self.lookup_local_api(id)?;
        let s = self.streams[i]
            .send
            .as_mut()
            .ok_or(StreamError::NotWritable(id))?;
        s.reset_now(error_code);
        self.maybe_retire(i);
        Ok(())
    }

    /// Asks the peer to stop sending (STOP_SENDING, §3.5).
    pub fn stop_sending(&mut self, id: u64, error_code: u64) -> Result<(), StreamError> {
        if error_code > varint::MAX {
            return Err(StreamError::BadErrorCode);
        }
        let i = self.lookup_local_api(id)?;
        let r = self.streams[i]
            .recv
            .as_mut()
            .ok_or(StreamError::NotReadable(id))?;
        if r.stop.is_none() && matches!(r.state(), RecvState::Recv | RecvState::SizeKnown) {
            r.stop = Some(error_code);
            r.stop_pending = true;
        }
        Ok(())
    }

    /// Reads in-order data. A reset stream reports its code once and is
    /// then done.
    pub fn read(&mut self, id: u64, out: &mut [u8]) -> Result<ReadOutcome, StreamError> {
        let i = self.lookup_local_api(id)?;
        let window = u64::from(self.cfg.stream_window);
        let r = self.streams[i]
            .recv
            .as_mut()
            .ok_or(StreamError::NotReadable(id))?;
        r.readable_signalled = false;
        if let Some((code, _)) = r.reset {
            r.reset_read = true;
            self.maybe_retire(i);
            return Err(StreamError::Reset {
                stream_id: id,
                error_code: code,
            });
        }
        let n = r.buf.read(out);
        let fin = r.final_size == Some(r.buf.read);
        if fin {
            r.all_read = true;
        }
        // Stream credit: re-advertise once half the window was consumed.
        if r.final_size.is_none() && r.buf.read + window - r.max_data >= window / 2 {
            r.max_data = r.buf.read + window;
            r.max_data_pending = true;
        }
        self.consume(n as u64);
        if fin {
            self.maybe_retire(i);
        }
        Ok(ReadOutcome { len: n, fin })
    }

    /// Bytes ready to read on a stream.
    pub fn readable(&self, id: u64) -> Result<u64, StreamError> {
        let i = self.lookup_local_api(id)?;
        let r = self.streams[i]
            .recv
            .as_ref()
            .ok_or(StreamError::NotReadable(id))?;
        Ok(r.buf.readable())
    }

    /// Connection credit: re-advertise once half the window was consumed.
    fn consume(&mut self, n: u64) {
        self.rx_consumed += n;
        let w = self.cfg.conn_window;
        if self.rx_consumed + w - self.rx_max_data >= w / 2 {
            self.rx_max_data = self.rx_consumed + w;
            self.max_data_pending = true;
        }
    }

    /// Forgets a stream whose parts are both done (§3.3) and returns its
    /// buffers to the pools; a peer stream's closing raises our MAX_STREAMS.
    fn maybe_retire(&mut self, i: usize) {
        let s = &self.streams[i];
        let send_done = s.send.as_ref().is_none_or(SendStream::done);
        let recv_done = s.recv.as_ref().is_none_or(RecvStream::done);
        if !(send_done && recv_done) {
            return;
        }
        let s = self.streams.swap_remove(i);
        if self.rr > i {
            self.rr -= 1;
        }
        let k = kind(is_uni(s.id));
        if self.is_local(s.id) {
            self.local_open_now[k] -= 1;
        } else {
            self.local_max_streams[k] += 1;
            self.max_streams_pending[k] = true;
        }
        if let Some(mut send) = s.send {
            send.buf.reset_for_reuse();
            self.send_pool.push(send.buf);
        }
        if let Some(mut recv) = s.recv {
            recv.buf.reset_for_reuse();
            self.recv_pool.push(recv.buf);
        }
        self.stats.streams_closed += 1;
    }

    // -- receiving frames ---------------------------------------------------

    /// The stream a peer frame names, opening peer streams implicitly
    /// (§2.1, §3.2). `Ok(None)`: a stream already closed, so the frame is
    /// a late retransmission and ignored.
    fn stream_for_frame(
        &mut self,
        id: u64,
        wants_send: bool,
        frame_type: u64,
    ) -> Result<Option<usize>, TransportError> {
        let err = |code| TransportError { code, frame_type };
        let uni = is_uni(id);
        let k = kind(uni);
        let index = id >> 2;
        if self.is_local(id) {
            if index >= self.local_opened[k] || (uni && !wants_send) {
                return Err(err(TransportErrorCode::StreamStateError));
            }
        } else {
            if uni && wants_send {
                return Err(err(TransportErrorCode::StreamStateError));
            }
            if index >= self.local_max_streams[k] {
                return Err(err(TransportErrorCode::StreamLimitError));
            }
            while self.remote_opened[k] <= index {
                let new = stream_id(
                    match self.role {
                        Role::Client => Role::Server,
                        Role::Server => Role::Client,
                    },
                    uni,
                    self.remote_opened[k],
                );
                self.remote_opened[k] += 1;
                self.stats.streams_opened_remote += 1;
                self.create(new);
                self.event(StreamEvent::Opened(new));
            }
        }
        Ok(self.find(id))
    }

    /// Applies one received frame. Frames this layer does not own are
    /// ignored.
    pub fn on_frame(&mut self, f: &Frame<'_>) -> Result<(), TransportError> {
        let frame_type = f.frame_type();
        let err = |code| TransportError { code, frame_type };
        match *f {
            Frame::Stream(s) => {
                let Some(i) = self.stream_for_frame(s.stream_id, false, frame_type)? else {
                    return Ok(());
                };
                let r = self.streams[i]
                    .recv
                    .as_mut()
                    .expect("checked by stream_for_frame");
                let grew = r.on_data(s.offset, s.data, s.fin).map_err(err)?;
                self.stats.stream_bytes_received += s.data.len() as u64;
                self.rx_highest += grew;
                if self.rx_highest > self.rx_max_data {
                    return Err(err(TransportErrorCode::FlowControlError));
                }
                let r = self.streams[i].recv.as_mut().expect("present");
                let ready = r.buf.readable() > 0 || r.state() == RecvState::DataRecvd;
                if ready && !r.readable_signalled && r.reset.is_none() {
                    r.readable_signalled = true;
                    let id = s.stream_id;
                    self.event(StreamEvent::Readable(id));
                }
            }
            Frame::ResetStream {
                stream_id,
                error_code,
                final_size,
            } => {
                let Some(i) = self.stream_for_frame(stream_id, false, frame_type)? else {
                    return Ok(());
                };
                let r = self.streams[i].recv.as_mut().expect("checked");
                let was_reset = r.reset.is_some();
                let (grew, released) = r.on_reset(error_code, final_size).map_err(err)?;
                self.rx_highest += grew;
                if self.rx_highest > self.rx_max_data {
                    return Err(err(TransportErrorCode::FlowControlError));
                }
                let r = self.streams[i].recv.as_ref().expect("present");
                if !was_reset && r.reset.is_some() {
                    self.consume(released);
                    self.event(StreamEvent::Reset {
                        stream_id,
                        error_code,
                    });
                }
            }
            Frame::StopSending {
                stream_id,
                error_code,
            } => {
                let Some(i) = self.stream_for_frame(stream_id, true, frame_type)? else {
                    return Ok(());
                };
                let s = self.streams[i].send.as_mut().expect("checked");
                if s.reset.is_none() && s.state() != SendState::DataRecvd {
                    s.reset_now(error_code);
                    self.event(StreamEvent::StopSending {
                        stream_id,
                        error_code,
                    });
                }
            }
            Frame::MaxStreamData { stream_id, max } => {
                let Some(i) = self.stream_for_frame(stream_id, true, frame_type)? else {
                    return Ok(());
                };
                let s = self.streams[i].send.as_mut().expect("checked");
                if max > s.max_data {
                    s.max_data = max;
                    if s.want_writable && s.reset.is_none() {
                        s.want_writable = false;
                        self.event(StreamEvent::Writable(stream_id));
                    }
                }
            }
            Frame::StreamDataBlocked { stream_id, .. } => {
                // Informational (§4.1); it still opens and checks the stream.
                self.stream_for_frame(stream_id, false, frame_type)?;
            }
            Frame::MaxData(max) => self.tx_max_data = self.tx_max_data.max(max),
            Frame::MaxStreams { uni, max } => {
                let k = kind(uni);
                self.peer_max_streams[k] = self.peer_max_streams[k].max(max);
            }
            Frame::DataBlocked(_) | Frame::StreamsBlocked { .. } => {}
            Frame::Crypto { offset, data } => {
                // In this endpoint only 1-RTT packets are processed.
                self.crypto[PnSpace::ApplicationData.index()]
                    .on_frame(offset, data)
                    .map_err(err)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// The peer's transport parameters as the handshake negotiated them
    /// (RFC 9000 §7.4), replacing the ones given at construction. Limits
    /// only grow, as if MAX_DATA and MAX_STREAMS had arrived; it is refused
    /// once a stream exists, since per-stream credit was already taken
    /// from the old values.
    pub fn set_peer_params(&mut self, peer: PeerParams) -> Result<(), &'static str> {
        if peer.initial_max_streams_bidi > frame::MAX_STREAM_COUNT
            || peer.initial_max_streams_uni > frame::MAX_STREAM_COUNT
        {
            return Err("peer stream limits out of range");
        }
        if !self.streams.is_empty() || self.local_opened != [0; 2] || self.remote_opened != [0; 2] {
            return Err("peer transport parameters after streams were opened");
        }
        self.peer = peer;
        self.peer_max_streams[BIDI] =
            self.peer_max_streams[BIDI].max(peer.initial_max_streams_bidi);
        self.peer_max_streams[UNI] = self.peer_max_streams[UNI].max(peer.initial_max_streams_uni);
        self.tx_max_data = self.tx_max_data.max(peer.initial_max_data);
        Ok(())
    }

    /// The peer's transport parameters in force.
    pub const fn peer_params(&self) -> &PeerParams {
        &self.peer
    }

    /// Queues a HANDSHAKE_DONE frame (server, RFC 9001 §4.1.2). It goes
    /// out with the next 1-RTT packet and is resent until acknowledged.
    pub fn queue_handshake_done(&mut self) {
        if !self.handshake_done_acked {
            self.handshake_done_pending = true;
        }
    }

    /// Whether the peer acknowledged our HANDSHAKE_DONE.
    pub const fn handshake_done_acked(&self) -> bool {
        self.handshake_done_acked
    }

    /// A CRYPTO frame received in `space` (for the handshake layer).
    pub fn on_crypto_frame(
        &mut self,
        space: PnSpace,
        offset: u64,
        data: &[u8],
    ) -> Result<(), TransportError> {
        self.crypto[space.index()]
            .on_frame(offset, data)
            .map_err(|code| TransportError {
                code,
                frame_type: 0x06,
            })
    }

    // -- building packets ---------------------------------------------------

    /// Queues *_BLOCKED frames for data that waits on the peer's credit.
    fn detect_blocked(&mut self) {
        let mut conn_blocked = false;
        for s in &mut self.streams {
            let Some(send) = s.send.as_mut() else {
                continue;
            };
            if send.reset.is_some() || send.buf.unsent() == 0 {
                continue;
            }
            if send.credit() == 0 {
                if send.blocked_at != Some(send.max_data) {
                    send.blocked_at = Some(send.max_data);
                    send.blocked_pending = true;
                }
            } else if self.tx_sent >= self.tx_max_data {
                conn_blocked = true;
            }
        }
        if conn_blocked && self.data_blocked_at != Some(self.tx_max_data) {
            self.data_blocked_at = Some(self.tx_max_data);
            self.data_blocked_pending = true;
        }
    }

    /// Whether [`StreamSet::fill`] has anything to send.
    pub fn has_pending(&mut self) -> bool {
        self.detect_blocked();
        let conn_credit = self.tx_max_data.saturating_sub(self.tx_sent);
        self.max_data_pending
            || self.handshake_done_pending
            || self.new_cids.iter().flatten().any(|f| f.pending)
            || self.retire_cids.iter().flatten().any(|(_, p)| *p)
            || self.new_token.is_some_and(|t| t.pending)
            || self.data_blocked_pending
            || self.max_streams_pending.iter().any(|p| *p)
            || self.streams_blocked_pending.iter().any(|p| *p)
            || self.crypto[PnSpace::ApplicationData.index()].has_data()
            || self.streams.iter().any(|s| {
                s.send.as_ref().is_some_and(|t| {
                    t.reset_pending || t.blocked_pending || t.has_data(conn_credit)
                }) || s
                    .recv
                    .as_ref()
                    .is_some_and(|r| r.max_data_pending || r.stop_pending)
            })
    }

    /// Writes control frames, 1-RTT CRYPTO data and stream data (lost
    /// ranges first, then new data under both credits, round-robin across
    /// streams) into `out`; records them in `record`. Returns the length.
    /// Call [`StreamSet::commit`] with the packet number once the packet is
    /// sent, or [`StreamSet::requeue`] if it is not.
    pub fn fill(&mut self, out: &mut [u8], record: &mut PacketFrames) -> usize {
        record.clear();
        self.detect_blocked();
        let mut at = 0usize;
        // Control frames with their current values (§13.3).
        macro_rules! control {
            ($frame:expr, $sent:expr) => {{
                if record.is_full() {
                    return at;
                }
                match $frame.encode(&mut out[at..]) {
                    Ok(n) => {
                        at += n;
                        record.push($sent);
                        self.stats.control_frames_sent += 1;
                        true
                    }
                    Err(_) => return at,
                }
            }};
        }
        if self.handshake_done_pending && control!(Frame::HandshakeDone, SentFrame::HandshakeDone) {
            self.handshake_done_pending = false;
        }
        if self.max_data_pending && control!(Frame::MaxData(self.rx_max_data), SentFrame::MaxData) {
            self.max_data_pending = false;
        }
        for i in 0..MAX_CID_FRAMES {
            if let Some(f) = self.new_cids[i]
                && f.pending
                && control!(
                    Frame::NewConnectionId {
                        seq: f.seq,
                        retire_prior_to: f.retire_prior_to,
                        cid: &f.cid[..usize::from(f.len)],
                        reset_token: &f.token,
                    },
                    SentFrame::NewConnectionId { seq: f.seq }
                )
            {
                self.new_cids[i].as_mut().expect("present").pending = false;
            }
            if let Some((seq, true)) = self.retire_cids[i]
                && control!(
                    Frame::RetireConnectionId { seq },
                    SentFrame::RetireConnectionId { seq }
                )
            {
                self.retire_cids[i] = Some((seq, false));
            }
        }
        if let Some(t) = self.new_token
            && t.pending
            && control!(
                Frame::NewToken {
                    token: &t.bytes[..usize::from(t.len)]
                },
                SentFrame::NewToken
            )
        {
            self.new_token.as_mut().expect("present").pending = false;
        }
        for uni in [false, true] {
            let k = kind(uni);
            if self.max_streams_pending[k]
                && control!(
                    Frame::MaxStreams {
                        uni,
                        max: self.local_max_streams[k]
                    },
                    SentFrame::MaxStreams { uni }
                )
            {
                self.max_streams_pending[k] = false;
            }
            if self.streams_blocked_pending[k] {
                let limit = self.peer_max_streams[k];
                if control!(
                    Frame::StreamsBlocked { uni, limit },
                    SentFrame::StreamsBlocked { uni, limit }
                ) {
                    self.streams_blocked_pending[k] = false;
                    self.stats.blocked_frames_sent += 1;
                }
            }
        }
        if self.data_blocked_pending {
            let limit = self.tx_max_data;
            if control!(Frame::DataBlocked(limit), SentFrame::DataBlocked { limit }) {
                self.data_blocked_pending = false;
                self.stats.blocked_frames_sent += 1;
            }
        }
        for i in 0..self.streams.len() {
            let id = self.streams[i].id;
            if let Some(s) = self.streams[i].send.as_ref()
                && s.reset_pending
                && let Some((error_code, final_size)) = s.reset
                && control!(
                    Frame::ResetStream {
                        stream_id: id,
                        error_code,
                        final_size
                    },
                    SentFrame::ResetStream { stream_id: id }
                )
            {
                self.streams[i]
                    .send
                    .as_mut()
                    .expect("present")
                    .reset_pending = false;
            }
            if let Some(s) = self.streams[i].send.as_ref()
                && s.blocked_pending
            {
                let limit = s.max_data;
                if control!(
                    Frame::StreamDataBlocked {
                        stream_id: id,
                        limit
                    },
                    SentFrame::StreamDataBlocked {
                        stream_id: id,
                        limit
                    }
                ) {
                    self.streams[i]
                        .send
                        .as_mut()
                        .expect("present")
                        .blocked_pending = false;
                    self.stats.blocked_frames_sent += 1;
                }
            }
            if let Some(r) = self.streams[i].recv.as_ref()
                && r.stop_pending
                && let Some(error_code) = r.stop
                && control!(
                    Frame::StopSending {
                        stream_id: id,
                        error_code
                    },
                    SentFrame::StopSending { stream_id: id }
                )
            {
                self.streams[i].recv.as_mut().expect("present").stop_pending = false;
            }
            if let Some(r) = self.streams[i].recv.as_ref()
                && r.max_data_pending
                && control!(
                    Frame::MaxStreamData {
                        stream_id: id,
                        max: r.max_data
                    },
                    SentFrame::MaxStreamData { stream_id: id }
                )
            {
                self.streams[i]
                    .recv
                    .as_mut()
                    .expect("present")
                    .max_data_pending = false;
            }
        }
        // 1-RTT CRYPTO data (post-handshake messages).
        while !record.is_full() {
            let c = &mut self.crypto[PnSpace::ApplicationData.index()];
            let Some((n, offset, len)) = c.write_frame(&mut out[at..]) else {
                break;
            };
            at += n;
            self.stats.crypto_bytes_sent += len as u64;
            record.push(SentFrame::Crypto {
                space: PnSpace::ApplicationData,
                offset,
                len: len as u32,
            });
        }
        // Stream data in RFC 9218 priority order.
        let count = self.streams.len();
        if count == 0 {
            return at;
        }
        // Streams that had data but could not write a frame this packet
        // (bounded: one bit per stream, at most 64 tracked; beyond that the
        // loop stops at the first refusal).
        let mut refused: u64 = 0;
        while !record.is_full() && out.len() - at >= 8 {
            let Some(i) = self.next_stream(refused) else {
                break;
            };
            match self.stream_chunk(i, &mut out[at..], record) {
                Some(n) => {
                    at += n;
                    if self.streams[i].priority.incremental {
                        // The next incremental frame goes to the next stream.
                        self.rr = (i + 1) % count;
                    }
                }
                None if i < 64 => refused |= 1 << i,
                None => break,
            }
        }
        at
    }

    /// The stream to send from next: the lowest urgency with data; within
    /// it, the non-incremental stream with the lowest ID, else the next
    /// incremental one after the round-robin cursor.
    fn next_stream(&self, refused: u64) -> Option<usize> {
        let conn_credit = self.tx_max_data.saturating_sub(self.tx_sent);
        let count = self.streams.len();
        let mut best: Option<(u8, bool, u64, usize)> = None;
        for k in 0..count {
            // Visit from the cursor so ties among incremental streams go
            // round-robin.
            let i = (self.rr + k) % count;
            if i < 64 && refused & (1 << i) != 0 {
                continue;
            }
            let s = &self.streams[i];
            if !s.send.as_ref().is_some_and(|t| t.has_data(conn_credit)) {
                continue;
            }
            let p = s.priority;
            // Order: urgency, then non-incremental before incremental, then
            // (non-incremental only) stream ID; incremental keeps visit order.
            let key = (
                p.urgency,
                p.incremental,
                if p.incremental { 0 } else { s.id },
                i,
            );
            let better = match best {
                None => true,
                Some((u, inc, id, _)) => (key.0, key.1, key.2) < (u, inc, id),
            };
            if better {
                best = Some(key);
            }
        }
        best.map(|(_, _, _, i)| i)
    }

    /// One STREAM frame from stream `i` into `out`, or `None`.
    fn stream_chunk(
        &mut self,
        i: usize,
        out: &mut [u8],
        record: &mut PacketFrames,
    ) -> Option<usize> {
        let conn_credit = self.tx_max_data.saturating_sub(self.tx_sent);
        let id = self.streams[i].id;
        let s = self.streams[i].send.as_mut()?;
        if !s.has_data(conn_credit) {
            return None;
        }
        // Room for data after a header sized for the worst case.
        let worst = frame::stream_header_len(id, s.buf.end.max(1), out.len());
        let room = out.len().saturating_sub(worst);
        let (off, len, retransmit) = if let Some((off, len)) = s.buf.next_lost(room) {
            (off, len, true)
        } else {
            let allowed = s.credit().min(conn_credit).min(room as u64);
            let (off, len) = s.buf.take_new(allowed);
            if len == 0 && !s.fin_due() {
                return None;
            }
            (off, len, false)
        };
        if len == 0 && room == 0 && worst > out.len() {
            return None;
        }
        let end = off + len as u64;
        let fin = s.fin == Some(end) && s.reset.is_none();
        let Ok(h) = frame::encode_stream_header(out, id, off, len, fin) else {
            // Cannot happen with the worst-case header; put it back.
            if retransmit {
                s.buf.on_lost(off, len);
            }
            return None;
        };
        s.buf.copy_out(off, &mut out[h..h + len]);
        if fin {
            s.fin_sent = true;
            s.fin_pending = false;
        }
        if retransmit {
            self.stats.stream_bytes_retransmitted += len as u64;
        } else {
            self.tx_sent += len as u64;
            self.stats.stream_bytes_sent += len as u64;
        }
        record.push(SentFrame::Stream {
            stream_id: id,
            offset: off,
            len: len as u32,
            fin,
        });
        Some(h + len)
    }

    /// Records the frames of a sent packet against its packet number
    /// (packet numbers are committed in increasing order).
    pub fn commit(&mut self, pn: u64, record: &PacketFrames) {
        debug_assert!(self.sent.back().is_none_or(|(p, _)| *p < pn));
        for f in record.frames() {
            if self.sent.len() == self.sent.capacity() {
                // Full: the oldest packet's frames are treated as lost.
                let oldest = self.sent.front().map(|(p, _)| *p).expect("non-empty");
                self.stats.records_evicted += 1;
                self.on_packet_lost(oldest);
            }
            self.sent.push_back((pn, *f));
        }
    }

    /// The packet carrying `record` was never sent: everything in it is
    /// queued again.
    pub fn requeue(&mut self, record: &PacketFrames) {
        for f in record.frames() {
            self.frame_lost(*f);
        }
    }

    fn take_packet(&mut self, pn: u64, out: &mut PacketFrames) {
        out.clear();
        let start = self.sent.partition_point(|(p, _)| *p < pn);
        let mut end = start;
        while end < self.sent.len() && self.sent[end].0 == pn {
            if !out.is_full() {
                out.push(self.sent[end].1);
            }
            end += 1;
        }
        if end > start {
            self.sent.drain(start..end);
        }
    }

    /// Every recorded packet was lost at once (wave six): a client whose
    /// 0-RTT packets a Retry discarded queues their frames again.
    pub fn on_all_packets_lost(&mut self) {
        while let Some(&(pn, _)) = self.sent.front() {
            self.on_packet_lost(pn);
        }
    }

    /// RFC 9002 acknowledged a packet: its ranges free buffer space.
    pub fn on_packet_acked(&mut self, pn: u64) {
        let mut frames = PacketFrames::default();
        self.take_packet(pn, &mut frames);
        if frames.is_empty() {
            return;
        }
        self.stats.packets_acked += 1;
        for f in frames.frames() {
            self.frame_acked(*f);
        }
    }

    /// RFC 9002 declared a packet lost: §13.3 repairs for its frames.
    pub fn on_packet_lost(&mut self, pn: u64) {
        let mut frames = PacketFrames::default();
        self.take_packet(pn, &mut frames);
        if frames.is_empty() {
            return;
        }
        self.stats.packets_lost += 1;
        for f in frames.frames() {
            self.frame_lost(*f);
        }
    }

    fn frame_acked(&mut self, f: SentFrame) {
        match f {
            SentFrame::Stream {
                stream_id,
                offset,
                len,
                fin,
            } => {
                let Some(i) = self.find(stream_id) else {
                    return;
                };
                let Some(s) = self.streams[i].send.as_mut() else {
                    return;
                };
                if s.reset.is_some() {
                    return;
                }
                let before = s.buf.free_space();
                s.buf.on_ack(offset, len as usize);
                if fin {
                    s.fin_acked = true;
                    s.fin_pending = false;
                }
                let freed = s.buf.free_space() > before;
                let writable = freed && s.want_writable && s.fin.is_none();
                if writable {
                    s.want_writable = false;
                }
                let finished = s.state() == SendState::DataRecvd;
                if writable {
                    self.event(StreamEvent::Writable(stream_id));
                }
                if finished {
                    self.event(StreamEvent::Finished(stream_id));
                    self.maybe_retire(i);
                }
            }
            SentFrame::Crypto { space, offset, len } => {
                self.crypto[space.index()].on_ack(offset, len as usize);
            }
            SentFrame::ResetStream { stream_id } => {
                if let Some(i) = self.find(stream_id)
                    && let Some(s) = self.streams[i].send.as_mut()
                {
                    s.reset_acked = true;
                    s.reset_pending = false;
                    self.maybe_retire(i);
                }
            }
            SentFrame::HandshakeDone => {
                self.handshake_done_acked = true;
                self.handshake_done_pending = false;
            }
            SentFrame::NewConnectionId { seq } => {
                for f in &mut self.new_cids {
                    if f.is_some_and(|f| f.seq == seq && !f.pending) {
                        *f = None;
                    }
                }
            }
            SentFrame::RetireConnectionId { seq } => {
                for f in &mut self.retire_cids {
                    if *f == Some((seq, false)) {
                        *f = None;
                    }
                }
            }
            SentFrame::NewToken if self.new_token.is_some_and(|t| !t.pending) => {
                self.new_token = None;
            }
            _ => {}
        }
    }

    fn frame_lost(&mut self, f: SentFrame) {
        match f {
            SentFrame::Stream {
                stream_id,
                offset,
                len,
                fin,
            } => {
                self.stats.stream_frames_lost += 1;
                if let Some(i) = self.find(stream_id)
                    && let Some(s) = self.streams[i].send.as_mut()
                    && s.reset.is_none()
                {
                    s.buf.on_lost(offset, len as usize);
                    if fin && !s.fin_acked {
                        s.fin_pending = true;
                    }
                }
            }
            SentFrame::Crypto { space, offset, len } => {
                self.crypto[space.index()].on_lost(offset, len as usize);
            }
            other => {
                self.stats.control_frames_lost += 1;
                self.control_lost(other);
            }
        }
    }

    fn control_lost(&mut self, f: SentFrame) {
        match f {
            SentFrame::ResetStream { stream_id } => {
                if let Some(i) = self.find(stream_id)
                    && let Some(s) = self.streams[i].send.as_mut()
                    && !s.reset_acked
                {
                    s.reset_pending = true;
                }
            }
            SentFrame::StopSending { stream_id } => {
                if let Some(i) = self.find(stream_id)
                    && let Some(r) = self.streams[i].recv.as_mut()
                    && matches!(r.state(), RecvState::Recv | RecvState::SizeKnown)
                {
                    r.stop_pending = true;
                }
            }
            SentFrame::MaxData => self.max_data_pending = true,
            SentFrame::HandshakeDone => {
                if !self.handshake_done_acked {
                    self.handshake_done_pending = true;
                }
            }
            SentFrame::MaxStreamData { stream_id } => {
                if let Some(i) = self.find(stream_id)
                    && let Some(r) = self.streams[i].recv.as_mut()
                    && r.state() == RecvState::Recv
                {
                    r.max_data_pending = true;
                }
            }
            SentFrame::MaxStreams { uni } => self.max_streams_pending[kind(uni)] = true,
            SentFrame::DataBlocked { limit } => {
                if self.tx_max_data == limit {
                    self.data_blocked_pending = true;
                }
            }
            SentFrame::StreamDataBlocked { stream_id, limit } => {
                if let Some(i) = self.find(stream_id)
                    && let Some(s) = self.streams[i].send.as_mut()
                    && s.max_data == limit
                    && s.reset.is_none()
                {
                    s.blocked_pending = true;
                }
            }
            SentFrame::StreamsBlocked { uni, limit } => {
                let k = kind(uni);
                if self.peer_max_streams[k] == limit {
                    self.streams_blocked_pending[k] = true;
                }
            }
            SentFrame::NewConnectionId { seq } => {
                for f in self.new_cids.iter_mut().flatten() {
                    if f.seq == seq {
                        f.pending = true;
                    }
                }
            }
            SentFrame::RetireConnectionId { seq } => {
                for f in self.retire_cids.iter_mut().flatten() {
                    if f.0 == seq {
                        f.1 = true;
                    }
                }
            }
            SentFrame::NewToken => {
                if let Some(t) = self.new_token.as_mut() {
                    t.pending = true;
                }
            }
            SentFrame::Stream { .. } | SentFrame::Crypto { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_find_and_wrap() {
        let mut b = Bits::new(100);
        b.set(90, 130); // wraps: bits 90..100 and 0..30
        assert!(b.get(95) && b.get(110) && b.get(129) && !b.get(130));
        assert_eq!(b.find(80, 180, true), Some(90));
        assert_eq!(b.find(90, 180, false), Some(130));
        b.clear(100, 120);
        assert_eq!(b.find(90, 180, false), Some(100));
        assert_eq!(b.find(100, 180, true), Some(120));
    }

    #[test]
    fn send_buf_retransmits_only_unacked_lost_bytes() {
        let mut s = SendBuf::new(16);
        assert_eq!(s.write(&[1; 20]), 16);
        assert_eq!(s.take_new(10), (0, 10));
        s.on_ack(4, 2);
        s.on_lost(0, 10);
        assert_eq!(s.next_lost(100), Some((0, 4)));
        assert_eq!(s.next_lost(100), Some((6, 4)));
        assert_eq!(s.next_lost(100), None);
        s.on_ack(0, 10);
        assert_eq!(s.acked_prefix(), 10);
        assert_eq!(s.free_space(), 10);
    }

    #[test]
    fn recv_buf_reassembles_with_wrap() {
        let mut r = RecvBuf::new(8);
        r.insert(4, b"efgh").unwrap();
        assert_eq!(r.readable(), 0);
        r.insert(0, b"abcd").unwrap();
        let mut out = [0u8; 6];
        assert_eq!(r.read(&mut out), 6);
        assert_eq!(&out, b"abcdef");
        r.insert(8, b"ijkl").unwrap();
        assert_eq!(r.insert(12, b"xxxx"), Err(WindowExceeded));
        let mut out = [0u8; 8];
        assert_eq!(r.read(&mut out), 6);
        assert_eq!(&out[..6], b"ghijkl");
    }
}
