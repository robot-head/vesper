//! The Vesper virtqueue walks against the Rust ones: two queues over their
//! own copies of guest RAM run the same random captures, completions,
//! flushes and driver writes, on both ring formats, with and without
//! indirect tables.

use std::collections::VecDeque;

use crate::devices::queue::*;
use crate::guest_mem::GuestMemory;
use crate::virtio::{FLAG_AVAIL, FLAG_INDIRECT, FLAG_NEXT, FLAG_USED, FLAG_WRITE, QueueLimits};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const DESC: u64 = 0x1000;
const DRIVER: u64 = 0x3000;
const DEVICE: u64 = 0x4000;
/// Indirect tables live here, 0x100 bytes apart.
const TABLES: u64 = 0x6000;

fn memory() -> GuestMemory {
    // Two regions, so the walks translate addresses through the table.
    GuestMemory::new(&[(0, 0x10000), (0x20000, 0x8000)]).unwrap()
}

/// A buffer address and length, usually inside RAM.
fn buffer(rng: &mut Rng) -> (u64, u32) {
    let gpa = match rng.below(40) {
        0 => rng.next(),
        1 => 0xfff0 + rng.below(0x40),
        2 => 0x27f00 + rng.below(0x200),
        3 | 4 => 0x20000 + rng.below(0x7000),
        _ => 0x8000 + rng.below(0x7000),
    };
    let len = match rng.below(40) {
        0 => 0,
        1 => rng.next() as u32,
        2 => 0x1000 + rng.below(0x8000) as u32,
        _ => 1 + rng.below(0x200) as u32,
    };
    (gpa, len)
}

fn flags(rng: &mut Rng, indirect: bool) -> u16 {
    let mut f = 0;
    if rng.below(4) == 0 {
        f |= FLAG_NEXT;
    }
    if (f & FLAG_NEXT == 0 && rng.below(3) == 0) || rng.below(20) == 0 {
        f |= FLAG_WRITE;
    }
    if indirect && rng.below(6) == 0 {
        f |= FLAG_INDIRECT;
    }
    if rng.below(80) == 0 {
        f |= 1 << rng.below(16);
    }
    f
}

/// A split descriptor (or indirect entry) at `at`.
fn put_split(m: &mut GuestMemory, rng: &mut Rng, at: u64, size: u16, table: Option<u64>) {
    let mut f = flags(rng, table.is_some());
    let (mut gpa, mut len) = buffer(rng);
    if f & FLAG_INDIRECT != 0 {
        f &= !FLAG_NEXT;
        gpa = table.unwrap();
        len = 16 * (1 + rng.below(6) as u32) + if rng.below(10) == 0 { 3 } else { 0 };
    }
    let next = if rng.below(10) == 0 {
        rng.next() as u16
    } else {
        rng.below(u64::from(size) + 1) as u16
    };
    let mut d = [0u8; 16];
    d[0..8].copy_from_slice(&gpa.to_le_bytes());
    d[8..12].copy_from_slice(&len.to_le_bytes());
    d[12..14].copy_from_slice(&f.to_le_bytes());
    d[14..16].copy_from_slice(&next.to_le_bytes());
    m.write(at, &d).unwrap();
}

/// A packed descriptor at `at`, usually in the phase `wrap` expects.
fn put_packed(m: &mut GuestMemory, rng: &mut Rng, at: u64, size: u16, wrap: bool, table: u64) {
    let mut f = flags(rng, true);
    let (mut gpa, mut len) = buffer(rng);
    if f & FLAG_INDIRECT != 0 {
        f &= !FLAG_NEXT;
        gpa = table;
        len = 16 * (1 + rng.below(6) as u32) + if rng.below(10) == 0 { 5 } else { 0 };
    }
    f |= match rng.below(10) {
        0 => [0, FLAG_AVAIL, FLAG_USED, FLAG_AVAIL | FLAG_USED][rng.below(4) as usize],
        _ if wrap => FLAG_AVAIL,
        _ => FLAG_USED,
    };
    let id = if rng.below(10) == 0 {
        rng.next() as u16
    } else {
        rng.below(u64::from(size) + 1) as u16
    };
    let mut d = [0u8; 16];
    d[0..8].copy_from_slice(&gpa.to_le_bytes());
    d[8..12].copy_from_slice(&len.to_le_bytes());
    d[12..14].copy_from_slice(&id.to_le_bytes());
    d[14..16].copy_from_slice(&f.to_le_bytes());
    m.write(at, &d).unwrap();
}

