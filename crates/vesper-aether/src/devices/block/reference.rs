//! The Rust virtio-blk device, kept only as the reference the Vesper
//! device (`super::BlockDevice`, `vesper/src/block.ves`) is checked against
//! (src/port_reference_tests/block.rs).
#![allow(dead_code)]

use super::*;
use crate::devices::mmio::{DeviceError, QueueIo, VirtioDevice};

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
    pub(crate) fn span(&self, sector: u64, len: u64) -> Option<std::ops::Range<usize>> {
        self.span_reference(sector, len)
    }

    /// The Rust implementation of `span`, exposed for differential tests.
    #[doc(hidden)]
    pub fn span_reference(&self, sector: u64, len: u64) -> Option<std::ops::Range<usize>> {
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

    /// Exposed for `tests/block_port.rs`.
    #[doc(hidden)]
    pub fn handle_for_port_test(
        &mut self,
        readable: &[u8],
        writable: u64,
    ) -> Result<Vec<u8>, DeviceError> {
        self.handle(readable, writable)
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
    pub(crate) fn discard_or_zero(&mut self, payload: &[u8], data_in: u64, discard: bool) -> u8 {
        self.discard_or_zero_reference(payload, data_in, discard)
    }
    /// The Rust implementation of `discard_or_zero`, exposed for differential tests.
    #[doc(hidden)]
    pub fn discard_or_zero_reference(&mut self, payload: &[u8], data_in: u64, discard: bool) -> u8 {
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
        for seg in payload.as_chunks::<SEGMENT_BYTES>().0 {
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
        {
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
