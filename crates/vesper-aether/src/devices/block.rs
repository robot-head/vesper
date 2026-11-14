//! virtio-blk device model (virtio-v1.2 §5.2) over a bounded in-memory disk.
//!
//! One request queue, or up to [`MAX_QUEUES`] with `VIRTIO_BLK_F_MQ`
//! ([`BlockDevice::with_queues`]); every queue serves the same disk. Supported
//! request types: `IN`, `OUT`, `FLUSH`, `GET_ID`, and on a writable disk
//! `DISCARD` and `WRITE_ZEROES` (§5.2.6.2); anything else completes with
//! `VIRTIO_BLK_S_UNSUPP`. Offered features: `VIRTIO_BLK_F_BLK_SIZE`,
//! `VIRTIO_BLK_F_FLUSH`, `VIRTIO_BLK_F_MQ` (more than one queue),
//! `VIRTIO_BLK_F_DISCARD` and `VIRTIO_BLK_F_WRITE_ZEROES` (writable disks),
//! and `VIRTIO_BLK_F_RO` for a read-only disk.
//!
//! A discard or write-zeroes request carries an array of 16-byte segments
//! (`sector`, `num_sectors`, `flags`). Every segment is validated before any
//! is applied, so a request either takes effect whole or not at all: unknown
//! flag bits, and `UNMAP` on a discard, complete with `UNSUPP` (as §5.2.6.2
//! requires); a payload that is not whole segments, more segments than
//! `max_*_seg`, a segment above `max_*_sectors`, a misaligned discard, or a
//! range past the capacity completes with `IOERR`. Write-zeroes makes the range
//! read as zeros; discard also zeroes it (the specification leaves discarded
//! contents unspecified). Neither type is valid unless its feature was
//! negotiated. Requests that address sectors past
//! the capacity (with overflow checks), data lengths that are not whole
//! sectors, and writes to a read-only disk complete with `VIRTIO_BLK_S_IOERR`
//! and leave the disk unchanged. A chain without room for the status byte
//! cannot be answered in-band and is a device error (`DEVICE_NEEDS_RESET`).
#![forbid(unsafe_code)]

use super::mmio::{DeviceError, QueueIo, VirtioDevice};

pub const VIRTIO_ID_BLOCK: u32 = 2;
pub const SECTOR_BYTES: u64 = 512;
pub const VIRTIO_BLK_F_RO: u64 = 1 << 5;
pub const VIRTIO_BLK_F_BLK_SIZE: u64 = 1 << 6;
pub const VIRTIO_BLK_F_FLUSH: u64 = 1 << 9;
pub const VIRTIO_BLK_F_MQ: u64 = 1 << 12;
pub const VIRTIO_BLK_F_DISCARD: u64 = 1 << 13;
pub const VIRTIO_BLK_F_WRITE_ZEROES: u64 = 1 << 14;
pub const VIRTIO_BLK_T_IN: u32 = 0;
pub const VIRTIO_BLK_T_OUT: u32 = 1;
pub const VIRTIO_BLK_T_FLUSH: u32 = 4;
pub const VIRTIO_BLK_T_GET_ID: u32 = 8;
pub const VIRTIO_BLK_T_DISCARD: u32 = 11;
pub const VIRTIO_BLK_T_WRITE_ZEROES: u32 = 13;
/// `virtio_blk_discard_write_zeroes.flags`: the device may unmap the range.
pub const VIRTIO_BLK_WRITE_ZEROES_FLAG_UNMAP: u32 = 1;
/// One `struct virtio_blk_discard_write_zeroes` segment.
pub const SEGMENT_BYTES: usize = 16;
/// Limits the device advertises in its configuration space.
pub const MAX_DISCARD_SECTORS: u32 = 1 << 16;
pub const MAX_DISCARD_SEG: u32 = 16;
pub const DISCARD_SECTOR_ALIGNMENT: u32 = 8;
pub const MAX_WRITE_ZEROES_SECTORS: u32 = 1 << 16;
pub const MAX_WRITE_ZEROES_SEG: u32 = 16;
/// Request queues with `VIRTIO_BLK_F_MQ`.
pub const MAX_QUEUES: u16 = 8;
pub const VIRTIO_BLK_S_OK: u8 = 0;
pub const VIRTIO_BLK_S_IOERR: u8 = 1;
pub const VIRTIO_BLK_S_UNSUPP: u8 = 2;
/// `struct virtio_blk_req` header: type, reserved, sector.
pub const REQUEST_HEADER_BYTES: usize = 16;
/// Length of the `GET_ID` string.
pub const ID_BYTES: usize = 20;
/// Disk size ceiling for the in-memory model (256 MiB).
pub const MAX_DISK_BYTES: u64 = 256 << 20;
const QUEUE_SIZE_MAX: u16 = 256;
/// Configuration space through `write_zeroes_may_unmap` and its padding
/// (offsets 0..60).
const CONFIG_BYTES: usize = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskError {
    Empty,
    NotSectorMultiple,
    TooLarge,
    /// Zero or more than [`MAX_QUEUES`] request queues.
    QueueCount,
}

