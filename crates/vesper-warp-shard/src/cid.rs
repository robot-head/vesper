//! QUIC connection IDs (RFC 9000 §5.1) as fixed-size values.

use core::fmt;

/// Longest connection ID allowed by QUIC version 1 (RFC 9000 §17.2).
pub const MAX_CID_LEN: usize = 20;

/// Number of 32-bit words used to store a lane's CID (`ceil(20 / 4)`).
pub const CID_WORDS: usize = MAX_CID_LEN.div_ceil(4);

/// A connection ID of 0..=20 bytes, stored inline without allocation.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionId {
    len: u8,
    bytes: [u8; MAX_CID_LEN],
}

/// A connection ID longer than [`MAX_CID_LEN`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CidTooLong {
    /// Rejected length.
    pub len: usize,
}

impl fmt::Display for CidTooLong {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "connection ID of {} bytes exceeds 20", self.len)
    }
}

impl std::error::Error for CidTooLong {}

impl ConnectionId {
    /// The empty connection ID.
    pub const EMPTY: ConnectionId = ConnectionId {
        len: 0,
        bytes: [0; MAX_CID_LEN],
    };

    /// Copies `bytes` into a connection ID.
    pub fn from_slice(bytes: &[u8]) -> Result<ConnectionId, CidTooLong> {
        if bytes.len() > MAX_CID_LEN {
            return Err(CidTooLong { len: bytes.len() });
        }
        let mut out = ConnectionId::EMPTY;
        out.bytes[..bytes.len()].copy_from_slice(bytes);
        // Cannot truncate: length checked to be <= 20 above.
        out.len = bytes.len() as u8;
        Ok(out)
    }

    /// An 8-byte connection ID from a big-endian integer (test convenience).
    pub fn from_u64(v: u64) -> ConnectionId {
        let mut out = ConnectionId::EMPTY;
        out.bytes[..8].copy_from_slice(&v.to_be_bytes());
        out.len = 8;
        out
    }

    /// Length in bytes.
    pub fn len(&self) -> usize {
        usize::from(self.len)
    }

    /// Whether the CID has zero length.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The CID bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len()]
    }

    /// The CID as little-endian 32-bit words, zero padded to 20 bytes.
    pub(crate) fn words(&self) -> [u32; CID_WORDS] {
        let mut w = [0u32; CID_WORDS];
        let (chunks, _) = self.bytes.as_chunks::<4>();
        for (word, chunk) in w.iter_mut().zip(chunks) {
            *word = u32::from_le_bytes(*chunk);
        }
        w
    }

    pub(crate) fn from_words(words: &[u32; CID_WORDS], len: u8) -> ConnectionId {
        let mut out = ConnectionId::EMPTY;
        for (i, w) in words.iter().enumerate() {
            out.bytes[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
        }
        out.len = len;
        out
    }
}

impl fmt::Debug for ConnectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Cid(")?;
        for b in self.as_bytes() {
            write!(f, "{b:02x}")?;
        }
        f.write_str(")")
    }
}