/// An indirect table entry (split field order; packed tables use the same
/// address and length words).
fn put_entry(m: &mut GuestMemory, rng: &mut Rng, at: u64, packed: bool) {
    let nested = rng.below(8) == 0;
    let mut f = flags(rng, nested);
    if packed && rng.below(2) == 0 {
        f &= !FLAG_NEXT;
    }
    let (gpa, len) = buffer(rng);
    let tail = rng.below(8) as u16;
    let mut d = [0u8; 16];
    d[0..8].copy_from_slice(&gpa.to_le_bytes());
    d[8..12].copy_from_slice(&len.to_le_bytes());
    if packed {
        d[12..14].copy_from_slice(&tail.to_le_bytes());
        d[14..16].copy_from_slice(&f.to_le_bytes());
    } else {
        d[12..14].copy_from_slice(&f.to_le_bytes());
        d[14..16].copy_from_slice(&tail.to_le_bytes());
    }
    m.write(at, &d).unwrap();
}

fn limits(rng: &mut Rng) -> QueueLimits {
    let mut l = QueueLimits::default();
    if rng.below(3) == 0 {
        l.max_chain_descriptors = 1 + rng.below(5) as usize;
    }
    if rng.below(3) == 0 {
        l.max_chain_bytes = 1 + rng.below(0x800);
    }
    if rng.below(3) == 0 {
        l.max_work = 1 + rng.below(10) as usize;
    }
    l
}

fn same_chain(a: &Chain, b: &Chain) {
    assert_eq!(a.buffer_id(), b.buffer_id());
    assert_eq!(a.buffers(), b.buffers());
    assert_eq!(a.readable(), b.readable());
    assert_eq!(a.writable_len(), b.writable_len());
    assert_eq!(a.is_indirect(), b.is_indirect());
}

fn same_memory(a: &GuestMemory, b: &GuestMemory) {
    for i in 0..2 {
        assert_eq!(a.region_bytes(i), b.region_bytes(i));
    }
}

/// One driver write: a descriptor, an indirect entry, an avail entry or
/// index (split), or the driver event area.
fn driver_write(m: &mut GuestMemory, rng: &mut Rng, format: RingFormat, size: u16, wrap: bool) {
    let n = u64::from(size);
    match (format, rng.below(5)) {
        (_, 0) => {
            let packed = format == RingFormat::Packed;
            let at = TABLES + 16 * rng.below(0x60);
            put_entry(m, rng, at, packed);
        }
        (RingFormat::Split, 1) => {
            let idx = m.read_u16(DRIVER + 2).unwrap();
            let step = if rng.below(12) == 0 {
                rng.next() as u16
            } else {
                1
            };
            m.write(DRIVER + 2, &idx.wrapping_add(step).to_le_bytes())
                .unwrap();
        }
        (RingFormat::Split, 2) => {
            let head = if rng.below(10) == 0 {
                rng.next() as u16
            } else {
                rng.below(n) as u16
            };
            m.write(DRIVER + 4 + 2 * rng.below(n), &head.to_le_bytes())
                .unwrap();
        }
        (RingFormat::Split, _) => {
            let table = Some(TABLES + 0x100 * rng.below(6));
            let at = DESC + 16 * rng.below(n);
            put_split(m, rng, at, size, table);
        }
        (RingFormat::Packed, 1) => {
            m.write(DRIVER, &(rng.next() as u32).to_le_bytes()).unwrap();
        }
        (RingFormat::Packed, _) => {
            let table = TABLES + 0x100 * rng.below(6);
            let at = DESC + 16 * rng.below(n);
            put_packed(m, rng, at, size, wrap, table);
        }
    }
}

