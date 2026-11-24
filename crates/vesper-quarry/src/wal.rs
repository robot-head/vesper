//! Write-ahead log: record codec, checked frames, two ping-pong headers and
//! checkpoint truncation.
//!
//! File layout: header slots at byte 0 and byte 32, frames from byte 64.
//!
//! | Header bytes | Field |
//! | --- | --- |
//! | 0..8 | magic `QRYWAL01` |
//! | 8..16 | generation `seq` (u64 LE) |
//! | 16..24 | `base_lsn`: LSN of the first frame (u64 LE) |
//! | 24..28 | CRC32C of bytes 0..24 |
//! | 28..32 | zero |
//!
//! The valid header with the larger generation wins. A frame is
//! `[len u32][lsn u64][crc u32][payload; len]` where the CRC32C covers the
//! generation, `len`, `lsn` and the payload. Binding the generation makes
//! frames left over from before a truncation invalid even when their bytes
//! survive. LSNs are consecutive record numbers starting at `base_lsn`; the
//! log ends at the first torn, mismatching or out-of-sequence frame.
use crate::device::{Device, FileId};
use crate::durable::{DbError, DbErrorKind};
use crate::page::{crc32c, crc32c_update, PAGE_SIZE};

pub const HEADER_BYTES: u64 = 64;
const MAGIC: &[u8; 8] = b"QRYWAL01";
const FRAME_HEADER: usize = 16;
/// The largest payload: an update carrying before and after images of up
/// to a page each, plus fixed fields (a full page image is smaller).
pub const MAX_PAYLOAD: usize = 2 * PAGE_SIZE + 64;

/// One logged action. Slot images include Quarry's one-byte record tag
/// (`1` live, `0` tombstone).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogRecord {
    /// Full page image logged before the first change to a page after a
    /// checkpoint (or for a new page). Redo-only; repairs torn page writes.
    PageImage {
        page: u64,
        bytes: Vec<u8>,
    },
    Insert {
        txn: u64,
        prev: u64,
        page: u64,
        slot: u16,
        after: Vec<u8>,
    },
    Update {
        txn: u64,
        prev: u64,
        page: u64,
        slot: u16,
        before: Vec<u8>,
        after: Vec<u8>,
    },
    /// Redo-only compensation written while undoing; `undo_next` is the
    /// next record of the transaction still to undo (0 when none).
    Compensation {
        txn: u64,
        prev: u64,
        page: u64,
        slot: u16,
        image: Vec<u8>,
        undo_next: u64,
    },
    Commit {
        txn: u64,
        prev: u64,
    },
    /// The transaction is finished after a complete rollback.
    End {
        txn: u64,
        prev: u64,
    },
}

