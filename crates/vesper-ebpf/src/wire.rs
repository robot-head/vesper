//! §27.2 wire structs with exact byte layouts and XDP return codes.

use std::fmt;

/// A padding-free wire struct with a fixed size (the `WireFrame` bound).
pub trait WireFrame: Sized {
    const SIZE: usize;
    fn encode(&self) -> Vec<u8>;
    fn decode(bytes: &[u8]) -> Result<Self, WireError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WireError {
    Short { need: usize, got: usize },
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WireError::Short { need, got } => write!(f, "need {need} bytes, got {got}"),
        }
    }
}

impl std::error::Error for WireError {}

/// `copy enum XdpAction`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum XdpAction {
    Aborted = 0,
    Drop = 1,
    Pass = 2,
    Tx = 3,
    Redirect = 4,
}

impl XdpAction {
    pub fn from_u64(v: u64) -> Option<XdpAction> {
        Some(match v {
            0 => XdpAction::Aborted,
            1 => XdpAction::Drop,
            2 => XdpAction::Pass,
            3 => XdpAction::Tx,
            4 => XdpAction::Redirect,
            _ => return None,
        })
    }
}

pub const ETH_P_IPV4: u16 = 0x0800;
pub const IPPROTO_TCP: u8 = 6;
pub const TCP_FLAG_SYN: u16 = 0x0002;
pub const TCP_FLAG_ACK: u16 = 0x0010;

/// Ethernet + IPv4 (no options) + TCP header: 14 + 20 + 20 = 54 bytes
/// (432 bits), no implicit padding. Multi-byte fields are big-endian on the
/// wire; this struct holds them as host integers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct EthIpv4TcpWire {
    pub dst_mac: [u8; 6],
    pub src_mac: [u8; 6],
    pub ether_type: u16,
    pub ver_ihl: u8,
    pub tos: u8,
    pub total_len: u16,
    pub ident: u16,
    pub frag_off: u16,
    pub ttl: u8,
    pub proto: u8,
    pub ip_checksum: u16,
    pub src_ipv4: u32,
    pub dst_ipv4: u32,
    pub src_port: u16,
    pub dst_port: u16,
    pub seq_num: u32,
    pub ack_num: u32,
    pub data_offset_flags: u16,
    pub window_size: u16,
    pub tcp_checksum: u16,
    pub urgent_ptr: u16,
}

/// Field byte offsets of [`EthIpv4TcpWire`].
pub mod eth_ipv4_tcp {
    pub const DST_MAC: usize = 0;
    pub const SRC_MAC: usize = 6;
    pub const ETHER_TYPE: usize = 12;
    pub const VER_IHL: usize = 14;
    pub const TOS: usize = 15;
    pub const TOTAL_LEN: usize = 16;
    pub const IDENT: usize = 18;
    pub const FRAG_OFF: usize = 20;
    pub const TTL: usize = 22;
    pub const PROTO: usize = 23;
    pub const IP_CHECKSUM: usize = 24;
    pub const SRC_IPV4: usize = 26;
    pub const DST_IPV4: usize = 30;
    pub const SRC_PORT: usize = 34;
    pub const DST_PORT: usize = 36;
    pub const SEQ_NUM: usize = 38;
    pub const ACK_NUM: usize = 42;
    pub const DATA_OFFSET_FLAGS: usize = 46;
    pub const WINDOW_SIZE: usize = 48;
    pub const TCP_CHECKSUM: usize = 50;
    pub const URGENT_PTR: usize = 52;
}

fn be16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

impl WireFrame for EthIpv4TcpWire {
    const SIZE: usize = 54;

    fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(54);
        b.extend_from_slice(&self.dst_mac);
        b.extend_from_slice(&self.src_mac);
        b.extend_from_slice(&self.ether_type.to_be_bytes());
        b.push(self.ver_ihl);
        b.push(self.tos);
        b.extend_from_slice(&self.total_len.to_be_bytes());
        b.extend_from_slice(&self.ident.to_be_bytes());
        b.extend_from_slice(&self.frag_off.to_be_bytes());
        b.push(self.ttl);
        b.push(self.proto);
        b.extend_from_slice(&self.ip_checksum.to_be_bytes());
        b.extend_from_slice(&self.src_ipv4.to_be_bytes());
        b.extend_from_slice(&self.dst_ipv4.to_be_bytes());
        b.extend_from_slice(&self.src_port.to_be_bytes());
        b.extend_from_slice(&self.dst_port.to_be_bytes());
        b.extend_from_slice(&self.seq_num.to_be_bytes());
        b.extend_from_slice(&self.ack_num.to_be_bytes());
        b.extend_from_slice(&self.data_offset_flags.to_be_bytes());
        b.extend_from_slice(&self.window_size.to_be_bytes());
        b.extend_from_slice(&self.tcp_checksum.to_be_bytes());
        b.extend_from_slice(&self.urgent_ptr.to_be_bytes());
        debug_assert_eq!(b.len(), Self::SIZE);
        b
    }

    fn decode(b: &[u8]) -> Result<Self, WireError> {
        use eth_ipv4_tcp as o;
        if b.len() < Self::SIZE {
            return Err(WireError::Short {
                need: Self::SIZE,
                got: b.len(),
            });
        }
        let mut dst_mac = [0; 6];
        dst_mac.copy_from_slice(&b[0..6]);
        let mut src_mac = [0; 6];
        src_mac.copy_from_slice(&b[6..12]);
        Ok(Self {
            dst_mac,
            src_mac,
            ether_type: be16(b, o::ETHER_TYPE),
            ver_ihl: b[o::VER_IHL],
            tos: b[o::TOS],
            total_len: be16(b, o::TOTAL_LEN),
            ident: be16(b, o::IDENT),
            frag_off: be16(b, o::FRAG_OFF),
            ttl: b[o::TTL],
            proto: b[o::PROTO],
            ip_checksum: be16(b, o::IP_CHECKSUM),
            src_ipv4: be32(b, o::SRC_IPV4),
            dst_ipv4: be32(b, o::DST_IPV4),
            src_port: be16(b, o::SRC_PORT),
            dst_port: be16(b, o::DST_PORT),
            seq_num: be32(b, o::SEQ_NUM),
            ack_num: be32(b, o::ACK_NUM),
            data_offset_flags: be16(b, o::DATA_OFFSET_FLAGS),
            window_size: be16(b, o::WINDOW_SIZE),
            tcp_checksum: be16(b, o::TCP_CHECKSUM),
            urgent_ptr: be16(b, o::URGENT_PTR),
        })
    }
}

/// Kernel-to-user SYN drop alert: 4 + 2 + 2 + 8 = 16 bytes, no padding.
/// `src_ipv4`, `dst_port` and `tcp_flags` are big-endian; `observed_pps`
/// is little-endian, as §27.2 states.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SynDropAlertWire {
    pub src_ipv4: u32,
    pub dst_port: u16,
    pub tcp_flags: u16,
    pub observed_pps: u64,
}

pub mod syn_drop_alert {
    pub const SRC_IPV4: usize = 0;
    pub const DST_PORT: usize = 4;
    pub const TCP_FLAGS: usize = 6;
    pub const OBSERVED_PPS: usize = 8;
}

impl WireFrame for SynDropAlertWire {
    const SIZE: usize = 16;

    fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(16);
        b.extend_from_slice(&self.src_ipv4.to_be_bytes());
        b.extend_from_slice(&self.dst_port.to_be_bytes());
        b.extend_from_slice(&self.tcp_flags.to_be_bytes());
        b.extend_from_slice(&self.observed_pps.to_le_bytes());
        b
    }

    fn decode(b: &[u8]) -> Result<Self, WireError> {
        if b.len() < Self::SIZE {
            return Err(WireError::Short {
                need: Self::SIZE,
                got: b.len(),
            });
        }
        let mut pps = [0u8; 8];
        pps.copy_from_slice(&b[8..16]);
        Ok(Self {
            src_ipv4: be32(b, 0),
            dst_port: be16(b, 4),
            tcp_flags: be16(b, 6),
            observed_pps: u64::from_le_bytes(pps),
        })
    }
}
