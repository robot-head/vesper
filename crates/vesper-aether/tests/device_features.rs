//! Device feature bits: virtio-blk `DISCARD`, `WRITE_ZEROES` and `MQ`
//! (§5.2), virtio-net `MQ` with the control queue and `MRG_RXBUF` (§5.1),
//! and virtio-vsock `SEQPACKET` (§5.10), over both ring formats.
mod common;

use common::*;
use vesper_aether::devices::block::*;
use vesper_aether::devices::mmio::*;
use vesper_aether::devices::net::*;
use vesper_aether::devices::queue::RingFormat;
use vesper_aether::devices::vsock::*;
use vesper_aether::guest_mem::GuestMemory;

fn base(format: RingFormat) -> u64 {
    VIRTIO_F_VERSION_1
        | match format {
            RingFormat::Split => 0,
            RingFormat::Packed => VIRTIO_F_RING_PACKED,
        }
}

fn config_u32<D: VirtioDevice>(t: &mut MmioTransport<D>, off: u64) -> u32 {
    let mut b = [0u8; 4];
    t.read(reg::CONFIG + off, &mut b).unwrap();
    u32::from_le_bytes(b)
}
fn config_u16<D: VirtioDevice>(t: &mut MmioTransport<D>, off: u64) -> u16 {
    let mut b = [0u8; 2];
    t.read(reg::CONFIG + off, &mut b).unwrap();
    u16::from_le_bytes(b)
}

// ---------------------------------------------------------------- block

fn seg(sector: u64, count: u32, flags: u32) -> [u8; 16] {
    let mut s = [0u8; 16];
    s[0..8].copy_from_slice(&sector.to_le_bytes());
    s[8..12].copy_from_slice(&count.to_le_bytes());
    s[12..16].copy_from_slice(&flags.to_le_bytes());
    s
}

fn req(kind: u32) -> [u8; 16] {
    let mut h = [0u8; 16];
    h[0..4].copy_from_slice(&kind.to_le_bytes());
    h
}

/// Sends one discard/write-zeroes request; returns its status byte.
fn segments_request(
    t: &mut MmioTransport<BlockDevice>,
    m: &mut GuestMemory,
    q: &mut DriverQueue,
    kind: u32,
    segs: &[[u8; 16]],
) -> u8 {
    m.write(0x1_0000, &req(kind)).unwrap();
    let payload: Vec<u8> = segs.concat();
    m.write(0x1_1000, &payload).unwrap();
    let mut bufs = vec![(0x1_0000, 16, false)];
    if !payload.is_empty() {
        bufs.push((0x1_1000, payload.len() as u32, false));
    }
    bufs.push((0x1_2000, 1, true));
    let id = q.post(m, &bufs);
    w32(t, m, reg::QUEUE_NOTIFY, q.index);
    assert_eq!(q.used(m), Some((id, 1)));
    let mut s = [0u8];
    m.read(0x1_2000, &mut s).unwrap();
    s[0]
}

fn filled_disk() -> Vec<u8> {
    vec![0xee; 256 * 512]
}

