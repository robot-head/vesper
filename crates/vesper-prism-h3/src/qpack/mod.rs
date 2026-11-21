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
pub(crate) mod port;
pub mod prefix;

pub use decoder::{Decoded, DecoderConfig, FieldSection};
pub use encoder::{EncoderConfig, EncoderStats};
pub use port::{Decoder, Encoder, live_allocations};

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

/// RFC 9204 §3.2.1: an entry's size is its name and value lengths plus 32.
const ENTRY_OVERHEAD: u64 = 32;

pub(crate) fn entry_size(name: &[u8], value: &[u8]) -> u64 {
    name.len() as u64 + value.len() as u64 + ENTRY_OVERHEAD
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
