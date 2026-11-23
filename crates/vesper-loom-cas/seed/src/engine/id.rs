//! Frozen Rust object identity formatting, parsing and blob hashing.

use crate::error::CasError;
use crate::id::{Digest, Domain, ObjectHasher, ObjectId, ObjectKind};

impl Digest {
    pub(crate) fn to_hex_rust(self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    }

    pub(crate) fn from_hex_rust(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != 64 {
            return None;
        }
        fn nibble(c: u8) -> Option<u8> {
            match c {
                b'0'..=b'9' => Some(c - b'0'),
                b'a'..=b'f' => Some(c - b'a' + 10),
                _ => None,
            }
        }
        let mut out = [0u8; 32];
        for (i, pair) in bytes.as_chunks::<2>().0.iter().enumerate() {
            out[i] = (nibble(pair[0])? << 4) | nibble(pair[1])?;
        }
        Some(Digest(out))
    }
}

impl ObjectId {
    pub(crate) fn from_str_rust(
        s: &str,
        invalid: impl Fn() -> CasError + Copy,
    ) -> Result<Self, CasError> {
        let mut parts = s.split(':');
        let (Some(scheme), Some(domain), Some(kind), Some(hex), None) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            return Err(invalid());
        };
        if scheme != "b3v1" {
            return Err(invalid());
        }
        let domain = Domain::parse(domain).ok_or_else(invalid)?;
        let kind = ObjectKind::parse(kind).ok_or_else(invalid)?;
        let digest = Digest::from_hex(hex).ok_or_else(invalid)?;
        Ok(ObjectId::new(domain, kind, digest))
    }
}

pub(crate) fn blob_id_rust(domain: Domain, bytes: &[u8]) -> ObjectId {
    let mut hasher = ObjectHasher::new(domain, ObjectKind::Blob);
    hasher.update(bytes);
    hasher.finish()
}