#[test]
fn block_discard_and_write_zeroes_segments() {
    for format in [RingFormat::Split, RingFormat::Packed] {
        let mut m = ram();
        let mut t = MmioTransport::new(BlockDevice::new(filled_disk(), false, "z").unwrap(), 2);
        let mut q = DriverQueue::with_format(format, 0, 0x1000, 8);
        let f = base(format) | VIRTIO_BLK_F_DISCARD | VIRTIO_BLK_F_WRITE_ZEROES;
        assert_eq!(
            init(&mut t, &mut m, f, &[&q]) & status::DRIVER_OK,
            status::DRIVER_OK
        );
        // Configuration limits (§5.2.4).
        assert_eq!(config_u32(&mut t, 36), MAX_DISCARD_SECTORS);
        assert_eq!(config_u32(&mut t, 40), MAX_DISCARD_SEG);
        assert_eq!(config_u32(&mut t, 44), DISCARD_SECTOR_ALIGNMENT);
        assert_eq!(config_u32(&mut t, 48), MAX_WRITE_ZEROES_SECTORS);
        assert_eq!(config_u32(&mut t, 52), MAX_WRITE_ZEROES_SEG);
        let zeroed = |t: &MmioTransport<BlockDevice>, sector: usize, n: usize| {
            t.device().disk()[sector * 512..(sector + n) * 512]
                .iter()
                .all(|&b| b == 0)
        };
        // Two write-zeroes segments, one with UNMAP (allowed for write zeroes).
        let s = segments_request(
            &mut t,
            &mut m,
            &mut q,
            VIRTIO_BLK_T_WRITE_ZEROES,
            &[seg(3, 2, 0), seg(10, 1, VIRTIO_BLK_WRITE_ZEROES_FLAG_UNMAP)],
        );
        assert_eq!(s, VIRTIO_BLK_S_OK, "{format:?}");
        assert!(zeroed(&t, 3, 2) && zeroed(&t, 10, 1));
        assert_eq!(t.device().disk()[5 * 512], 0xee);
        // An aligned discard.
        let s = segments_request(
            &mut t,
            &mut m,
            &mut q,
            VIRTIO_BLK_T_DISCARD,
            &[seg(16, 8, 0)],
        );
        assert_eq!(s, VIRTIO_BLK_S_OK);
        assert!(zeroed(&t, 16, 8));
        let before = t.device().disk().to_vec();
        let check = |t: &mut MmioTransport<BlockDevice>,
                     m: &mut GuestMemory,
                     q: &mut DriverQueue,
                     kind,
                     segs: &[[u8; 16]],
                     want: u8| {
            assert_eq!(segments_request(t, m, q, kind, segs), want, "{segs:?}");
            assert_eq!(t.device().disk(), &before[..], "disk unchanged");
        };
        // UNMAP on a discard, and unknown flag bits, are UNSUPP (§5.2.6.2).
        check(
            &mut t,
            &mut m,
            &mut q,
            VIRTIO_BLK_T_DISCARD,
            &[seg(32, 8, 1)],
            VIRTIO_BLK_S_UNSUPP,
        );
        check(
            &mut t,
            &mut m,
            &mut q,
            VIRTIO_BLK_T_WRITE_ZEROES,
            &[seg(32, 1, 4)],
            VIRTIO_BLK_S_UNSUPP,
        );
        // Misaligned discard, out of range, zero sectors, over the per-segment
        // limit: IOERR. A bad later segment stops the good earlier one too.
        check(
            &mut t,
            &mut m,
            &mut q,
            VIRTIO_BLK_T_DISCARD,
            &[seg(33, 8, 0)],
            VIRTIO_BLK_S_IOERR,
        );
        check(
            &mut t,
            &mut m,
            &mut q,
            VIRTIO_BLK_T_DISCARD,
            &[seg(32, 7, 0)],
            VIRTIO_BLK_S_IOERR,
        );
        check(
            &mut t,
            &mut m,
            &mut q,
            VIRTIO_BLK_T_WRITE_ZEROES,
            &[seg(40, 1, 0), seg(255, 2, 0)],
            VIRTIO_BLK_S_IOERR,
        );
        check(
            &mut t,
            &mut m,
            &mut q,
            VIRTIO_BLK_T_WRITE_ZEROES,
            &[seg(40, 0, 0)],
            VIRTIO_BLK_S_IOERR,
        );
        check(
            &mut t,
            &mut m,
            &mut q,
            VIRTIO_BLK_T_WRITE_ZEROES,
            &[seg(u64::MAX / 512, 1, 0)],
            VIRTIO_BLK_S_IOERR,
        );
        check(
            &mut t,
            &mut m,
            &mut q,
            VIRTIO_BLK_T_WRITE_ZEROES,
            &[seg(0, MAX_WRITE_ZEROES_SECTORS + 1, 0)],
            VIRTIO_BLK_S_IOERR,
        );
        // More segments than max_write_zeroes_seg; no segments; a partial one.
        let many = vec![seg(40, 1, 0); MAX_WRITE_ZEROES_SEG as usize + 1];
        check(
            &mut t,
            &mut m,
            &mut q,
            VIRTIO_BLK_T_WRITE_ZEROES,
            &many,
            VIRTIO_BLK_S_IOERR,
        );
        check(
            &mut t,
            &mut m,
            &mut q,
            VIRTIO_BLK_T_WRITE_ZEROES,
            &[],
            VIRTIO_BLK_S_IOERR,
        );
        m.write(0x1_0000, &req(VIRTIO_BLK_T_WRITE_ZEROES)).unwrap();
        let id = q.post(
            &mut m,
            &[
                (0x1_0000, 16, false),
                (0x1_1000, 12, false),
                (0x1_2000, 1, true),
            ],
        );
        w32(&mut t, &mut m, reg::QUEUE_NOTIFY, 0);
        assert_eq!(q.used(&m), Some((id, 1)));
        let mut s = [0u8];
        m.read(0x1_2000, &mut s).unwrap();
        assert_eq!(s[0], VIRTIO_BLK_S_IOERR);
        assert_eq!(t.device().stats().write_zeroes, 1);
        assert_eq!(t.device().stats().discards, 1);
    }
}

