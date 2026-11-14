//! VirtIO MMIO transport and device models, driven by the specification-level
//! driver in `common` (virtio-v1.2 §2.1, §2.8, §3.1, §4.2, §5.1, §5.2, §5.10).
mod common;

use common::*;
use vesper_aether::devices::block::*;
use vesper_aether::devices::mmio::*;
use vesper_aether::devices::net::*;
use vesper_aether::devices::vsock::*;
use vesper_aether::devices::queue::{
    RingFormat, VIRTIO_F_EVENT_IDX, VIRTIO_F_IN_ORDER, VIRTIO_F_INDIRECT_DESC,
    VIRTIO_F_NOTIFICATION_DATA,
};
use vesper_aether::virtio::QueueError;

/// The block features a writable disk offers.
const BLOCK_FEATURES: u64 = VIRTIO_BLK_F_BLK_SIZE
    | VIRTIO_BLK_F_FLUSH
    | VIRTIO_BLK_F_DISCARD
    | VIRTIO_BLK_F_WRITE_ZEROES;

const BASE: u64 = VIRTIO_F_VERSION_1 | VIRTIO_F_RING_PACKED;

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
    read_only: bool,
) -> (
    MmioTransport<BlockDevice>,
    vesper_aether::guest_mem::GuestMemory,
    DriverQueue,
) {
    let mut m = ram();
    let mut t = MmioTransport::new(
        BlockDevice::new(disk(64), read_only, "aether-disk-0").unwrap(),
        7,
    );
    let q = DriverQueue::new(0, 0x1000, 8);
    let st = init(&mut t, &mut m, BASE | VIRTIO_BLK_F_FLUSH, &[&q]);
    assert_eq!(st & status::DRIVER_OK, status::DRIVER_OK);
    (t, m, q)
}

#[test]
fn register_file_identifies_a_version_2_device() {
    let mut t = MmioTransport::new(BlockDevice::new(disk(8), false, "d").unwrap(), 1);
    assert_eq!(r32(&mut t, reg::MAGIC_VALUE), 0x7472_6976);
    assert_eq!(r32(&mut t, reg::VERSION), 2);
    assert_eq!(r32(&mut t, reg::DEVICE_ID), VIRTIO_ID_BLOCK);
    assert_eq!(r32(&mut t, reg::VENDOR_ID), AETHER_VENDOR_ID);
    let mut m = ram();
    w32(&mut t, &mut m, reg::DEVICE_FEATURES_SEL, 1);
    let hi = r32(&mut t, reg::DEVICE_FEATURES);
    assert_eq!(
        u64::from(hi) << 32,
        VIRTIO_F_VERSION_1 | VIRTIO_F_RING_PACKED | VIRTIO_F_IN_ORDER | VIRTIO_F_NOTIFICATION_DATA,
        "VERSION_1 (32), RING_PACKED (34), IN_ORDER (35), NOTIFICATION_DATA (38) offered"
    );
    w32(&mut t, &mut m, reg::DEVICE_FEATURES_SEL, 0);
    let lo = r32(&mut t, reg::DEVICE_FEATURES);
    assert_eq!(
        u64::from(lo),
        BLOCK_FEATURES | VIRTIO_F_INDIRECT_DESC | VIRTIO_F_EVENT_IDX
    );
    w32(&mut t, &mut m, reg::DEVICE_FEATURES_SEL, 2);
    assert_eq!(r32(&mut t, reg::DEVICE_FEATURES), 0);
    // Queue 0 exists; queue 1 reports QueueNumMax 0.
    assert_eq!(r32(&mut t, reg::QUEUE_NUM_MAX), 256);
    w32(&mut t, &mut m, reg::QUEUE_SEL, 1);
    assert_eq!(r32(&mut t, reg::QUEUE_NUM_MAX), 0);
    // No shared memory regions: length reads as -1.
    assert_eq!(r32(&mut t, reg::SHM_LEN_LOW), u32::MAX);
    assert_eq!(r32(&mut t, reg::SHM_LEN_HIGH), u32::MAX);
    // Block config: capacity (64-bit) and blk_size.
    let mut cap = [0u8; 8];
    t.read(reg::CONFIG, &mut cap).unwrap();
    assert_eq!(u64::from_le_bytes(cap), 8);
    let mut bs = [0u8; 4];
    t.read(reg::CONFIG + 20, &mut bs).unwrap();
    assert_eq!(u32::from_le_bytes(bs), 512);
}

