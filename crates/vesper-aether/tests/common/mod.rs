//! A minimal VirtIO 1.2 MMIO *driver* for split (§2.7) and packed (§2.8)
//! rings, written from the specification, to exercise the device models
//! without KVM. It supports indirect tables, notification suppression
//! (`avail.flags` / `used_event`, packed driver event structure), the
//! `IN_ORDER` batch convention and `NOTIFICATION_DATA` values.
#![allow(dead_code)]

use std::collections::{BTreeMap, VecDeque};
use vesper_aether::devices::mmio::{MmioTransport, VirtioDevice, reg, status};
use vesper_aether::devices::queue::{
    RING_EVENT_FLAGS_DESC, RING_EVENT_FLAGS_DISABLE, RING_EVENT_FLAGS_ENABLE, RingFormat,
    VIRTQ_AVAIL_F_NO_INTERRUPT,
};
use vesper_aether::guest_mem::GuestMemory;
use vesper_aether::virtio::{
    FLAG_AVAIL, FLAG_INDIRECT, FLAG_NEXT, FLAG_USED, FLAG_WRITE, PackedDescriptor,
};

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

/// Encodes a split descriptor (`struct virtq_desc`).
pub fn split_desc(addr: u64, len: u32, flags: u16, next: u16) -> [u8; 16] {
    let mut d = [0u8; 16];
    d[0..8].copy_from_slice(&addr.to_le_bytes());
    d[8..12].copy_from_slice(&len.to_le_bytes());
    d[12..14].copy_from_slice(&flags.to_le_bytes());
    d[14..16].copy_from_slice(&next.to_le_bytes());
    d
}

#[derive(Debug)]
struct Posted {
    id: u16,
    slots: u16,
    writable: u32,
    descs: Vec<u16>,
}

/// Driver side of one virtqueue.
#[derive(Debug)]
pub struct DriverQueue {
    pub format: RingFormat,
    pub index: u32,
    pub desc: u64,
    pub driver: u64,
    pub device: u64,
    pub size: u16,
    /// Set by the caller when IN_ORDER was negotiated.
    pub in_order: bool,
    // Packed.
    next: u16,
    wrap: bool,
    used_next: u16,
    used_wrap: bool,
    free_ids: Vec<u16>,
    // Split.
    free_descs: Vec<u16>,
    avail_idx: u16,
    last_used: u16,
    outstanding: VecDeque<Posted>,
    expanded: VecDeque<(u16, u32)>,
    by_id: BTreeMap<u16, ()>,
}

impl DriverQueue {
    /// A packed queue (the format of the wave-seven tests).
    pub fn new(index: u32, desc: u64, size: u16) -> Self {
        Self::with_format(RingFormat::Packed, index, desc, size)
    }
    pub fn split(index: u32, desc: u64, size: u16) -> Self {
        Self::with_format(RingFormat::Split, index, desc, size)
    }
    pub fn with_format(format: RingFormat, index: u32, desc: u64, size: u16) -> Self {
        let driver = 0x8_0000 + u64::from(index) * 0x2000;
        Self {
            format,
            index,
            desc,
            driver,
            device: driver + 0x1000,
            size,
            in_order: false,
            next: 0,
            wrap: true,
            used_next: 0,
            used_wrap: true,
            free_ids: (0..size).rev().collect(),
            free_descs: (0..size).rev().collect(),
            avail_idx: 0,
            last_used: 0,
            outstanding: VecDeque::new(),
            expanded: VecDeque::new(),
            by_id: BTreeMap::new(),
        }
    }

    /// Zeroes the three areas (a fresh ring has nothing available).
    pub fn clear(&self, m: &mut GuestMemory) {
        m.fill_zero(self.desc, u64::from(self.size) * 16).unwrap();
        m.fill_zero(self.driver, 0x1000).unwrap();
        m.fill_zero(self.device, 0x1000).unwrap();
    }

    fn packed_flags(&self, write: bool, next: bool) -> u16 {
        let mut f = if self.wrap { FLAG_AVAIL } else { FLAG_USED };
        if write {
            f |= FLAG_WRITE;
        }
        if next {
            f |= FLAG_NEXT;
        }
        f
    }
    fn packed_advance(&mut self) {
        self.next += 1;
        if self.next == self.size {
            self.next = 0;
            self.wrap = !self.wrap;
        }
    }

