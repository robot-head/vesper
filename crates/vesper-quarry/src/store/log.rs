//! Write-ahead log of the table store: two log regions in one file, a
//! ping-pong header, generation-bound frames and grouped page records.
//!
//! File layout: two 64-byte header slots at bytes 0 and 64, then region 0 at
//! byte 128 and region 1 right after it, each `region_bytes` long.
//!
//! | Header bytes | Field |
//! | --- | --- |
//! | 0..8 | magic `QRYWAL02` |
//! | 8..16 | header sequence (the valid slot with the larger one wins) |
//! | 16..24 | generation of the current region's frames |
//! | 24..32 | LSN of the current region's first frame |
//! | 32..40 | generation of the previous region (0: none needed) |
//! | 40..48 | LSN of the previous region's first frame |
//! | 48..56 | bytes of frames in the previous region |
//! | 56 | current region (0 or 1) |
//! | 57..60 | region size in KiB (u24 LE) |
//! | 60..64 | CRC32C of bytes 0..60 |
//!
//! A frame is `[len u32][lsn u64][crc u32][payload]`, the CRC32C covering the
//! region generation, `len`, `lsn` and the payload, so frames of an earlier
//! lap of a region fail their checksum. Records come in commit groups: page
//! records (full images or byte-range diffs) followed by one `Commit`.
//!
//! A fuzzy checkpoint *switches* regions (the old one becomes "previous" and
//! stays part of recovery), writes back the pages that were dirty at the
//! switch while commits continue in the new region, syncs the data file and
//! then *drops* the previous region from the header.
use super::error::{corrupt, overflow, StoreError, StoreErrorKind, StoreResult};
use crate::device::{Device, FileId};
use crate::page::{crc32c, crc32c_update, PAGE_SIZE};

pub const LOG_HEADER_BYTES: u64 = 128;
#[cfg(test)]
const MAGIC: &[u8; 8] = b"QRYWAL02";
const LOG_HDR_SLOT_LEN: usize = 64;
const LOG_HDR_CRC_OFF: usize = 60;
const LOG_HDR_FIELD_LEN: usize = 8;
const LOG_HDR_HSEQ: usize = 0;
const LOG_HDR_CUR_GEN: usize = 1;
const LOG_HDR_CUR_BASE: usize = 2;
const LOG_HDR_PREV_GEN: usize = 3;
const LOG_HDR_PREV_BASE: usize = 4;
const LOG_HDR_PREV_END: usize = 5;
const LOG_HDR_CUR_REGION: usize = 6;
const LOG_HDR_REGION_KIB: usize = 7;
const LOG_HDR_VALID: u64 = 1;
const FRAME_HEADER: usize = 16;
/// Region sizes: 64 KiB to 128 MiB.
#[cfg(test)]
pub(crate) const MIN_REGION_KIB: u32 = 64;
#[cfg(test)]
pub(crate) const MAX_REGION_KIB: u32 = 128 << 10;
/// Largest payload: a full page image plus its fixed fields.
pub const MAX_LOG_PAYLOAD: usize = PAGE_SIZE + 64;