impl LogRecord {
    pub fn txn(&self) -> Option<u64> {
        match self {
            Self::PageImage { .. } => None,
            Self::Insert { txn, .. }
            | Self::Update { txn, .. }
            | Self::Compensation { txn, .. }
            | Self::Commit { txn, .. }
            | Self::End { txn, .. } => Some(*txn),
        }
    }
    pub fn page(&self) -> Option<u64> {
        match self {
            Self::PageImage { page, .. }
            | Self::Insert { page, .. }
            | Self::Update { page, .. }
            | Self::Compensation { page, .. } => Some(*page),
            Self::Commit { .. } | Self::End { .. } => None,
        }
    }
    pub(crate) fn encode(&self) -> Vec<u8> {
        let (tag, txn, prev, page, undo_next, slot, b1, b2): (
            u64,
            u64,
            u64,
            u64,
            u64,
            u64,
            &[u8],
            &[u8],
        ) = match self {
            Self::PageImage { page, bytes } => (1, 0, 0, *page, 0, 0, bytes, &[]),
            Self::Insert {
                txn,
                prev,
                page,
                slot,
                after,
            } => (2, *txn, *prev, *page, 0, u64::from(*slot), after, &[]),
            Self::Update {
                txn,
                prev,
                page,
                slot,
                before,
                after,
            } => (3, *txn, *prev, *page, 0, u64::from(*slot), before, after),
            Self::Compensation {
                txn,
                prev,
                page,
                slot,
                image,
                undo_next,
            } => (
                4,
                *txn,
                *prev,
                *page,
                *undo_next,
                u64::from(*slot),
                image,
                &[],
            ),
            Self::Commit { txn, prev } => (5, *txn, *prev, 0, 0, 0, &[], &[]),
            Self::End { txn, prev } => (6, *txn, *prev, 0, 0, 0, &[], &[]),
        };
        let mut out = vec![0u8; 64 + b1.len() + b2.len()];
        let len = crate::port::binding::wal_encode(
            tag, txn, prev, page, undo_next, slot, b1, b2, &mut out,
        ) as usize;
        out.truncate(len);
        out
    }
    /// Strict decode: trailing bytes, unknown tags and bad lengths are errors.
    pub fn decode(payload: &[u8]) -> Result<Self, DbError> {
        let mut meta = [0u64; 10];
        match crate::port::binding::wal_decode(payload, &mut meta) {
            0 => {}
            1 => return Err(corrupt("unknown log record tag")),
            2 => return Err(corrupt("page image length")),
            3 => return Err(corrupt("trailing log record bytes")),
            4 => return Err(corrupt("log byte string too long")),
            _ => return Err(corrupt("truncated log record")),
        }
        let [tag, txn, prev, page, undo_next, slot, b1_off, b1_len, b2_off, b2_len] = meta;
        let b1 = payload[b1_off as usize..(b1_off + b1_len) as usize].to_vec();
        match tag {
            1 => Ok(Self::PageImage { page, bytes: b1 }),
            2 => Ok(Self::Insert {
                txn,
                prev,
                page,
                slot: slot as u16,
                after: b1,
            }),
            3 => Ok(Self::Update {
                txn,
                prev,
                page,
                slot: slot as u16,
                before: b1,
                after: payload[b2_off as usize..(b2_off + b2_len) as usize].to_vec(),
            }),
            4 => Ok(Self::Compensation {
                txn,
                prev,
                page,
                slot: slot as u16,
                image: b1,
                undo_next,
            }),
            5 => Ok(Self::Commit { txn, prev }),
            _ => Ok(Self::End { txn, prev }),
        }
    }
}
fn corrupt(message: &str) -> DbError {
    DbError::new(DbErrorKind::Corrupt, message)
}

/// Why the scanned log ended before the end of the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TailDefect {
    TornHeader,
    TornPayload,
    Oversize,
    BadChecksum,
    OutOfSequence,
}

#[derive(Debug)]
pub(crate) struct Scan {
    pub seq: u64,
    pub base_lsn: u64,
    pub records: Vec<(u64, LogRecord)>,
    pub valid_end: u64,
    pub file_len: u64,
    pub tail: Option<TailDefect>,
}

fn encode_header(seq: u64, base_lsn: u64) -> [u8; 32] {
    let mut h = [0u8; 32];
    h[..8].copy_from_slice(MAGIC);
    h[8..16].copy_from_slice(&seq.to_le_bytes());
    h[16..24].copy_from_slice(&base_lsn.to_le_bytes());
    let crc = crc32c(&h[..24]);
    h[24..28].copy_from_slice(&crc.to_le_bytes());
    h
}
fn decode_header(h: &[u8]) -> Option<(u64, u64)> {
    let mut out = [0u64; 2];
    if crate::port::binding::wal_decode_header(h, &mut out) == 0 {
        return None;
    }
    if crc32c(&h[..24]) != u32::from_le_bytes(h[24..28].try_into().ok()?) {
        return None;
    }
    Some((out[0], out[1]))
}
fn frame_crc(seq: u64, len: u32, lsn: u64, payload: &[u8]) -> u32 {
    let mut c = crc32c_update(!0, &seq.to_le_bytes());
    c = crc32c_update(c, &len.to_le_bytes());
    c = crc32c_update(c, &lsn.to_le_bytes());
    !crc32c_update(c, payload)
}