/// Per-device counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockStats {
    pub reads: u64,
    pub writes: u64,
    pub flushes: u64,
    pub io_errors: u64,
    pub unsupported: u64,
    pub discards: u64,
    pub write_zeroes: u64,
    /// Requests served, by queue index.
    pub per_queue: [u64; MAX_QUEUES as usize],
}

#[derive(Debug)]
pub struct BlockDevice {
    disk: Vec<u8>,
    read_only: bool,
    id: [u8; ID_BYTES],
    flushed: Vec<u8>,
    features: u64,
    queue_max: Vec<u16>,
    stats: BlockStats,
}

impl BlockDevice {
    pub fn new(disk: Vec<u8>, read_only: bool, id: &str) -> Result<Self, DiskError> {
        if disk.is_empty() {
            return Err(DiskError::Empty);
        }
        if !(disk.len() as u64).is_multiple_of(SECTOR_BYTES) {
            return Err(DiskError::NotSectorMultiple);
        }
        if disk.len() as u64 > MAX_DISK_BYTES {
            return Err(DiskError::TooLarge);
        }
        let mut idb = [0u8; ID_BYTES];
        let n = id.len().min(ID_BYTES);
        idb[..n].copy_from_slice(&id.as_bytes()[..n]);
        Ok(Self {
            flushed: disk.clone(),
            disk,
            read_only,
            id: idb,
            features: 0,
            queue_max: vec![QUEUE_SIZE_MAX],
            stats: BlockStats::default(),
        })
    }
    /// A device with `queues` request queues (`VIRTIO_BLK_F_MQ` if > 1).
    pub fn with_queues(mut self, queues: u16) -> Result<Self, DiskError> {
        if queues == 0 || queues > MAX_QUEUES {
            return Err(DiskError::QueueCount);
        }
        self.queue_max = vec![QUEUE_SIZE_MAX; usize::from(queues)];
        Ok(self)
    }
    pub fn num_queues(&self) -> u16 {
        self.queue_max.len() as u16
    }
    pub fn capacity_sectors(&self) -> u64 {
        self.disk.len() as u64 / SECTOR_BYTES
    }
    pub fn disk(&self) -> &[u8] {
        &self.disk
    }
    /// The disk contents as of the last completed `FLUSH`.
    pub fn durable(&self) -> &[u8] {
        &self.flushed
    }
    pub fn stats(&self) -> BlockStats {
        self.stats
    }

    /// Byte range of `len` bytes starting at `sector`, if inside the disk.
    fn span(&self, sector: u64, len: u64) -> Option<std::ops::Range<usize>> {
        if !len.is_multiple_of(SECTOR_BYTES) {
            return None;
        }
        let start = sector.checked_mul(SECTOR_BYTES)?;
        let end = start.checked_add(len)?;
        if end > self.disk.len() as u64 {
            return None;
        }
        Some(start as usize..end as usize)
    }

