//! Cross-check the fixed TCP header offsets against a `vesper-ffi` Bridge
//! wire layout (§13.2 big-endian wire integers), and read a fixture header
//! through the generic view.

use vesper_ffi::{Declaration, Endian, Field, Integer, Registry, WireType, view_wire};
use vesper_warp_tcp::wire::{HEADER_LEN, offsets};

fn be(bits: u16) -> WireType {
    WireType::Integer(Integer::unsigned(bits, Endian::Big))
}

#[test]
fn tcp_header_layout_matches_bridge_wire_struct() {
    let decl = Declaration::new(
        "TcpHeaderWire",
        vec![
            Field::new("src_port", be(16)),
            Field::new("dst_port", be(16)),
            Field::new("seq", be(32)),
            Field::new("ack", be(32)),
            Field::new("data_offset", be(8)),
            Field::new("flags", be(8)),
            Field::new("window", be(16)),
            Field::new("checksum", be(16)),
            Field::new("urgent", be(16)),
        ],
    );
    let reg = Registry::new([decl]).unwrap();
    let layout = reg.compile("TcpHeaderWire").unwrap();
    assert_eq!(layout.size(), HEADER_LEN);
    for (name, off) in [
        ("src_port", offsets::SRC_PORT),
        ("dst_port", offsets::DST_PORT),
        ("seq", offsets::SEQ),
        ("ack", offsets::ACK),
        ("data_offset", offsets::DATA_OFFSET),
        ("flags", offsets::FLAGS),
        ("window", offsets::WINDOW),
        ("checksum", offsets::CHECKSUM),
        ("urgent", offsets::URGENT),
    ] {
        assert_eq!(layout.field(name).unwrap().offset(), off, "{name}");
    }
    let pcap = include_bytes!("../fixtures/tcp_fixtures.pcap");
    // First record: IPv4 SYN; TCP header starts after the 20-byte IP header.
    let tcp = &pcap[24 + 16 + 20..24 + 16 + 20 + HEADER_LEN];
    let v = view_wire(&layout, tcp).unwrap();
    assert_eq!(v.read_u64(&["src_port"]).unwrap(), 40000);
    assert_eq!(v.read_u64(&["seq"]).unwrap(), 0x0102_0304);
    assert_eq!(v.read_u64(&["window"]).unwrap(), 64240);
}
