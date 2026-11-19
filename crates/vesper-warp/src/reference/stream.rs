//! The Rust reference implementation of [`crate::stream`]'s byte buffers
//! ([`SendBuf`], [`RecvBuf`] and their ring bitmaps), compiled without
//! `vesper-port` (the Vesper build is `vesper/src/stream.ves`). Frozen: bug
//! fixes only (#1066).

use crate::stream::WindowExceeded;

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
    pub(crate) fn discard(&mut self) {
        let end = self.next;
        self.acked.clear(self.base, end);
        self.lost.clear(self.base, end);
        self.base = end;
        self.end = end;
        self.lost_hint = u64::MAX;
    }
    /// Back to empty at offset 0, for reuse by another stream.
    pub(crate) fn reset_for_reuse(&mut self) {
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
    pub(crate) fn reset_for_reuse(&mut self) {
        self.have.words.fill(0);
        self.read = 0;
        self.contig = 0;
        self.highest = 0;
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
}
