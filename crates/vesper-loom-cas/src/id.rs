//! Domain-separated BLAKE3 object identities.
//!
//! Every object hash is computed with BLAKE3's `derive_key` mode under a
//! context string that names the store schema version, the object's domain
//! (source, interface or artifact) and its kind (blob or tree). The same bytes
//! therefore receive different identities in different domains, and a blob can
//! never be confused with a tree whose canonical encoding happens to match it.

use std::fmt;
use std::str::FromStr;

use crate::error::CasError;

/// Version of the object hashing and encoding schema. It is part of every
/// hashing context and of the on-disk layout path.
pub const OBJECT_SCHEMA_VERSION: u32 = 1;

/// Explicit content domains (design §8.2.6 and the contract audit's
/// separation of source identity, interface identity and derived artifacts).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Domain {
    /// Unpacked package source files and trees (`source_blake3`).
    Source,
    /// Exported interface descriptions such as `.vhir` files (`interface_mif`).
    Interface,
    /// Derived build outputs: objects, archives, `.olean` files, rewrite rules.
    Artifact,
}

impl Domain {
    pub const ALL: [Domain; 3] = [Domain::Source, Domain::Interface, Domain::Artifact];

    pub fn as_str(self) -> &'static str {
        match self {
            Domain::Source => "source",
            Domain::Interface => "interface",
            Domain::Artifact => "artifact",
        }
    }

    pub(crate) fn tag(self) -> u8 {
        match self {
            Domain::Source => 1,
            Domain::Interface => 2,
            Domain::Artifact => 3,
        }
    }

    pub(crate) fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Domain::Source),
            2 => Some(Domain::Interface),
            3 => Some(Domain::Artifact),
            _ => None,
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        Domain::ALL.into_iter().find(|d| d.as_str() == s)
    }
}

impl fmt::Display for Domain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Object kinds stored in the CAS.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ObjectKind {
    /// Opaque file contents.
    Blob,
    /// A canonical directory listing (see [`crate::Tree`]).
    Tree,
}

impl ObjectKind {
    pub const ALL: [ObjectKind; 2] = [ObjectKind::Blob, ObjectKind::Tree];

    pub fn as_str(self) -> &'static str {
        match self {
            ObjectKind::Blob => "blob",
            ObjectKind::Tree => "tree",
        }
    }

    pub(crate) fn tag(self) -> u8 {
        match self {
            ObjectKind::Blob => 1,
            ObjectKind::Tree => 2,
        }
    }

    pub(crate) fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(ObjectKind::Blob),
            2 => Some(ObjectKind::Tree),
            _ => None,
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        ObjectKind::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

impl fmt::Display for ObjectKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A raw 32-byte BLAKE3 output.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest(pub [u8; 32]);

impl Digest {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lowercase hexadecimal rendering (64 characters).
    pub fn to_hex(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    }

    /// Strict parser: exactly 64 lowercase hexadecimal characters. Store paths
    /// are derived from this form, so no other spelling is accepted.
    pub fn from_hex(text: &str) -> Option<Self> {
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
        for (i, pair) in bytes.chunks_exact(2).enumerate() {
            out[i] = (nibble(pair[0])? << 4) | nibble(pair[1])?;
        }
        Some(Digest(out))
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Digest({})", self.to_hex())
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// The full identity of a stored object: domain, kind and digest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectId {
    pub domain: Domain,
    pub kind: ObjectKind,
    pub digest: Digest,
}

/// Number of bytes in the binary encoding of an [`ObjectId`].
pub(crate) const OBJECT_ID_BYTES: usize = 34;

impl ObjectId {
    pub fn new(domain: Domain, kind: ObjectKind, digest: Digest) -> Self {
        ObjectId {
            domain,
            kind,
            digest,
        }
    }

    pub(crate) fn encode_into(&self, out: &mut Vec<u8>) {
        out.push(self.domain.tag());
        out.push(self.kind.tag());
        out.extend_from_slice(&self.digest.0);
    }

    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != OBJECT_ID_BYTES {
            return None;
        }
        let domain = Domain::from_tag(bytes[0])?;
        let kind = ObjectKind::from_tag(bytes[1])?;
        let mut digest = [0u8; 32];
        digest.copy_from_slice(&bytes[2..]);
        Some(ObjectId::new(domain, kind, Digest(digest)))
    }
}

/// Text form: `b3v1:<domain>:<kind>:<64 lowercase hex>`.
impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "b3v{}:{}:{}:{}",
            OBJECT_SCHEMA_VERSION, self.domain, self.kind, self.digest
        )
    }
}

impl FromStr for ObjectId {
    type Err = CasError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || CasError::InvalidId(s.to_owned());
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

/// BLAKE3 `derive_key` context for an object of `domain`/`kind`. These
/// strings are frozen for schema version 1: changing one changes every id.
pub(crate) fn object_context(domain: Domain, kind: ObjectKind) -> &'static str {
    match (domain, kind) {
        (Domain::Source, ObjectKind::Blob) => "vesper-loom-cas 2026-11 v1 source blob",
        (Domain::Source, ObjectKind::Tree) => "vesper-loom-cas 2026-11 v1 source tree",
        (Domain::Interface, ObjectKind::Blob) => "vesper-loom-cas 2026-11 v1 interface blob",
        (Domain::Interface, ObjectKind::Tree) => "vesper-loom-cas 2026-11 v1 interface tree",
        (Domain::Artifact, ObjectKind::Blob) => "vesper-loom-cas 2026-11 v1 artifact blob",
        (Domain::Artifact, ObjectKind::Tree) => "vesper-loom-cas 2026-11 v1 artifact tree",
    }
}

pub(crate) const DERIVATION_CONTEXT: &str = "vesper-loom-cas 2026-11 v1 derivation key";

/// Incremental object hasher.
#[derive(Debug)]
pub struct ObjectHasher {
    domain: Domain,
    kind: ObjectKind,
    inner: blake3::Hasher,
    len: u64,
}

impl ObjectHasher {
    pub fn new(domain: Domain, kind: ObjectKind) -> Self {
        ObjectHasher {
            domain,
            kind,
            inner: blake3::Hasher::new_derive_key(object_context(domain, kind)),
            len: 0,
        }
    }

    pub fn update(&mut self, bytes: &[u8]) {
        self.inner.update(bytes);
        self.len += bytes.len() as u64;
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn finish(&self) -> ObjectId {
        ObjectId::new(
            self.domain,
            self.kind,
            Digest(*self.inner.finalize().as_bytes()),
        )
    }
}

/// Identity of `bytes` stored as a blob in `domain`.
pub fn blob_id(domain: Domain, bytes: &[u8]) -> ObjectId {
    let mut hasher = ObjectHasher::new(domain, ObjectKind::Blob);
    hasher.update(bytes);
    hasher.finish()
}
