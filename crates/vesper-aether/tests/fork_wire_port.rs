//! The Vesper 24-byte `AetherVmGenIdWire` codec against the Rust reference
//! (under `vesper-port`).
#![cfg(feature = "vesper-port")]

use vesper_aether::fork::{ForkError, VMGENID_WIRE_BYTES, VmGenIdWire};

#[test]
fn encode_roundtrip_matches_rust() {
    let w = VmGenIdWire {
        vm_gen_lo: 0x0102_0304_0506_0708,
        vm_gen_hi: 0x1112_1314_1516_1718,
        host_tsc_khz: 3_200_000,
        virtio_mmio_count: 4,
    };
    let port = w.encode();
    let reference = w.encode_reference();
    assert_eq!(port, reference);
    assert_eq!(
        VmGenIdWire::decode(&port),
        VmGenIdWire::decode_reference(&reference)
    );
}

#[test]
fn decode_length_matches_rust() {
    let valid = VmGenIdWire {
        vm_gen_lo: 1,
        vm_gen_hi: 2,
        host_tsc_khz: 3,
        virtio_mmio_count: 4,
    }
    .encode();
    for len in [
        0usize,
        1,
        VMGENID_WIRE_BYTES - 1,
        VMGENID_WIRE_BYTES + 1,
        32,
    ] {
        let mut buf = vec![0u8; len];
        let copy = len.min(valid.len());
        buf[..copy].copy_from_slice(&valid[..copy]);
        assert_eq!(VmGenIdWire::decode(&buf), Err(ForkError::WrongLength));
        assert_eq!(
            VmGenIdWire::decode(&buf),
            VmGenIdWire::decode_reference(&buf)
        );
    }
}