    /// Makes a chain of `(gpa, len, device_writable)` buffers available.
    /// Packed: the head descriptor's flags are written last (§2.8.21).
    /// Split: the ring entry, then `avail.idx` (§2.7.13).
    pub fn post(&mut self, m: &mut GuestMemory, bufs: &[(u64, u32, bool)]) -> u16 {
        let writable: u32 = bufs.iter().filter(|b| b.2).map(|b| b.1).sum();
        match self.format {
            RingFormat::Packed => {
                let id = self.free_ids.pop().expect("free id");
                let mut head = None;
                for (i, &(gpa, len, write)) in bufs.iter().enumerate() {
                    let d = PackedDescriptor {
                        guest_address: gpa,
                        byte_len: len,
                        buffer_id: id,
                        flags: self.packed_flags(write, i + 1 < bufs.len()),
                    }
                    .encode();
                    let at = self.desc + u64::from(self.next) * 16;
                    if i == 0 {
                        m.write(at, &d[..14]).unwrap();
                        head = Some((at, d));
                    } else {
                        m.write(at, &d).unwrap();
                    }
                    self.packed_advance();
                }
                let (at, d) = head.expect("non-empty chain");
                m.write(at + 14, &d[14..]).unwrap();
                self.track(id, bufs.len() as u16, writable, Vec::new());
                id
            }
            RingFormat::Split => {
                let descs: Vec<u16> = (0..bufs.len())
                    .map(|_| self.free_descs.pop().expect("free descriptor"))
                    .collect();
                for (i, &(gpa, len, write)) in bufs.iter().enumerate() {
                    let mut flags = if write { FLAG_WRITE } else { 0 };
                    let next = if i + 1 < bufs.len() {
                        flags |= FLAG_NEXT;
                        descs[i + 1]
                    } else {
                        0
                    };
                    m.write(
                        self.desc + u64::from(descs[i]) * 16,
                        &split_desc(gpa, len, flags, next),
                    )
                    .unwrap();
                }
                let head = descs[0];
                self.publish_split(m, head);
                self.track(head, 1, writable, descs);
                head
            }
        }
    }

    /// Makes one indirect descriptor available whose table (at `table`) holds
    /// `bufs` (§2.7.5.3 split, §2.8.19 packed).
    pub fn post_indirect(
        &mut self,
        m: &mut GuestMemory,
        table: u64,
        bufs: &[(u64, u32, bool)],
    ) -> u16 {
        let writable: u32 = bufs.iter().filter(|b| b.2).map(|b| b.1).sum();
        let len = (bufs.len() * 16) as u32;
        match self.format {
            RingFormat::Split => {
                for (i, &(gpa, l, write)) in bufs.iter().enumerate() {
                    let mut flags = if write { FLAG_WRITE } else { 0 };
                    if i + 1 < bufs.len() {
                        flags |= FLAG_NEXT;
                    }
                    m.write(
                        table + i as u64 * 16,
                        &split_desc(gpa, l, flags, i as u16 + 1),
                    )
                    .unwrap();
                }
                let head = self.free_descs.pop().expect("free descriptor");
                m.write(
                    self.desc + u64::from(head) * 16,
                    &split_desc(table, len, FLAG_INDIRECT, 0),
                )
                .unwrap();
                self.publish_split(m, head);
                self.track(head, 1, writable, vec![head]);
                head
            }
            RingFormat::Packed => {
                for (i, &(gpa, l, write)) in bufs.iter().enumerate() {
                    let d = PackedDescriptor {
                        guest_address: gpa,
                        byte_len: l,
                        buffer_id: 0,
                        flags: if write { FLAG_WRITE } else { 0 },
                    }
                    .encode();
                    m.write(table + i as u64 * 16, &d).unwrap();
                }
                let id = self.free_ids.pop().expect("free id");
                let d = PackedDescriptor {
                    guest_address: table,
                    byte_len: len,
                    buffer_id: id,
                    flags: self.packed_flags(false, false) | FLAG_INDIRECT,
                }
                .encode();
                let at = self.desc + u64::from(self.next) * 16;
                m.write(at, &d[..14]).unwrap();
                m.write(at + 14, &d[14..]).unwrap();
                self.packed_advance();
                self.track(id, 1, writable, Vec::new());
                id
            }
        }
    }

