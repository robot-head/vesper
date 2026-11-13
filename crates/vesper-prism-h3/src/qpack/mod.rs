//! QPACK field compression (RFC 9204).
//!
//! [`Encoder`] and [`Decoder`] are sans-I/O: field sections go in and out
//! as byte vectors, and each side's unidirectional instruction stream is
//! fed with `on_*_stream` and drained with `take_*_stream`. Every limit is
//! checked before memory is committed, and every malformed or over-limit
//! input is a typed [`QpackError`] that names the RFC 9204 §6 error code.

mod decoder;
mod encoder;
pub mod huffman;
mod huffman_table;
pub mod prefix;
mod static_table;

pub use decoder::{Decoded, Decoder, DecoderConfig, FieldSection};
pub use encoder::{Encoder, EncoderConfig, EncoderStats};
pub use static_table::STATIC_TABLE;

use std::collections::VecDeque;
use std::fmt;

use crate::error::Code;

/// One field line. `sensitive` fields are never inserted into the dynamic
/// table and are sent with the N ("never index") bit; a decoded field
/// reports the bit it arrived with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub name: Vec<u8>,
    pub value: Vec<u8>,
    pub sensitive: bool,
}

impl Field {
    pub fn new(name: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
            sensitive: false,
        }
    }

    pub fn sensitive(name: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) -> Self {
        Self {
            sensitive: true,
            ..Self::new(name, value)
        }
    }

    /// RFC 9204 §3.2.1 entry size, also RFC 9114 §4.2.2 field size.
    pub fn size(&self) -> u64 {
        entry_size(&self.name, &self.value)
    }
}

pub(crate) fn entry_size(name: &[u8], value: &[u8]) -> u64 {
    name.len() as u64 + value.len() as u64 + 32
}

/// What exactly was wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Truncated,
    IntegerOverflow,
    StringTooLong,
    Huffman(huffman::HuffmanError),
    /// A static index above 98.
    BadStaticIndex(u64),
    /// A dynamic reference to an evicted entry, or one at or past the
    /// Required Insert Count (field lines) or the Insert Count (encoder
    /// instructions).
    BadDynamicIndex,
    /// An Encoded Insert Count no conformant encoder could produce, or a
    /// Required Insert Count larger than any reference needs.
    BadRequiredInsertCount,
    /// Sign bit set with Delta Base >= Required Insert Count.
    NegativeBase,
    /// Set Dynamic Table Capacity above the decoder's maximum.
    CapacityExceeded,
    /// An inserted entry larger than the table capacity.
    EntryTooLarge,
    /// The decoder promised fewer blocked streams than this needs.
    TooManyBlockedStreams,
    /// Bytes held for blocked streams would exceed the configured budget.
    BlockedBytesLimit,
    /// A Section Acknowledgment for a stream with no unacknowledged section.
    UnexpectedAcknowledgment(u64),
    /// Insert Count Increment of zero.
    ZeroIncrement,
    /// Insert Count Increment past the encoder's insert count.
    IncrementBeyondInserts,
    /// One encoder/decoder instruction is longer than any valid one.
    InstructionTooLong,
    /// A second field section was given for a stream that is still blocked.
    StreamAlreadyBlocked(u64),
}

/// A QPACK failure, with the RFC 9204 §6 class it belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QpackError {
    /// QPACK_DECOMPRESSION_FAILED: a bad field section (connection error).
    Decompression(Reason),
    /// QPACK_ENCODER_STREAM_ERROR: a bad encoder instruction.
    EncoderStream(Reason),
    /// QPACK_DECODER_STREAM_ERROR: a bad decoder instruction.
    DecoderStream(Reason),
    /// The decoded section exceeds the decoder's field section limit
    /// (RFC 9114 §4.2.2). A stream error, not a connection error: the
    /// stream is cancelled and the rest of the connection is unaffected.
    FieldSectionTooLarge { stream_id: u64 },
}

impl QpackError {
    pub fn code(self) -> Code {
        match self {
            Self::Decompression(_) => Code::QPACK_DECOMPRESSION_FAILED,
            Self::EncoderStream(_) => Code::QPACK_ENCODER_STREAM_ERROR,
            Self::DecoderStream(_) => Code::QPACK_DECODER_STREAM_ERROR,
            Self::FieldSectionTooLarge { .. } => Code::EXCESSIVE_LOAD,
        }
    }
}

impl fmt::Display for QpackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {self:?}", self.code())
    }
}

impl std::error::Error for QpackError {}

impl From<prefix::PrefixError> for Reason {
    fn from(e: prefix::PrefixError) -> Self {
        match e {
            prefix::PrefixError::Incomplete => Self::Truncated,
            prefix::PrefixError::IntegerOverflow => Self::IntegerOverflow,
            prefix::PrefixError::StringTooLong => Self::StringTooLong,
            prefix::PrefixError::Huffman(h) => Self::Huffman(h),
        }
    }
}