#[test]
fn register_access_rules_are_typed_errors() {
    let mut t = MmioTransport::new(BlockDevice::new(disk(8), false, "d").unwrap(), 1);
    let mut m = ram();
    let mut b2 = [0u8; 2];
    assert!(matches!(
        t.read(reg::STATUS, &mut b2),
        Err(MmioError::Width { .. })
    ));
    let mut b4 = [0u8; 4];
    assert!(matches!(
        t.read(reg::STATUS + 1, &mut b4),
        Err(MmioError::Width { .. })
    ));
    assert!(matches!(
        t.read(reg::QUEUE_NOTIFY, &mut b4),
        Err(MmioError::WriteOnlyRegister { .. })
    ));
    assert!(matches!(
        t.read(reg::DRIVER_FEATURES, &mut b4),
        Err(MmioError::WriteOnlyRegister { .. })
    ));
    assert!(matches!(
        t.write(&mut m, reg::MAGIC_VALUE, &[0; 4]),
        Err(MmioError::ReadOnlyRegister { .. })
    ));
    assert!(matches!(
        t.read(0x200, &mut b4),
        Err(MmioError::OutOfWindow { .. })
    ));
    assert!(matches!(
        t.read(reg::QUEUE_RESET, &mut b4),
        Err(MmioError::UnsupportedRegister { .. })
    ));
    let mut b3 = [0u8; 3];
    assert!(matches!(
        t.read(reg::CONFIG, &mut b3),
        Err(MmioError::Width { .. })
    ));
    let mut b8 = [0u8; 8];
    assert!(matches!(
        t.read(reg::CONFIG + 56, &mut b8),
        Err(MmioError::ConfigOutOfRange { .. })
    ));
    assert_eq!(
        t.write(&mut m, reg::CONFIG, &[0; 4]),
        Err(MmioError::ConfigReadOnly)
    );
    // Notify before DRIVER_OK.
    assert_eq!(
        t.write(&mut m, reg::QUEUE_NOTIFY, &0u32.to_le_bytes()),
        Err(MmioError::NotifyBeforeDriverOk)
    );
}

#[test]
fn feature_negotiation_follows_section_3_1() {
    let mut m = ram();
    let q = DriverQueue::new(0, 0x1000, 8);
    // A driver that does not accept RING_PACKED gets split rings.
    let mut t = MmioTransport::new(BlockDevice::new(disk(8), false, "d").unwrap(), 1);
    let split = DriverQueue::split(0, 0x1000, 8);
    let st = init(&mut t, &mut m, VIRTIO_F_VERSION_1, &[&split]);
    assert_eq!(st & status::DRIVER_OK, status::DRIVER_OK);
    assert_eq!(t.ring_format(), RingFormat::Split);
    // Accepting a feature the device did not offer sees FEATURES_OK cleared.
    let st = init(&mut t, &mut m, VIRTIO_F_VERSION_1 | (1 << 40), &[&q]);
    assert_eq!(st & status::FEATURES_OK, 0);
    assert_eq!(t.stats().rejected_feature_sets, 1);
    assert_eq!(
        t.write(
            &mut m,
            reg::STATUS,
            &(status::ACKNOWLEDGE | status::DRIVER | status::DRIVER_OK).to_le_bytes()
        ),
        Err(MmioError::DriverOkWithoutFeaturesOk)
    );
    // So is a device feature it did not offer (RO on a writable disk).
    let st = init(&mut t, &mut m, BASE | VIRTIO_BLK_F_RO, &[&q]);
    assert_eq!(st & status::FEATURES_OK, 0);
    // Legacy-only (no VERSION_1) is refused.
    let st = init(&mut t, &mut m, VIRTIO_F_RING_PACKED, &[&q]);
    assert_eq!(st & status::FEATURES_OK, 0);
    // A valid set negotiates and activates.
    let st = init(&mut t, &mut m, BASE | VIRTIO_BLK_F_FLUSH, &[&q]);
    assert_eq!(st & status::DRIVER_OK, status::DRIVER_OK);
    assert_eq!(t.negotiated_features(), BASE | VIRTIO_BLK_F_FLUSH);
    // Features are frozen after FEATURES_OK; status bits cannot be cleared.
    assert_eq!(
        t.write(&mut m, reg::DRIVER_FEATURES, &0u32.to_le_bytes()),
        Err(MmioError::FeaturesAfterFeaturesOk)
    );
    assert!(matches!(
        t.write(&mut m, reg::STATUS, &status::ACKNOWLEDGE.to_le_bytes()),
        Err(MmioError::StatusRegression { .. })
    ));
    assert_eq!(
        t.write(
            &mut m,
            reg::STATUS,
            &(st | status::DEVICE_NEEDS_RESET).to_le_bytes()
        ),
        Err(MmioError::DriverSetNeedsReset)
    );
}