    /// Writes a raw split descriptor and publishes `head` (for malformed
    /// chains). The descriptors named must not be in use.
    pub fn post_raw_split(&mut self, m: &mut GuestMemory, descs: &[(u16, [u8; 16])], head: u16) {
        for (i, d) in descs {
            m.write(self.desc + u64::from(*i) * 16, d).unwrap();
        }
        self.publish_split(m, head);
    }

    fn publish_split(&mut self, m: &mut GuestMemory, head: u16) {
        let slot = u64::from(self.avail_idx % self.size);
        m.write(self.driver + 4 + 2 * slot, &head.to_le_bytes())
            .unwrap();
        self.avail_idx = self.avail_idx.wrapping_add(1);
        m.write(self.driver + 2, &self.avail_idx.to_le_bytes())
            .unwrap();
    }

    fn track(&mut self, id: u16, slots: u16, writable: u32, descs: Vec<u16>) {
        self.by_id.insert(id, ());
        self.outstanding.push_back(Posted {
            id,
            slots,
            writable,
            descs,
        });
    }

    /// Chains posted and not yet used.
    pub fn outstanding(&self) -> usize {
        self.outstanding.len()
    }

    /// The `QueueNotify` value with `NOTIFICATION_DATA` (§2.9).
    pub fn notify_data(&self) -> u32 {
        let data = match self.format {
            RingFormat::Split => u32::from(self.avail_idx),
            RingFormat::Packed => u32::from(self.next) | (u32::from(self.wrap) << 15),
        };
        self.index | (data << 16)
    }

    /// Asks for no used-buffer notifications.
    pub fn disable_interrupts(&self, m: &mut GuestMemory) {
        match self.format {
            RingFormat::Split => m
                .write(self.driver, &VIRTQ_AVAIL_F_NO_INTERRUPT.to_le_bytes())
                .unwrap(),
            RingFormat::Packed => m
                .write(self.driver + 2, &RING_EVENT_FLAGS_DISABLE.to_le_bytes())
                .unwrap(),
        }
    }
    pub fn enable_interrupts(&self, m: &mut GuestMemory) {
        match self.format {
            RingFormat::Split => m.write(self.driver, &0u16.to_le_bytes()).unwrap(),
            RingFormat::Packed => m
                .write(self.driver + 2, &RING_EVENT_FLAGS_ENABLE.to_le_bytes())
                .unwrap(),
        }
    }
    /// EVENT_IDX: interrupt once `ahead` more used entries (split) or used
    /// descriptors (packed) than are consumed so far have been written.
    pub fn interrupt_after(&self, m: &mut GuestMemory, ahead: u16) {
        match self.format {
            RingFormat::Split => {
                let ev = self.last_used.wrapping_add(ahead).wrapping_sub(1);
                m.write(
                    self.driver + 4 + 2 * u64::from(self.size),
                    &ev.to_le_bytes(),
                )
                .unwrap();
            }
            RingFormat::Packed => {
                let mut off = u32::from(self.used_next) + u32::from(ahead) - 1;
                let mut wrap = self.used_wrap;
                if off >= u32::from(self.size) {
                    off -= u32::from(self.size);
                    wrap = !wrap;
                }
                let off_wrap = off as u16 | (u16::from(wrap) << 15);
                m.write(self.driver, &off_wrap.to_le_bytes()).unwrap();
                m.write(self.driver + 2, &RING_EVENT_FLAGS_DESC.to_le_bytes())
                    .unwrap();
            }
        }
    }
    /// Split EVENT_IDX: the device's `avail_event`.
    pub fn avail_event(&self, m: &GuestMemory) -> u16 {
        m.read_u16(self.device + 4 + 8 * u64::from(self.size))
            .unwrap()
    }
    /// Split: the device's `used.idx`.
    pub fn used_idx(&self, m: &GuestMemory) -> u16 {
        m.read_u16(self.device + 2).unwrap()
    }