    /// Returns the bytes to write into the chain's writable part.
    fn handle(&mut self, readable: &[u8], writable: u64) -> Result<Vec<u8>, DeviceError> {
        if writable == 0 {
            return Err(DeviceError::Protocol(
                "virtio-blk request without status byte",
            ));
        }
        let data_in = writable - 1;
        // The status byte is always the last writable byte (§5.2.6).
        let status_only = |s: u8| {
            let mut v = vec![0u8; data_in as usize];
            v.push(s);
            v
        };
        if readable.len() < REQUEST_HEADER_BYTES {
            self.stats.io_errors += 1;
            return Ok(status_only(VIRTIO_BLK_S_IOERR));
        }
        let kind = u32::from_le_bytes(readable[0..4].try_into().expect("4 bytes"));
        let sector = u64::from_le_bytes(readable[8..16].try_into().expect("8 bytes"));
        let payload = &readable[REQUEST_HEADER_BYTES..];
        match kind {
            VIRTIO_BLK_T_IN => {
                if !payload.is_empty() {
                    self.stats.io_errors += 1;
                    return Ok(status_only(VIRTIO_BLK_S_IOERR));
                }
                match self.span(sector, data_in) {
                    Some(r) if data_in > 0 => {
                        self.stats.reads += 1;
                        let mut out = self.disk[r].to_vec();
                        out.push(VIRTIO_BLK_S_OK);
                        Ok(out)
                    }
                    _ => {
                        self.stats.io_errors += 1;
                        // The data part is left unwritten; only the status
                        // byte at the end is meaningful (`used.len` covers it).
                        let mut out = vec![0u8; data_in as usize];
                        out.push(VIRTIO_BLK_S_IOERR);
                        Ok(out)
                    }
                }
            }
            VIRTIO_BLK_T_OUT => {
                if data_in != 0 {
                    self.stats.io_errors += 1;
                    let mut out = vec![0u8; data_in as usize];
                    out.push(VIRTIO_BLK_S_IOERR);
                    return Ok(out);
                }
                let status = match self.span(sector, payload.len() as u64) {
                    _ if self.read_only => VIRTIO_BLK_S_IOERR,
                    Some(r) if !payload.is_empty() => {
                        self.disk[r].copy_from_slice(payload);
                        self.stats.writes += 1;
                        VIRTIO_BLK_S_OK
                    }
                    _ => VIRTIO_BLK_S_IOERR,
                };
                if status != VIRTIO_BLK_S_OK {
                    self.stats.io_errors += 1;
                }
                Ok(status_only(status))
            }
            VIRTIO_BLK_T_FLUSH if self.features & VIRTIO_BLK_F_FLUSH != 0 => {
                self.flushed.copy_from_slice(&self.disk);
                self.stats.flushes += 1;
                let mut out = vec![0u8; data_in as usize];
                out.push(VIRTIO_BLK_S_OK);
                Ok(out)
            }
            VIRTIO_BLK_T_GET_ID if data_in >= ID_BYTES as u64 => {
                let mut out = vec![0u8; data_in as usize];
                out[..ID_BYTES].copy_from_slice(&self.id);
                out.push(VIRTIO_BLK_S_OK);
                Ok(out)
            }
            VIRTIO_BLK_T_DISCARD if self.features & VIRTIO_BLK_F_DISCARD != 0 => {
                let status = self.discard_or_zero(payload, data_in, true);
                Ok(status_only(status))
            }
            VIRTIO_BLK_T_WRITE_ZEROES if self.features & VIRTIO_BLK_F_WRITE_ZEROES != 0 => {
                let status = self.discard_or_zero(payload, data_in, false);
                Ok(status_only(status))
            }
            _ => {
                self.stats.unsupported += 1;
                let mut out = vec![0u8; data_in as usize];
                out.push(VIRTIO_BLK_S_UNSUPP);
                Ok(out)
            }
        }
    }