/// Reads the active header and all valid frames. `Ok(None)` means there is
/// no valid header at all (a never-initialized or destroyed log).
pub(crate) fn scan(dev: &mut impl Device, max_bytes: u64) -> Result<Option<Scan>, DbError> {
    let file_len = dev.len(FileId::Wal)?;
    if file_len < HEADER_BYTES {
        return Ok(None);
    }
    // Recovery may append compensations past the normal limit (see Db::open).
    if file_len > HEADER_BYTES + max_bytes.saturating_mul(2) + MAX_PAYLOAD as u64 * 4 {
        return Err(DbError::new(DbErrorKind::Limit, "log file exceeds limit"));
    }
    let mut file = vec![0u8; file_len as usize];
    dev.read_at(FileId::Wal, 0, &mut file)?;
    let headers = [decode_header(&file[..32]), decode_header(&file[32..64])];
    let Some((seq, base_lsn)) = headers.into_iter().flatten().max_by_key(|h| h.0) else {
        return Ok(None);
    };
    let mut at = HEADER_BYTES as usize;
    let mut expected = base_lsn;
    let mut records = Vec::new();
    let mut tail = None;
    while at < file.len() {
        let Some(h) = file.get(at..at + FRAME_HEADER) else {
            tail = Some(TailDefect::TornHeader);
            break;
        };
        let len = u32::from_le_bytes(h[..4].try_into().expect("4"));
        let lsn = u64::from_le_bytes(h[4..12].try_into().expect("8"));
        let crc = u32::from_le_bytes(h[12..16].try_into().expect("4"));
        if len as usize > MAX_PAYLOAD {
            tail = Some(TailDefect::Oversize);
            break;
        }
        let start = at + FRAME_HEADER;
        let Some(payload) = file.get(start..start + len as usize) else {
            tail = Some(TailDefect::TornPayload);
            break;
        };
        if frame_crc(seq, len, lsn, payload) != crc {
            tail = Some(TailDefect::BadChecksum);
            break;
        }
        if lsn != expected {
            tail = Some(TailDefect::OutOfSequence);
            break;
        }
        // A checked frame that does not decode is corruption, not a torn tail.
        records.push((lsn, LogRecord::decode(payload)?));
        expected = expected
            .checked_add(1)
            .ok_or_else(|| DbError::new(DbErrorKind::Overflow, "LSN exhausted"))?;
        at = start + len as usize;
    }
    Ok(Some(Scan {
        seq,
        base_lsn,
        records,
        valid_end: at as u64,
        file_len,
        tail,
    }))
}

