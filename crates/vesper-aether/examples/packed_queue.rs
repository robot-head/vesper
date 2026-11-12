//! Ordinary byte-ring execution; no mapped guest memory or device DMA.
use vesper_aether::virtio::*;
use vesper_aether::{Limits, MemorySlot, PageMapping, Permission, SlotSnapshot, SnapshotMap};

fn write_descriptor(ring: &mut [u8], index: usize, descriptor: PackedDescriptor) {
    ring[index * DESCRIPTOR_BYTES..(index + 1) * DESCRIPTOR_BYTES]
        .copy_from_slice(&descriptor.encode());
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let map = SnapshotMap::validate(
        &[SlotSnapshot {
            slot: MemorySlot {
                vm_id: 7,
                slot_id: 0,
                guest_start: 4096,
                host_virtual_start: 65536,
                byte_len: 8192,
                permission: Permission::Private,
            },
            pages: vec![PageMapping::Zero; 2],
        }],
        Limits::default(),
    )?;
    let limits = QueueLimits::default();
    let authorization = GuestAuthorization::new(
        &map,
        7,
        &[
            AuthorizedRange {
                guest_start: 4096,
                byte_len: 4096,
                access: DeviceAccess::Read,
            },
            AuthorizedRange {
                guest_start: 8192,
                byte_len: 4096,
                access: DeviceAccess::Write,
            },
        ],
        limits,
    )?;
    let mut queue = PackedQueue::new(3, &authorization, limits)?;
    let mut ring = vec![0; 3 * DESCRIPTOR_BYTES];
    // Move to the final entry of a non-power-of-two ring.
    for index in 0..2 {
        write_descriptor(
            &mut ring,
            index,
            PackedDescriptor {
                guest_address: 4096,
                byte_len: 1,
                buffer_id: 0,
                flags: FLAG_AVAIL,
            },
        );
        let lease = queue.capture(&ring)?;
        queue.complete(&mut ring, lease, 0).map_err(|e| e.error)?;
    }
    write_descriptor(
        &mut ring,
        2,
        PackedDescriptor {
            guest_address: 4096,
            byte_len: 16,
            buffer_id: u16::MAX,
            flags: FLAG_AVAIL | FLAG_NEXT,
        },
    );
    write_descriptor(
        &mut ring,
        0,
        PackedDescriptor {
            guest_address: 8192,
            byte_len: 32,
            buffer_id: 1,
            flags: FLAG_USED | FLAG_WRITE,
        },
    );
    let lease = queue.capture(&ring)?;
    assert_eq!(lease.chain().descriptors().len(), 2);
    assert_eq!(lease.chain().buffer_id(), 1); // ID is in the LAST descriptor.
    assert_eq!(lease.chain().writable_bytes(), 32);
    let snapshot = lease.chain().descriptors().to_vec();
    ring.fill(0xff); // hostile source changes cannot affect captured metadata.
    assert_eq!(lease.chain().descriptors(), snapshot);
    let before = queue.state();
    let failure = queue.complete(&mut ring, lease, 33).unwrap_err();
    assert_eq!(failure.error, QueueError::InvalidWrittenLength);
    assert_eq!(queue.state(), before);
    assert!(ring.iter().all(|b| *b == 0xff));
    let used = queue
        .complete(&mut ring, failure.lease, 32)
        .map_err(|e| e.error)?;
    assert_eq!(
        used.cursor,
        RingCursor {
            index: 2,
            wrap: true
        }
    );
    assert_eq!(used.skipped_descriptors, 2);
    assert_eq!(used.descriptor.buffer_id, 1);
    assert_eq!(PackedDescriptor::decode(&ring[32..48])?, used.descriptor);
    assert!(used.descriptor.is_used(true));
    assert_eq!(
        queue.state().next_used,
        RingCursor {
            index: 1,
            wrap: false
        }
    );
    assert_eq!(queue.state().in_flight_descriptors, 0);
    println!(
        "captured two descriptors across ring wrap; terminal ID=1, written=32; invalid completion preserved lease/state"
    );
    println!(
        "one used record skipped both descriptors; immutable metadata survived source mutation; host byte model only"
    );
    Ok(())
}