/// One logged action of the table store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreRecord {
    /// The complete page after its change. Logged for a page's first change
    /// after a region switch (repairing torn page writes) and for new pages.
    /// Bytes 0..8 (page LSN) and 16..20 (checksum) are zero in the log.
    PageImage { page: u64, bytes: Vec<u8> },
    /// Byte ranges of a page that change from the page at `prev_lsn`.
    /// Redo applies only to a page whose LSN is exactly `prev_lsn`.
    PageDiff {
        page: u64,
        prev_lsn: u64,
        ranges: Vec<(u16, Vec<u8>)>,
    },
    /// Ends a commit group: a user transaction (`txn > 0`) with commit
    /// timestamp `ts`, or a system group (`txn == 0`: DDL, vacuum).
    Commit { txn: u64, ts: u64 },
}
impl StoreRecord {
    pub fn page(&self) -> Option<u64> {
        match self {
            Self::PageImage { page, .. } | Self::PageDiff { page, .. } => Some(*page),
            Self::Commit { .. } => None,
        }
    }
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Self::PageImage { page, bytes } => {
                out.push(1);
                out.extend_from_slice(&page.to_le_bytes());
                out.extend_from_slice(bytes);
            }
            Self::PageDiff {
                page,
                prev_lsn,
                ranges,
            } => {
                out.push(2);
                out.extend_from_slice(&page.to_le_bytes());
                out.extend_from_slice(&prev_lsn.to_le_bytes());
                out.extend_from_slice(&(ranges.len() as u16).to_le_bytes());
                for (offset, bytes) in ranges {
                    out.extend_from_slice(&offset.to_le_bytes());
                    out.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
                    out.extend_from_slice(bytes);
                }
            }
            Self::Commit { txn, ts } => {
                out.push(3);
                out.extend_from_slice(&txn.to_le_bytes());
                out.extend_from_slice(&ts.to_le_bytes());
            }
        }
        out
    }
    /// Strict decode: unknown tags, bad lengths, overlapping or out-of-page
    /// ranges and trailing bytes are `Corrupt`.
    pub fn decode(payload: &[u8]) -> StoreResult<Self> {
        let mut hdr = [0u64; 4];
        let mut stack_ranges = [0u64; 64 * 3];
        let mut heap_ranges = Vec::new();
        let mut status =
            crate::port::binding::store_log_record_decode(payload, &mut hdr, &mut stack_ranges);
        if status == 5 {
            heap_ranges.resize(payload.len(), 0);
            status =
                crate::port::binding::store_log_record_decode(payload, &mut hdr, &mut heap_ranges);
        }
        let ranges_buf: &[u64] = if heap_ranges.is_empty() {
            &stack_ranges
        } else {
            &heap_ranges
        };
        match status {
            0 => {}
            1 => return Err(corrupt("truncated store log record")),
            2 => return Err(corrupt("unknown store log record tag")),
            3 => return Err(corrupt("page diff range")),
            4 => return Err(corrupt("trailing store log record bytes")),
            _ => return Err(corrupt("store log record")),
        }
        match hdr[0] {
            1 => Ok(Self::PageImage {
                page: hdr[1],
                bytes: payload[9..9 + PAGE_SIZE].to_vec(),
            }),
            2 => {
                let count = hdr[3] as usize;
                let mut ranges = Vec::with_capacity(count);
                for i in 0..count {
                    let rb = i * 3;
                    let off = ranges_buf[rb] as u16;
                    let len = ranges_buf[rb + 1] as usize;
                    let at = ranges_buf[rb + 2] as usize;
                    ranges.push((off, payload[at..at + len].to_vec()));
                }
                Ok(Self::PageDiff {
                    page: hdr[1],
                    prev_lsn: hdr[2],
                    ranges,
                })
            }
            _ => Ok(Self::Commit {
                txn: hdr[1],
                ts: hdr[2],
            }),
        }
    }
}