/// In-memory log tail plus its durable position.
#[derive(Debug)]
pub(crate) struct Wal {
    seq: u64,
    base_lsn: u64,
    next_lsn: u64,
    durable_lsn: u64,
    end_offset: u64,
    buffer: Vec<u8>,
    max_bytes: u64,
    reserved: u64,
}
impl Wal {
    pub fn create(dev: &mut impl Device, max_bytes: u64) -> Result<Self, DbError> {
        dev.set_len(FileId::Wal, 0)?;
        dev.write_at(FileId::Wal, 0, &encode_header(1, 1))?;
        dev.write_at(FileId::Wal, 32, &[0; 32])?;
        dev.sync(FileId::Wal)?;
        Ok(Self::at(1, 1, 1, HEADER_BYTES, max_bytes))
    }
    pub fn resume(scan: &Scan, max_bytes: u64) -> Self {
        let next = scan.base_lsn + scan.records.len() as u64;
        Self::at(scan.seq, scan.base_lsn, next, scan.valid_end, max_bytes)
    }
    fn at(seq: u64, base_lsn: u64, next_lsn: u64, end_offset: u64, max_bytes: u64) -> Self {
        Self {
            seq,
            base_lsn,
            next_lsn,
            durable_lsn: next_lsn - 1,
            end_offset,
            buffer: Vec::new(),
            max_bytes,
            reserved: 0,
        }
    }
    pub fn next_lsn(&self) -> u64 {
        self.next_lsn
    }
    pub fn base_lsn(&self) -> u64 {
        self.base_lsn
    }
    pub fn durable_lsn(&self) -> u64 {
        self.durable_lsn
    }
    /// Log bytes after the headers, including the unflushed buffer.
    pub fn used(&self) -> u64 {
        self.end_offset - HEADER_BYTES + self.buffer.len() as u64
    }
    pub fn reserved(&self) -> u64 {
        self.reserved
    }
    /// Checks that `need` more bytes plus `reserve` future bytes fit, then
    /// holds `reserve` until `release`.
    pub fn reserve(&mut self, need: u64, reserve: u64) -> Result<(), DbError> {
        let total = self
            .used()
            .checked_add(self.reserved)
            .and_then(|x| x.checked_add(need))
            .and_then(|x| x.checked_add(reserve))
            .ok_or_else(|| DbError::new(DbErrorKind::Overflow, "log size"))?;
        if total > self.max_bytes {
            return Err(DbError::new(
                DbErrorKind::LogFull,
                "write-ahead log limit reached; commit or abort and checkpoint",
            ));
        }
        self.reserved += reserve;
        Ok(())
    }
    pub fn release(&mut self, bytes: u64) {
        self.reserved = self.reserved.saturating_sub(bytes);
    }
    /// Buffers one record and returns its LSN. Space is checked by callers.
    pub fn append(&mut self, record: &LogRecord) -> Result<u64, DbError> {
        let payload = record.encode();
        if payload.len() > MAX_PAYLOAD {
            return Err(DbError::new(DbErrorKind::Limit, "log record too large"));
        }
        let lsn = self.next_lsn;
        let next = lsn
            .checked_add(1)
            .ok_or_else(|| DbError::new(DbErrorKind::Overflow, "LSN exhausted"))?;
        let len = payload.len() as u32;
        self.buffer.extend_from_slice(&len.to_le_bytes());
        self.buffer.extend_from_slice(&lsn.to_le_bytes());
        self.buffer
            .extend_from_slice(&frame_crc(self.seq, len, lsn, &payload).to_le_bytes());
        self.buffer.extend_from_slice(&payload);
        self.next_lsn = next;
        Ok(lsn)
    }
    /// Writes and syncs everything buffered.
    pub fn flush(&mut self, dev: &mut impl Device) -> Result<(), DbError> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        dev.write_at(FileId::Wal, self.end_offset, &self.buffer)?;
        dev.sync(FileId::Wal)?;
        self.end_offset += self.buffer.len() as u64;
        self.buffer.clear();
        self.durable_lsn = self.next_lsn - 1;
        Ok(())
    }
    /// The WAL rule: a page with `page_lsn` may be written only after this.
    pub fn flush_to(&mut self, dev: &mut impl Device, lsn: u64) -> Result<(), DbError> {
        if lsn > self.durable_lsn {
            self.flush(dev)?;
        }
        Ok(())
    }
    /// Starts a new empty generation at `next_lsn`. Callers must have made
    /// every page durable first. Crash-safe: until the new header is durable
    /// the old one wins; afterwards old frames fail the generation check.
    pub fn truncate(&mut self, dev: &mut impl Device) -> Result<(), DbError> {
        self.flush(dev)?;
        let seq = self
            .seq
            .checked_add(1)
            .ok_or_else(|| DbError::new(DbErrorKind::Overflow, "log generation"))?;
        // Generation 1 lives in slot 0, 2 in slot 1, 3 in slot 0, ...
        let slot = ((seq + 1) % 2) * 32;
        dev.write_at(FileId::Wal, slot, &encode_header(seq, self.next_lsn))?;
        dev.sync(FileId::Wal)?;
        dev.set_len(FileId::Wal, HEADER_BYTES)?;
        dev.sync(FileId::Wal)?;
        self.seq = seq;
        self.base_lsn = self.next_lsn;
        self.end_offset = HEADER_BYTES;
        self.durable_lsn = self.next_lsn - 1;
        Ok(())
    }
}