#[test]
fn queue_registers_are_validated() {
    let mut m = ram();
    let mut t = MmioTransport::new(BlockDevice::new(disk(8), false, "d").unwrap(), 1);
    w32(&mut t, &mut m, reg::QUEUE_SEL, 0);
    w32(&mut t, &mut m, reg::QUEUE_NUM, 8);
    // The ring format is only known after FEATURES_OK.
    assert_eq!(
        t.write(&mut m, reg::QUEUE_READY, &1u32.to_le_bytes()),
        Err(MmioError::QueueReadyBeforeFeaturesOk)
    );
    assert_eq!(init(&mut t, &mut m, BASE, &[]) & status::DRIVER_OK, status::DRIVER_OK);
    w32(&mut t, &mut m, reg::QUEUE_SEL, 0);
    assert!(matches!(
        t.write(&mut m, reg::QUEUE_NUM, &257u32.to_le_bytes()),
        Err(MmioError::InvalidQueueSize {
            size: 257,
            max: 256
        })
    ));
    assert!(matches!(
        t.write(&mut m, reg::QUEUE_NUM, &0u32.to_le_bytes()),
        Err(MmioError::InvalidQueueSize { .. })
    ));
    w32(&mut t, &mut m, reg::QUEUE_NUM, 8);
    // Descriptor ring outside RAM.
    w32(&mut t, &mut m, reg::QUEUE_DESC_LOW, 0xffff_f000);
    w32(&mut t, &mut m, reg::QUEUE_DRIVER_LOW, 0x8_0000);
    w32(&mut t, &mut m, reg::QUEUE_DEVICE_LOW, 0x8_0080);
    assert_eq!(
        t.write(&mut m, reg::QUEUE_READY, &1u32.to_le_bytes()),
        Err(MmioError::RingNotInRam)
    );
    // Misaligned ring.
    w32(&mut t, &mut m, reg::QUEUE_DESC_LOW, 0x1008);
    assert_eq!(
        t.write(&mut m, reg::QUEUE_READY, &1u32.to_le_bytes()),
        Err(MmioError::RingNotInRam)
    );
    w32(&mut t, &mut m, reg::QUEUE_DESC_LOW, 0x1000);
    w32(&mut t, &mut m, reg::QUEUE_READY, 1);
    assert_eq!(r32(&mut t, reg::QUEUE_READY), 1);
    // Queue registers are frozen while ready.
    assert_eq!(
        t.write(&mut m, reg::QUEUE_NUM, &4u32.to_le_bytes()),
        Err(MmioError::QueueConfiguredWhileReady)
    );
    w32(&mut t, &mut m, reg::QUEUE_SEL, 3);
    assert_eq!(
        t.write(&mut m, reg::QUEUE_NUM, &4u32.to_le_bytes()),
        Err(MmioError::InvalidQueue { index: 3 })
    );
}

#[test]
fn block_read_write_flush_and_get_id() {
    let (mut t, mut m, mut q) = block(false);
    // OUT: write sector 3 with 0xAB.
    m.write(0x1_0000, &blk_req(VIRTIO_BLK_T_OUT, 3)).unwrap();
    m.write(0x1_1000, &[0xAB; 512]).unwrap();
    let id = q.post(
        &mut m,
        &[
            (0x1_0000, 16, false),
            (0x1_1000, 512, false),
            (0x1_2000, 1, true),
        ],
    );
    w32(&mut t, &mut m, reg::QUEUE_NOTIFY, 0);
    assert_eq!(q.used(&m), Some((id, 1)));
    assert_eq!(
        m.read_u16(0x1_2000).unwrap() & 0xff,
        u16::from(VIRTIO_BLK_S_OK)
    );
    assert!(t.interrupt_pending());
    assert_eq!(
        r32(&mut t, reg::INTERRUPT_STATUS) & INT_USED_BUFFER,
        INT_USED_BUFFER
    );
    w32(&mut t, &mut m, reg::INTERRUPT_ACK, INT_USED_BUFFER);
    assert!(!t.interrupt_pending());
    assert_eq!(&t.device().disk()[3 * 512..4 * 512], &[0xAB; 512]);
    assert_ne!(&t.device().durable()[3 * 512..4 * 512], &[0xAB; 512]);
    // IN: read sectors 2..4 back, split over two writable buffers.
    m.write(0x1_0000, &blk_req(VIRTIO_BLK_T_IN, 2)).unwrap();
    let id = q.post(
        &mut m,
        &[
            (0x1_0000, 16, false),
            (0x2_0000, 512, true),
            (0x2_1000, 513, true),
        ],
    );
    w32(&mut t, &mut m, reg::QUEUE_NOTIFY, 0);
    assert_eq!(q.used(&m), Some((id, 1025)));
    let mut a = [0u8; 512];
    m.read(0x2_0000, &mut a).unwrap();
    assert_eq!(a, [2u8; 512]);
    let mut b = [0u8; 513];
    m.read(0x2_1000, &mut b).unwrap();
    assert_eq!(&b[..512], &[0xAB; 512]);
    assert_eq!(b[512], VIRTIO_BLK_S_OK);
    // FLUSH makes the write durable.
    m.write(0x1_0000, &blk_req(VIRTIO_BLK_T_FLUSH, 0)).unwrap();
    let id = q.post(&mut m, &[(0x1_0000, 16, false), (0x1_2000, 1, true)]);
    w32(&mut t, &mut m, reg::QUEUE_NOTIFY, 0);
    assert_eq!(q.used(&m), Some((id, 1)));
    assert_eq!(&t.device().durable()[3 * 512..4 * 512], &[0xAB; 512]);
    // GET_ID: 20 bytes, NUL padded.
    m.write(0x1_0000, &blk_req(VIRTIO_BLK_T_GET_ID, 0)).unwrap();
    let id = q.post(&mut m, &[(0x1_0000, 16, false), (0x3_0000, 21, true)]);
    w32(&mut t, &mut m, reg::QUEUE_NOTIFY, 0);
    assert_eq!(q.used(&m), Some((id, 21)));
    let mut idb = [0u8; 21];
    m.read(0x3_0000, &mut idb).unwrap();
    assert_eq!(&idb[..13], b"aether-disk-0");
    assert_eq!(idb[13..20], [0; 7]);
    assert_eq!(idb[20], VIRTIO_BLK_S_OK);
    assert_eq!(t.device().stats().writes, 1);
    assert_eq!(t.device().stats().reads, 1);
    assert_eq!(t.device().stats().flushes, 1);
}