/// Why the current region's frames ended before the end of the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogTail {
    TornHeader,
    TornPayload,
    Oversize,
    BadChecksum,
    OutOfSequence,
    /// The region is full up to its end (no defect).
    RegionEnd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Header {
    hseq: u64,
    cur_gen: u64,
    cur_base: u64,
    prev_gen: u64,
    prev_base: u64,
    prev_end: u64,
    cur_region: u8,
    region_kib: u32,
}
impl Header {
    fn encode(&self) -> [u8; LOG_HDR_SLOT_LEN] {
        let fields = [
            self.hseq,
            self.cur_gen,
            self.cur_base,
            self.prev_gen,
            self.prev_base,
            self.prev_end,
            u64::from(self.cur_region),
            u64::from(self.region_kib),
        ];
        let mut h = [0u8; LOG_HDR_SLOT_LEN];
        crate::port::binding::store_log_header_encode(&fields, &mut h);
        let crc = crc32c(&h[..LOG_HDR_CRC_OFF]);
        h[LOG_HDR_CRC_OFF..LOG_HDR_SLOT_LEN].copy_from_slice(&crc.to_le_bytes());
        h
    }
    fn decode(h: &[u8]) -> Option<Self> {
        let mut fields = [0u64; LOG_HDR_FIELD_LEN];
        if crate::port::binding::store_log_header_decode(h, &mut fields) != LOG_HDR_VALID {
            return None;
        }
        let stored_crc = u32::from_le_bytes(h[LOG_HDR_CRC_OFF..LOG_HDR_SLOT_LEN].try_into().ok()?);
        if crc32c(&h[..LOG_HDR_CRC_OFF]) != stored_crc {
            return None;
        }
        Some(Self {
            hseq: fields[LOG_HDR_HSEQ],
            cur_gen: fields[LOG_HDR_CUR_GEN],
            cur_base: fields[LOG_HDR_CUR_BASE],
            prev_gen: fields[LOG_HDR_PREV_GEN],
            prev_base: fields[LOG_HDR_PREV_BASE],
            prev_end: fields[LOG_HDR_PREV_END],
            cur_region: fields[LOG_HDR_CUR_REGION] as u8,
            region_kib: fields[LOG_HDR_REGION_KIB] as u32,
        })
    }
}

fn frame_crc(gen: u64, len: u32, lsn: u64, payload: &[u8]) -> u32 {
    let mut c = crc32c_update(!0, &gen.to_le_bytes());
    c = crc32c_update(c, &len.to_le_bytes());
    c = crc32c_update(c, &lsn.to_le_bytes());
    !crc32c_update(c, payload)
}

/// Result of reading the log at open.
#[derive(Debug)]
pub(crate) struct LogScan {
    /// Records of the previous region (if still needed) then the current one.
    pub records: Vec<(u64, StoreRecord)>,
    pub tail: Option<LogTail>,
    /// Region switch state found in the header.
    pub prev_needed: bool,
    pub hseq: u64,
    pub next_lsn: u64,
    pub cur_gen: u64,
}

struct Frames {
    records: Vec<(u64, StoreRecord)>,
    end: u64,
    tail: Option<LogTail>,
}
/// Reads frames of generation `gen` from `bytes` (a region) starting at LSN
/// `base`, stopping at the first defect.
fn read_frames(bytes: &[u8], gen: u64, base: u64) -> StoreResult<Frames> {
    let mut at = 0usize;
    let mut expected = base;
    let mut records = Vec::new();
    let tail = loop {
        if at == bytes.len() {
            break Some(LogTail::RegionEnd);
        }
        let Some(h) = bytes.get(at..at + FRAME_HEADER) else {
            break Some(LogTail::TornHeader);
        };
        let len = u32::from_le_bytes(h[..4].try_into().expect("4"));
        let lsn = u64::from_le_bytes(h[4..12].try_into().expect("8"));
        let crc = u32::from_le_bytes(h[12..16].try_into().expect("4"));
        if len as usize > MAX_LOG_PAYLOAD {
            break Some(LogTail::Oversize);
        }
        let start = at + FRAME_HEADER;
        let Some(payload) = bytes.get(start..start + len as usize) else {
            break Some(LogTail::TornPayload);
        };
        if frame_crc(gen, len, lsn, payload) != crc {
            break Some(LogTail::BadChecksum);
        }
        if lsn != expected {
            break Some(LogTail::OutOfSequence);
        }
        records.push((lsn, StoreRecord::decode(payload)?));
        expected = expected
            .checked_add(1)
            .ok_or_else(|| overflow("LSN exhausted"))?;
        at = start + len as usize;
    };
    Ok(Frames {
        records,
        end: at as u64,
        tail,
    })
}

