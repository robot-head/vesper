//! Bounded CRYPTO-frame offset reassembly for one packet-number space
//! (RFC 9000 §7.5, §19.6).
//!
//! Bytes are stored in a fixed ring of `capacity` bytes covering the window
//! `[delivered, delivered + capacity)`. Data past the window is
//! `CRYPTO_BUFFER_EXCEEDED`; data before it is a duplicate and dropped.
//! Retransmitted bytes that overlap still-buffered bytes must be identical.

use std::fmt;

use crate::varint::VARINT_MAX;

/// Default per-space buffer: the RFC 9000 §7.5 minimum is 4096 bytes; this
/// leaves room for certificate chains arriving out of order.
pub const DEFAULT_CRYPTO_BUFFER: usize = 64 * 1024;
/// Default bound on the number of disjoint buffered ranges, so adversarial
/// fragmentation cannot grow the range list without bound.
pub const DEFAULT_MAX_RANGES: usize = 256;

/// Why CRYPTO data was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CryptoBufferError {
    /// `offset + len` exceeds `2^62 - 1` (`FRAME_ENCODING_ERROR`).
    OffsetOverflow,
    /// The data ends past the reassembly window (`CRYPTO_BUFFER_EXCEEDED`).
    BufferExceeded { end: u64, limit: u64 },
    /// Too many disjoint gaps are buffered (`CRYPTO_BUFFER_EXCEEDED`).
    TooFragmented,
    /// Retransmitted bytes differ from the bytes already buffered at the
    /// same offsets (`PROTOCOL_VIOLATION`).
    ConflictingData { offset: u64 },
}

impl CryptoBufferError {
    pub const fn quic_error_code(self) -> u64 {
        use crate::error::code;
        match self {
            CryptoBufferError::OffsetOverflow => code::FRAME_ENCODING_ERROR,
            CryptoBufferError::BufferExceeded { .. } | CryptoBufferError::TooFragmented => {
                code::CRYPTO_BUFFER_EXCEEDED
            }
            CryptoBufferError::ConflictingData { .. } => code::PROTOCOL_VIOLATION,
        }
    }
}

impl fmt::Display for CryptoBufferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for CryptoBufferError {}

/// Reassembles one CRYPTO stream.
pub struct CryptoReassembler {
    ring: Box<[u8]>,
    max_ranges: usize,
    /// Everything below this offset has been delivered.
    delivered: u64,
    /// Sorted, disjoint, non-adjacent `[start, end)` ranges at or above
    /// `delivered`.
    ranges: Vec<(u64, u64)>,
    /// Largest `offset + len` accepted so far.
    highest_end: u64,
}

impl CryptoReassembler {
    /// `capacity` must be non-zero.
    pub fn new(capacity: usize, max_ranges: usize) -> Self {
        assert!(
            capacity > 0 && max_ranges > 0,
            "reassembly bounds must be non-zero"
        );
        Self {
            ring: vec![0; capacity].into_boxed_slice(),
            max_ranges,
            delivered: 0,
            ranges: Vec::new(),
            highest_end: 0,
        }
    }

    pub fn capacity(&self) -> usize {
        self.ring.len()
    }

    /// Offset of the next byte to deliver.
    pub fn delivered(&self) -> u64 {
        self.delivered
    }

    /// Largest stream end received so far.
    pub fn highest_end(&self) -> u64 {
        self.highest_end
    }

    /// Bytes buffered but not yet delivered.
    pub fn buffered(&self) -> u64 {
        self.ranges.iter().map(|(s, e)| e - s).sum()
    }

    /// Number of contiguous bytes ready at the delivery point.
    pub fn readable(&self) -> usize {
        match self.ranges.first() {
            // Bounded by the ring length, so the cast cannot truncate.
            Some(&(s, e)) if s == self.delivered => (e - s) as usize,
            _ => 0,
        }
    }

    fn slot(&self, offset: u64) -> usize {
        (offset % self.ring.len() as u64) as usize
    }

