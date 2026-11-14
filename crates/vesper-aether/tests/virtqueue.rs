//! Ring features on both virtqueue formats, from descriptor chains laid out
//! as the specification describes them (virtio-v1.2 §2.7 split, §2.8 packed,
//! §2.9 notifications): indirect tables, used-buffer notification suppression
//! with and without EVENT_IDX, IN_ORDER batches and NOTIFICATION_DATA.
mod common;

use common::*;
use vesper_aether::devices::block::*;
use vesper_aether::devices::mmio::*;
use vesper_aether::devices::net::*;
use vesper_aether::devices::queue::*;
use vesper_aether::guest_mem::GuestMemory;
use vesper_aether::virtio::{FLAG_INDIRECT, FLAG_NEXT, FLAG_WRITE, QueueError};

const FORMATS: [RingFormat; 2] = [RingFormat::Split, RingFormat::Packed];

fn base(format: RingFormat) -> u64 {
    VIRTIO_F_VERSION_1
        | match format {
            RingFormat::Split => 0,
            RingFormat::Packed => VIRTIO_F_RING_PACKED,
        }
}

fn disk(sectors: usize) -> Vec<u8> {
    (0..sectors * 512).map(|i| (i / 512) as u8).collect()
}

fn blk_req(kind: u32, sector: u64) -> [u8; 16] {
    let mut h = [0u8; 16];
    h[0..4].copy_from_slice(&kind.to_le_bytes());
    h[8..16].copy_from_slice(&sector.to_le_bytes());
    h
}

fn block(
    format: RingFormat,
    extra: u64,
    size: u16,
) -> (MmioTransport<BlockDevice>, GuestMemory, DriverQueue) {
    let mut m = ram();
    let mut t = MmioTransport::new(BlockDevice::new(disk(64), false, "vq").unwrap(), 5);
    let mut q = DriverQueue::with_format(format, 0, 0x1000, size);
    q.in_order = extra & VIRTIO_F_IN_ORDER != 0;
    let st = init(&mut t, &mut m, base(format) | extra, &[&q]);
    assert_eq!(st & status::DRIVER_OK, status::DRIVER_OK, "{format:?}");
    assert_eq!(t.ring_format(), format);
    (t, m, q)
}

/// Posts a block IN of `sector` reading into `data` (+ status right after).
fn post_read(m: &mut GuestMemory, q: &mut DriverQueue, k: u64, sector: u64) -> u16 {
    let hdr = 0x1_0000 + k * 0x100;
    m.write(hdr, &blk_req(VIRTIO_BLK_T_IN, sector)).unwrap();
    q.post(
        m,
        &[
            (hdr, 16, false),
            (0x4_0000 + k * 0x1000, 512, true),
            (0x4_0000 + k * 0x1000 + 512, 1, true),
        ],
    )
}

fn notify<D: VirtioDevice>(t: &mut MmioTransport<D>, m: &mut GuestMemory, v: u32) {
    w32(t, m, reg::QUEUE_NOTIFY, v);
}

fn interrupt_and_ack<D: VirtioDevice>(t: &mut MmioTransport<D>, m: &mut GuestMemory) -> bool {
    let s = r32(t, reg::INTERRUPT_STATUS);
    w32(t, m, reg::INTERRUPT_ACK, s);
    s & INT_USED_BUFFER != 0
}

#[test]
fn split_ring_serves_block_requests_and_wraps_the_16_bit_indices() {
    // A 2-entry split ring: 3-descriptor chains do not fit, so GET_ID chains
    // of 2 descriptors, one at a time; 70 000 rounds wrap avail.idx and
    // used.idx (u16) once.
    let (mut t, mut m, mut q) = block(RingFormat::Split, 0, 2);
    for round in 0..70_000u32 {
        m.write(0x1_0000, &blk_req(VIRTIO_BLK_T_GET_ID, 0)).unwrap();
        let id = q.post(&mut m, &[(0x1_0000, 16, false), (0x2_0000, 21, true)]);
        notify(&mut t, &mut m, 0);
        assert_eq!(q.used(&m), Some((id, 21)), "round {round}");
    }
    assert_eq!(q.used_idx(&m), (70_000u32 % 65_536) as u16);
    let mut idb = [0u8; 20];
    m.read(0x2_0000, &mut idb).unwrap();
    assert_eq!(&idb[..2], b"vq");
    // Data reads back on a larger split ring with several chains per pass.
    let (mut t, mut m, mut q) = block(RingFormat::Split, 0, 8);
    for round in 0..20u64 {
        let a = post_read(&mut m, &mut q, 0, round % 64);
        let b = post_read(&mut m, &mut q, 1, (round + 1) % 64);
        notify(&mut t, &mut m, 0);
        assert_eq!(q.used(&m), Some((a, 513)));
        assert_eq!(q.used(&m), Some((b, 513)));
        let mut x = [0u8; 1];
        m.read(0x4_1000, &mut x).unwrap();
        assert_eq!(u64::from(x[0]), (round + 1) % 64);
    }
}

