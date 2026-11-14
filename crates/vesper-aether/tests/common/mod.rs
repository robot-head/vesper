//! A minimal VirtIO 1.2 MMIO + packed-ring *driver*, written from the
//! specification, to exercise the device models without KVM.
#![allow(dead_code)]

use std::collections::BTreeMap;
use vesper_aether::devices::mmio::{MmioTransport, VirtioDevice, reg, status};
use vesper_aether::guest_mem::GuestMemory;
use vesper_aether::virtio::{FLAG_AVAIL, FLAG_NEXT, FLAG_USED, FLAG_WRITE, PackedDescriptor};

pub const RAM: (u64, u64) = (0, 1 << 20);

pub fn ram() -> GuestMemory {
    GuestMemory::new(&[RAM]).unwrap()
}

pub fn w32<D: VirtioDevice>(t: &mut MmioTransport<D>, m: &mut GuestMemory, off: u64, v: u32) {
    t.write(m, off, &v.to_le_bytes())
        .unwrap_or_else(|e| panic!("write {off:#x}={v:#x}: {e:?}"));
}
pub fn r32<D: VirtioDevice>(t: &mut MmioTransport<D>, off: u64) -> u32 {
    let mut b = [0u8; 4];
    t.read(off, &mut b)
        .unwrap_or_else(|e| panic!("read {off:#x}: {e:?}"));
    u32::from_le_bytes(b)
}

/// Driver side of one packed virtqueue.
#[derive(Debug)]
pub struct DriverQueue {
    pub index: u32,
    pub desc: u64,
    pub size: u16,
    next: u16,
    wrap: bool,
    used_next: u16,
    used_wrap: bool,
    free_ids: Vec<u16>,
    chain_len: BTreeMap<u16, u16>,
}

impl DriverQueue {
    pub fn new(index: u32, desc: u64, size: u16) -> Self {
        Self {
            index,
            desc,
            size,
            next: 0,
            wrap: true,
            used_next: 0,
            used_wrap: true,
            free_ids: (0..size).rev().collect(),
            chain_len: BTreeMap::new(),
        }
    }
    fn avail_flags(wrap: bool) -> u16 {
        if wrap { FLAG_AVAIL } else { FLAG_USED }
    }
    /// Makes a chain of `(gpa, len, device_writable)` buffers available.
    /// The head descriptor's flags are written last (§2.8.21).
    pub fn post(&mut self, m: &mut GuestMemory, bufs: &[(u64, u32, bool)]) -> u16 {
        let id = self.free_ids.pop().expect("free id");
        let head = self.next;
        let head_wrap = self.wrap;
        let mut head_bytes = None;
        for (i, &(gpa, len, write)) in bufs.iter().enumerate() {
            let mut flags = Self::avail_flags(self.wrap);
            if write {
                flags |= FLAG_WRITE;
            }
            if i + 1 < bufs.len() {
                flags |= FLAG_NEXT;
            }
            let d = PackedDescriptor {
                guest_address: gpa,
                byte_len: len,
                buffer_id: id,
                flags,
            }
            .encode();
            let at = self.desc + u64::from(self.next) * 16;
            if i == 0 {
                m.write(at, &d[..14]).unwrap();
                head_bytes = Some((at, d));
            } else {
                m.write(at, &d).unwrap();
            }
            self.next += 1;
            if self.next == self.size {
                self.next = 0;
                self.wrap = !self.wrap;
            }
        }
        let _ = (head, head_wrap);
        let (at, d) = head_bytes.expect("non-empty chain");
        m.write(at + 14, &d[14..]).unwrap();
        self.chain_len.insert(id, bufs.len() as u16);
        id
    }
    /// Chains posted and not yet used.
    pub fn outstanding(&self) -> usize {
        self.chain_len.len()
    }
    /// The next used element `(id, written_len)`, if the device published one.
    pub fn used(&mut self, m: &GuestMemory) -> Option<(u16, u32)> {
        let at = self.desc + u64::from(self.used_next) * 16;
        let mut b = [0u8; 16];
        m.read(at, &mut b).unwrap();
        let d = PackedDescriptor::decode(&b).unwrap();
        if !d.is_used(self.used_wrap) {
            return None;
        }
        let n = self.chain_len.remove(&d.buffer_id).expect("known id");
        for _ in 0..n {
            self.used_next += 1;
            if self.used_next == self.size {
                self.used_next = 0;
                self.used_wrap = !self.used_wrap;
            }
        }
        self.free_ids.push(d.buffer_id);
        Some((d.buffer_id, d.byte_len))
    }
}

/// Runs the §3.1.1 initialization sequence, accepting `features` (the
/// transport's VERSION_1 | RING_PACKED must be included by the caller), and
/// sets up `queues`. Returns the status read back after FEATURES_OK.
pub fn init<D: VirtioDevice>(
    t: &mut MmioTransport<D>,
    m: &mut GuestMemory,
    features: u64,
    queues: &[&DriverQueue],
) -> u32 {
    w32(t, m, reg::STATUS, 0);
    w32(t, m, reg::STATUS, status::ACKNOWLEDGE);
    w32(t, m, reg::STATUS, status::ACKNOWLEDGE | status::DRIVER);
    w32(t, m, reg::DRIVER_FEATURES_SEL, 0);
    w32(t, m, reg::DRIVER_FEATURES, features as u32);
    w32(t, m, reg::DRIVER_FEATURES_SEL, 1);
    w32(t, m, reg::DRIVER_FEATURES, (features >> 32) as u32);
    w32(
        t,
        m,
        reg::STATUS,
        status::ACKNOWLEDGE | status::DRIVER | status::FEATURES_OK,
    );
    let st = r32(t, reg::STATUS);
    if st & status::FEATURES_OK == 0 {
        return st;
    }
    for q in queues {
        // A fresh ring is zeroed (no descriptor is available).
        m.fill_zero(q.desc, u64::from(q.size) * 16).unwrap();
        w32(t, m, reg::QUEUE_SEL, q.index);
        assert!(r32(t, reg::QUEUE_NUM_MAX) >= u32::from(q.size));
        w32(t, m, reg::QUEUE_NUM, u32::from(q.size));
        w32(t, m, reg::QUEUE_DESC_LOW, q.desc as u32);
        w32(t, m, reg::QUEUE_DESC_HIGH, (q.desc >> 32) as u32);
        let driver = 0x8_0000 + u64::from(q.index) * 0x100;
        w32(t, m, reg::QUEUE_DRIVER_LOW, driver as u32);
        w32(t, m, reg::QUEUE_DRIVER_HIGH, 0);
        w32(t, m, reg::QUEUE_DEVICE_LOW, (driver + 0x80) as u32);
        w32(t, m, reg::QUEUE_DEVICE_HIGH, 0);
        w32(t, m, reg::QUEUE_READY, 1);
    }
    w32(
        t,
        m,
        reg::STATUS,
        status::ACKNOWLEDGE | status::DRIVER | status::FEATURES_OK | status::DRIVER_OK,
    );
    r32(t, reg::STATUS)
}