#[test]
fn block_errors_report_in_band_and_leave_the_disk_unchanged() {
    let (mut t, mut m, mut q) = block(false);
    let before = t.device().disk().to_vec();
    let cases: &[(u32, u64, u32, u8)] = &[
        // (type, sector, payload bytes, expected status)
        (VIRTIO_BLK_T_OUT, 64, 512, VIRTIO_BLK_S_IOERR), // past the end
        (VIRTIO_BLK_T_OUT, u64::MAX / 256, 512, VIRTIO_BLK_S_IOERR), // overflow
        (VIRTIO_BLK_T_OUT, 0, 100, VIRTIO_BLK_S_IOERR),  // not whole sectors
        (77, 0, 0, VIRTIO_BLK_S_UNSUPP),                 // unknown type
    ];
    for &(kind, sector, n, want) in cases {
        m.write(0x1_0000, &blk_req(kind, sector)).unwrap();
        let mut bufs = vec![(0x1_0000, 16, false)];
        if n > 0 {
            bufs.push((0x1_1000, n, false));
        }
        bufs.push((0x1_2000, 1, true));
        let id = q.post(&mut m, &bufs);
        w32(&mut t, &mut m, reg::QUEUE_NOTIFY, 0);
        assert_eq!(q.used(&m), Some((id, 1)), "case {kind} {sector}");
        let mut s = [0u8];
        m.read(0x1_2000, &mut s).unwrap();
        assert_eq!(s[0], want, "case {kind} {sector} {n}");
    }
    // IN past the end: status at the last writable byte.
    m.write(0x1_0000, &blk_req(VIRTIO_BLK_T_IN, 63)).unwrap();
    let id = q.post(&mut m, &[(0x1_0000, 16, false), (0x2_0000, 1025, true)]);
    w32(&mut t, &mut m, reg::QUEUE_NOTIFY, 0);
    assert_eq!(q.used(&m), Some((id, 1025)));
    let mut s = [0u8];
    m.read(0x2_0000 + 1024, &mut s).unwrap();
    assert_eq!(s[0], VIRTIO_BLK_S_IOERR);
    // Short header.
    let id = q.post(&mut m, &[(0x1_0000, 8, false), (0x1_2000, 1, true)]);
    w32(&mut t, &mut m, reg::QUEUE_NOTIFY, 0);
    assert_eq!(q.used(&m), Some((id, 1)));
    m.read(0x1_2000, &mut s).unwrap();
    assert_eq!(s[0], VIRTIO_BLK_S_IOERR);
    assert_eq!(t.device().disk(), &before[..]);

    // A read-only disk refuses OUT and offers VIRTIO_BLK_F_RO.
    let (mut t, mut m, mut q) = block(true);
    assert_ne!(t.offered_features() & VIRTIO_BLK_F_RO, 0);
    m.write(0x1_0000, &blk_req(VIRTIO_BLK_T_OUT, 0)).unwrap();
    let id = q.post(
        &mut m,
        &[
            (0x1_0000, 16, false),
            (0x1_1000, 512, false),
            (0x1_2000, 1, true),
        ],
    );
    w32(&mut t, &mut m, reg::QUEUE_NOTIFY, 0);
    assert_eq!(q.used(&m), Some((id, 1)));
    m.read(0x1_2000, &mut s).unwrap();
    assert_eq!(s[0], VIRTIO_BLK_S_IOERR);
    assert_eq!(t.device().disk(), &disk(64)[..]);
}

#[test]
fn ring_wraps_many_times_with_in_order_completion() {
    let (mut t, mut m, mut q) = block(false);
    for round in 0..40u64 {
        // Two requests per notification on an 8-entry ring (3 descriptors
        // each): the ring wraps every few rounds.
        let mut ids = Vec::new();
        for k in 0..2u64 {
            let hdr = 0x1_0000 + k * 0x100;
            let st = 0x1_2000 + k;
            m.write(hdr, &blk_req(VIRTIO_BLK_T_IN, (round + k) % 64))
                .unwrap();
            ids.push(q.post(
                &mut m,
                &[
                    (hdr, 16, false),
                    (0x4_0000 + k * 0x1000, 512, true),
                    (st, 1, true),
                ],
            ));
        }
        w32(&mut t, &mut m, reg::QUEUE_NOTIFY, 0);
        for (k, id) in ids.into_iter().enumerate() {
            assert_eq!(q.used(&m), Some((id, 513)), "round {round}");
            let mut b = [0u8; 1];
            m.read(0x4_0000 + k as u64 * 0x1000, &mut b).unwrap();
            assert_eq!(b[0], ((round + k as u64) % 64) as u8);
        }
        assert_eq!(q.used(&m), None);
    }
    assert_eq!(t.stats().chains_completed, 80);
}

