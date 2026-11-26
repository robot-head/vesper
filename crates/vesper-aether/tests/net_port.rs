//! The Vesper virtio-net device's transmit validation, control queue and
//! configuration space against the rules of virtio-v1.2 §5.1 (the Rust
//! device it replaced is compared in src/port_reference_tests/net.rs).

use vesper_aether::devices::mmio::VirtioDevice;
use vesper_aether::devices::net::{
    ETHERNET_HEADER_BYTES, NET_HDR_BYTES, NetDevice, VIRTIO_NET_CTRL_MQ,
    VIRTIO_NET_CTRL_MQ_VQ_PAIRS_SET, VIRTIO_NET_ERR, VIRTIO_NET_F_MQ, VIRTIO_NET_OK,
};

#[test]
fn config_space_matches_rust() {
    let mut dev = NetDevice::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56], 1500)
        .unwrap()
        .with_queue_pairs(4)
        .unwrap();
    let mut cfg = [0u8; 12];
    dev.read_config(0, &mut cfg);
    assert_eq!(
        cfg,
        [0x52, 0x54, 0x00, 0x12, 0x34, 0x56, 1, 0, 4, 0, 0xdc, 0x05]
    );
    dev.set_link(false);
    let mut sub = [0u8; 6];
    dev.read_config(6, &mut sub);
    assert_eq!(sub, [0, 0, 4, 0, 0xdc, 0x05]);
}

#[test]
fn tx_and_ctrl_match_rust() {
    let mtu = 1500u16;
    let max_frame = ETHERNET_HEADER_BYTES + usize::from(mtu);

    let short = vec![0u8; NET_HDR_BYTES + ETHERNET_HEADER_BYTES - 1];
    let mut bad_flags = vec![0u8; NET_HDR_BYTES + ETHERNET_HEADER_BYTES];
    bad_flags[0] = 1;
    let mut bad_gso = vec![0u8; NET_HDR_BYTES + ETHERNET_HEADER_BYTES];
    bad_gso[1] = 1;
    let valid_min = vec![0u8; NET_HDR_BYTES + ETHERNET_HEADER_BYTES];
    let valid_max = vec![0u8; NET_HDR_BYTES + max_frame];
    let oversized = vec![0u8; NET_HDR_BYTES + max_frame + 1];

    for link_up in [true, false] {
        for (frame, valid) in [
            (&short, false),
            (&bad_flags, false),
            (&bad_gso, false),
            (&valid_min, true),
            (&valid_max, true),
            (&oversized, false),
        ] {
            let mut dev = NetDevice::new([0x02, 0, 0, 0, 0, 1], mtu).unwrap();
            dev.set_link(link_up);
            dev.transmit_for_port_test(frame);
            let ok = valid && link_up;
            assert_eq!(dev.stats().tx_frames == 1, ok, "{} bytes", frame.len());
            assert_eq!(dev.stats().tx_dropped == 1, !ok);
            assert_eq!(dev.take_tx().is_some(), ok);
        }
    }

    let ctrl_cmds: &[(&[u8], Option<u16>)] = &[
        (
            &[VIRTIO_NET_CTRL_MQ, VIRTIO_NET_CTRL_MQ_VQ_PAIRS_SET, 0, 0],
            None,
        ),
        (
            &[VIRTIO_NET_CTRL_MQ, VIRTIO_NET_CTRL_MQ_VQ_PAIRS_SET, 1, 0],
            Some(1),
        ),
        (
            &[VIRTIO_NET_CTRL_MQ, VIRTIO_NET_CTRL_MQ_VQ_PAIRS_SET, 4, 0],
            Some(4),
        ),
        (
            &[VIRTIO_NET_CTRL_MQ, VIRTIO_NET_CTRL_MQ_VQ_PAIRS_SET, 5, 0],
            None,
        ),
        (&[VIRTIO_NET_CTRL_MQ, 1, 2, 0], None),
        (&[9, VIRTIO_NET_CTRL_MQ_VQ_PAIRS_SET, 2, 0], None),
        (
            &[VIRTIO_NET_CTRL_MQ, VIRTIO_NET_CTRL_MQ_VQ_PAIRS_SET, 2],
            None,
        ),
    ];
    for features in [0u64, VIRTIO_NET_F_MQ] {
        for &(cmd, pairs) in ctrl_cmds {
            let mut dev = NetDevice::new([0x02, 0, 0, 0, 0, 2], mtu)
                .unwrap()
                .with_queue_pairs(4)
                .unwrap();
            dev.activate(features);
            let pairs = pairs.filter(|_| features != 0);
            let ack = dev.control_for_port_test(cmd);
            assert_eq!(
                ack,
                if pairs.is_some() {
                    VIRTIO_NET_OK
                } else {
                    VIRTIO_NET_ERR
                }
            );
            assert_eq!(dev.active_pairs(), pairs.unwrap_or(1));
            assert_eq!(dev.stats().ctrl_ok, u64::from(pairs.is_some()));
            assert_eq!(dev.stats().ctrl_err, u64::from(pairs.is_none()));
        }
    }
}