#[test]
fn block_discard_needs_the_feature_and_a_writable_disk() {
    let mut m = ram();
    let mut t = MmioTransport::new(BlockDevice::new(filled_disk(), false, "z").unwrap(), 2);
    let mut q = DriverQueue::split(0, 0x1000, 8);
    init(&mut t, &mut m, VIRTIO_F_VERSION_1, &[&q]);
    assert_eq!(
        segments_request(
            &mut t,
            &mut m,
            &mut q,
            VIRTIO_BLK_T_DISCARD,
            &[seg(0, 8, 0)]
        ),
        VIRTIO_BLK_S_UNSUPP
    );
    assert_eq!(t.device().disk()[0], 0xee);
    let ro = BlockDevice::new(filled_disk(), true, "ro").unwrap();
    let f = ro.device_features();
    assert_eq!(f & (VIRTIO_BLK_F_DISCARD | VIRTIO_BLK_F_WRITE_ZEROES), 0);
    assert_ne!(f & VIRTIO_BLK_F_RO, 0);
}

#[test]
fn block_multiqueue_serves_every_queue() {
    assert_eq!(
        BlockDevice::new(filled_disk(), false, "q")
            .unwrap()
            .with_queues(0)
            .unwrap_err(),
        DiskError::QueueCount
    );
    let mut m = ram();
    let dev = BlockDevice::new(filled_disk(), false, "q")
        .unwrap()
        .with_queues(4)
        .unwrap();
    assert_ne!(dev.device_features() & VIRTIO_BLK_F_MQ, 0);
    let mut t = MmioTransport::new(dev, 2);
    assert_eq!(config_u16(&mut t, 34), 4, "num_queues");
    let mut qs: Vec<DriverQueue> = (0..4)
        .map(|i| DriverQueue::split(i, 0x1000 + u64::from(i) * 0x1000, 8))
        .collect();
    let refs: Vec<&DriverQueue> = qs.iter().collect();
    let f = VIRTIO_F_VERSION_1 | VIRTIO_BLK_F_MQ | VIRTIO_BLK_F_WRITE_ZEROES;
    assert_eq!(
        init(&mut t, &mut m, f, &refs) & status::DRIVER_OK,
        status::DRIVER_OK
    );
    for (i, q) in qs.iter_mut().enumerate() {
        let s = segments_request(
            &mut t,
            &mut m,
            q,
            VIRTIO_BLK_T_WRITE_ZEROES,
            &[seg(i as u64 * 10, 1, 0)],
        );
        assert_eq!(s, VIRTIO_BLK_S_OK);
    }
    assert_eq!(&t.device().stats().per_queue[..4], &[1, 1, 1, 1]);
    // Queue 4 does not exist.
    w32(&mut t, &mut m, reg::QUEUE_SEL, 4);
    assert_eq!(r32(&mut t, reg::QUEUE_NUM_MAX), 0);
}

// ---------------------------------------------------------------- net