#[test]
fn indirect_tables_on_both_formats() {
    for format in FORMATS {
        let (mut t, mut m, mut q) = block(format, VIRTIO_F_INDIRECT_DESC, 4);
        // OUT through an indirect table of three entries, then IN back.
        m.write(0x1_0000, &blk_req(VIRTIO_BLK_T_OUT, 9)).unwrap();
        m.write(0x1_1000, &[0x5a; 512]).unwrap();
        let id = q.post_indirect(
            &mut m,
            0x7_0000,
            &[
                (0x1_0000, 16, false),
                (0x1_1000, 512, false),
                (0x1_2000, 1, true),
            ],
        );
        notify(&mut t, &mut m, 0);
        assert_eq!(q.used(&m), Some((id, 1)), "{format:?}");
        assert_eq!(&t.device().disk()[9 * 512..10 * 512], &[0x5a; 512][..]);
        m.write(0x1_0000, &blk_req(VIRTIO_BLK_T_IN, 9)).unwrap();
        let id = q.post_indirect(
            &mut m,
            0x7_0100,
            &[
                (0x1_0000, 16, false),
                (0x3_0000, 256, true),
                (0x3_0100, 256, true),
                (0x3_0200, 1, true),
            ],
        );
        notify(&mut t, &mut m, 0);
        assert_eq!(q.used(&m), Some((id, 513)));
        let mut got = [0u8; 513];
        m.read(0x3_0000, &mut got).unwrap();
        assert_eq!(&got[..512], &[0x5a; 512][..]);
        assert_eq!(got[512], VIRTIO_BLK_S_OK);
        let st = t.queue_stats(0).unwrap();
        assert_eq!(st.indirect_chains, 2);
        // The indirect descriptor takes one ring slot: 4 more fit a 4-ring.
        for k in 0..4 {
            m.write(0x1_0000, &blk_req(VIRTIO_BLK_T_GET_ID, 0)).unwrap();
            q.post_indirect(
                &mut m,
                0x7_1000 + k * 0x100,
                &[(0x1_0000, 16, false), (0x2_0000 + k * 0x100, 21, true)],
            );
        }
        notify(&mut t, &mut m, 0);
        for _ in 0..4 {
            assert_eq!(q.used(&m).map(|u| u.1), Some(21));
        }
    }
}

/// Posts a raw indirect-flagged chain and returns the error the device reports.
fn indirect_error(format: RingFormat, features: u64, table: &[[u8; 16]], flags: u16) -> MmioError {
    let (mut t, mut m, q) = block(format, features, 4);
    for (i, e) in table.iter().enumerate() {
        m.write(0x7_0000 + i as u64 * 16, e).unwrap();
    }
    let len = (table.len() * 16) as u32;
    match format {
        RingFormat::Split => {
            let mut q = q;
            q.post_raw_split(&mut m, &[(0, split_desc(0x7_0000, len, flags, 1))], 0);
        }
        RingFormat::Packed => {
            let d = vesper_aether::virtio::PackedDescriptor {
                guest_address: 0x7_0000,
                byte_len: len,
                buffer_id: 0,
                flags: flags | vesper_aether::virtio::FLAG_AVAIL,
            }
            .encode();
            m.write(q.desc, &d).unwrap();
            if flags & FLAG_NEXT != 0 {
                let d2 = vesper_aether::virtio::PackedDescriptor {
                    guest_address: 0x1_0000,
                    byte_len: 16,
                    buffer_id: 0,
                    flags: vesper_aether::virtio::FLAG_AVAIL,
                }
                .encode();
                m.write(q.desc + 16, &d2).unwrap();
            }
        }
    }
    let e = t
        .write(&mut m, reg::QUEUE_NOTIFY, &0u32.to_le_bytes())
        .unwrap_err();
    assert_ne!(t.status() & status::DEVICE_NEEDS_RESET, 0);
    e
}

