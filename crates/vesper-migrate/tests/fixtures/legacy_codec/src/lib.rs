//! legacy_codec 0.8: the Rust half of the vesper migrate worked example.
//! `parse` and `stats` are safe and are executed by the shadow tests; this
//! root module holds the C-ABI shim and items that the lifter must refuse.

pub mod parse;
pub mod stats;

use parse::{HEADER_LEN, ParseError, parse_batch_header};
use stats::DecoderStats;

/// C-ABI entry point used by existing C callers.
/// vesper-migrate: raw_bytes in len raw_len
/// vesper-migrate: out_crc inout
/// vesper-migrate: out_count inout
///
/// # Safety
/// `raw_bytes` must be valid for `raw_len` bytes; outputs must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn legacy_parse_batch_header(
    raw_bytes: *const u8,
    raw_len: usize,
    out_crc: *mut u32,
    out_count: *mut i32,
) -> i32 {
    let raw = unsafe { std::slice::from_raw_parts(raw_bytes, raw_len) };
    let mut stats = DecoderStats::default();
    match parse_batch_header(raw, &mut stats) {
        Ok(_) => {
            unsafe {
                *out_crc = stats.last_crc;
                *out_count = stats.record_count;
            }
            0
        }
        Err(e) => -(e as i32),
    }
}

/// Slice form: `out[0]` receives the CRC and `out[1]` the record count.
/// vesper-migrate: raw_bytes in len raw_len
/// vesper-migrate: out inout len out_len
///
/// # Safety
/// Both pointers must be valid for their lengths.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn legacy_parse_batch_header_into(
    raw_bytes: *const u8,
    raw_len: usize,
    out: *mut u32,
    out_len: usize,
) -> i32 {
    let raw = unsafe { std::slice::from_raw_parts(raw_bytes, raw_len) };
    let out = unsafe { std::slice::from_raw_parts_mut(out, out_len) };
    let mut stats = DecoderStats::default();
    match parse_batch_header(raw, &mut stats) {
        Ok(_) if out.len() >= 2 => {
            out[0] = stats.last_crc;
            out[1] = stats.record_count as u32;
            0
        }
        Ok(_) => -3,
        Err(e) => -(e as i32),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn legacy_header_len() -> usize {
    HEADER_LEN
}

#[unsafe(no_mangle)]
pub extern "C" fn legacy_classify(code: u32) -> ParseError {
    if code == 1 { ParseError::Short } else { ParseError::BadMagic }
}

/// A cursor that stores a borrow: refused (BorrowingType).
pub struct Cursor<'a> {
    pub buf: &'a [u8],
    pub pos: usize,
}

pub fn cursor_peek(cursor: &Cursor<'_>) -> u8 {
    cursor.buf[cursor.pos]
}

pub fn first_byte(raw: &[u8]) -> &u8 {
    &raw[0]
}

pub fn rewind<'a>(window: &'a mut Cursor<'a>) {
    window.pos = 0;
}

/// Shared counters mutated through `&self`: refused (InteriorMutability).
pub struct SharedHits {
    pub hits: std::cell::Cell<u32>,
}

pub fn bump(shared: &SharedHits) {
    shared.hits.set(shared.hits.get() + 1);
}

pub fn raw_peek(p: *const u8) -> u8 {
    unsafe { *p }
}

pub fn checksum<T: AsRef<[u8]>>(data: T) -> u32 {
    data.as_ref().iter().map(|&b| u32::from(b)).sum()
}

pub fn on_record(callback: fn(u32)) {
    callback(0)
}

pub const MAX_BATCH: usize = 1 << 20;