fn frame(n: usize, tag: u8) -> Vec<u8> {
    let mut f = vec![tag; n];
    f[12] = 0x08;
    f[13] = 0x00;
    f
}

/// Sends a control command; returns the ack byte.
fn ctrl(
    t: &mut MmioTransport<NetDevice>,
    m: &mut GuestMemory,
    q: &mut DriverQueue,
    cmd: &[u8],
) -> u8 {
    m.write(0x6_0000, cmd).unwrap();
    m.write(0x6_1000, &[0xff]).unwrap();
    let id = q.post(
        m,
        &[(0x6_0000, cmd.len() as u32, false), (0x6_1000, 1, true)],
    );
    w32(t, m, reg::QUEUE_NOTIFY, q.index);
    assert_eq!(q.used(m), Some((id, 1)));
    let mut a = [0u8];
    m.read(0x6_1000, &mut a).unwrap();
    a[0]
}

#[test]
fn net_multiqueue_pairs_are_enabled_by_the_control_queue() {
    for format in [RingFormat::Split, RingFormat::Packed] {
        let mut m = ram();
        let dev = NetDevice::new([2, 0, 0, 0, 0, 5], 1500)
            .unwrap()
            .with_queue_pairs(4)
            .unwrap();
        let mut t = MmioTransport::new(dev, 8);
        assert_eq!(config_u16(&mut t, 8), 4, "max_virtqueue_pairs");
        // receiveq1..4 = 0,2,4,6; transmitq1..4 = 1,3,5,7; controlq = 8.
        let mut qs: Vec<DriverQueue> = (0..9)
            .map(|i| DriverQueue::with_format(format, i, 0x1000 + u64::from(i) * 0x1000, 16))
            .collect();
        let refs: Vec<&DriverQueue> = qs.iter().collect();
        let f = base(format) | VIRTIO_NET_F_MQ | VIRTIO_NET_F_CTRL_VQ;
        assert_eq!(
            init(&mut t, &mut m, f, &refs) & status::DRIVER_OK,
            status::DRIVER_OK
        );
        assert_eq!(t.device().ctrl_queue(), Some(8));
        assert_eq!(t.device().active_pairs(), 1);
        // Pair 3 is not enabled yet.
        assert!(!t.with_device(|d| d.inject_rx_on(2, &frame(60, 1))));
        assert_eq!(t.device().stats().rx_dropped_inactive, 1);
        let set = |n: u16| {
            let mut c = vec![VIRTIO_NET_CTRL_MQ, VIRTIO_NET_CTRL_MQ_VQ_PAIRS_SET];
            c.extend_from_slice(&n.to_le_bytes());
            c
        };
        let (left, right) = qs.split_at_mut(8);
        let cq = &mut right[0];
        assert_eq!(
            ctrl(&mut t, &mut m, cq, &set(3)),
            VIRTIO_NET_OK,
            "{format:?}"
        );
        assert_eq!(t.device().active_pairs(), 3);
        assert_eq!(
            ctrl(&mut t, &mut m, cq, &set(5)),
            VIRTIO_NET_ERR,
            "above max"
        );
        assert_eq!(ctrl(&mut t, &mut m, cq, &set(0)), VIRTIO_NET_ERR);
        assert_eq!(
            ctrl(&mut t, &mut m, cq, &[0, 0, 1]),
            VIRTIO_NET_ERR,
            "CTRL_RX not offered"
        );
        assert_eq!(t.device().active_pairs(), 3);
        // A frame steered to pair 3 arrives on receiveq3 (queue 4).
        let g = frame(80, 0x33);
        assert!(t.with_device(|d| d.inject_rx_on(2, &g)));
        let rxq = &mut left[4];
        let id = rxq.post(&mut m, &[(0x2_0000, 1526, true)]);
        w32(&mut t, &mut m, reg::QUEUE_NOTIFY, 4);
        assert_eq!(rxq.used(&m), Some((id, (NET_HDR_BYTES + 80) as u32)));
        let mut got = vec![0u8; 80];
        m.read(0x2_0000 + NET_HDR_BYTES as u64, &mut got).unwrap();
        assert_eq!(got, g);
        // Transmit on transmitq3 (queue 5).
        let txq = &mut left[5];
        m.write(0x3_0000, &[0u8; NET_HDR_BYTES]).unwrap();
        m.write(0x3_0000 + NET_HDR_BYTES as u64, &frame(60, 0x44))
            .unwrap();
        let id = txq.post(&mut m, &[(0x3_0000, (NET_HDR_BYTES + 60) as u32, false)]);
        w32(&mut t, &mut m, reg::QUEUE_NOTIFY, 5);
        assert_eq!(txq.used(&m), Some((id, 0)));
        assert_eq!(t.with_device(|d| d.take_tx()), Some(frame(60, 0x44)));
        // Shrinking to one pair drops the backlog of the disabled pairs.
        assert!(t.with_device(|d| d.inject_rx_on(1, &frame(60, 5))));
        assert_eq!(ctrl(&mut t, &mut m, cq, &set(1)), VIRTIO_NET_OK);
        assert_eq!(t.device().rx_backlog(), 0);
        assert_eq!(t.device().stats().rx_dropped_inactive, 2);
        assert_eq!(t.device().stats().ctrl_ok, 2);
        assert_eq!(t.device().stats().ctrl_err, 3);
    }
}