fn packed_entry(addr: u64, len: u32, flags: u16) -> [u8; 16] {
    vesper_aether::virtio::PackedDescriptor {
        guest_address: addr,
        byte_len: len,
        buffer_id: 0,
        flags,
    }
    .encode()
}

#[test]
fn malformed_indirect_chains_are_typed_errors() {
    let entry = |addr, len, flags| split_desc(addr, len, flags, 0);
    let good = [
        split_desc(0x1_0000, 16, FLAG_NEXT, 1),
        entry(0x2_0000, 21, FLAG_WRITE),
    ];
    // Packed tables hold packed descriptors: (addr, len, id, flags).
    let packed_good = [
        packed_entry(0x1_0000, 16, 0),
        packed_entry(0x2_0000, 21, FLAG_WRITE),
    ];
    for format in FORMATS {
        let table: &[[u8; 16]] = match format {
            RingFormat::Split => &good,
            RingFormat::Packed => &packed_good,
        };
        assert_eq!(
            indirect_error(format, 0, table, FLAG_INDIRECT),
            MmioError::Queue(QueueError::IndirectNotNegotiated),
            "{format:?}"
        );
        assert_eq!(
            indirect_error(
                format,
                VIRTIO_F_INDIRECT_DESC,
                table,
                FLAG_INDIRECT | FLAG_NEXT
            ),
            MmioError::Queue(QueueError::IndirectPlacement)
        );
        let nested = [match format {
            RingFormat::Split => entry(0x7_0000, 32, FLAG_INDIRECT),
            RingFormat::Packed => packed_entry(0x7_0000, 32, FLAG_INDIRECT),
        }];
        assert_eq!(
            indirect_error(format, VIRTIO_F_INDIRECT_DESC, &nested, FLAG_INDIRECT),
            MmioError::Queue(QueueError::IndirectNested)
        );
        let (mut t, mut m, mut q) = block(format, VIRTIO_F_INDIRECT_DESC, 4);
        // A 24-byte table is not whole descriptors.
        match format {
            RingFormat::Split => q.post_raw_split(
                &mut m,
                &[(0, split_desc(0x7_0000, 24, FLAG_INDIRECT, 0))],
                0,
            ),
            RingFormat::Packed => {
                q.post_indirect(&mut m, 0x7_0000, &[(0x1_0000, 16, false)]);
                m.write(q.desc + 8, &24u32.to_le_bytes()).unwrap();
            }
        }
        assert_eq!(
            t.write(&mut m, reg::QUEUE_NOTIFY, &0u32.to_le_bytes()),
            Err(MmioError::Queue(QueueError::IndirectTableLength))
        );
    }
    // Split only: an indirect chain whose `next` leaves the table.
    let bad_next = [
        split_desc(0x1_0000, 16, FLAG_NEXT, 7),
        entry(0x2_0000, 21, FLAG_WRITE),
    ];
    assert_eq!(
        indirect_error(
            RingFormat::Split,
            VIRTIO_F_INDIRECT_DESC,
            &bad_next,
            FLAG_INDIRECT
        ),
        MmioError::Queue(QueueError::InvalidNext)
    );
    let looped = [
        split_desc(0x1_0000, 16, FLAG_NEXT, 1),
        split_desc(0x1_0000, 16, FLAG_NEXT, 0),
    ];
    assert_eq!(
        indirect_error(
            RingFormat::Split,
            VIRTIO_F_INDIRECT_DESC,
            &looped,
            FLAG_INDIRECT
        ),
        MmioError::Queue(QueueError::ChainLoop)
    );
}

