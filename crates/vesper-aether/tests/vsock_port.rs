//! The Vesper virtio-vsock header codec and peer-credit calculation against
//! the Rust reference (under `vesper-port`).
#![cfg(feature = "vesper-port")]

use vesper_aether::devices::mmio::{DeviceError, VirtioDevice};
use vesper_aether::devices::vsock::{
    HEADER_BYTES, HOST_CID, OP_CREDIT_UPDATE, OP_RESPONSE, TYPE_STREAM, VsockDevice, VsockError,
    VsockHeader,
};

#[test]
fn header_codec_matches_rust() {
    let hdr = VsockHeader {
        src_cid: 3,
        dst_cid: HOST_CID,
        src_port: 1025,
        dst_port: 8080,
        len: 512,
        kind: TYPE_STREAM,
        op: OP_RESPONSE,
        flags: 0x03,
        buf_alloc: 65536,
        fwd_cnt: 4096,
    };
    let port_bytes = hdr.encode();
    let ref_bytes = hdr.encode_reference();
    assert_eq!(port_bytes, ref_bytes);
    assert_eq!(
        VsockHeader::decode(&port_bytes),
        VsockHeader::decode_reference(&ref_bytes)
    );
    assert_eq!(VsockHeader::decode(&port_bytes[..HEADER_BYTES - 1]), None);
}

#[test]
fn credit_saturates_on_shrunken_alloc() {
    let guest_cid = 3u64;
    let mut dev = VsockDevice::new(guest_cid).unwrap();
    dev.activate(0);
    let key = dev.connect(4000, 5000).unwrap();

    // Deliver guest OP_RESPONSE granting 10 bytes of credit.
    let resp = VsockHeader {
        src_cid: guest_cid,
        dst_cid: HOST_CID,
        src_port: 5000,
        dst_port: 4000,
        len: 0,
        kind: TYPE_STREAM,
        op: OP_RESPONSE,
        flags: 0,
        buf_alloc: 10,
        fwd_cnt: 0,
    }
    .encode();
    dev.deliver_tx_for_port_test(&resp).unwrap();
    assert_eq!(dev.send(key, &[1u8; 10]), Ok(10));

    // Guest shrinks buf_alloc to 5 while tx_cnt == 10, peer_fwd_cnt == 0.
    let shrink = VsockHeader {
        src_cid: guest_cid,
        dst_cid: HOST_CID,
        src_port: 5000,
        dst_port: 4000,
        len: 0,
        kind: TYPE_STREAM,
        op: OP_CREDIT_UPDATE,
        flags: 0,
        buf_alloc: 5,
        fwd_cnt: 0,
    }
    .encode();
    dev.deliver_tx_for_port_test(&shrink).unwrap();
    let res: Result<usize, VsockError> = dev.send(key, &[2u8; 1]);
    assert_eq!(res, Ok(0));
    let _: fn(&mut VsockDevice, &[u8]) -> Result<(), DeviceError> =
        VsockDevice::deliver_tx_for_port_test;
}