type Entry = (Box<[u8]>, Box<[u8]>);

/// The dynamic table (RFC 9204 §3.2), oldest entry first.
#[derive(Debug, Default)]
pub(crate) struct Table {
    entries: VecDeque<Entry>,
    size: u64,
    capacity: u64,
    /// Total insertions so far: the Insert Count.
    inserted: u64,
}

impl Table {
    pub(crate) fn insert_count(&self) -> u64 {
        self.inserted
    }
    pub(crate) fn capacity(&self) -> u64 {
        self.capacity
    }
    pub(crate) fn size(&self) -> u64 {
        self.size
    }
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
    /// Absolute index of the oldest entry still present.
    pub(crate) fn first_index(&self) -> u64 {
        self.inserted - self.entries.len() as u64
    }
    pub(crate) fn get(&self, abs: u64) -> Option<(&[u8], &[u8])> {
        let i = abs.checked_sub(self.first_index())?;
        self.entries
            .get(usize::try_from(i).ok()?)
            .map(|(n, v)| (&n[..], &v[..]))
    }
    /// Entries (oldest first) that must go for `needed` more bytes to fit.
    pub(crate) fn evictions_for(&self, needed: u64) -> Option<usize> {
        if needed > self.capacity {
            return None;
        }
        let mut size = self.size;
        let mut n = 0;
        while size + needed > self.capacity {
            let (name, value) = &self.entries[n];
            size -= entry_size(name, value);
            n += 1;
        }
        Some(n)
    }
    pub(crate) fn set_capacity(&mut self, capacity: u64) {
        self.capacity = capacity;
        while self.size > capacity {
            self.evict_one();
        }
    }
    fn evict_one(&mut self) {
        if let Some((n, v)) = self.entries.pop_front() {
            self.size -= entry_size(&n, &v);
        }
    }
    /// Inserts after evicting; the caller has checked the entry fits.
    pub(crate) fn insert(&mut self, name: Box<[u8]>, value: Box<[u8]>) {
        let needed = entry_size(&name, &value);
        debug_assert!(needed <= self.capacity);
        while self.size + needed > self.capacity {
            self.evict_one();
        }
        self.size += needed;
        self.entries.push_back((name, value));
        self.inserted += 1;
    }
}

/// RFC 9204 §4.5.1.1 encoding of the Required Insert Count.
pub(crate) fn encode_ric(ric: u64, max_entries: u64) -> u64 {
    if ric == 0 {
        0
    } else {
        ric % (2 * max_entries) + 1
    }
}

/// RFC 9204 §4.5.1.1 decoding, rejecting values no conformant encoder
/// could have produced.
pub(crate) fn decode_ric(encoded: u64, max_entries: u64, total_inserts: u64) -> Option<u64> {
    if encoded == 0 {
        return Some(0);
    }
    let full = max_entries.checked_mul(2)?;
    if encoded > full {
        return None;
    }
    let max_value = total_inserts.checked_add(max_entries)?;
    let max_wrapped = max_value / full * full;
    let mut ric = max_wrapped + encoded - 1;
    if ric > max_value {
        if ric <= full {
            return None;
        }
        ric -= full;
    }
    (ric != 0).then_some(ric)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ric_example_from_section_4_5_1_1() {
        // 100-byte table: MaxEntries 3, encoded modulo 6. With 10 inserts
        // an encoded value of 4 is a Required Insert Count of 9.
        assert_eq!(decode_ric(4, 3, 10), Some(9));
        assert_eq!(encode_ric(9, 3), 4);
        assert_eq!(decode_ric(7, 3, 10), None, "beyond 2 * MaxEntries");
        // Zero tables can only carry zero.
        assert_eq!(decode_ric(1, 0, 0), None);
        for inserts in 0u64..40 {
            for ric in inserts.saturating_sub(2)..=inserts + 3 {
                if ric == 0 {
                    continue;
                }
                assert_eq!(decode_ric(encode_ric(ric, 3), 3, inserts), Some(ric));
            }
        }
    }

    #[test]
    fn static_table_matches_rfc_spot_checks() {
        assert_eq!(STATIC_TABLE[0], (&b":authority"[..], &b""[..]));
        assert_eq!(STATIC_TABLE[17], (&b":method"[..], &b"GET"[..]));
        assert_eq!(STATIC_TABLE[25], (&b":status"[..], &b"200"[..]));
        assert_eq!(
            STATIC_TABLE[98],
            (&b"x-frame-options"[..], &b"sameorigin"[..])
        );
    }
}
