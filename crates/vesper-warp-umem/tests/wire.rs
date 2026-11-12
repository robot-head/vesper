//! `XdpDescWire` is the exact Linux `struct xdp_desc` layout.

use vesper_warp_umem::{UmemConfig, UmemError, XDP_DESC_BYTES, XdpDesc, XdpDescBytes, XdpDescWire};

#[test]
fn layout_is_sixteen_little_endian_bytes() {
    let wire = XdpDescWire::new();
    let layout = wire.layout();
    assert_eq!(layout.size(), XDP_DESC_BYTES);
    // Explicit-endian wire integers are byte-aligned in vesper-ffi; the
    // kernel's 8-byte alignment is checked against libc below.
    assert_eq!(layout.alignment(), 1);
    let offsets: Vec<_> = layout
        .fields()
        .iter()
        .map(|f| (f.name().to_string(), f.offset(), f.size()))
        .collect();
    assert_eq!(
        offsets,
        [
            ("umem_addr".to_string(), 0, 8),
            ("pkt_byte_len".to_string(), 8, 4),
            ("options".to_string(), 12, 4)
        ]
    );
    let desc = XdpDesc {
        addr: 0x0102_0304_0506_0708,
        len: 0x1122_3344,
        options: 0x0000_0001,
    };
    let bytes = wire.encode(desc).unwrap();
    assert_eq!(
        bytes.0,
        [8, 7, 6, 5, 4, 3, 2, 1, 0x44, 0x33, 0x22, 0x11, 1, 0, 0, 0]
    );
    assert_eq!(wire.decode(&bytes).unwrap(), desc);
    assert_eq!(
        wire.decode(&XdpDescBytes::default()).unwrap(),
        XdpDesc::default()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn matches_libc_xdp_desc() {
    use std::mem::{align_of, offset_of, size_of};
    assert_eq!(size_of::<libc::xdp_desc>(), XDP_DESC_BYTES);
    assert_eq!(align_of::<libc::xdp_desc>(), 8);
    let wire = XdpDescWire::new();
    let field = |n| wire.layout().field(n).unwrap().offset();
    assert_eq!(offset_of!(libc::xdp_desc, addr), field("umem_addr"));
    assert_eq!(offset_of!(libc::xdp_desc, len), field("pkt_byte_len"));
    assert_eq!(offset_of!(libc::xdp_desc, options), field("options"));
}

#[test]
fn config_validation_rejects_bad_geometry() {
    assert!(UmemConfig::default().validate().is_ok());
    let base = UmemConfig::with_frames(64);
    let cases = [
        UmemConfig {
            frame_count: 0,
            ..base
        },
        UmemConfig {
            frame_size: 3000,
            ..base
        },
        UmemConfig {
            frame_size: 1024,
            ..base
        },
        UmemConfig {
            frame_size: 65536,
            ..base
        },
        UmemConfig {
            headroom: 4096 - 63,
            ..base
        },
        UmemConfig {
            fill_size: 32,
            ..base
        },
        UmemConfig {
            rx_size: 100,
            ..base
        },
        UmemConfig { tx_size: 0, ..base },
    ];
    for bad in cases {
        assert!(
            matches!(bad.validate(), Err(UmemError::InvalidConfig(_))),
            "{bad:?}"
        );
    }
    assert!(
        UmemConfig {
            headroom: 4096 - 64,
            ..base
        }
        .validate()
        .is_ok()
    );
    assert_eq!(base.chunk_base(3), 3 * 4096);
}
