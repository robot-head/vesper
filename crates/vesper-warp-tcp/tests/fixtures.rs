//! Checksums and parsing against hand-constructed pcap fixtures
//! (fixtures/gen_fixtures.py; checksums computed there by an independent
//! Python RFC 1071 implementation).

use vesper_warp_tcp::wire::{self, HeaderFields};
use vesper_warp_tcp::{Flags, IpPair, Segment};

struct Packet {
    ips: IpPair,
    tcp: Vec<u8>,
}

fn load() -> Vec<Packet> {
    let bytes = include_bytes!("../fixtures/tcp_fixtures.pcap");
    assert_eq!(&bytes[..4], &0xa1b2c3d4u32.to_le_bytes());
    assert_eq!(
        u32::from_le_bytes(bytes[20..24].try_into().unwrap()),
        101,
        "LINKTYPE_RAW"
    );
    let mut i = 24;
    let mut out = Vec::new();
    while i < bytes.len() {
        let incl = u32::from_le_bytes(bytes[i + 8..i + 12].try_into().unwrap()) as usize;
        let ip = &bytes[i + 16..i + 16 + incl];
        i += 16 + incl;
        match ip[0] >> 4 {
            4 => {
                let ihl = usize::from(ip[0] & 15) * 4;
                assert_eq!(
                    wire::fold(wire::sum_bytes(0, &ip[..ihl])),
                    0xffff,
                    "IPv4 header checksum"
                );
                assert_eq!(ip[9], 6);
                out.push(Packet {
                    ips: IpPair::V4 {
                        src: ip[12..16].try_into().unwrap(),
                        dst: ip[16..20].try_into().unwrap(),
                    },
                    tcp: ip[ihl..].to_vec(),
                });
            }
            6 => {
                assert_eq!(ip[6], 6);
                out.push(Packet {
                    ips: IpPair::V6 {
                        src: ip[8..24].try_into().unwrap(),
                        dst: ip[24..40].try_into().unwrap(),
                    },
                    tcp: ip[40..].to_vec(),
                });
            }
            v => panic!("ip version {v}"),
        }
    }
    out
}

#[test]
fn fixture_checksums_verify_and_recompute() {
    let pkts = load();
    assert_eq!(pkts.len(), 8);
    for (k, p) in pkts.iter().enumerate() {
        wire::verify_checksum(p.ips, &p.tcp).unwrap_or_else(|e| panic!("packet {k}: {e}"));
        let stored = u16::from_be_bytes([p.tcp[16], p.tcp[17]]);
        assert_eq!(
            wire::compute_checksum(p.ips, &p.tcp).unwrap(),
            stored,
            "packet {k}"
        );
        // Any single-bit flip is detected.
        let mut bad = p.tcp.clone();
        let last = bad.len() - 1;
        bad[last] ^= 0x10;
        assert!(wire::verify_checksum(p.ips, &bad).is_err());
        // The wrong pseudo-header is detected.
        assert!(
            wire::verify_checksum(p.ips.reversed(), &p.tcp).is_ok(),
            "addresses are summed symmetrically"
        );
        let other = match p.ips {
            IpPair::V4 { src, .. } => IpPair::V4 {
                src,
                dst: [1, 1, 1, 1],
            },
            IpPair::V6 { src, .. } => IpPair::V6 { src, dst: [1; 16] },
        };
        assert!(wire::verify_checksum(other, &p.tcp).is_err());
    }
}

#[test]
fn fixture_known_checksum_values() {
    let pkts = load();
    // Values printed by gen_fixtures.py (independent implementation).
    let sums: Vec<u16> = pkts
        .iter()
        .map(|p| u16::from_be_bytes([p.tcp[16], p.tcp[17]]))
        .collect();
    assert_eq!(sums[0], 0xafaa);
    assert_eq!(sums[1], 0x4a80);
}

#[test]
fn fixture_fields_and_options_decode() {
    let pkts = load();
    let syn = Segment::parse(&pkts[0].tcp).unwrap();
    assert_eq!(
        (syn.src_port, syn.dst_port, syn.seq.0, syn.flags),
        (40000, 80, 0x0102_0304, Flags::SYN)
    );
    assert_eq!(syn.window, 64240);
    assert_eq!(syn.options.mss, Some(1460));
    assert_eq!(syn.options.window_scale, Some(7));
    assert!(syn.options.sack_permitted);
    assert_eq!(
        syn.options.timestamps.map(|t| (t.val, t.ecr)),
        Some((4242, 0))
    );
    let data = Segment::parse(&pkts[2].tcp).unwrap();
    assert_eq!(data.payload, b"GET / HTTP/1.1\r\n\r\n!");
    assert_eq!(data.payload.len() % 2, 1, "odd length exercises padding");
    assert!(data.flags.contains(Flags::PSH | Flags::ACK));
    let fin = Segment::parse(&pkts[3].tcp).unwrap();
    assert_eq!(fin.seq_len(), 1);
    let urg = Segment::parse(&pkts[4].tcp).unwrap();
    assert_eq!(urg.urgent, 5);
    let rst = Segment::parse(&pkts[5].tcp).unwrap();
    assert_eq!((rst.flags, rst.seq.0), (Flags::RST, 0xffff_fffe));
    let sack = Segment::parse(&pkts[6].tcp).unwrap();
    let b = sack.options.sack();
    assert_eq!(b.len(), 2);
    assert_eq!(
        (b[0].left.0, b[0].right.0, b[1].left.0, b[1].right.0),
        (0x1000_0100, 0x1000_0200, 0x1000_0300, 0x1000_0400)
    );
    let v6 = Segment::parse(&pkts[7].tcp).unwrap();
    assert_eq!(v6.payload.len(), 255);
}

#[test]
fn fixtures_reserialize_byte_for_byte() {
    // Packets whose options use this crate's canonical layout.
    let pkts = load();
    for k in [2usize, 3, 5, 6, 7] {
        let p = &pkts[k];
        let s = Segment::parse(&p.tcp).unwrap();
        let mut out = vec![0u8; p.tcp.len()];
        let h = HeaderFields {
            src_port: s.src_port,
            dst_port: s.dst_port,
            seq: s.seq,
            ack: s.ack,
            flags: s.flags,
            window: s.window,
            urgent: s.urgent,
        };
        let hl = wire::write_header(&mut out, &h, &s.options).unwrap();
        assert_eq!(hl, s.header_len, "packet {k}");
        out[hl..].copy_from_slice(s.payload);
        wire::fill_checksum(p.ips, &mut out).unwrap();
        assert_eq!(out, p.tcp, "packet {k}");
    }
}
