//! QPACK wire patterns (RFC 9204 §4): the first-byte bits of each field
//! line and instruction and the prefix length of its integer.

/// §3.2.1: an entry's size is its name and value lengths plus 32.
pub(crate) const ENTRY_OVERHEAD: u64 = 32;

/// §4.1.2: a string literal's Huffman bit and length prefix when it fills
/// the rest of its byte (field values and instruction values).
pub(crate) const VALUE_PREFIX: u8 = 7;

/// `EncoderConfig::max_entry_share` is in 1/256ths of the capacity.
pub(crate) const ENTRY_SHARE_SCALE: u64 = 256;
/// A decoder instruction is at most 10 bytes; more pending than this is
/// malformed.
pub(crate) const MAX_PENDING_INSTRUCTION: usize = 16;

/// §4.3.2 Insert with Name Reference: `1 T index(6+)`.
pub(crate) const INSERT_NAME_REF: u8 = 0x80;
pub(crate) const INSERT_NAME_REF_STATIC: u8 = 0x40;
pub(crate) const INSERT_NAME_REF_PREFIX: u8 = 6;
/// §4.3.3 Insert with Literal Name: `01 H name-length(5+)`.
pub(crate) const INSERT_LITERAL: u8 = 0x40;
pub(crate) const INSERT_LITERAL_PREFIX: u8 = 5;
/// §4.3.1 Set Dynamic Table Capacity: `001 capacity(5+)`.
pub(crate) const SET_CAPACITY: u8 = 0x20;
pub(crate) const SET_CAPACITY_PREFIX: u8 = 5;
/// §4.3.4 Duplicate: `000 index(5+)`.
pub(crate) const DUPLICATE_PREFIX: u8 = 5;

/// §4.4.1 Section Acknowledgment: `1 stream-id(7+)`.
pub(crate) const SECTION_ACK: u8 = 0x80;
pub(crate) const SECTION_ACK_PREFIX: u8 = 7;
/// §4.4.2 Stream Cancellation: `01 stream-id(6+)`.
pub(crate) const STREAM_CANCEL: u8 = 0x40;
pub(crate) const STREAM_CANCEL_PREFIX: u8 = 6;
/// §4.4.3 Insert Count Increment: `00 increment(6+)`.
pub(crate) const INSERT_COUNT_INCREMENT: u8 = 0x00;
pub(crate) const INSERT_COUNT_INCREMENT_PREFIX: u8 = 6;

/// §4.5.1 Encoded Field Section Prefix: the Required Insert Count (8+),
/// then the Delta Base's sign bit and its 7-bit prefix.
pub(crate) const RIC_PREFIX: u8 = 8;
pub(crate) const BASE_NEGATIVE: u8 = 0x80;
pub(crate) const DELTA_BASE_PREFIX: u8 = 7;

/// §4.5.2 Indexed Field Line: `1 T index(6+)`.
pub(crate) const INDEXED: u8 = 0x80;
pub(crate) const INDEXED_STATIC: u8 = 0x40;
pub(crate) const INDEXED_PREFIX: u8 = 6;
/// §4.5.3 Indexed Field Line with Post-Base Index: `0001 index(4+)`.
pub(crate) const INDEXED_POST_BASE: u8 = 0x10;
pub(crate) const INDEXED_POST_BASE_PREFIX: u8 = 4;
/// §4.5.4 Literal Field Line with Name Reference: `01 N T index(4+)`.
pub(crate) const NAME_REF: u8 = 0x40;
pub(crate) const NAME_REF_NEVER: u8 = 0x20;
pub(crate) const NAME_REF_STATIC: u8 = 0x10;
pub(crate) const NAME_REF_PREFIX: u8 = 4;
/// §4.5.5 Literal Field Line with Post-Base Name Reference:
/// `0000 N index(3+)`.
pub(crate) const POST_BASE_NAME_REF_NEVER: u8 = 0x08;
pub(crate) const POST_BASE_NAME_REF_PREFIX: u8 = 3;
/// §4.5.6 Literal Field Line with Literal Name: `001 N H name-length(3+)`.
pub(crate) const LITERAL_NAME: u8 = 0x20;
pub(crate) const LITERAL_NAME_NEVER: u8 = 0x10;
pub(crate) const LITERAL_NAME_PREFIX: u8 = 3;