#[test]
fn malformed_split_rings_are_typed_errors() {
    let notify_err = |t: &mut MmioTransport<BlockDevice>, m: &mut GuestMemory| {
        t.write(m, reg::QUEUE_NOTIFY, &0u32.to_le_bytes())
            .unwrap_err()
    };
    // Head index outside the table.
    let (mut t, mut m, mut q) = block(RingFormat::Split, 0, 4);
    q.post_raw_split(&mut m, &[], 9);
    assert_eq!(
        notify_err(&mut t, &mut m),
        MmioError::Queue(QueueError::InvalidBufferId)
    );
    // `next` outside the table.
    let (mut t, mut m, mut q) = block(RingFormat::Split, 0, 4);
    q.post_raw_split(&mut m, &[(0, split_desc(0x1_0000, 16, FLAG_NEXT, 4))], 0);
    assert_eq!(
        notify_err(&mut t, &mut m),
        MmioError::Queue(QueueError::InvalidNext)
    );
    // A loop 0 -> 1 -> 0.
    let (mut t, mut m, mut q) = block(RingFormat::Split, 0, 4);
    q.post_raw_split(
        &mut m,
        &[
            (0, split_desc(0x1_0000, 16, FLAG_NEXT, 1)),
            (1, split_desc(0x1_0000, 16, FLAG_NEXT, 0)),
        ],
        0,
    );
    assert_eq!(
        notify_err(&mut t, &mut m),
        MmioError::Queue(QueueError::ChainLoop)
    );
    // Two available chains sharing descriptor 1.
    let (mut t, mut m, mut q) = block(RingFormat::Split, 0, 4);
    m.write(0x1_0000, &blk_req(VIRTIO_BLK_T_GET_ID, 0)).unwrap();
    q.post_raw_split(
        &mut m,
        &[
            (0, split_desc(0x1_0000, 16, FLAG_NEXT, 1)),
            (1, split_desc(0x2_0000, 21, FLAG_WRITE, 0)),
            (2, split_desc(0x1_0000, 16, FLAG_NEXT, 1)),
        ],
        0,
    );
    q.post_raw_split(&mut m, &[], 2);
    assert_eq!(
        notify_err(&mut t, &mut m),
        MmioError::Queue(QueueError::DescriptorInUse)
    );
    // avail.idx more than the queue size ahead.
    let (mut t, mut m, q) = block(RingFormat::Split, 0, 4);
    m.write(q.driver + 2, &5u16.to_le_bytes()).unwrap();
    assert_eq!(
        notify_err(&mut t, &mut m),
        MmioError::Queue(QueueError::AvailOverrun)
    );
    // Split queue sizes must be powers of two (§2.7).
    let mut m = ram();
    let mut t = MmioTransport::new(BlockDevice::new(disk(8), false, "x").unwrap(), 1);
    init(&mut t, &mut m, VIRTIO_F_VERSION_1, &[]);
    w32(&mut t, &mut m, reg::QUEUE_SEL, 0);
    w32(&mut t, &mut m, reg::QUEUE_NUM, 6);
    w32(&mut t, &mut m, reg::QUEUE_DESC_LOW, 0x1000);
    w32(&mut t, &mut m, reg::QUEUE_DRIVER_LOW, 0x8_0000);
    w32(&mut t, &mut m, reg::QUEUE_DEVICE_LOW, 0x8_1000);
    assert!(matches!(
        t.write(&mut m, reg::QUEUE_READY, &1u32.to_le_bytes()),
        Err(MmioError::InvalidQueueSize { size: 6, .. })
    ));
    // A misaligned split used ring (4-byte alignment).
    w32(&mut t, &mut m, reg::QUEUE_NUM, 8);
    w32(&mut t, &mut m, reg::QUEUE_DEVICE_LOW, 0x8_1002);
    assert_eq!(
        t.write(&mut m, reg::QUEUE_READY, &1u32.to_le_bytes()),
        Err(MmioError::RingNotInRam)
    );
}

#[test]
fn interrupt_suppression_flags_without_event_idx() {
    for format in FORMATS {
        let (mut t, mut m, mut q) = block(format, 0, 8);
        post_read(&mut m, &mut q, 0, 1);
        notify(&mut t, &mut m, 0);
        assert!(interrupt_and_ack(&mut t, &mut m), "{format:?}");
        q.used(&m).unwrap();
        // NO_INTERRUPT (split) / DISABLE (packed): no used-buffer interrupt.
        q.disable_interrupts(&mut m);
        post_read(&mut m, &mut q, 0, 2);
        notify(&mut t, &mut m, 0);
        assert!(!interrupt_and_ack(&mut t, &mut m));
        assert_eq!(q.used(&m).map(|u| u.1), Some(513), "buffer still used");
        assert_eq!(t.stats().interrupts_suppressed, 1);
        q.enable_interrupts(&mut m);
        post_read(&mut m, &mut q, 0, 3);
        notify(&mut t, &mut m, 0);
        assert!(interrupt_and_ack(&mut t, &mut m));
        assert_eq!(t.queue_stats(0).unwrap().interrupts_suppressed, 1);
    }
}