#[test]
fn capture_matches_the_rust_capture() {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let (mut captured, mut failed, mut empty) = (0u32, 0u32, 0u32);
    for trial in 0..8000 {
        let format = if trial % 2 == 0 {
            RingFormat::Split
        } else {
            RingFormat::Packed
        };
        let size = match format {
            RingFormat::Split => 1 << rng.below(5),
            RingFormat::Packed => 1 + rng.below(12) as u16,
        };
        let features = RingFeatures {
            indirect: rng.below(3) != 0,
            event_idx: rng.below(2) == 0,
            in_order: rng.below(3) == 0,
        };
        let layout = RingLayout {
            format,
            size,
            desc: DESC,
            driver: DRIVER,
            device: DEVICE,
        };
        let limits = limits(&mut rng);
        let mut m = memory();
        let mut wrap = true;
        for _ in 0..3 * u64::from(size) + 8 {
            driver_write(&mut m, &mut rng, format, size, wrap);
        }
        if format == RingFormat::Split {
            // Start with at most a ring's worth of chains available.
            let idx = rng.below(u64::from(size) + 1) as u16;
            m.write(DRIVER + 2, &idx.to_le_bytes()).unwrap();
        }
        let mut m_rust = m.clone();
        let mut port = VirtQueue::new(layout, features, limits, &m).unwrap();
        let mut rust = VirtQueue::new(layout, features, limits, &m_rust).unwrap();
        let (mut chains, mut chains_rust) = (VecDeque::new(), VecDeque::new());
        for _ in 0..40 {
            match rng.below(8) {
                0..=2 => {
                    let hint = if rng.below(4) == 0 {
                        port.hint_from_data(rng.next() as u16).ok()
                    } else {
                        None
                    };
                    match (
                        port.capture(&m, hint),
                        rust.capture_reference(&m_rust, hint),
                    ) {
                        (Ok(Some(a)), Ok(Some(b))) => {
                            same_chain(&a, &b);
                            captured += 1;
                            chains.push_back(a);
                            chains_rust.push_back(b);
                        }
                        (Ok(None), Ok(None)) => empty += 1,
                        (Err(a), Err(b)) => {
                            assert_eq!(a, b);
                            failed += 1;
                        }
                        (a, b) => panic!("port {a:?}, rust {b:?}"),
                    }
                }
                3 => {
                    if let (Some(a), Some(b)) = (chains.pop_front(), chains_rust.pop_front()) {
                        let written = rng.below(a.writable_len() + 2) as u32;
                        let x = port.complete(&mut m, a, written);
                        let y = rust.complete(&mut m_rust, b, written);
                        assert_eq!(x.is_ok(), y.is_ok(), "complete: {x:?} {y:?}");
                    }
                }
                4 => {
                    let x = port.flush(&mut m);
                    let y = rust.flush(&mut m_rust);
                    assert_eq!(format!("{x:?}"), format!("{y:?}"));
                }
                _ => {
                    if format == RingFormat::Packed && rng.below(8) == 0 {
                        wrap = !wrap;
                    }
                    // Both copies of guest RAM are equal (checked below).
                    driver_write(&mut m, &mut rng, format, size, wrap);
                    m_rust = m.clone();
                }
            }
            assert_eq!(port.stats(), rust.stats());
            assert_eq!(port.in_flight(), rust.in_flight());
            same_memory(&m, &m_rust);
        }
    }
    assert!(
        captured > 5000 && failed > 10000 && empty > 10000,
        "{captured} captured, {failed} failed, {empty} empty"
    );
}
