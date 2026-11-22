//! Toeplitz receive-side-scaling hash, indirection table and 5-tuple input.
//!
//! This is a software model of NIC RSS. Nothing here programs a NIC's RSS key
//! or indirection table; a deployment that relies on hardware steering must
//! install the same key and table through the driver, and that is not done or
//! validated by this crate.

use core::fmt;
use std::net::IpAddr;

/// The default 40-byte key from Microsoft's RSS verification suite, also the
/// default of many NIC drivers.
pub const MICROSOFT_RSS_KEY: [u8; 40] = [
    0x6d, 0x5a, 0x56, 0xda, 0x25, 0x5b, 0x0e, 0xc2, 0x41, 0x67, 0x25, 0x3d, 0x43, 0xa3, 0x8f, 0xb0,
    0xd0, 0xca, 0x2b, 0xcb, 0xae, 0x7b, 0x30, 0xb4, 0x77, 0xcb, 0x2d, 0xa3, 0x80, 0x30, 0xf2, 0x0c,
    0x6a, 0x42, 0xb7, 0x3b, 0xbe, 0xac, 0x01, 0xfa,
];

/// Longest hash input: IPv6 source and destination plus two ports.
pub const MAX_RSS_INPUT: usize = 36;

/// Largest indirection table.
pub const MAX_INDIRECTION: usize = 512;

/// RSS errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RssError {
    /// Input longer than [`MAX_RSS_INPUT`].
    InputTooLong {
        /// Supplied length.
        len: usize,
    },
    /// Source and destination are different address families.
    MixedFamilies,
    /// Indirection table length not a power of two in 1..=512.
    IndirectionLength {
        /// Requested length.
        len: usize,
    },
    /// Zero shards, or more than `u16::MAX`.
    ShardCount {
        /// Requested count.
        shards: usize,
    },
    /// Indirection entry names a shard that does not exist.
    ShardOutOfRange {
        /// Entry position.
        entry: usize,
        /// Named shard.
        shard: u16,
    },
}

impl fmt::Display for RssError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for RssError {}

#[cfg(feature = "vesper-port")]
pub use crate::port::rss::{Toeplitz, toeplitz_reference};
#[cfg(not(feature = "vesper-port"))]
pub use crate::reference::rss::{Toeplitz, toeplitz_reference};

/// IP protocol numbers whose ports participate in the hash.
pub const IPPROTO_TCP: u8 = 6;
/// UDP.
pub const IPPROTO_UDP: u8 = 17;

/// A flow 5-tuple.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FiveTuple {
    /// Source address.
    pub src: IpAddr,
    /// Destination address.
    pub dst: IpAddr,
    /// Source port.
    pub src_port: u16,
    /// Destination port.
    pub dst_port: u16,
    /// IP protocol number.
    pub protocol: u8,
}

/// Which tuple fields feed the hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HashFields {
    /// Source and destination addresses only.
    Addresses,
    /// Addresses plus ports for TCP and UDP; addresses only otherwise.
    /// (Hashing UDP ports matches Linux `ethtool rx-flow-hash udp4 sdfn`;
    /// Windows RSS hashes UDP by addresses only.)
    AddressesAndPorts,
}

/// Serialized hash input: `src | dst | src_port | dst_port`, network order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RssInput {
    bytes: [u8; MAX_RSS_INPUT],
    len: u8,
}

impl RssInput {
    /// Builds the hash input for `t`.
    pub fn new(t: &FiveTuple, fields: HashFields) -> Result<RssInput, RssError> {
        let mut bytes = [0u8; MAX_RSS_INPUT];
        let mut len = match (t.src, t.dst) {
            (IpAddr::V4(s), IpAddr::V4(d)) => {
                bytes[..4].copy_from_slice(&s.octets());
                bytes[4..8].copy_from_slice(&d.octets());
                8
            }
            (IpAddr::V6(s), IpAddr::V6(d)) => {
                bytes[..16].copy_from_slice(&s.octets());
                bytes[16..32].copy_from_slice(&d.octets());
                32
            }
            _ => return Err(RssError::MixedFamilies),
        };
        let ports = fields == HashFields::AddressesAndPorts
            && (t.protocol == IPPROTO_TCP || t.protocol == IPPROTO_UDP);
        if ports {
            bytes[len..len + 2].copy_from_slice(&t.src_port.to_be_bytes());
            bytes[len + 2..len + 4].copy_from_slice(&t.dst_port.to_be_bytes());
            len += 4;
        }
        // len <= 36 fits u8.
        Ok(RssInput {
            bytes,
            len: len as u8,
        })
    }

    /// The serialized bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

/// RSS indirection table: `shard = entries[hash & (len - 1)]`, using the low
/// bits of the hash as NICs do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndirectionTable {
    entries: Box<[u16]>,
    shards: u16,
}

impl IndirectionTable {
    /// Round-robin table of `len` entries over `shards` shards.
    pub fn round_robin(len: usize, shards: usize) -> Result<IndirectionTable, RssError> {
        if len == 0 || len > MAX_INDIRECTION || !len.is_power_of_two() {
            return Err(RssError::IndirectionLength { len });
        }
        let shards16 = u16::try_from(shards)
            .ok()
            .filter(|&s| s > 0)
            .ok_or(RssError::ShardCount { shards })?;
        // i % shards < shards <= u16::MAX.
        let entries = (0..len).map(|i| (i % shards) as u16).collect();
        Ok(IndirectionTable {
            entries,
            shards: shards16,
        })
    }

    /// Replaces entry `entry` (rebalancing).
    pub fn set(&mut self, entry: usize, shard: u16) -> Result<(), RssError> {
        if shard >= self.shards || entry >= self.entries.len() {
            return Err(RssError::ShardOutOfRange { entry, shard });
        }
        self.entries[entry] = shard;
        Ok(())
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the table is empty (never true for a constructed table).
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of shards.
    pub fn shards(&self) -> u16 {
        self.shards
    }

    /// Shard for a hash value.
    pub fn shard_for(&self, hash: u32) -> u16 {
        self.entries[hash as usize & (self.entries.len() - 1)]
    }
}