#[test]
fn event_idx_interrupts_only_when_the_used_index_passes_the_event() {
    for format in FORMATS {
        let (mut t, mut m, mut q) = block(format, VIRTIO_F_EVENT_IDX, 8);
        // Ask for an interrupt with the 3rd used buffer. Split counts used
        // entries; packed names a ring position, and each chain takes 3
        // slots, so the 3rd chain's used descriptor is written at slot 6.
        let ahead = match format {
            RingFormat::Split => 3,
            RingFormat::Packed => 7,
        };
        q.interrupt_after(&mut m, ahead);
        for k in 0..2 {
            post_read(&mut m, &mut q, 0, k);
            notify(&mut t, &mut m, 0);
            assert!(!interrupt_and_ack(&mut t, &mut m), "{format:?} k={k}");
            q.used(&m).unwrap();
        }
        post_read(&mut m, &mut q, 0, 2);
        notify(&mut t, &mut m, 0);
        assert!(interrupt_and_ack(&mut t, &mut m), "{format:?} third");
        q.used(&m).unwrap();
        assert_eq!(t.queue_stats(0).unwrap().interrupts_suppressed, 2);
        if format == RingFormat::Split {
            // avail_event tells the driver where the device has read to.
            assert_eq!(q.avail_event(&m), 3);
        }
        // An event several entries back (already passed) does not interrupt.
        let behind = q.used_idx(&m).wrapping_sub(2);
        if format == RingFormat::Split {
            m.write(q.driver + 4 + 2 * 8, &behind.to_le_bytes())
                .unwrap();
            post_read(&mut m, &mut q, 0, 4);
            notify(&mut t, &mut m, 0);
            assert!(!interrupt_and_ack(&mut t, &mut m));
            q.used(&m).unwrap();
        }
        // Several completions in one pass crossing the event interrupt once.
        q.interrupt_after(&mut m, 2);
        post_read(&mut m, &mut q, 0, 5);
        post_read(&mut m, &mut q, 1, 6);
        notify(&mut t, &mut m, 0);
        assert!(interrupt_and_ack(&mut t, &mut m));
    }
}

#[test]
fn packed_event_desc_without_event_idx_is_treated_as_enable() {
    let (mut t, mut m, mut q) = block(RingFormat::Packed, 0, 8);
    q.interrupt_after(&mut m, 100); // writes flags = DESC
    post_read(&mut m, &mut q, 0, 1);
    notify(&mut t, &mut m, 0);
    assert!(interrupt_and_ack(&mut t, &mut m));
    assert_eq!(t.queue_stats(0).unwrap().invalid_event_flags, 1);
}

#[test]
fn in_order_batches_fully_written_buffers_into_one_used_entry() {
    for format in FORMATS {
        let (mut t, mut m, mut q) = block(format, VIRTIO_F_IN_ORDER, 16);
        let ids: Vec<u16> = (0..4)
            .map(|k| post_read(&mut m, &mut q, k, k + 10))
            .collect();
        notify(&mut t, &mut m, 0);
        let st = t.queue_stats(0).unwrap();
        assert_eq!(st.used_entries, 1, "{format:?}: one entry for the batch");
        assert_eq!(st.batched, 3);
        if format == RingFormat::Split {
            // used.idx skips the whole batch; the entry names the last head.
            assert_eq!(q.used_idx(&m), 4);
            assert_eq!(
                m.read_u32(q.device + 4).unwrap(),
                u32::from(ids[3]),
                "id of the last buffer"
            );
        }
        for (k, id) in ids.iter().enumerate() {
            assert_eq!(q.used(&m), Some((*id, 513)), "{format:?} {k}");
            let mut x = [0u8; 1];
            m.read(0x4_0000 + k as u64 * 0x1000, &mut x).unwrap();
            assert_eq!(x[0], 10 + k as u8);
        }
        assert_eq!(q.used(&m), None);
        // The next batch continues after the skipped entries.
        let id = post_read(&mut m, &mut q, 0, 20);
        notify(&mut t, &mut m, 0);
        assert_eq!(q.used(&m), Some((id, 513)));
    }
}