#[test]
fn malformed_chains_set_needs_reset_and_reset_recovers() {
    let (mut t, mut m, mut q) = block(false);
    // A descriptor pointing outside guest RAM.
    m.write(0x1_0000, &blk_req(VIRTIO_BLK_T_IN, 0)).unwrap();
    q.post(&mut m, &[(0x1_0000, 16, false), (0xdead_0000, 512, true)]);
    assert_eq!(
        t.write(&mut m, reg::QUEUE_NOTIFY, &0u32.to_le_bytes()),
        Err(MmioError::Queue(QueueError::UnauthorizedRange))
    );
    assert_ne!(t.status() & status::DEVICE_NEEDS_RESET, 0);
    assert_ne!(r32(&mut t, reg::INTERRUPT_STATUS) & INT_CONFIG_CHANGE, 0);
    assert_eq!(
        t.write(&mut m, reg::QUEUE_NOTIFY, &0u32.to_le_bytes()),
        Err(MmioError::DeviceNeedsReset)
    );
    // Reset and reinitialise: the device works again.
    let mut q = DriverQueue::new(0, 0x1000, 8);
    let st = init(&mut t, &mut m, BASE, &[&q]);
    assert_eq!(st & status::DEVICE_NEEDS_RESET, 0);
    m.write(0x1_0000, &blk_req(VIRTIO_BLK_T_IN, 5)).unwrap();
    let id = q.post(&mut m, &[(0x1_0000, 16, false), (0x2_0000, 513, true)]);
    w32(&mut t, &mut m, reg::QUEUE_NOTIFY, 0);
    assert_eq!(q.used(&m), Some((id, 513)));

    // Readable-after-writable ordering is malformed (§2.8.4).
    let mut q2 = DriverQueue::new(0, 0x1000, 8);
    init(&mut t, &mut m, BASE, &[&q2]);
    q2.post(&mut m, &[(0x2_0000, 16, true), (0x1_0000, 16, false)]);
    assert_eq!(
        t.write(&mut m, reg::QUEUE_NOTIFY, &0u32.to_le_bytes()),
        Err(MmioError::Queue(QueueError::ReadAfterWrite))
    );
    // A chain without a status byte cannot be answered in-band.
    let mut q3 = DriverQueue::new(0, 0x1000, 8);
    init(&mut t, &mut m, BASE, &[&q3]);
    q3.post(&mut m, &[(0x1_0000, 16, false)]);
    assert_eq!(
        t.write(&mut m, reg::QUEUE_NOTIFY, &0u32.to_le_bytes()),
        Err(MmioError::DeviceNeedsReset)
    );
}

fn net() -> (
    MmioTransport<NetDevice>,
    vesper_aether::guest_mem::GuestMemory,
    DriverQueue,
    DriverQueue,
) {
    let mut m = ram();
    let mut t = MmioTransport::new(NetDevice::new([2, 0, 0, 0, 0, 1], 1500).unwrap(), 3);
    let rx = DriverQueue::new(0, 0x1000, 16);
    let tx = DriverQueue::new(1, 0x2000, 16);
    let f = BASE | VIRTIO_NET_F_MAC | VIRTIO_NET_F_STATUS | VIRTIO_NET_F_MTU;
    assert_eq!(
        init(&mut t, &mut m, f, &[&rx, &tx]) & status::DRIVER_OK,
        status::DRIVER_OK
    );
    (t, m, rx, tx)
}

fn frame(n: usize, tag: u8) -> Vec<u8> {
    let mut f = vec![tag; n];
    f[12] = 0x08; // EtherType IPv4
    f[13] = 0x00;
    f
}

