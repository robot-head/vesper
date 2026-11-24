//! The Vesper virtio-net TX validation, control-queue parsing, and
//! configuration-space encoder against the Rust reference (under `vesper-port`).
#![cfg(feature = "vesper-port")]

use vesper_aether::devices::mmio::VirtioDevice;
use vesper_aether::devices::net::{
    ETHERNET_HEADER_BYTES, NET_HDR_BYTES, NetDevice, VIRTIO_NET_CTRL_MQ,
    VIRTIO_NET_CTRL_MQ_VQ_PAIRS_SET, VIRTIO_NET_F_MQ,
};

#[test]
fn config_space_matches_rust() {
    let mut dev = NetDevice::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56], 1500)
        .unwrap()
        .with_queue_pairs(4)
        .unwrap();
    let mut port_cfg = [0u8; 12];
    let mut ref_cfg = [0u8; 12];
    dev.read_config(0, &mut port_cfg);
    dev.read_config_reference(0, &mut ref_cfg);
    assert_eq!(port_cfg, ref_cfg);

    dev.set_link(false);
    let mut port_sub = [0u8; 6];
    let mut ref_sub = [0u8; 6];
    dev.read_config(6, &mut port_sub);
    dev.read_config_reference(6, &mut ref_sub);
    assert_eq!(port_sub, ref_sub);
}

#[test]
fn tx_and_ctrl_match_rust() {
    let mtu = 1500u16;
    let max_frame = ETHERNET_HEADER_BYTES + usize::from(mtu);

    let mut short = vec![0u8; NET_HDR_BYTES + ETHERNET_HEADER_BYTES - 1];
    let mut bad_flags = vec![0u8; NET_HDR_BYTES + ETHERNET_HEADER_BYTES];
    bad_flags[0] = 1;
    let mut bad_gso = vec![0u8; NET_HDR_BYTES + ETHERNET_HEADER_BYTES];
    bad_gso[1] = 1;
    let valid_min = vec![0u8; NET_HDR_BYTES + ETHERNET_HEADER_BYTES];
    let valid_max = vec![0u8; NET_HDR_BYTES + max_frame];
    let oversized = vec![0u8; NET_HDR_BYTES + max_frame + 1];
    short[0] = 0;

    for link_up in [true, false] {
        for frame in [
            &short, &bad_flags, &bad_gso, &valid_min, &valid_max, &oversized,
        ] {
            let mut dev = NetDevice::new([0x02, 0, 0, 0, 0, 1], mtu).unwrap();
            dev.set_link(link_up);
            let expected_ok = dev.transmit_valid_reference(frame);
            dev.transmit_for_port_test(frame);
            assert_eq!(dev.stats().tx_frames == 1, expected_ok);
            assert_eq!(dev.stats().tx_dropped == 1, !expected_ok);
        }
    }

    let ctrl_cmds: &[&[u8]] = &[
        &[VIRTIO_NET_CTRL_MQ, VIRTIO_NET_CTRL_MQ_VQ_PAIRS_SET, 0, 0],
        &[VIRTIO_NET_CTRL_MQ, VIRTIO_NET_CTRL_MQ_VQ_PAIRS_SET, 1, 0],
        &[VIRTIO_NET_CTRL_MQ, VIRTIO_NET_CTRL_MQ_VQ_PAIRS_SET, 4, 0],
        &[VIRTIO_NET_CTRL_MQ, VIRTIO_NET_CTRL_MQ_VQ_PAIRS_SET, 5, 0],
        &[VIRTIO_NET_CTRL_MQ, 1, 2, 0],
        &[9, VIRTIO_NET_CTRL_MQ_VQ_PAIRS_SET, 2, 0],
        &[VIRTIO_NET_CTRL_MQ, VIRTIO_NET_CTRL_MQ_VQ_PAIRS_SET, 2],
    ];
    for features in [0u64, VIRTIO_NET_F_MQ] {
        for cmd in ctrl_cmds {
            let mut dev_port = NetDevice::new([0x02, 0, 0, 0, 0, 2], mtu)
                .unwrap()
                .with_queue_pairs(4)
                .unwrap();
            let mut dev_ref = NetDevice::new([0x02, 0, 0, 0, 0, 2], mtu)
                .unwrap()
                .with_queue_pairs(4)
                .unwrap();
            dev_port.activate(features);
            dev_ref.activate(features);
            assert_eq!(
                dev_port.control_for_port_test(cmd),
                dev_ref.control_reference(cmd)
            );
            assert_eq!(dev_port.stats(), dev_ref.stats());
        }
    }
}
