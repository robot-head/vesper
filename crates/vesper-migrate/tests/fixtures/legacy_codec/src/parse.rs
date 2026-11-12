//! Batch header parsing (safe; executed by the shadow tests).

use crate::stats::DecoderStats;

/// Wire header: base_offset i64 BE, batch_length i32 BE, leader_epoch i32 BE,
/// magic u8 (must be 2), crc u32 BE, records_count i32 BE.
pub const HEADER_LEN: usize = 25;

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    Short = 1,
    BadMagic = 2,
}

fn be_u32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

pub fn parse_batch_header(raw_bytes: &[u8], state: &mut DecoderStats) -> Result<i64, ParseError> {
    if raw_bytes.len() < HEADER_LEN {
        return Err(ParseError::Short);
    }
    if raw_bytes[16] != 2 {
        return Err(ParseError::BadMagic);
    }
    state.last_crc = be_u32(&raw_bytes[17..21]);
    state.record_count = be_u32(&raw_bytes[21..25]) as i32;
    let mut base = [0u8; 8];
    base.copy_from_slice(&raw_bytes[0..8]);
    Ok(i64::from_be_bytes(base))
}

pub fn record_bytes_hint(state: &DecoderStats, per_record: u32) -> u64 {
    u64::from(per_record) * state.record_count.max(0) as u64
}