#[test]
fn net_transmit_and_receive_frames() {
    let (mut t, mut m, mut rx, mut tx) = net();
    let mut mac = [0u8; 6];
    for (i, b) in mac.iter_mut().enumerate() {
        let mut x = [0u8];
        t.read(reg::CONFIG + i as u64, &mut x).unwrap();
        *b = x[0];
    }
    assert_eq!(mac, [2, 0, 0, 0, 0, 1]);
    let mut st = [0u8; 2];
    t.read(reg::CONFIG + 6, &mut st).unwrap();
    assert_eq!(u16::from_le_bytes(st), VIRTIO_NET_S_LINK_UP);
    // TX: header and frame in separate descriptors.
    let f = frame(60, 0x11);
    m.write(0x1_0000, &[0u8; NET_HDR_BYTES]).unwrap();
    m.write(0x1_1000, &f).unwrap();
    let id = tx.post(
        &mut m,
        &[
            (0x1_0000, NET_HDR_BYTES as u32, false),
            (0x1_1000, 60, false),
        ],
    );
    w32(&mut t, &mut m, reg::QUEUE_NOTIFY, 1);
    assert_eq!(tx.used(&m), Some((id, 0)));
    assert_eq!(t.with_device(|d| d.take_tx()), Some(f));
    // TX with a GSO header is dropped (no offloads negotiated), buffer returned.
    let mut h = [0u8; NET_HDR_BYTES];
    h[1] = 1; // GSO_TCPV4
    m.write(0x1_0000, &h).unwrap();
    let id = tx.post(
        &mut m,
        &[
            (0x1_0000, NET_HDR_BYTES as u32, false),
            (0x1_1000, 60, false),
        ],
    );
    w32(&mut t, &mut m, reg::QUEUE_NOTIFY, 1);
    assert_eq!(tx.used(&m), Some((id, 0)));
    assert_eq!(t.with_device(|d| d.take_tx()), None);
    assert_eq!(t.device().stats().tx_dropped, 1);
    // RX: a frame arriving before buffers waits; posting buffers delivers it.
    let g = frame(100, 0x22);
    assert!(t.with_device(|d| d.inject_rx(&g)));
    let id_small = rx.post(&mut m, &[(0x2_0000, 64, true)]);
    let id_big = rx.post(&mut m, &[(0x3_0000, 1526, true)]);
    w32(&mut t, &mut m, reg::QUEUE_NOTIFY, 0);
    // The too-small buffer is returned empty; the frame goes to the next.
    assert_eq!(rx.used(&m), Some((id_small, 0)));
    assert_eq!(rx.used(&m), Some((id_big, (NET_HDR_BYTES + 100) as u32)));
    let mut got = vec![0u8; NET_HDR_BYTES + 100];
    m.read(0x3_0000, &mut got).unwrap();
    assert_eq!(u16::from_le_bytes([got[10], got[11]]), 1, "num_buffers");
    assert_eq!(&got[NET_HDR_BYTES..], &g[..]);
    // A frame injected while buffers are posted is delivered on poll.
    let id = rx.post(&mut m, &[(0x3_0000, 1526, true)]);
    assert!(t.with_device(|d| d.inject_rx(&frame(70, 0x33))));
    t.poll(&mut m, 0).unwrap();
    assert_eq!(rx.used(&m), Some((id, (NET_HDR_BYTES + 70) as u32)));
    // Oversized frames are refused at injection.
    assert!(!t.with_device(|d| d.inject_rx(&frame(1600, 0))));
    assert_eq!(t.device().stats().rx_frames, 2);
    assert_eq!(t.device().stats().rx_dropped_small_buffer, 1);
}

#[test]
fn net_transmit_backlog_is_bounded() {
    let (mut t, mut m, _rx, mut tx) = net();
    let f = frame(60, 0x44);
    m.write(0x1_0000, &[0u8; NET_HDR_BYTES]).unwrap();
    m.write(0x1_1000, &f).unwrap();
    for _ in 0..TX_BACKLOG + 10 {
        let id = tx.post(
            &mut m,
            &[
                (0x1_0000, NET_HDR_BYTES as u32, false),
                (0x1_1000, 60, false),
            ],
        );
        w32(&mut t, &mut m, reg::QUEUE_NOTIFY, 1);
        assert_eq!(tx.used(&m), Some((id, 0)));
    }
    let stats = t.device().stats();
    assert_eq!(stats.tx_frames, TX_BACKLOG as u64);
    assert_eq!(stats.tx_dropped_backlog, 10);
    let mut drained = 0;
    while t.with_device(|d| d.take_tx()).is_some() {
        drained += 1;
    }
    assert_eq!(drained, TX_BACKLOG);
}

#[test]
fn net_link_change_bumps_config_generation() {
    let (mut t, mut m, _rx, _tx) = net();
    let g0 = r32(&mut t, reg::CONFIG_GENERATION);
    w32(&mut t, &mut m, reg::INTERRUPT_ACK, 3);
    t.with_device(|d| d.set_link(false));
    assert_eq!(r32(&mut t, reg::CONFIG_GENERATION), g0 + 1);
    assert_ne!(r32(&mut t, reg::INTERRUPT_STATUS) & INT_CONFIG_CHANGE, 0);
    let mut st = [0u8; 2];
    t.read(reg::CONFIG + 6, &mut st).unwrap();
    assert_eq!(u16::from_le_bytes(st), 0);
}