#[test]
fn net_control_queue_without_multiqueue_sits_at_index_2() {
    let mut m = ram();
    let dev = NetDevice::new([2, 0, 0, 0, 0, 5], 1500)
        .unwrap()
        .with_queue_pairs(2)
        .unwrap();
    let mut t = MmioTransport::new(dev, 8);
    let rx = DriverQueue::split(0, 0x1000, 8);
    let tx = DriverQueue::split(1, 0x2000, 8);
    let mut cq = DriverQueue::split(2, 0x3000, 8);
    let f = VIRTIO_F_VERSION_1 | VIRTIO_NET_F_CTRL_VQ;
    assert_eq!(
        init(&mut t, &mut m, f, &[&rx, &tx, &cq]) & status::DRIVER_OK,
        status::DRIVER_OK
    );
    assert_eq!(t.device().ctrl_queue(), Some(2));
    let cmd = [VIRTIO_NET_CTRL_MQ, VIRTIO_NET_CTRL_MQ_VQ_PAIRS_SET, 2, 0];
    assert_eq!(
        ctrl(&mut t, &mut m, &mut cq, &cmd),
        VIRTIO_NET_ERR,
        "MQ not negotiated"
    );
    assert_eq!(t.device().active_pairs(), 1);
}

#[test]
fn net_mergeable_receive_buffers_spread_a_frame_over_chains() {
    for format in [RingFormat::Split, RingFormat::Packed] {
        let mut m = ram();
        let mut t = MmioTransport::new(NetDevice::new([2, 0, 0, 0, 0, 6], 1500).unwrap(), 8);
        let mut rx = DriverQueue::with_format(format, 0, 0x1000, 16);
        let tx = DriverQueue::with_format(format, 1, 0x2000, 16);
        let f = base(format) | VIRTIO_NET_F_MRG_RXBUF;
        assert_eq!(
            init(&mut t, &mut m, f, &[&rx, &tx]) & status::DRIVER_OK,
            status::DRIVER_OK
        );
        let big = (0..1514).map(|i| i as u8).collect::<Vec<u8>>();
        assert!(t.with_device(|d| d.inject_rx(&big)));
        // Two 600-byte buffers are not enough for 12 + 1514 bytes: they stay
        // posted and the frame waits.
        let a = rx.post(&mut m, &[(0x2_0000, 600, true)]);
        let b = rx.post(&mut m, &[(0x2_1000, 600, true)]);
        w32(&mut t, &mut m, reg::QUEUE_NOTIFY, 0);
        assert_eq!(rx.used(&m), None, "{format:?}");
        assert_eq!(t.device().rx_backlog(), 1);
        let c = rx.post(&mut m, &[(0x2_2000, 600, true)]);
        w32(&mut t, &mut m, reg::QUEUE_NOTIFY, 0);
        assert_eq!(rx.used(&m), Some((a, 600)));
        assert_eq!(rx.used(&m), Some((b, 600)));
        assert_eq!(rx.used(&m), Some((c, (NET_HDR_BYTES + 1514 - 1200) as u32)));
        let mut hdr = [0u8; NET_HDR_BYTES];
        m.read(0x2_0000, &mut hdr).unwrap();
        assert_eq!(u16::from_le_bytes([hdr[10], hdr[11]]), 3, "num_buffers");
        let mut got = vec![0u8; 1514];
        m.read(
            0x2_0000 + NET_HDR_BYTES as u64,
            &mut got[..600 - NET_HDR_BYTES],
        )
        .unwrap();
        m.read(
            0x2_1000,
            &mut got[600 - NET_HDR_BYTES..1200 - NET_HDR_BYTES],
        )
        .unwrap();
        m.read(0x2_2000, &mut got[1200 - NET_HDR_BYTES..]).unwrap();
        assert_eq!(got, big);
        // A small frame still uses one buffer with num_buffers = 1.
        assert!(t.with_device(|d| d.inject_rx(&frame(60, 9))));
        let d = rx.post(&mut m, &[(0x2_3000, 600, true)]);
        w32(&mut t, &mut m, reg::QUEUE_NOTIFY, 0);
        assert_eq!(rx.used(&m), Some((d, (NET_HDR_BYTES + 60) as u32)));
        assert_eq!(t.device().stats().rx_merged, 1);
        // A buffer below the header size is a driver error (§5.1.6.3.1).
        assert!(t.with_device(|d| d.inject_rx(&frame(60, 9))));
        rx.post(&mut m, &[(0x2_3000, 8, true)]);
        assert_eq!(
            t.write(&mut m, reg::QUEUE_NOTIFY, &0u32.to_le_bytes()),
            Err(MmioError::DeviceNeedsReset)
        );
    }
}

