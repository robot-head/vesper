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

/// The virtio-blk device. The Vesper port (`vesper/src/block.ves`) parses
/// and serves every request over the disk and its flushed copy, and owns
/// the features, configuration and stats; this side keeps the disk bytes
/// and the queue loop.
#[derive(Debug)]
pub struct BlockDevice {
    disk: Vec<u8>,
    flushed: Vec<u8>,
    state: crate::binding::BlkState,
    queue_max: Vec<u16>,
    /// One response, reused.
    scratch: Vec<u8>,
}

impl From<crate::port_types::BlkStats> for BlockStats {
    fn from(s: crate::port_types::BlkStats) -> Self {
        BlockStats {
            reads: s.reads,
            writes: s.writes,
            flushes: s.flushes,
            io_errors: s.io_errors,
            unsupported: s.unsupported,
            discards: s.discards,
            write_zeroes: s.write_zeroes,
            per_queue: [s.q0, s.q1, s.q2, s.q3, s.q4, s.q5, s.q6, s.q7],
        }
    }
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
            state: crate::binding::blk_state_new(read_only, &idb, 1),
            queue_max: vec![QUEUE_SIZE_MAX],
            scratch: Vec::new(),
        })
    }
    /// A device with `queues` request queues (`VIRTIO_BLK_F_MQ` if > 1).
    pub fn with_queues(mut self, queues: u16) -> Result<Self, DiskError> {
        if queues == 0 || queues > MAX_QUEUES {
            return Err(DiskError::QueueCount);
        }
        crate::binding::blk_set_queues(&mut self.state, queues);
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
        crate::binding::blk_stats(&self.state).into()
    }

    /// One request on `queue`; the response is `self.scratch[..writable]`.
    fn handle(&mut self, queue: u16, readable: &[u8], writable: u64) -> Result<(), DeviceError> {
        let need = writable as usize;
        if self.scratch.len() < need {
            self.scratch.resize(need, 0);
        }
        let n = crate::binding::blk_request(
            &mut self.state,
            &mut self.disk,
            &mut self.flushed,
            queue,
            readable,
            writable,
            &mut self.scratch,
        );
        if n == u64::MAX {
            return Err(DeviceError::Protocol(
                "virtio-blk request without status byte",
            ));
        }
        Ok(())
    }

    /// One request outside the queues (not counted for any queue).
    /// Exposed for `tests/block_port.rs`.
    #[doc(hidden)]
    pub fn handle_for_port_test(
        &mut self,
        readable: &[u8],
        writable: u64,
    ) -> Result<Vec<u8>, DeviceError> {
        self.handle(u16::MAX, readable, writable)?;
        Ok(self.scratch[..writable as usize].to_vec())
    }
}

impl VirtioDevice for BlockDevice {
    fn device_type(&self) -> u32 {
        VIRTIO_ID_BLOCK
    }
    fn device_features(&self) -> u64 {
        crate::binding::blk_device_features(&self.state)
    }
    fn queue_max_sizes(&self) -> &[u16] {
        &self.queue_max
    }
    fn config_len(&self) -> usize {
        CONFIG_BYTES
    }
    fn read_config(&self, offset: usize, out: &mut [u8]) {
        let mut cfg = [0u8; CONFIG_BYTES];
        let read_only = crate::binding::blk_device_features(&self.state) & VIRTIO_BLK_F_RO != 0;
        crate::binding::blk_config_space(
            self.disk.len() as u64,
            read_only,
            self.num_queues(),
            &mut cfg,
        );
        out.copy_from_slice(&cfg[offset..offset + out.len()]);
    }
    fn activate(&mut self, features: u64) {
        crate::binding::blk_activate(&mut self.state, features);
    }
    fn reset(&mut self) {
        crate::binding::blk_activate(&mut self.state, 0);
    }
    fn process(&mut self, queue: u16, io: &mut QueueIo<'_>) -> Result<(), DeviceError> {
        if usize::from(queue) >= self.queue_max.len() {
            return Err(DeviceError::Protocol("virtio-blk has no such queue"));
        }
        while let Some(chain) = io.pop() {
            let writable = chain.writable_len();
            if let Err(e) = self.handle(queue, chain.readable(), writable) {
                io.unpop(chain);
                return Err(e);
            }
            io.complete(chain, &self.scratch[..writable as usize])?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod reference;
#[cfg(test)]
pub(crate) use reference::BlockDevice as ReferenceBlockDevice;