/// The log state of an open store.
#[derive(Debug)]
pub(crate) struct StoreLog {
    region_bytes: u64,
    hseq: u64,
    cur_region: u8,
    cur_gen: u64,
    cur_base: u64,
    /// Bytes of frames written to the current region (excluding the buffer).
    cur_end: u64,
    prev: Option<(u64, u64, u64)>, // (gen, base, end)
    next_lsn: u64,
    durable_lsn: u64,
    buffer: Vec<u8>,
    flushes: u64,
}

fn region_offset(region: u8, region_bytes: u64) -> u64 {
    LOG_HEADER_BYTES + u64::from(region) * region_bytes
}

/// Reads the log. `Ok(None)`: no valid header (a new or destroyed log).
pub(crate) fn scan(dev: &mut impl Device) -> StoreResult<Option<LogScan>> {
    let file_len = dev.len(FileId::Wal)?;
    if file_len < LOG_HEADER_BYTES {
        return Ok(None);
    }
    let mut head = [0u8; LOG_HEADER_BYTES as usize];
    dev.read_at(FileId::Wal, 0, &mut head)?;
    let Some(h) = [Header::decode(&head[..64]), Header::decode(&head[64..])]
        .into_iter()
        .flatten()
        .max_by_key(|h| h.hseq)
    else {
        return Ok(None);
    };
    let region_bytes = u64::from(h.region_kib) << 10;
    let read_region = |dev: &mut dyn FnMut(u64, &mut [u8]) -> StoreResult<()>,
                       region: u8,
                       want: u64|
     -> StoreResult<Vec<u8>> {
        let start = region_offset(region, region_bytes);
        let avail = file_len.saturating_sub(start).min(region_bytes).min(want);
        let mut bytes = vec![0u8; avail as usize];
        if avail > 0 {
            dev(start, &mut bytes)?;
        }
        Ok(bytes)
    };
    let mut read = |offset: u64, buf: &mut [u8]| -> StoreResult<()> {
        dev.read_at(FileId::Wal, offset, buf)
            .map_err(StoreError::from)
    };
    let mut records = Vec::new();
    let prev_needed = h.prev_gen != 0;
    if prev_needed {
        if h.prev_end > region_bytes {
            return Err(corrupt("previous log region length"));
        }
        let bytes = read_region(&mut read, 1 - h.cur_region, h.prev_end)?;
        if (bytes.len() as u64) < h.prev_end {
            return Err(corrupt("previous log region is short"));
        }
        let frames = read_frames(&bytes, h.prev_gen, h.prev_base)?;
        if frames.end != h.prev_end {
            return Err(corrupt("previous log region has a defect"));
        }
        let next = h.prev_base + frames.records.len() as u64;
        if next != h.cur_base {
            return Err(corrupt("log regions are not contiguous"));
        }
        records = frames.records;
    }
    let bytes = read_region(&mut read, h.cur_region, region_bytes)?;
    let frames = read_frames(&bytes, h.cur_gen, h.cur_base)?;
    let next_lsn = h.cur_base + frames.records.len() as u64;
    let tail = match frames.tail {
        // Bytes past the last frame that are all zero (or the file end)
        // are free space, not a defect.
        Some(LogTail::TornHeader) | Some(LogTail::BadChecksum) | Some(LogTail::Oversize)
            if bytes[frames.end as usize..].iter().all(|&b| b == 0) =>
        {
            None
        }
        Some(LogTail::RegionEnd) if (bytes.len() as u64) < region_bytes => None,
        other => other,
    };
    records.extend(frames.records);
    Ok(Some(LogScan {
        records,
        tail,
        prev_needed,
        hseq: h.hseq,
        next_lsn,
        cur_gen: h.cur_gen,
    }))
}