// ---------------------------------------------------------------- vsock

type Vsock = (
    MmioTransport<VsockDevice>,
    GuestMemory,
    DriverQueue,
    DriverQueue,
);

fn vsock(format: RingFormat, features: u64) -> Vsock {
    let mut m = ram();
    let mut dev = VsockDevice::new(42).unwrap();
    dev.listen(5000).unwrap();
    dev.listen_seqpacket(6000).unwrap();
    let mut t = MmioTransport::new(dev, 9);
    let rx = DriverQueue::with_format(format, 0, 0x1000, 16);
    let tx = DriverQueue::with_format(format, 1, 0x2000, 16);
    let ev = DriverQueue::with_format(format, 2, 0x3000, 4);
    assert_eq!(
        init(&mut t, &mut m, base(format) | features, &[&rx, &tx, &ev]) & status::DRIVER_OK,
        status::DRIVER_OK
    );
    (t, m, rx, tx)
}

fn pkt(kind: u16, op: u16, dst_port: u32, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut v = VsockHeader {
        src_cid: 42,
        dst_cid: HOST_CID,
        src_port: 1234,
        dst_port,
        len: payload.len() as u32,
        kind,
        op,
        flags,
        buf_alloc: 64 * 1024,
        fwd_cnt: 0,
    }
    .encode()
    .to_vec();
    v.extend_from_slice(payload);
    v
}

fn send(v: &mut Vsock, p: &[u8]) {
    let (t, m, _, tx) = v;
    m.write(0x5_0000, p).unwrap();
    let id = tx.post(m, &[(0x5_0000, p.len() as u32, false)]);
    w32(t, m, reg::QUEUE_NOTIFY, 1);
    assert_eq!(tx.used(m), Some((id, 0)));
}

fn recv(v: &mut Vsock, room: u32) -> Option<(VsockHeader, Vec<u8>)> {
    let (t, m, rx, _) = v;
    if rx.outstanding() == 0 {
        rx.post(m, &[(0x6_0000, room, true)]);
    }
    t.poll(m, 0).unwrap();
    let (_, len) = rx.used(m)?;
    let mut b = vec![0u8; len as usize];
    m.read(0x6_0000, &mut b).unwrap();
    Some((VsockHeader::decode(&b).unwrap(), b[HEADER_BYTES..].to_vec()))
}

