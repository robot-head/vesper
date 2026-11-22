//! The QPACK encoder (RFC 9204 §2.1, §4.3, §4.4, §4.5).
//!
//! A single-pass encoder: the Base of each field section is the Insert
//! Count when encoding starts, entries inserted for the section are
//! referenced with post-base indices, and the Required Insert Count is the
//! largest referenced absolute index plus one. The encoder only references
//! unacknowledged entries while the peer's blocked-stream budget allows it,
//! and never evicts an entry that is unacknowledged or still referenced by
//! an unacknowledged section (§2.1.1).

/// How the encoder uses the dynamic table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncoderConfig {
    /// The capacity we would like; the table uses
    /// `min(this, peer's SETTINGS_QPACK_MAX_TABLE_CAPACITY)`.
    pub max_table_capacity: u64,
    /// Fields whose entry is larger than this share of the capacity
    /// (in 1/256ths) are sent as literals instead of being inserted.
    pub max_entry_share: u64,
    /// Unacknowledged field sections tracked; past this the encoder stops
    /// referencing the dynamic table until acknowledgments arrive.
    pub max_outstanding_sections: usize,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            max_table_capacity: 4096,
            max_entry_share: 192,
            max_outstanding_sections: 256,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EncoderStats {
    pub sections: u64,
    pub inserts: u64,
    pub static_refs: u64,
    pub dynamic_refs: u64,
    pub literals: u64,
    pub blocking_sections: u64,
}
