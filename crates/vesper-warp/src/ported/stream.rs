//! [`crate::stream`]'s byte buffers forwarded to `vesper/src/stream.ves`.
//! The wrapper owns each ring and its bitmaps (one bit per ring byte) and
//! lends them; the offsets are the Vesper `SendSt` / `RecvSt` mirrors.

use super::binding::{self as b, repr};
use crate::stream::WindowExceeded;

fn words(cap: usize) -> Box<[u64]> {
    vec![0; cap.div_ceil(64)].into_boxed_slice()
}

/// The bytes of one stream (or CRYPTO stream) the peer has not
/// acknowledged, in a fixed ring.
///
/// Offsets: `[0, base)` acknowledged and freed; `[base, next)` sent, each
/// byte acknowledged, lost (to resend) or in flight; `[next, end)` written
/// but never sent.
#[derive(Debug, Clone)]
pub struct SendBuf {
    data: Box<[u8]>,
    acked: Box<[u64]>,
    lost: Box<[u64]>,
    st: repr::SendSt,
}

impl SendBuf {
    pub fn new(capacity: usize) -> Self {
        Self {
            data: vec![0; capacity].into_boxed_slice(),
            acked: words(capacity),
            lost: words(capacity),
            st: b::send_buf_new(capacity as u64),
        }
    }
    pub fn capacity(&self) -> usize {
        self.data.len()
    }
    /// Lowest offset not yet acknowledged.
    pub const fn acked_prefix(&self) -> u64 {
        self.st.base
    }
    /// Offset one past the highest byte ever sent.
    pub const fn sent_offset(&self) -> u64 {
        self.st.next
    }
    /// Offset one past the last byte written.
    pub const fn written(&self) -> u64 {
        self.st.end
    }
    /// Bytes buffered (unacknowledged or unsent).
    pub const fn buffered(&self) -> u64 {
        self.st.end - self.st.base
    }
    pub fn free_space(&self) -> usize {
        b::send_buf_free_space(&self.st) as usize
    }
    /// Copies as much of `data` as fits; returns the bytes accepted.
    pub fn write(&mut self, data: &[u8]) -> usize {
        b::send_buf_write(&mut self.st, &mut self.data, data) as usize
    }
    pub const fn has_lost(&self) -> bool {
        self.st.lost_hint < self.st.next
    }
    /// Takes up to `max` bytes of the lowest lost range for retransmission.
    pub fn next_lost(&mut self, max: usize) -> Option<(u64, usize)> {
        let r = b::send_buf_next_lost(&mut self.st, &mut self.lost, max as u64);
        r.has.get().then_some((r.off, r.len as usize))
    }
    /// Bytes written but never sent.
    pub const fn unsent(&self) -> u64 {
        self.st.end - self.st.next
    }
    /// Takes up to `max` never-sent bytes; returns their offset and length.
    pub fn take_new(&mut self, max: u64) -> (u64, usize) {
        let r = b::send_buf_take_new(&mut self.st, max);
        (r.off, r.len as usize)
    }
    /// Copies buffered bytes at `off` (within `[base, end)`) into `out`.
    pub fn copy_out(&self, off: u64, out: &mut [u8]) {
        debug_assert!(off >= self.st.base && off + out.len() as u64 <= self.st.end);
        b::send_buf_copy_out(&self.data, off, out);
    }
    /// The peer acknowledged `[off, off + len)`; frees the acknowledged
    /// prefix.
    pub fn on_ack(&mut self, off: u64, len: usize) {
        b::send_buf_on_ack(
            &mut self.st,
            &mut self.acked,
            &mut self.lost,
            off,
            len as u64,
        );
    }
    /// `[off, off + len)` was in a lost packet: its unacknowledged bytes
    /// are queued for retransmission.
    pub fn on_lost(&mut self, off: u64, len: usize) {
        b::send_buf_on_lost(&mut self.st, &mut self.lost, &self.acked, off, len as u64);
    }
    /// Drops every buffered byte (after RESET_STREAM); nothing more is sent.
    pub(crate) fn discard(&mut self) {
        b::send_buf_discard(&mut self.st, &mut self.acked, &mut self.lost);
    }
    /// Back to empty at offset 0, for reuse by another stream.
    pub(crate) fn reset_for_reuse(&mut self) {
        b::send_buf_reset_for_reuse(&mut self.st, &mut self.acked, &mut self.lost);
    }
}

/// Reassembles one stream's (or CRYPTO stream's) data. Holds exactly one
/// window: offsets `[read, read + capacity)`.
#[derive(Debug, Clone)]
pub struct RecvBuf {
    data: Box<[u8]>,
    have: Box<[u64]>,
    st: repr::RecvSt,
}

impl RecvBuf {
    pub fn new(capacity: usize) -> Self {
        Self {
            data: vec![0; capacity].into_boxed_slice(),
            have: words(capacity),
            st: b::recv_buf_new(capacity as u64),
        }
    }
    pub fn capacity(&self) -> usize {
        self.data.len()
    }
    /// Offset the application has read up to.
    pub const fn read_offset(&self) -> u64 {
        self.st.read
    }
    /// One past the highest offset received.
    pub const fn highest(&self) -> u64 {
        self.st.highest
    }
    /// Contiguous bytes ready to read.
    pub const fn readable(&self) -> u64 {
        self.st.contig - self.st.read
    }
    /// One past the last contiguously received byte.
    pub const fn contiguous(&self) -> u64 {
        self.st.contig
    }
    /// Stores `data` at `off`. Bytes already read are ignored, duplicates
    /// overwrite (RFC 9000 §2.2 lets an endpoint assume they are equal).
    pub fn insert(&mut self, off: u64, data: &[u8]) -> Result<(), WindowExceeded> {
        if b::recv_buf_insert(&mut self.st, &mut self.data, &mut self.have, off, data) {
            Ok(())
        } else {
            Err(WindowExceeded)
        }
    }
    /// Reads contiguous bytes into `out`; returns the count.
    pub fn read(&mut self, out: &mut [u8]) -> usize {
        b::recv_buf_read(&mut self.st, &self.data, &mut self.have, out) as usize
    }
    pub(crate) fn reset_for_reuse(&mut self) {
        b::recv_buf_reset_for_reuse(&mut self.st, &mut self.have);
    }
}
