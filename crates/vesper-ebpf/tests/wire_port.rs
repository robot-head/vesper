//! The port's §27.2 wire codecs against the Rust ones, byte for byte and
//! field for field: random `EthIpv4TcpWire` frames and `SynDropAlertWire`
//! alerts encoded by both builds, every buffer length up to one past each
//! size decoded by both (short buffers are the same typed error), and
//! random byte strings decoded by both. The port encodes into a buffer the
//! caller lends (#2183).
#![cfg(feature = "vesper-port")]

use vesper_ebpf::wire::{EthIpv4TcpWire, SynDropAlertWire, WireError, WireFrame};
use vesper_ebpf_rust::wire as rust;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn bytes<const N: usize>(&mut self) -> [u8; N] {
        let mut out = [0; N];
        for b in &mut out {
            *b = self.next() as u8;
        }
        out
    }
}

fn frame(r: &mut Rng) -> (EthIpv4TcpWire, rust::EthIpv4TcpWire) {
    let (dst_mac, src_mac) = (r.bytes::<6>(), r.bytes::<6>());
    let w = [r.next(), r.next(), r.next(), r.next()];
    let port = EthIpv4TcpWire {
        dst_mac,
        src_mac,
        ether_type: w[0] as u16,
        ver_ihl: (w[0] >> 16) as u8,
        tos: (w[0] >> 24) as u8,
        total_len: (w[0] >> 32) as u16,
        ident: (w[0] >> 48) as u16,
        frag_off: w[1] as u16,
        ttl: (w[1] >> 16) as u8,
        proto: (w[1] >> 24) as u8,
        ip_checksum: (w[1] >> 32) as u16,
        src_ipv4: w[2] as u32,
        dst_ipv4: (w[2] >> 32) as u32,
        src_port: (w[1] >> 48) as u16,
        dst_port: w[3] as u16,
        seq_num: (w[3] >> 16) as u32,
        ack_num: (w[3] >> 32) as u32 ^ w[0] as u32,
        data_offset_flags: (w[3] >> 48) as u16,
        window_size: (w[2] >> 8) as u16,
        tcp_checksum: (w[2] >> 40) as u16,
        urgent_ptr: (w[1] >> 8) as u16,
    };
    let rust = rust::EthIpv4TcpWire {
        dst_mac: port.dst_mac,
        src_mac: port.src_mac,
        ether_type: port.ether_type,
        ver_ihl: port.ver_ihl,
        tos: port.tos,
        total_len: port.total_len,
        ident: port.ident,
        frag_off: port.frag_off,
        ttl: port.ttl,
        proto: port.proto,
        ip_checksum: port.ip_checksum,
        src_ipv4: port.src_ipv4,
        dst_ipv4: port.dst_ipv4,
        src_port: port.src_port,
        dst_port: port.dst_port,
        seq_num: port.seq_num,
        ack_num: port.ack_num,
        data_offset_flags: port.data_offset_flags,
        window_size: port.window_size,
        tcp_checksum: port.tcp_checksum,
        urgent_ptr: port.urgent_ptr,
    };
    (port, rust)
}

/// The Rust decode result in the port's types (the structs are the same
/// fields; `Debug` text is the same too, which keeps the comparison exact).
fn same<A: std::fmt::Debug, B: std::fmt::Debug>(a: &A, b: &B) -> bool {
    format!("{a:?}") == format!("{b:?}")
}

fn short(e: &WireError, r: &rust::WireError) -> bool {
    let (WireError::Short { need, got }, rust::WireError::Short { need: rn, got: rg }) = (e, r);
    need == rn && got == rg
}

#[test]
fn frames_encode_and_decode_like_rust() {
    let mut r = Rng(0x2183_eb9f_0000_0001);
    for _ in 0..20_000 {
        let (port, rust) = frame(&mut r);
        let bytes = port.encode();
        assert_eq!(bytes, rust::WireFrame::encode(&rust), "{port:?}");
        assert_eq!(bytes.len(), EthIpv4TcpWire::SIZE);
        let back = EthIpv4TcpWire::decode(&bytes).expect("decodes");
        assert_eq!(back, port);
    }
}

#[test]
fn alerts_encode_and_decode_like_rust() {
    let mut r = Rng(0x2183_eb9f_0000_0002);
    for _ in 0..20_000 {
        let (a, b) = (r.next(), r.next());
        let port = SynDropAlertWire {
            src_ipv4: a as u32,
            dst_port: (a >> 32) as u16,
            tcp_flags: (a >> 48) as u16,
            observed_pps: b,
        };
        let rust = rust::SynDropAlertWire {
            src_ipv4: port.src_ipv4,
            dst_port: port.dst_port,
            tcp_flags: port.tcp_flags,
            observed_pps: port.observed_pps,
        };
        let bytes = port.encode();
        assert_eq!(bytes, rust::WireFrame::encode(&rust), "{port:?}");
        assert_eq!(bytes.len(), SynDropAlertWire::SIZE);
        assert_eq!(SynDropAlertWire::decode(&bytes).expect("decodes"), port);
    }
}

#[test]
fn every_length_and_random_bytes_decode_like_rust() {
    let mut r = Rng(0x2183_eb9f_0000_0003);
    for _ in 0..2_000 {
        let bytes: Vec<u8> = (0..80).map(|_| r.next() as u8).collect();
        for len in 0..=EthIpv4TcpWire::SIZE + 1 {
            let s = &bytes[..len];
            match (
                EthIpv4TcpWire::decode(s),
                <rust::EthIpv4TcpWire as rust::WireFrame>::decode(s),
            ) {
                (Ok(p), Ok(q)) => assert!(same(&p, &q), "{len}: {p:?} vs {q:?}"),
                (Err(e), Err(f)) => assert!(short(&e, &f), "{len}: {e:?} vs {f:?}"),
                (p, q) => panic!("frame of {len} bytes: {p:?} vs {q:?}"),
            }
        }
        for len in 0..=SynDropAlertWire::SIZE + 1 {
            let s = &bytes[..len];
            match (
                SynDropAlertWire::decode(s),
                <rust::SynDropAlertWire as rust::WireFrame>::decode(s),
            ) {
                (Ok(p), Ok(q)) => assert!(same(&p, &q), "{len}: {p:?} vs {q:?}"),
                (Err(e), Err(f)) => assert!(short(&e, &f), "{len}: {e:?} vs {f:?}"),
                (p, q) => panic!("alert of {len} bytes: {p:?} vs {q:?}"),
            }
        }
    }
}