#[test]
fn vsock_seqpacket_preserves_message_boundaries() {
    for format in [RingFormat::Split, RingFormat::Packed] {
        let mut v = vsock(format, VIRTIO_VSOCK_F_SEQPACKET);
        let key = (1234, 6000);
        send(&mut v, &pkt(TYPE_SEQPACKET, OP_REQUEST, 6000, 0, &[]));
        let (h, _) = recv(&mut v, 8192).unwrap();
        assert_eq!((h.op, h.kind), (OP_RESPONSE, TYPE_SEQPACKET), "{format:?}");
        // A message in three packets; only the last carries EOM (and EOR).
        send(&mut v, &pkt(TYPE_SEQPACKET, OP_RW, 6000, 0, b"hello "));
        send(&mut v, &pkt(TYPE_SEQPACKET, OP_RW, 6000, 0, b"seq"));
        assert_eq!(v.0.with_device(|d| d.recv_message(key)), Ok(None));
        send(
            &mut v,
            &pkt(TYPE_SEQPACKET, OP_RW, 6000, SEQ_EOM | SEQ_EOR, b"packet"),
        );
        send(
            &mut v,
            &pkt(TYPE_SEQPACKET, OP_RW, 6000, SEQ_EOM, b"second"),
        );
        let first = v.0.with_device(|d| d.recv_message(key)).unwrap().unwrap();
        assert_eq!(
            first,
            SeqMessage {
                data: b"hello seqpacket".to_vec(),
                eor: true
            }
        );
        let second = v.0.with_device(|d| d.recv_message(key)).unwrap().unwrap();
        assert_eq!(
            second,
            SeqMessage {
                data: b"second".to_vec(),
                eor: false
            }
        );
        assert_eq!(v.0.device().stats().messages_rx, 2);
        // Stream calls on a seqpacket connection are refused.
        assert_eq!(v.0.with_device(|d| d.recv(key)), Err(VsockError::WrongType));
        assert_eq!(
            v.0.with_device(|d| d.send(key, b"x")),
            Err(VsockError::WrongType)
        );
        // Credit updates for the bytes the host consumed come first.
        while let Some((h, _)) = recv(&mut v, 8192) {
            assert_eq!(h.op, OP_CREDIT_UPDATE);
        }
        // Host to guest: 6000 bytes go as two packets, EOM|EOR on the last.
        let msg: Vec<u8> = (0..6000).map(|i| (i % 251) as u8).collect();
        v.0.with_device(|d| d.send_message(key, &msg, true))
            .unwrap();
        let (h1, p1) = recv(&mut v, 8192).unwrap();
        let (h2, p2) = recv(&mut v, 8192).unwrap();
        assert_eq!((h1.op, h1.flags, p1.len()), (OP_RW, 0, HOST_PACKET_BYTES));
        assert_eq!(
            (h2.flags, p2.len()),
            (SEQ_EOM | SEQ_EOR, 6000 - HOST_PACKET_BYTES)
        );
        assert_eq!([p1, p2].concat(), msg);
        // A receive buffer that splits a packet keeps the flags on the end.
        v.0.with_device(|d| d.send_message(key, b"0123456789", false))
            .unwrap();
        let (a, pa) = recv(&mut v, (HEADER_BYTES + 4) as u32).unwrap();
        let (b, pb) = recv(&mut v, (HEADER_BYTES + 4) as u32).unwrap();
        let (c, pc) = recv(&mut v, (HEADER_BYTES + 4) as u32).unwrap();
        assert_eq!((a.flags, b.flags, c.flags), (0, 0, SEQ_EOM));
        assert_eq!([pa, pb, pc].concat(), b"0123456789");
        assert_eq!(v.0.device().stats().messages_tx, 2);
    }
}