    /// Accepts one CRYPTO frame's data. On error nothing changes.
    pub fn insert(&mut self, offset: u64, data: &[u8]) -> Result<(), CryptoBufferError> {
        let end = offset
            .checked_add(data.len() as u64)
            .filter(|&e| e <= VARINT_MAX)
            .ok_or(CryptoBufferError::OffsetOverflow)?;
        if end <= self.delivered {
            return Ok(());
        }
        let limit = self.delivered + self.ring.len() as u64;
        if end > limit {
            return Err(CryptoBufferError::BufferExceeded { end, limit });
        }
        let start = offset.max(self.delivered);
        let data = &data[(start - offset) as usize..];

        // Overlapping buffered bytes must agree.
        for &(s, e) in &self.ranges {
            let lo = s.max(start);
            let hi = e.min(end);
            for o in lo..hi {
                if self.ring[self.slot(o)] != data[(o - start) as usize] {
                    return Err(CryptoBufferError::ConflictingData { offset: o });
                }
            }
        }

        // Merge into the range list (computed before mutation).
        let first = self.ranges.partition_point(|&(_, e)| e < start);
        let last = self.ranges.partition_point(|&(s, _)| s <= end);
        let merged = if first < last {
            (
                start.min(self.ranges[first].0),
                end.max(self.ranges[last - 1].1),
            )
        } else {
            (start, end)
        };
        let new_len = self.ranges.len() - (last - first) + 1;
        if new_len > self.max_ranges {
            return Err(CryptoBufferError::TooFragmented);
        }

        for (i, &b) in data.iter().enumerate() {
            let slot = self.slot(start + i as u64);
            self.ring[slot] = b;
        }
        self.ranges.splice(first..last, [merged]);
        self.highest_end = self.highest_end.max(end);
        Ok(())
    }

    /// Appends every contiguous byte at the delivery point to `out` and
    /// advances past them. Returns how many bytes were appended.
    pub fn read_into(&mut self, out: &mut Vec<u8>) -> usize {
        let n = self.readable();
        if n == 0 {
            return 0;
        }
        let first = self.slot(self.delivered);
        let tail = (self.ring.len() - first).min(n);
        out.extend_from_slice(&self.ring[first..first + tail]);
        out.extend_from_slice(&self.ring[..n - tail]);
        self.delivered += n as u64;
        self.ranges.remove(0);
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(r: &mut CryptoReassembler) -> Vec<u8> {
        let mut v = Vec::new();
        r.read_into(&mut v);
        v
    }

    #[test]
    fn out_of_order_duplicate_and_wrapping() {
        let mut r = CryptoReassembler::new(8, 4);
        r.insert(4, b"efgh").unwrap();
        assert_eq!(r.readable(), 0);
        r.insert(0, b"abcd").unwrap();
        r.insert(2, b"cdef").unwrap();
        assert_eq!(drain(&mut r), b"abcdefgh");
        r.insert(0, b"abcdefgh").unwrap(); // fully delivered duplicate
        r.insert(6, b"ghij").unwrap(); // overlaps delivered bytes
        assert_eq!(drain(&mut r), b"ij");
        // Wraps the ring.
        r.insert(10, b"klmnopqr").unwrap();
        assert_eq!(drain(&mut r), b"klmnopqr");
        assert_eq!(r.delivered(), 18);
    }

    #[test]
    fn rejects_overflow_window_conflict_and_fragmentation() {
        let mut r = CryptoReassembler::new(8, 2);
        assert_eq!(
            r.insert(VARINT_MAX, b"x"),
            Err(CryptoBufferError::OffsetOverflow)
        );
        assert_eq!(
            r.insert(5, b"abcd"),
            Err(CryptoBufferError::BufferExceeded { end: 9, limit: 8 })
        );
        r.insert(1, b"b").unwrap();
        assert_eq!(
            r.insert(0, b"aX"),
            Err(CryptoBufferError::ConflictingData { offset: 1 })
        );
        r.insert(3, b"d").unwrap();
        assert_eq!(r.insert(5, b"f"), Err(CryptoBufferError::TooFragmented));
        assert_eq!(r.buffered(), 2);
        // Filling a gap merges ranges and is still accepted.
        r.insert(2, b"c").unwrap();
        r.insert(0, b"a").unwrap();
        assert_eq!(drain(&mut r), b"abcd");
    }
}
