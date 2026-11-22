//! The Vesper packed-ring capture against the Rust one (both builds are in
//! the crate under `vesper-port`): two queues run the same random captures
//! and completions over their own copies of a random ring.
#![cfg(feature = "vesper-port")]

use std::collections::VecDeque;

use vesper_aether::virtio::*;
use vesper_aether::{Limits, MemorySlot, PageMapping, Permission, SlotSnapshot, SnapshotMap};

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

fn authorization() -> GuestAuthorization {
    let map = SnapshotMap::validate(
        &[SlotSnapshot {
            slot: MemorySlot {
                vm_id: 7,
                slot_id: 1,
                guest_start: 0x1000,
                host_virtual_start: 0x10000,
                byte_len: 0x4000,
                permission: Permission::Private,
            },
            pages: vec![PageMapping::Zero; 4],
        }],
        Limits::default(),
    )
    .unwrap();
    let range = |guest_start, access| AuthorizedRange {
        guest_start,
        byte_len: 0x1000,
        access,
    };
    GuestAuthorization::new(
        &map,
        7,
        &[
            range(0x3000, DeviceAccess::ReadWrite),
            range(0x1000, DeviceAccess::Read),
            range(0x2000, DeviceAccess::Write),
        ],
        QueueLimits::default(),
    )
    .unwrap()
}

/// A descriptor that is usually well formed for the queue's current phase.
fn random_descriptor(rng: &mut Rng, size: usize, wrap: bool) -> PackedDescriptor {
    let phase = if rng.below(8) == 0 {
        let k = rng.below(4);
        (if k & 1 != 0 { FLAG_AVAIL } else { 0 }) | if k & 2 != 0 { FLAG_USED } else { 0 }
    } else if wrap {
        FLAG_AVAIL
    } else {
        FLAG_USED
    };
    let mut flags = phase;
    if rng.below(3) == 0 {
        flags |= FLAG_NEXT;
    }
    if rng.below(3) == 0 && flags & FLAG_NEXT == 0 || rng.below(10) == 0 {
        flags |= FLAG_WRITE;
    }
    if rng.below(24) == 0 {
        flags |= 1 << rng.below(16);
    }
    let guest_address = match rng.below(16) {
        0 => rng.next(),
        1 => u64::MAX - rng.below(64),
        2 => 0x800 + rng.below(0x5000),
        // Writes mostly go to the writable ranges.
        _ if flags & FLAG_WRITE != 0 => 0x2000 + rng.below(0x1f00),
        _ => 0x1000 + rng.below(0x2f00),
    };
    let byte_len = match rng.below(16) {
        0 => 0,
        1 => rng.next() as u32,
        2 => 1 + rng.below(0x1200) as u32,
        _ => 1 + rng.below(0x100) as u32,
    };
    let buffer_id = if rng.below(10) == 0 {
        rng.next() as u16
    } else {
        rng.below(size as u64 + 1) as u16
    };
    PackedDescriptor {
        guest_address,
        byte_len,
        buffer_id,
        flags,
    }
}

fn limits(rng: &mut Rng) -> QueueLimits {
    let mut l = QueueLimits::default();
    if rng.below(3) == 0 {
        l.max_chain_descriptors = 1 + rng.below(4) as usize;
    }
    if rng.below(3) == 0 {
        l.max_chain_bytes = 1 + rng.below(0x3000);
    }
    if rng.below(3) == 0 {
        l.max_work = 1 + rng.below(12) as usize;
    }
    l
}

fn same_chain(a: &DescriptorLease, b: &DescriptorLease) {
    let (a, b) = (a.chain(), b.chain());
    assert_eq!(a.vm_id(), b.vm_id());
    assert_eq!(a.descriptors(), b.descriptors());
    assert_eq!(a.buffer_id(), b.buffer_id());
    assert_eq!(a.readable_bytes(), b.readable_bytes());
    assert_eq!(a.writable_bytes(), b.writable_bytes());
}

#[test]
fn capture_matches_the_rust_capture() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let auth = authorization();
    let (mut captured, mut failed) = (0u32, 0u32);
    for _ in 0..6000 {
        let size = 1 + rng.below(12) as usize;
        let limits = limits(&mut rng);
        let mut port = PackedQueue::new(size, &auth, limits).unwrap();
        let mut rust = PackedQueue::new(size, &auth, limits).unwrap();
        let mut ring = vec![0u8; 16 * size];
        for i in 0..size {
            let d = random_descriptor(&mut rng, size, true);
            ring[16 * i..16 * i + 16].copy_from_slice(&d.encode());
        }
        let mut ring_rust = ring.clone();
        let (mut leases, mut leases_rust) = (VecDeque::new(), VecDeque::new());
        for _ in 0..24 {
            match rng.below(5) {
                0 | 1 => {
                    let a = port.capture(&ring);
                    let b = rust.capture_reference(&ring_rust);
                    match (a, b) {
                        (Ok(a), Ok(b)) => {
                            same_chain(&a, &b);
                            captured += 1;
                            leases.push_back(a);
                            leases_rust.push_back(b);
                        }
                        (Err(a), Err(b)) => {
                            assert_eq!(a, b);
                            failed += 1;
                        }
                        (a, b) => panic!("port {a:?}, rust {b:?}"),
                    }
                }
                2 => {
                    if let (Some(a), Some(b)) = (leases.pop_front(), leases_rust.pop_front()) {
                        let written = rng.below(0x1400) as u32;
                        match (
                            port.complete(&mut ring, a, written),
                            rust.complete(&mut ring_rust, b, written),
                        ) {
                            (Ok(a), Ok(b)) => assert_eq!(a, b),
                            (Err(a), Err(b)) => assert_eq!(a.error, b.error),
                            (a, b) => panic!("port {a:?}, rust {b:?}"),
                        }
                    }
                }
                _ => {
                    // The driver posts a descriptor at the next available slot
                    // or anywhere.
                    let state = port.state();
                    let at = if rng.below(2) == 0 {
                        usize::from(state.next_available.index)
                    } else {
                        rng.below(size as u64) as usize
                    };
                    let d = random_descriptor(&mut rng, size, state.next_available.wrap);
                    ring[16 * at..16 * at + 16].copy_from_slice(&d.encode());
                    ring_rust[16 * at..16 * at + 16].copy_from_slice(&d.encode());
                }
            }
            assert_eq!(port.state(), rust.state());
            assert_eq!(ring, ring_rust);
        }
    }
    // Both outcomes are exercised.
    assert!(captured > 5000 && failed > 20000, "{captured} {failed}");
}