#[test]
fn vsock_seqpacket_type_rules_and_credit() {
    let mut v = vsock(RingFormat::Split, VIRTIO_VSOCK_F_SEQPACKET);
    // A stream request to the seqpacket listener is reset, and vice versa.
    send(&mut v, &pkt(TYPE_STREAM, OP_REQUEST, 6000, 0, &[]));
    assert_eq!(recv(&mut v, 4096).unwrap().0.op, OP_RST);
    send(&mut v, &pkt(TYPE_SEQPACKET, OP_REQUEST, 5000, 0, &[]));
    assert_eq!(recv(&mut v, 4096).unwrap().0.op, OP_RST);
    assert!(v.0.device().connections().is_empty());
    // A stream packet on a seqpacket connection resets it.
    send(&mut v, &pkt(TYPE_SEQPACKET, OP_REQUEST, 6000, 0, &[]));
    assert_eq!(recv(&mut v, 4096).unwrap().0.op, OP_RESPONSE);
    send(&mut v, &pkt(TYPE_STREAM, OP_RW, 6000, 0, b"x"));
    assert_eq!(recv(&mut v, 4096).unwrap().0.op, OP_RST);
    assert!(v.0.device().connections().is_empty());
    // The host never queues part of a message: no credit, no message.
    let key = v.0.with_device(|d| d.connect_seqpacket(7000, 80)).unwrap();
    let (h, _) = recv(&mut v, 4096).unwrap();
    assert_eq!((h.op, h.kind), (OP_REQUEST, TYPE_SEQPACKET));
    // The guest answers with only 100 bytes of buffer.
    let mut resp = VsockHeader::decode(&pkt(TYPE_SEQPACKET, OP_RESPONSE, 7000, 0, &[])).unwrap();
    resp.src_port = 80;
    resp.buf_alloc = 100;
    send(&mut v, &resp.encode());
    assert_eq!(
        v.0.device().connection_state(key),
        Some(ConnState::Connected)
    );
    assert_eq!(
        v.0.with_device(|d| d.send_message(key, &[0u8; 101], false)),
        Err(VsockError::WouldBlock)
    );
    assert_eq!(v.0.device().outbox_len(), 0);
    v.0.with_device(|d| d.send_message(key, &[0u8; 100], false))
        .unwrap();
    assert_eq!(recv(&mut v, 4096).unwrap().0.flags, SEQ_EOM);
}

#[test]
fn vsock_seqpacket_unread_messages_are_bounded() {
    // Empty messages use no credit, so the message count is bounded on its
    // own: one past the limit resets the connection.
    let mut v = vsock(RingFormat::Split, VIRTIO_VSOCK_F_SEQPACKET);
    let key = (1234, 6000);
    send(&mut v, &pkt(TYPE_SEQPACKET, OP_REQUEST, 6000, 0, &[]));
    assert_eq!(recv(&mut v, 4096).unwrap().0.op, OP_RESPONSE);
    for _ in 0..MAX_QUEUED_MESSAGES {
        send(&mut v, &pkt(TYPE_SEQPACKET, OP_RW, 6000, SEQ_EOM, &[]));
    }
    assert_eq!(
        v.0.device().connection_state(key),
        Some(ConnState::Connected)
    );
    send(&mut v, &pkt(TYPE_SEQPACKET, OP_RW, 6000, SEQ_EOM, &[]));
    assert_eq!(v.0.device().connection_state(key), None);
    assert_eq!(v.0.device().stats().message_overflows, 1);
    assert_eq!(v.0.device().stats().credit_violations, 0);
    assert_eq!(recv(&mut v, 4096).unwrap().0.op, OP_RST);
}

#[test]
fn vsock_seqpacket_needs_the_feature() {
    let mut v = vsock(RingFormat::Packed, 0);
    assert_eq!(
        v.0.with_device(|d| d.connect_seqpacket(7000, 80)),
        Err(VsockError::WrongType)
    );
    send(&mut v, &pkt(TYPE_SEQPACKET, OP_REQUEST, 6000, 0, &[]));
    assert_eq!(recv(&mut v, 4096).unwrap().0.op, OP_RST);
    // Stream still works.
    send(&mut v, &pkt(TYPE_STREAM, OP_REQUEST, 5000, 0, &[]));
    assert_eq!(recv(&mut v, 4096).unwrap().0.op, OP_RESPONSE);
}