    /// `DISCARD` / `WRITE_ZEROES` (§5.2.6.2): validates every segment, then
    /// zeroes them all.
    fn discard_or_zero(&mut self, payload: &[u8], data_in: u64, discard: bool) -> u8 {
        let (max_sectors, max_seg) = if discard {
            (MAX_DISCARD_SECTORS, MAX_DISCARD_SEG)
        } else {
            (MAX_WRITE_ZEROES_SECTORS, MAX_WRITE_ZEROES_SEG)
        };
        let ioerr = |stats: &mut BlockStats| {
            stats.io_errors += 1;
            VIRTIO_BLK_S_IOERR
        };
        if self.read_only {
            return ioerr(&mut self.stats);
        }
        if data_in != 0
            || payload.is_empty()
            || !payload.len().is_multiple_of(SEGMENT_BYTES)
            || payload.len() / SEGMENT_BYTES > max_seg as usize
        {
            return ioerr(&mut self.stats);
        }
        let mut spans = Vec::with_capacity(payload.len() / SEGMENT_BYTES);
        for seg in payload.chunks_exact(SEGMENT_BYTES) {
            let sector = u64::from_le_bytes(seg[0..8].try_into().expect("8 bytes"));
            let count = u32::from_le_bytes(seg[8..12].try_into().expect("4 bytes"));
            let flags = u32::from_le_bytes(seg[12..16].try_into().expect("4 bytes"));
            // Unknown flags, and UNMAP on a discard, are UNSUPP (§5.2.6.2).
            if flags & !VIRTIO_BLK_WRITE_ZEROES_FLAG_UNMAP != 0
                || (discard && flags & VIRTIO_BLK_WRITE_ZEROES_FLAG_UNMAP != 0)
            {
                self.stats.unsupported += 1;
                return VIRTIO_BLK_S_UNSUPP;
            }
            if count == 0
                || count > max_sectors
                || (discard
                    && (!sector.is_multiple_of(u64::from(DISCARD_SECTOR_ALIGNMENT))
                        || !count.is_multiple_of(DISCARD_SECTOR_ALIGNMENT)))
            {
                return ioerr(&mut self.stats);
            }
            match self.span(sector, u64::from(count) * SECTOR_BYTES) {
                Some(r) => spans.push(r),
                None => return ioerr(&mut self.stats),
            }
        }
        for r in spans {
            self.disk[r].fill(0);
        }
        if discard {
            self.stats.discards += 1;
        } else {
            self.stats.write_zeroes += 1;
        }
        VIRTIO_BLK_S_OK
    }
}

impl VirtioDevice for BlockDevice {
    fn device_type(&self) -> u32 {
        VIRTIO_ID_BLOCK
    }
    fn device_features(&self) -> u64 {
        VIRTIO_BLK_F_BLK_SIZE
            | VIRTIO_BLK_F_FLUSH
            | if self.read_only {
                VIRTIO_BLK_F_RO
            } else {
                VIRTIO_BLK_F_DISCARD | VIRTIO_BLK_F_WRITE_ZEROES
            }
            | if self.queue_max.len() > 1 {
                VIRTIO_BLK_F_MQ
            } else {
                0
            }
    }
    fn queue_max_sizes(&self) -> &[u16] {
        &self.queue_max
    }
    fn config_len(&self) -> usize {
        CONFIG_BYTES
    }
    fn read_config(&self, offset: usize, out: &mut [u8]) {
        let mut cfg = [0u8; CONFIG_BYTES];
        cfg[0..8].copy_from_slice(&self.capacity_sectors().to_le_bytes());
        // size_max (8) and seg_max (12) are not offered and read as zero.
        cfg[20..24].copy_from_slice(&(SECTOR_BYTES as u32).to_le_bytes());
        cfg[34..36].copy_from_slice(&self.num_queues().to_le_bytes());
        if !self.read_only {
            cfg[36..40].copy_from_slice(&MAX_DISCARD_SECTORS.to_le_bytes());
            cfg[40..44].copy_from_slice(&MAX_DISCARD_SEG.to_le_bytes());
            cfg[44..48].copy_from_slice(&DISCARD_SECTOR_ALIGNMENT.to_le_bytes());
            cfg[48..52].copy_from_slice(&MAX_WRITE_ZEROES_SECTORS.to_le_bytes());
            cfg[52..56].copy_from_slice(&MAX_WRITE_ZEROES_SEG.to_le_bytes());
            // write_zeroes_may_unmap: zeroing never unmaps in this model.
            cfg[56] = 0;
        }
        out.copy_from_slice(&cfg[offset..offset + out.len()]);
    }
    fn activate(&mut self, features: u64) {
        self.features = features;
    }
    fn reset(&mut self) {
        self.features = 0;
    }
    fn process(&mut self, queue: u16, io: &mut QueueIo<'_>) -> Result<(), DeviceError> {
        let q = usize::from(queue);
        if q >= self.queue_max.len() {
            return Err(DeviceError::Protocol("virtio-blk has no such queue"));
        }
        while let Some(chain) = io.pop() {
            self.stats.per_queue[q] += 1;
            let response = match self.handle(chain.readable(), chain.writable_len()) {
                Ok(r) => r,
                Err(e) => {
                    io.unpop(chain);
                    return Err(e);
                }
            };
            io.complete(chain, &response)?;
        }
        Ok(())
    }
}
