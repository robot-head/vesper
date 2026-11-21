//! The QPACK decoder (RFC 9204 §2.2, §4.3, §4.4, §4.5).

use super::Field;

/// What this decoder advertises and how much it buffers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecoderConfig {
    /// SETTINGS_QPACK_MAX_TABLE_CAPACITY: the most the encoder may use.
    pub max_table_capacity: u64,
    /// SETTINGS_QPACK_BLOCKED_STREAMS.
    pub max_blocked_streams: u64,
    /// SETTINGS_MAX_FIELD_SECTION_SIZE (RFC 9114 §4.2.2), in the RFC's
    /// unit: name + value + 32 bytes per field line.
    pub max_field_section_size: u64,
    /// Encoded bytes held for all blocked streams together.
    pub max_blocked_bytes: usize,
}

impl Default for DecoderConfig {
    fn default() -> Self {
        Self {
            max_table_capacity: 4096,
            max_blocked_streams: 16,
            max_field_section_size: 64 * 1024,
            max_blocked_bytes: 64 * 1024,
        }
    }
}

/// A decoded field section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldSection {
    pub stream_id: u64,
    pub fields: Vec<Field>,
    pub required_insert_count: u64,
}

/// The result of offering a field section to the decoder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decoded {
    Fields(FieldSection),
    /// It references entries not yet received; the decoder holds a copy and
    /// returns it from [`Decoder::poll_unblocked`] once they arrive.
    Blocked,
}