impl StoreLog {
    /// Starts a fresh log in region 0 whose first frame gets `next_lsn`.
    /// Earlier frames become invalid (older generations). Used for a new
    /// store and at the end of recovery, after every page is durable.
    pub fn reset(
        dev: &mut impl Device,
        region_bytes: u64,
        hseq: u64,
        next_lsn: u64,
    ) -> StoreResult<Self> {
        let gen = hseq
            .checked_add(1)
            .ok_or_else(|| overflow("log generation"))?;
        let mut log = Self {
            region_bytes,
            hseq,
            cur_region: 0,
            cur_gen: gen,
            cur_base: next_lsn,
            cur_end: 0,
            prev: None,
            next_lsn,
            durable_lsn: next_lsn - 1,
            buffer: Vec::new(),
            flushes: 0,
        };
        log.write_header(dev)?;
        Ok(log)
    }
    /// The scanned log while recovery replays it: everything read is
    /// durable (recovery syncs the log first), nothing is appended.
    pub fn recovering(scan: &LogScan, region_bytes: u64) -> Self {
        Self {
            region_bytes,
            hseq: scan.hseq,
            cur_region: 0,
            cur_gen: scan.cur_gen,
            cur_base: scan.next_lsn,
            cur_end: 0,
            prev: None,
            next_lsn: scan.next_lsn,
            durable_lsn: scan.next_lsn - 1,
            buffer: Vec::new(),
            flushes: 0,
        }
    }
    fn write_header(&mut self, dev: &mut impl Device) -> StoreResult<()> {
        let hseq = self
            .hseq
            .checked_add(1)
            .ok_or_else(|| overflow("log header sequence"))?;
        let (prev_gen, prev_base, prev_end) = self.prev.unwrap_or((0, 0, 0));
        let header = Header {
            hseq,
            cur_gen: self.cur_gen,
            cur_base: self.cur_base,
            prev_gen,
            prev_base,
            prev_end,
            cur_region: self.cur_region,
            region_kib: (self.region_bytes >> 10) as u32,
        };
        dev.write_at(FileId::Wal, (hseq % 2) * 64, &header.encode())?;
        dev.sync(FileId::Wal)?;
        self.hseq = hseq;
        Ok(())
    }
    pub fn next_lsn(&self) -> u64 {
        self.next_lsn
    }
    pub fn durable_lsn(&self) -> u64 {
        self.durable_lsn
    }
    pub fn flushes(&self) -> u64 {
        self.flushes
    }
    pub fn region_bytes(&self) -> u64 {
        self.region_bytes
    }
    /// Bytes used in the current region, including the unflushed buffer.
    pub fn region_used(&self) -> u64 {
        self.cur_end + self.buffer.len() as u64
    }
    /// Buffers a group of records atomically (all or none fit the region)
    /// and returns the LSN of each.
    pub fn append_group(&mut self, records: &[StoreRecord]) -> StoreResult<Vec<u64>> {
        let payloads: Vec<Vec<u8>> = records.iter().map(StoreRecord::encode).collect();
        let bytes: u64 = payloads
            .iter()
            .map(|p| (FRAME_HEADER + p.len()) as u64)
            .sum();
        if payloads.iter().any(|p| p.len() > MAX_LOG_PAYLOAD) {
            return Err(StoreError::new(
                StoreErrorKind::Limit,
                "log record too large",
            ));
        }
        if self.region_used() + bytes > self.region_bytes {
            return Err(StoreError::new(
                StoreErrorKind::LogFull,
                "commit group does not fit the log region",
            ));
        }
        let last = self
            .next_lsn
            .checked_add(payloads.len() as u64)
            .ok_or_else(|| overflow("LSN exhausted"))?;
        let mut lsns = Vec::with_capacity(payloads.len());
        for payload in payloads {
            let lsn = self.next_lsn;
            let len = payload.len() as u32;
            self.buffer.extend_from_slice(&len.to_le_bytes());
            self.buffer.extend_from_slice(&lsn.to_le_bytes());
            self.buffer
                .extend_from_slice(&frame_crc(self.cur_gen, len, lsn, &payload).to_le_bytes());
            self.buffer.extend_from_slice(&payload);
            self.next_lsn += 1;
            lsns.push(lsn);
        }
        debug_assert_eq!(self.next_lsn, last);
        Ok(lsns)
    }
    /// Writes the whole buffer in one write and syncs the log file.
    pub fn flush(&mut self, dev: &mut impl Device) -> StoreResult<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let at = region_offset(self.cur_region, self.region_bytes) + self.cur_end;
        dev.write_at(FileId::Wal, at, &self.buffer)?;
        dev.sync(FileId::Wal)?;
        self.cur_end += self.buffer.len() as u64;
        self.buffer.clear();
        self.durable_lsn = self.next_lsn - 1;
        self.flushes += 1;
        Ok(())
    }
    /// The WAL rule: a page with `page_lsn` may be written only after this.
    pub fn flush_to(&mut self, dev: &mut impl Device, lsn: u64) -> StoreResult<()> {
        if lsn > self.durable_lsn {
            self.flush(dev)?;
        }
        Ok(())
    }
    /// Checkpoint begin: the current region becomes the previous one (still
    /// replayed by recovery) and appends continue in the other region under
    /// a new generation. Requires that no previous region is still needed.
    pub fn switch(&mut self, dev: &mut impl Device) -> StoreResult<()> {
        if self.prev.is_some() {
            return Err(StoreError::new(
                StoreErrorKind::State,
                "previous log region still needed",
            ));
        }
        self.flush(dev)?;
        let gen = self
            .hseq
            .checked_add(1)
            .ok_or_else(|| overflow("log generation"))?;
        self.prev = Some((self.cur_gen, self.cur_base, self.cur_end));
        self.cur_region = 1 - self.cur_region;
        self.cur_gen = gen;
        self.cur_base = self.next_lsn;
        self.cur_end = 0;
        // Frames an older lap left in the new region fail the generation
        // check, so nothing needs to be erased.
        self.write_header(dev)
    }
    /// Checkpoint end: every page changed by the previous region is durable
    /// in the data file, so recovery no longer needs it.
    pub fn drop_prev(&mut self, dev: &mut impl Device) -> StoreResult<()> {
        if self.prev.is_none() {
            return Ok(());
        }
        self.prev = None;
        self.write_header(dev)
    }
}