    fn retire_through(&mut self, id: u16, len: u32) -> u16 {
        // In order: every chain posted before `id` completed with its full
        // writable length; `id` carries the entry's length.
        let mut slots = 0u16;
        loop {
            let p = self.outstanding.pop_front().expect("known id");
            slots += p.slots;
            self.by_id.remove(&p.id);
            match self.format {
                RingFormat::Packed => self.free_ids.push(p.id),
                RingFormat::Split => self.free_descs.extend(p.descs.iter().rev()),
            }
            if p.id == id {
                self.expanded.push_back((p.id, len));
                return slots;
            }
            self.expanded.push_back((p.id, p.writable));
        }
    }

    /// The next used element `(id, written_len)`, if the device published one.
    pub fn used(&mut self, m: &GuestMemory) -> Option<(u16, u32)> {
        if let Some(e) = self.expanded.pop_front() {
            return Some(e);
        }
        match self.format {
            RingFormat::Packed => {
                let at = self.desc + u64::from(self.used_next) * 16;
                let mut b = [0u8; 16];
                m.read(at, &mut b).unwrap();
                let d = PackedDescriptor::decode(&b).unwrap();
                if !d.is_used(self.used_wrap) {
                    return None;
                }
                assert!(
                    self.by_id.contains_key(&d.buffer_id),
                    "unknown id {}",
                    d.buffer_id
                );
                let slots = if self.in_order {
                    self.retire_through(d.buffer_id, d.byte_len)
                } else {
                    let pos = self
                        .outstanding
                        .iter()
                        .position(|p| p.id == d.buffer_id)
                        .unwrap();
                    let p = self.outstanding.remove(pos).unwrap();
                    self.by_id.remove(&p.id);
                    self.free_ids.push(p.id);
                    self.expanded.push_back((p.id, d.byte_len));
                    p.slots
                };
                for _ in 0..slots {
                    self.used_next += 1;
                    if self.used_next == self.size {
                        self.used_next = 0;
                        self.used_wrap = !self.used_wrap;
                    }
                }
            }
            RingFormat::Split => {
                let idx = self.used_idx(m);
                if idx == self.last_used {
                    return None;
                }
                let at = self.device + 4 + 8 * u64::from(self.last_used % self.size);
                let id = m.read_u32(at).unwrap() as u16;
                let len = m.read_u32(at + 4).unwrap();
                assert!(self.by_id.contains_key(&id), "unknown head {id}");
                let n = if self.in_order {
                    let before = self.outstanding.len();
                    self.retire_through(id, len);
                    (before - self.outstanding.len()) as u16
                } else {
                    let pos = self.outstanding.iter().position(|p| p.id == id).unwrap();
                    let p = self.outstanding.remove(pos).unwrap();
                    self.by_id.remove(&p.id);
                    self.free_descs.extend(p.descs.iter().rev());
                    self.expanded.push_back((p.id, len));
                    1
                };
                self.last_used = self.last_used.wrapping_add(n);
            }
        }
        self.expanded.pop_front()
    }
}

/// Runs the §3.1.1 initialization sequence, accepting `features` (VERSION_1
/// must be included by the caller; RING_PACKED selects packed rings), and
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
        q.clear(m);
        w32(t, m, reg::QUEUE_SEL, q.index);
        assert!(r32(t, reg::QUEUE_NUM_MAX) >= u32::from(q.size));
        w32(t, m, reg::QUEUE_NUM, u32::from(q.size));
        w32(t, m, reg::QUEUE_DESC_LOW, q.desc as u32);
        w32(t, m, reg::QUEUE_DESC_HIGH, (q.desc >> 32) as u32);
        w32(t, m, reg::QUEUE_DRIVER_LOW, q.driver as u32);
        w32(t, m, reg::QUEUE_DRIVER_HIGH, (q.driver >> 32) as u32);
        w32(t, m, reg::QUEUE_DEVICE_LOW, q.device as u32);
        w32(t, m, reg::QUEUE_DEVICE_HIGH, (q.device >> 32) as u32);
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
