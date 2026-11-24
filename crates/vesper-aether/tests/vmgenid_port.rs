//! The Vesper vmgenid address validation and ID encoding against the Rust
//! reference (under `vesper-port`).
#![cfg(feature = "vesper-port")]

use vesper_aether::devices::vmgenid::VmGenIdDevice;
use vesper_aether::fork::VmGenId;

#[test]
fn address_validation_matches_rust() {
    let addrs = [
        0u64,
        8,
        4080,
        4088, // crosses 4 KiB page boundary
        4092,
        4096,
        0x1000_0ff0,
        0x1000_0ff8,
    ];
    for gpa in addrs {
        assert_eq!(
            VmGenIdDevice::new(gpa, 5).is_ok(),
            VmGenIdDevice::valid_addr_reference(gpa),
            "gpa {gpa:#x}"
        );
    }
}

#[test]
fn encode_matches_rust() {
    let id = VmGenId {
        lo: 0x0123_4567_89ab_cdef,
        hi: 0xfedc_ba98_7654_3210,
    };
    assert_eq!(
        VmGenIdDevice::encode(id),
        VmGenIdDevice::encode_reference(id)
    );
}