#[cfg(test)]
mod header_differential {
    use super::*;

    fn encode_reference(hdr: &Header) -> [u8; 64] {
        let mut h = [0u8; 64];
        h[..8].copy_from_slice(MAGIC);
        for (i, v) in [
            hdr.hseq,
            hdr.cur_gen,
            hdr.cur_base,
            hdr.prev_gen,
            hdr.prev_base,
            hdr.prev_end,
        ]
        .iter()
        .enumerate()
        {
            h[8 + 8 * i..16 + 8 * i].copy_from_slice(&v.to_le_bytes());
        }
        h[56] = hdr.cur_region;
        h[57..60].copy_from_slice(&hdr.region_kib.to_le_bytes()[..3]);
        let crc = crc32c(&h[..60]);
        h[60..64].copy_from_slice(&crc.to_le_bytes());
        h
    }

    fn decode_reference(h: &[u8]) -> Option<Header> {
        if h.len() != 64 || &h[..8] != MAGIC || h[56] > 1 {
            return None;
        }
        if crc32c(&h[..60]) != u32::from_le_bytes(h[60..64].try_into().ok()?) {
            return None;
        }
        let u = |i: usize| u64::from_le_bytes(h[8 + 8 * i..16 + 8 * i].try_into().expect("8"));
        let header = Header {
            hseq: u(0),
            cur_gen: u(1),
            cur_base: u(2),
            prev_gen: u(3),
            prev_base: u(4),
            prev_end: u(5),
            cur_region: h[56],
            region_kib: u32::from_le_bytes([h[57], h[58], h[59], 0]),
        };
        let region_ok = (MIN_REGION_KIB..=MAX_REGION_KIB).contains(&header.region_kib);
        (header.hseq > 0 && header.cur_gen > 0 && header.cur_base > 0 && region_ok)
            .then_some(header)
    }