fn vsock() -> (
    MmioTransport<VsockDevice>,
    vesper_aether::guest_mem::GuestMemory,
    DriverQueue,
    DriverQueue,
    DriverQueue,
) {
    let mut m = ram();
    let mut dev = VsockDevice::new(42).unwrap();
    dev.listen(5000).unwrap();
    let mut t = MmioTransport::new(dev, 9);
    let rx = DriverQueue::new(0, 0x1000, 16);
    let tx = DriverQueue::new(1, 0x2000, 16);
    let ev = DriverQueue::new(2, 0x3000, 4);
    assert_eq!(
        init(&mut t, &mut m, BASE, &[&rx, &tx, &ev]) & status::DRIVER_OK,
        status::DRIVER_OK
    );
    (t, m, rx, tx, ev)
}

fn guest_pkt(op: u16, src_port: u32, dst_port: u32, payload: &[u8], fwd_cnt: u32) -> Vec<u8> {
    let mut v = VsockHeader {
        src_cid: 42,
        dst_cid: HOST_CID,
        src_port,
        dst_port,
        len: payload.len() as u32,
        kind: TYPE_STREAM,
        op,
        flags: 0,
        buf_alloc: 8192,
        fwd_cnt,
    }
    .encode()
    .to_vec();
    v.extend_from_slice(payload);
    v
}

/// Sends one guest packet on TX; returns nothing (TX completions are length 0).
fn send<D: vesper_aether::devices::mmio::VirtioDevice>(
    t: &mut MmioTransport<D>,
    m: &mut vesper_aether::guest_mem::GuestMemory,
    tx: &mut DriverQueue,
    pkt: &[u8],
) {
    m.write(0x5_0000, pkt).unwrap();
    let id = tx.post(m, &[(0x5_0000, pkt.len() as u32, false)]);
    w32(t, m, vesper_aether::devices::mmio::reg::QUEUE_NOTIFY, 1);
    assert_eq!(tx.used(m), Some((id, 0)));
}

/// Posts one RX buffer, polls, and returns the packet delivered into it.
fn recv(
    t: &mut MmioTransport<VsockDevice>,
    m: &mut vesper_aether::guest_mem::GuestMemory,
    rx: &mut DriverQueue,
) -> Option<(VsockHeader, Vec<u8>)> {
    if rx.outstanding() == 0 {
        rx.post(m, &[(0x6_0000, 4096 + 44, true)]);
    }
    t.poll(m, 0).unwrap();
    let (_, len) = rx.used(m)?;
    let mut b = vec![0u8; len as usize];
    m.read(0x6_0000, &mut b).unwrap();
    let h = VsockHeader::decode(&b).unwrap();
    Some((h, b[44..].to_vec()))
}

#[test]
fn vsock_stream_connect_data_and_credit() {
    let (mut t, mut m, mut rx, mut tx, _ev) = vsock();
    let mut cid = [0u8; 8];
    t.read(reg::CONFIG, &mut cid).unwrap();
    assert_eq!(u64::from_le_bytes(cid), 42);
    // Guest connects to host port 5000.
    send(
        &mut t,
        &mut m,
        &mut tx,
        &guest_pkt(OP_REQUEST, 1234, 5000, &[], 0),
    );
    let (h, _) = recv(&mut t, &mut m, &mut rx).unwrap();
    assert_eq!(
        (h.op, h.src_cid, h.dst_cid, h.src_port, h.dst_port),
        (OP_RESPONSE, 2, 42, 5000, 1234)
    );
    assert_eq!(h.buf_alloc, HOST_BUF_ALLOC);
    let key = (1234, 5000);
    assert_eq!(t.device().connection_state(key), Some(ConnState::Connected));
    // Guest sends data; the host reads it and returns credit.
    send(
        &mut t,
        &mut m,
        &mut tx,
        &guest_pkt(OP_RW, 1234, 5000, b"hello aether", 0),
    );
    assert_eq!(t.with_device(|d| d.recv(key)).unwrap(), b"hello aether");
    let (h, _) = recv(&mut t, &mut m, &mut rx).unwrap();
    assert_eq!((h.op, h.fwd_cnt), (OP_CREDIT_UPDATE, 12));
    // Host sends no more than the guest's 8 KiB credit.
    let big = vec![7u8; 20_000];
    let sent = t.with_device(|d| d.send(key, &big)).unwrap();
    assert_eq!(sent, 8192);
    let mut total = 0;
    while let Some((h, p)) = recv(&mut t, &mut m, &mut rx) {
        assert_eq!(h.op, OP_RW);
        assert_eq!(h.len as usize, p.len());
        total += p.len();
    }
    assert_eq!(total, 8192);
    assert_eq!(
        t.with_device(|d| d.send(key, &big)).unwrap(),
        0,
        "no credit left"
    );
    // The guest consumes 4096 and says so (fwd_cnt): 4096 more may flow.
    send(
        &mut t,
        &mut m,
        &mut tx,
        &guest_pkt(OP_CREDIT_UPDATE, 1234, 5000, &[], 4096),
    );
    assert_eq!(t.with_device(|d| d.send(key, &big)).unwrap(), 4096);
    // The guest shrinks its allocation below what is in flight (8192 bytes):
    // credit saturates at zero rather than wrapping into a fresh grant.
    let mut shrink = guest_pkt(OP_CREDIT_UPDATE, 1234, 5000, &[], 4096);
    shrink[36..40].copy_from_slice(&4096u32.to_le_bytes());
    send(&mut t, &mut m, &mut tx, &shrink);
    assert_eq!(t.with_device(|d| d.send(key, &big)).unwrap(), 0);
    // Full shutdown from the guest: host answers RST and forgets the socket.
    while recv(&mut t, &mut m, &mut rx).is_some() {}
    let mut sd = guest_pkt(OP_SHUTDOWN, 1234, 5000, &[], 4096);
    sd[32..36].copy_from_slice(&(SHUTDOWN_RCV | SHUTDOWN_SEND).to_le_bytes());
    send(&mut t, &mut m, &mut tx, &sd);
    let (h, _) = recv(&mut t, &mut m, &mut rx).unwrap();
    assert_eq!(h.op, OP_RST);
    assert_eq!(t.device().connection_state(key), None);
}