#[test]
fn in_order_gives_partially_written_buffers_their_own_entry() {
    for format in FORMATS {
        let mut m = ram();
        let mut t = MmioTransport::new(NetDevice::new([2, 0, 0, 0, 0, 9], 1500).unwrap(), 4);
        let mut rx = DriverQueue::with_format(format, 0, 0x1000, 16);
        let tx = DriverQueue::with_format(format, 1, 0x2000, 16);
        rx.in_order = true;
        let f = base(format) | VIRTIO_F_IN_ORDER;
        assert_eq!(
            init(&mut t, &mut m, f, &[&rx, &tx]) & status::DRIVER_OK,
            status::DRIVER_OK
        );
        let frames: Vec<Vec<u8>> = (0..3u8).map(|i| vec![i; 60 + usize::from(i)]).collect();
        for fr in &frames {
            assert!(t.with_device(|d| d.inject_rx(fr)));
        }
        let ids: Vec<u16> = (0..3)
            .map(|k| rx.post(&mut m, &[(0x2_0000 + k * 0x1000, 2048, true)]))
            .collect();
        notify(&mut t, &mut m, 0);
        let st = t.queue_stats(0).unwrap();
        assert_eq!(
            st.used_entries, 3,
            "{format:?}: partial buffers are not batched"
        );
        assert_eq!(st.batched, 0);
        for (k, id) in ids.iter().enumerate() {
            let want = (NET_HDR_BYTES + 60 + k) as u32;
            assert_eq!(rx.used(&m), Some((*id, want)));
        }
    }
}

#[test]
fn notification_data_bounds_what_a_notification_captures() {
    for format in FORMATS {
        let (mut t, mut m, mut q) = block(format, VIRTIO_F_NOTIFICATION_DATA, 8);
        let a = post_read(&mut m, &mut q, 0, 1);
        let announced = q.notify_data();
        let b = post_read(&mut m, &mut q, 1, 2);
        // The driver announced only the first chain.
        notify(&mut t, &mut m, announced);
        assert_eq!(q.used(&m), Some((a, 513)), "{format:?}");
        assert_eq!(q.used(&m), None, "{format:?}: second chain not captured");
        if format == RingFormat::Split {
            assert_eq!(t.queue_stats(0).unwrap().hint_mismatches, 1);
        }
        // The full notification captures it.
        let full = q.notify_data();
        notify(&mut t, &mut m, full);
        assert_eq!(q.used(&m), Some((b, 513)));
        if format == RingFormat::Packed {
            // next_off past the ring is refused.
            assert_eq!(
                t.write(&mut m, reg::QUEUE_NOTIFY, &(9u32 << 16).to_le_bytes()),
                Err(MmioError::InvalidNotification { value: 9 << 16 })
            );
        }
    }
    // Without the feature, bits above the queue index name no queue.
    let (mut t, mut m, _q) = block(RingFormat::Split, 0, 8);
    assert!(matches!(
        t.write(&mut m, reg::QUEUE_NOTIFY, &(1u32 << 16).to_le_bytes()),
        Err(MmioError::InvalidQueue { .. })
    ));
}

#[test]
fn ring_features_can_be_withheld_by_the_embedder() {
    let mut m = ram();
    let mut t = MmioTransport::with_ring_features(
        BlockDevice::new(disk(8), false, "n").unwrap(),
        1,
        VIRTIO_F_INDIRECT_DESC,
    );
    assert_eq!(t.offered_features() & RING_FEATURES, VIRTIO_F_INDIRECT_DESC);
    let q = DriverQueue::split(0, 0x1000, 8);
    let st = init(
        &mut t,
        &mut m,
        VIRTIO_F_VERSION_1 | VIRTIO_F_RING_PACKED,
        &[&q],
    );
    assert_eq!(st & status::FEATURES_OK, 0, "packed not offered");
    let st = init(
        &mut t,
        &mut m,
        VIRTIO_F_VERSION_1 | VIRTIO_F_INDIRECT_DESC,
        &[&q],
    );
    assert_eq!(st & status::DRIVER_OK, status::DRIVER_OK);
}

#[test]
fn vring_need_event_matches_the_specification_formula() {
    // Exhaustive over a window around the 16-bit wrap.
    for old in [0u16, 1, 100, 65_530, 65_535] {
        for d in 0u16..8 {
            let new = old.wrapping_add(d);
            for e in 0u16..16 {
                let event = old.wrapping_add(e).wrapping_sub(4);
                // event in [old, new) modulo 2^16.
                let rel = event.wrapping_sub(old);
                assert_eq!(vring_need_event(event, new, old), rel < d, "{old} {d} {e}");
            }
        }
    }
}