    #[test]
    fn store_log_header_matches_reference() {
        const DIFF_CASES: usize = 256;
        let mut state: u64 = 0x6c6f_6768_1234_5678;
        let mut next = || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            state >> 32
        };
        for case in 0..DIFF_CASES {
            let hdr = Header {
                hseq: 1 + next(),
                cur_gen: 1 + next(),
                cur_base: 1 + next(),
                prev_gen: next(),
                prev_base: next(),
                prev_end: next(),
                cur_region: (next() % 2) as u8,
                region_kib: MIN_REGION_KIB + (next() as u32 % (MAX_REGION_KIB - MIN_REGION_KIB)),
            };
            let ep = hdr.encode();
            let er = encode_reference(&hdr);
            assert_eq!(ep, er, "case {case} encode");
            assert_eq!(
                Header::decode(&ep),
                decode_reference(&er),
                "case {case} decode"
            );

            let mut bad = ep;
            let at = (next() % 64) as usize;
            bad[at] ^= 1 << (next() % 8);
            assert_eq!(
                Header::decode(&bad),
                decode_reference(&bad),
                "case {case} bad"
            );
        }
    }

    #[test]
    #[ignore = "ABBA benchmark; run in release with --ignored --nocapture"]
    fn abba_store_log_header_decode() {
        use std::hint::black_box;
        use std::time::Instant;

        const ABBA_EPOCHS: usize = 30;
        const ABBA_WARMUP: usize = 2;
        const ABBA_ITERS: usize = 20_000;
        const SIGN_ALPHA_HALF: f64 = 0.025;
        const FLIP_LIMIT: f64 = 1.50;

        let hdr = Header {
            hseq: 7,
            cur_gen: 3,
            cur_base: 100,
            prev_gen: 2,
            prev_base: 50,
            prev_end: 4096,
            cur_region: 1,
            region_kib: 1024,
        };
        let bytes = hdr.encode();

        let mut ratios = Vec::with_capacity(ABBA_EPOCHS);
        for epoch in 0..(ABBA_WARMUP + ABBA_EPOCHS) {
            let t0 = Instant::now();
            for _ in 0..ABBA_ITERS {
                black_box(decode_reference(black_box(&bytes)).unwrap());
            }
            let r1 = t0.elapsed().as_nanos();

            let t1 = Instant::now();
            for _ in 0..ABBA_ITERS {
                black_box(Header::decode(black_box(&bytes)).unwrap());
            }
            let p1 = t1.elapsed().as_nanos();

            let t2 = Instant::now();
            for _ in 0..ABBA_ITERS {
                black_box(Header::decode(black_box(&bytes)).unwrap());
            }
            let p2 = t2.elapsed().as_nanos();

            let t3 = Instant::now();
            for _ in 0..ABBA_ITERS {
                black_box(decode_reference(black_box(&bytes)).unwrap());
            }
            let r2 = t3.elapsed().as_nanos();

            if epoch >= ABBA_WARMUP {
                ratios.push((p1 + p2) as f64 / (r1 + r2) as f64);
            }
        }
        ratios.sort_by(f64::total_cmp);
        let mid = ratios.len() / 2;
        let mut cdf = 0.0;
        let mut choose = 1.0;
        let mut k = 0usize;
        for j in 0..ratios.len() {
            cdf += choose * 0.5f64.powi(ratios.len() as i32);
            if cdf > SIGN_ALPHA_HALF {
                break;
            }
            k = j + 1;
            choose = choose * (ratios.len() - j) as f64 / (j + 1) as f64;
        }
        let i = k.saturating_sub(1).min(mid);
        let (ci_lo, ci_hi) = (ratios[i], ratios[ratios.len() - 1 - i]);
        println!(
            "ABBA store_log_header_decode: median {:.3}x [{ci_lo:.3}x, {ci_hi:.3}x]",
            ratios[mid]
        );
        assert!(
            ci_hi <= FLIP_LIMIT,
            "ABBA 95% upper bound {ci_hi:.3}x exceeds {FLIP_LIMIT:.2}x"
        );
    }
}