#[test]
fn vsock_rejects_spoofed_malformed_and_unknown_traffic() {
    let (mut t, mut m, mut rx, mut tx, _ev) = vsock();
    // No listener on 6000: RST.
    send(
        &mut t,
        &mut m,
        &mut tx,
        &guest_pkt(OP_REQUEST, 1, 6000, &[], 0),
    );
    assert_eq!(recv(&mut t, &mut m, &mut rx).unwrap().0.op, OP_RST);
    // Data on an unknown connection: RST.
    send(&mut t, &mut m, &mut tx, &guest_pkt(OP_RW, 9, 5000, b"x", 0));
    assert_eq!(recv(&mut t, &mut m, &mut rx).unwrap().0.op, OP_RST);
    // Spoofed source CID: dropped, no reply.
    let mut p = guest_pkt(OP_REQUEST, 1, 5000, &[], 0);
    p[0..8].copy_from_slice(&43u64.to_le_bytes());
    send(&mut t, &mut m, &mut tx, &p);
    // len disagreeing with the payload: dropped.
    let mut p = guest_pkt(OP_RW, 1, 5000, b"abc", 0);
    p[24..28].copy_from_slice(&10u32.to_le_bytes());
    send(&mut t, &mut m, &mut tx, &p);
    // Truncated header: dropped.
    send(&mut t, &mut m, &mut tx, &[0u8; 20]);
    assert!(recv(&mut t, &mut m, &mut rx).is_none());
    let s = t.device().stats();
    assert_eq!((s.spoofed, s.malformed), (1, 2));
    // Credit violation: guest sends more than HOST_BUF_ALLOC without the host
    // consuming: the connection is reset.
    send(
        &mut t,
        &mut m,
        &mut tx,
        &guest_pkt(OP_REQUEST, 77, 5000, &[], 0),
    );
    assert_eq!(recv(&mut t, &mut m, &mut rx).unwrap().0.op, OP_RESPONSE);
    let chunk = vec![1u8; 4096];
    for _ in 0..16 {
        send(
            &mut t,
            &mut m,
            &mut tx,
            &guest_pkt(OP_RW, 77, 5000, &chunk, 0),
        );
    }
    assert_eq!(
        t.device().connection_state((77, 5000)),
        Some(ConnState::Connected)
    );
    send(
        &mut t,
        &mut m,
        &mut tx,
        &guest_pkt(OP_RW, 77, 5000, &chunk, 0),
    );
    assert_eq!(t.device().connection_state((77, 5000)), None);
    assert_eq!(t.device().stats().credit_violations, 1);
    assert_eq!(recv(&mut t, &mut m, &mut rx).unwrap().0.op, OP_RST);
    assert!(VsockDevice::new(2).is_err());
    assert!(VsockDevice::new(u64::from(u32::MAX)).is_err());
}

#[test]
fn vsock_host_connect_and_transport_reset_event() {
    let (mut t, mut m, mut rx, mut tx, mut ev) = vsock();
    let key = t.with_device(|d| d.connect(7000, 80)).unwrap();
    let (h, _) = recv(&mut t, &mut m, &mut rx).unwrap();
    assert_eq!((h.op, h.dst_port, h.src_port), (OP_REQUEST, 80, 7000));
    assert_eq!(
        t.device().connection_state(key),
        Some(ConnState::Connecting)
    );
    send(
        &mut t,
        &mut m,
        &mut tx,
        &guest_pkt(OP_RESPONSE, 80, 7000, &[], 0),
    );
    assert_eq!(t.device().connection_state(key), Some(ConnState::Connected));
    // Snapshot restore: every connection is dropped and the guest is told.
    let id = ev.post(&mut m, &[(0x7_0000, 4, true)]);
    t.with_device(|d| d.transport_reset());
    assert!(t.device().connections().is_empty());
    t.poll(&mut m, 2).unwrap();
    assert_eq!(ev.used(&m), Some((id, 4)));
    assert_eq!(m.read_u32(0x7_0000).unwrap(), EVENT_TRANSPORT_RESET);
}
