use vesper_aether::virtio::*;
use vesper_aether::{Limits, MemorySlot, PageMapping, Permission, SlotSnapshot, SnapshotMap};

fn memory() -> SnapshotMap {
    SnapshotMap::validate(
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
    .unwrap()
}
fn authorization() -> GuestAuthorization {
    GuestAuthorization::new(
        &memory(),
        7,
        &[
            AuthorizedRange {
                guest_start: 0x1000,
                byte_len: 0x1000,
                access: DeviceAccess::Read,
            },
            AuthorizedRange {
                guest_start: 0x2000,
                byte_len: 0x1000,
                access: DeviceAccess::Write,
            },
            AuthorizedRange {
                guest_start: 0x3000,
                byte_len: 0x1000,
                access: DeviceAccess::ReadWrite,
            },
        ],
        QueueLimits::default(),
    )
    .unwrap()
}
fn descriptor(gpa: u64, len: u32, id: u16, flags: u16, wrap: bool) -> PackedDescriptor {
    PackedDescriptor {
        guest_address: gpa,
        byte_len: len,
        buffer_id: id,
        flags: flags | if wrap { FLAG_AVAIL } else { FLAG_USED },
    }
}
fn put(ring: &mut [u8], index: usize, desc: PackedDescriptor) {
    ring[index * 16..index * 16 + 16].copy_from_slice(&desc.encode());
}
fn get(ring: &[u8], index: usize) -> PackedDescriptor {
    PackedDescriptor::decode(&ring[index * 16..index * 16 + 16]).unwrap()
}
fn queue(size: usize) -> PackedQueue {
    PackedQueue::new(size, &authorization(), QueueLimits::default()).unwrap()
}

#[test]
fn exact_le_codec_and_phase_bit_truth_table() {
    let raw = [
        0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, 0x14, 0x13, 0x12, 0x11, 0x22, 0x21, 0x82,
        0x80,
    ];
    let d = PackedDescriptor::decode(&raw).unwrap();
    assert_eq!(d.guest_address, 0x0102_0304_0506_0708);
    assert_eq!(d.byte_len, 0x1112_1314);
    assert_eq!(d.buffer_id, 0x2122);
    assert_eq!(d.flags, 0x8082);
    assert_eq!(d.encode(), raw);
    assert_eq!(
        PackedDescriptor::decode(&raw[..15]),
        Err(QueueError::RingLength)
    );
    assert_eq!(
        PackedDescriptor::decode(&[0; 17]),
        Err(QueueError::RingLength)
    );
    for wrap in [false, true] {
        for avail in [false, true] {
            for used in [false, true] {
                let d = PackedDescriptor {
                    flags: if avail { FLAG_AVAIL } else { 0 } | if used { FLAG_USED } else { 0 },
                    ..d
                };
                assert_eq!(d.is_available(wrap), avail == wrap && used != wrap);
                assert_eq!(d.is_used(wrap), avail == wrap && used == wrap);
            }
        }
    }
}
#[test]
fn multi_descriptor_capture_and_one_used_record_skip_entire_chain() {
    let mut q = queue(4);
    let mut ring = vec![0; 64];
    // Nonterminal IDs are ignored. The final ID is the buffer identity (§2.8.6).
    put(
        &mut ring,
        0,
        descriptor(0x1000, 20, u16::MAX, FLAG_NEXT, true),
    );
    put(&mut ring, 1, descriptor(0x2000, 30, 2, FLAG_WRITE, true));
    let lease = q.capture(&ring).unwrap();
    assert_eq!(lease.chain().vm_id(), 7);
    assert_eq!(lease.chain().buffer_id(), 2);
    assert_eq!(lease.chain().readable_bytes(), 20);
    assert_eq!(lease.chain().writable_bytes(), 30);
    assert_eq!(lease.chain().descriptors().len(), 2);
    let tail = get(&ring, 1);
    let used = q.complete(&mut ring, lease, 17).unwrap();
    assert_eq!(
        used.cursor,
        RingCursor {
            index: 0,
            wrap: true
        }
    );
    assert_eq!(used.skipped_descriptors, 2);
    assert_eq!(
        get(&ring, 0),
        PackedDescriptor {
            guest_address: 0,
            byte_len: 17,
            buffer_id: 2,
            flags: FLAG_AVAIL | FLAG_USED | FLAG_WRITE
        }
    );
    assert_eq!(get(&ring, 1), tail); // no per-element used record.
    assert_eq!(q.state().next_used.index, 2);
    assert_eq!(q.state().in_flight_descriptors, 0);
}
#[test]
fn non_power_of_two_wrap_and_chain_crossing_both_phase_values() {
    let mut q = queue(3);
    let mut ring = vec![0; 48];
    for (round, wrap) in [(0, true), (1, false), (2, true)] {
        // Advance to index 2, then complete a chain straddling end/start.
        // After the chain leaves cursor 1, the second/third rounds use one filler.
        let filler = if round == 0 { 2 } else { 1 };
        for _ in 0..filler {
            let cursor = q.state().next_available;
            put(
                &mut ring,
                usize::from(cursor.index),
                descriptor(0x1000, 1, 0, 0, cursor.wrap),
            );
            let lease = q.capture(&ring).unwrap();
            q.complete(&mut ring, lease, 0).unwrap();
        }
        assert_eq!(q.state().next_available, RingCursor { index: 2, wrap });
        put(
            &mut ring,
            2,
            descriptor(0x1000, 4, u16::MAX, FLAG_NEXT, wrap),
        );
        put(&mut ring, 0, descriptor(0x2000, 5, 1, FLAG_WRITE, !wrap));
        let lease = q.capture(&ring).unwrap();
        assert_eq!(
            lease.chain().descriptors()[1].cursor,
            RingCursor {
                index: 0,
                wrap: !wrap
            }
        );
        assert_eq!(
            q.state().next_available,
            RingCursor {
                index: 1,
                wrap: !wrap
            }
        );
        q.complete(&mut ring, lease, 5).unwrap();
        assert!(get(&ring, 2).is_used(wrap));
        assert_eq!(
            q.state().next_used,
            RingCursor {
                index: 1,
                wrap: !wrap
            }
        );
    }
}
#[test]
fn full_queue_backpressure_and_id_reuse_after_completion() {
    let mut q = queue(2);
    let mut ring = vec![0; 32];
    put(&mut ring, 0, descriptor(0x1000, 1, 0, 0, true));
    put(&mut ring, 1, descriptor(0x1001, 1, 1, 0, true));
    let a = q.capture(&ring).unwrap();
    let b = q.capture(&ring).unwrap();
    let state = q.state();
    let bytes = ring.clone();
    assert_eq!(q.capture(&ring).unwrap_err(), QueueError::Full);
    assert_eq!(q.state(), state);
    assert_eq!(ring, bytes);
    q.complete(&mut ring, a, 0).unwrap();
    put(&mut ring, 0, descriptor(0x1000, 1, 0, 0, false));
    let c = q.capture(&ring).unwrap();
    q.complete(&mut ring, b, 0).unwrap();
    q.complete(&mut ring, c, 0).unwrap();
    assert_eq!(q.state().in_flight_descriptors, 0);
}
#[test]
fn failures_return_completion_owner_and_leave_bytes_and_state_untouched() {
    let mut q = queue(2);
    let mut other = queue(2);
    let mut ring = vec![0; 32];
    put(&mut ring, 0, descriptor(0x2000, 8, 0, FLAG_WRITE, true));
    put(&mut ring, 1, descriptor(0x2008, 8, 1, FLAG_WRITE, true));
    let a = q.capture(&ring).unwrap();
    let b = q.capture(&ring).unwrap();
    let state = q.state();
    let bytes = ring.clone();
    let other_state = other.state();
    let err = other.complete(&mut ring, a, 0).unwrap_err();
    assert_eq!(err.error, QueueError::ForeignLease);
    let a = err.lease;
    assert_eq!(other.state(), other_state);
    assert_eq!(q.state(), state);
    assert_eq!(ring, bytes);
    let err = q.complete(&mut ring, b, 1).unwrap_err();
    assert_eq!(err.error, QueueError::CompletionOrder);
    let b = err.lease;
    let err = q.complete(&mut ring[..16], a, 1).unwrap_err();
    assert_eq!(err.error, QueueError::RingLength);
    let a = err.lease;
    let err = q.complete(&mut ring, a, 9).unwrap_err();
    assert_eq!(err.error, QueueError::InvalidWrittenLength);
    let a = err.lease;
    assert_eq!(q.state(), state);
    assert_eq!(ring, bytes);
    q.complete(&mut ring, a, 8).unwrap();
    q.complete(&mut ring, b, 8).unwrap();
}
#[test]
fn hostile_ring_mutation_after_capture_cannot_change_snapshot_or_completion() {
    let mut q = queue(2);
    let mut ring = vec![0; 32];
    put(&mut ring, 0, descriptor(0x1000, 4, 99, FLAG_NEXT, true));
    put(&mut ring, 1, descriptor(0x2000, 8, 1, FLAG_WRITE, true));
    let lease = q.capture(&ring).unwrap();
    let captured: Vec<_> = lease.chain().descriptors().to_vec();
    ring.fill(0xff);
    assert_eq!(lease.chain().descriptors(), captured);
    let used = q.complete(&mut ring, lease, 8).unwrap();
    assert_eq!(used.descriptor.buffer_id, 1);
    assert_eq!(used.descriptor.byte_len, 8);
    assert_eq!(used.descriptor.flags, FLAG_AVAIL | FLAG_USED | FLAG_WRITE);
    assert_eq!(get(&ring, 0), used.descriptor);
}
#[test]
fn malformed_capture_is_transactional_including_partial_chain() {
    for (descriptors, expected) in [
        (
            vec![descriptor(0x1000, 1, 0, FLAG_INDIRECT, true)],
            QueueError::UnsupportedFlags,
        ),
        (
            vec![descriptor(0x1000, 1, 0, 8, true)],
            QueueError::UnsupportedFlags,
        ),
        (
            vec![descriptor(0x1000, 0, 0, 0, true)],
            QueueError::EmptyBuffer,
        ),
        (
            vec![descriptor(u64::MAX, 1, 0, 0, true)],
            QueueError::Overflow,
        ),
        (
            vec![descriptor(0x1fff, 2, 0, 0, true)],
            QueueError::UnauthorizedRange,
        ),
        (
            vec![descriptor(0x1000, 1, 0, FLAG_WRITE, true)],
            QueueError::UnauthorizedRange,
        ),
        (
            vec![descriptor(0x2000, 1, 0, 0, true)],
            QueueError::UnauthorizedRange,
        ),
        (
            vec![descriptor(0x1000, 1, 3, 0, true)],
            QueueError::InvalidBufferId,
        ),
        (
            vec![
                descriptor(0x2000, 1, 0, FLAG_WRITE | FLAG_NEXT, true),
                descriptor(0x1000, 1, 0, 0, true),
            ],
            QueueError::ReadAfterWrite,
        ),
        (
            vec![
                descriptor(0x1000, 1, 0, FLAG_NEXT, true),
                descriptor(0x1001, 1, 0, 0, false),
            ],
            QueueError::IncompleteChain,
        ),
        (
            vec![descriptor(0x1000, 1, 0, FLAG_NEXT, true); 3],
            QueueError::IncompleteChain,
        ),
    ] {
        let mut q = queue(3);
        let state = q.state();
        let mut ring = vec![0; 48];
        for (i, d) in descriptors.into_iter().enumerate() {
            put(&mut ring, i, d);
        }
        let bytes = ring.clone();
        assert_eq!(q.capture(&ring).unwrap_err(), expected);
        assert_eq!(q.state(), state);
        assert_eq!(ring, bytes);
        put(&mut ring, 0, descriptor(0x1000, 1, 0, 0, true));
        let good = q.capture(&ring).unwrap();
        q.complete(&mut ring, good, 0).unwrap();
    }
    let mut q = queue(3);
    assert_eq!(q.capture(&[0; 47]).unwrap_err(), QueueError::RingLength);
    assert_eq!(q.capture(&[0; 48]).unwrap_err(), QueueError::NotAvailable);
}
#[test]
fn duplicate_live_id_and_chain_overlapping_reserved_slots_fail_without_commit() {
    let mut q = queue(3);
    let mut ring = vec![0; 48];
    put(&mut ring, 0, descriptor(0x1000, 1, 1, 0, true));
    let a = q.capture(&ring).unwrap();
    let state = q.state();
    put(&mut ring, 1, descriptor(0x1000, 1, 1, 0, true));
    assert_eq!(q.capture(&ring).unwrap_err(), QueueError::BufferIdInUse);
    assert_eq!(q.state(), state);
    put(&mut ring, 1, descriptor(0x1000, 1, 0, FLAG_NEXT, true));
    put(&mut ring, 2, descriptor(0x1001, 1, 0, FLAG_NEXT, true));
    assert_eq!(q.capture(&ring).unwrap_err(), QueueError::Full);
    assert_eq!(q.state(), state);
    q.complete(&mut ring, a, 0).unwrap();
}
#[test]
fn zero_initialized_and_old_phase_bytes_never_reconsume() {
    let mut q = queue(1);
    let mut ring = vec![0; 16];
    assert_eq!(q.capture(&ring).unwrap_err(), QueueError::NotAvailable);
    put(&mut ring, 0, descriptor(0x1000, 1, 0, 0, true));
    let a = q.capture(&ring).unwrap();
    q.complete(&mut ring, a, 0).unwrap();
    assert_eq!(q.capture(&ring).unwrap_err(), QueueError::NotAvailable);
    put(&mut ring, 0, descriptor(0x1000, 1, 0, 0, false));
    let a = q.capture(&ring).unwrap();
    q.complete(&mut ring, a, 0).unwrap();
    assert_eq!(
        q.state().next_available,
        RingCursor {
            index: 0,
            wrap: true
        }
    );
    assert_eq!(q.capture(&ring).unwrap_err(), QueueError::NotAvailable);
}
#[test]
fn permission_allowlist_is_bounded_and_derived_from_validated_map() {
    let l = QueueLimits::default();
    let m = memory();
    let auth = |start, len, access| {
        GuestAuthorization::new(
            &m,
            7,
            &[AuthorizedRange {
                guest_start: start,
                byte_len: len,
                access,
            }],
            l,
        )
    };
    assert_eq!(
        auth(u64::MAX, 1, DeviceAccess::Read).unwrap_err(),
        QueueError::Overflow
    );
    assert_eq!(
        auth(0x1000, 0, DeviceAccess::Read).unwrap_err(),
        QueueError::EmptyBuffer
    );
    assert_eq!(
        auth(0x4fff, 2, DeviceAccess::Read).unwrap_err(),
        QueueError::UnauthorizedRange
    );
    assert_eq!(
        GuestAuthorization::new(
            &m,
            8,
            &[AuthorizedRange {
                guest_start: 0x1000,
                byte_len: 1,
                access: DeviceAccess::Read
            }],
            l
        )
        .unwrap_err(),
        QueueError::UnauthorizedRange
    );
    let overlap = [
        AuthorizedRange {
            guest_start: 0x1000,
            byte_len: 8,
            access: DeviceAccess::Read,
        },
        AuthorizedRange {
            guest_start: 0x1007,
            byte_len: 8,
            access: DeviceAccess::Read,
        },
    ];
    assert_eq!(
        GuestAuthorization::new(&m, 7, &overlap, l).unwrap_err(),
        QueueError::OverlappingAuthorization
    );
    assert_eq!(
        GuestAuthorization::new(
            &m,
            7,
            &overlap,
            QueueLimits {
                max_authorized_ranges: 1,
                ..l
            }
        )
        .unwrap_err(),
        QueueError::Limit
    );
    assert_eq!(
        GuestAuthorization::new(&m, 7, &overlap[..1], QueueLimits { max_work: 1, ..l })
            .unwrap_err(),
        QueueError::Limit
    );
    assert_eq!(
        PackedQueue::new(0, &authorization(), l).unwrap_err(),
        QueueError::InvalidQueueSize
    );
    assert_eq!(
        PackedQueue::new(32769, &authorization(), l).unwrap_err(),
        QueueError::InvalidQueueSize
    );
    let ro = vesper_loom_cas::blob_id(vesper_loom_cas::Domain::Artifact, &[0; 4096]);
    let map = SnapshotMap::validate(
        &[SlotSnapshot {
            slot: MemorySlot {
                vm_id: 7,
                slot_id: 0,
                guest_start: 0x1000,
                host_virtual_start: 0x10000,
                byte_len: 4096,
                permission: Permission::ReadOnlyCas {
                    object: ro,
                    object_len: 4096,
                    offset: 0,
                },
            },
            pages: vec![PageMapping::ReadOnlyCas(vesper_aether::CasPage {
                object: ro,
                object_len: 4096,
                offset: 0,
            })],
        }],
        Limits::default(),
    )
    .unwrap();
    for access in [DeviceAccess::Write, DeviceAccess::ReadWrite] {
        assert_eq!(
            GuestAuthorization::new(
                &map,
                7,
                &[AuthorizedRange {
                    guest_start: 0x1000,
                    byte_len: 8,
                    access
                }],
                l
            )
            .unwrap_err(),
            QueueError::WritableReadonlyMemory
        );
    }
    GuestAuthorization::new(
        &map,
        7,
        &[AuthorizedRange {
            guest_start: 0x1000,
            byte_len: 8,
            access: DeviceAccess::Read,
        }],
        l,
    )
    .unwrap();
}
#[test]
fn chain_bytes_descriptors_and_work_limits_fail_atomically() {
    let defaults = QueueLimits::default();
    for limits in [
        QueueLimits {
            max_chain_descriptors: 1,
            ..defaults
        },
        QueueLimits {
            max_chain_bytes: 1,
            ..defaults
        },
        QueueLimits {
            max_work: 1,
            ..defaults
        },
    ] {
        let mut q = PackedQueue::new(3, &authorization(), limits).unwrap();
        let mut ring = vec![0; 48];
        put(&mut ring, 0, descriptor(0x1000, 1, 0, FLAG_NEXT, true));
        put(&mut ring, 1, descriptor(0x1001, 1, 1, 0, true));
        let state = q.state();
        assert_eq!(q.capture(&ring).unwrap_err(), QueueError::Limit);
        assert_eq!(q.state(), state);
    }
    for limits in [
        QueueLimits {
            max_chain_bytes: u64::MAX,
            ..defaults
        },
        QueueLimits {
            max_work: usize::MAX,
            ..defaults
        },
        QueueLimits {
            max_chain_descriptors: 0,
            ..defaults
        },
    ] {
        assert_eq!(
            PackedQueue::new(3, &authorization(), limits).unwrap_err(),
            QueueError::Limit
        );
    }
}
#[test]
fn dropped_lease_stalls_instead_of_silently_recycling() {
    let mut q = queue(1);
    let mut ring = vec![0; 16];
    put(&mut ring, 0, descriptor(0x1000, 1, 0, 0, true));
    let a = q.capture(&ring).unwrap();
    drop(a);
    assert_eq!(q.state().in_flight_descriptors, 1);
    assert_eq!(q.capture(&ring).unwrap_err(), QueueError::Full);
}

#[test]
fn byte_ring_reference_cursor_model_across_many_sizes_and_full_chains() {
    for size in 1..10 {
        let mut q = queue(size);
        let mut ring = vec![0; size * DESCRIPTOR_BYTES];
        let (mut index, mut wrap) = (0usize, true);
        for round in 0..64 {
            let count = 1 + round % size;
            let start = RingCursor {
                index: index as u16,
                wrap,
            };
            for element in 0..count {
                let write = element == count - 1;
                let flags = if element < count - 1 { FLAG_NEXT } else { 0 }
                    | if write { FLAG_WRITE } else { 0 };
                put(
                    &mut ring,
                    index,
                    descriptor(
                        if write { 0x2000 } else { 0x1000 },
                        1,
                        (round % size) as u16,
                        flags,
                        wrap,
                    ),
                );
                index += 1;
                if index == size {
                    index = 0;
                    wrap = !wrap;
                }
            }
            let lease = q.capture(&ring).unwrap();
            assert_eq!(lease.chain().descriptors().len(), count);
            let expected = RingCursor {
                index: index as u16,
                wrap,
            };
            assert_eq!(q.state().next_available, expected);
            let used = q.complete(&mut ring, lease, 1).unwrap();
            assert_eq!(used.cursor, start);
            assert!(used.descriptor.is_used(start.wrap));
            assert_eq!(used.skipped_descriptors, count);
            assert_eq!(q.state().next_used, expected);
            assert_eq!(q.state().in_flight_descriptors, 0);
        }
    }
}
