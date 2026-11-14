//! Prefixed BLAKE3 digest strings and the canonical source-tree root.

use crate::BootstrapVerifyError;

/// A BLAKE3 digest parsed from a lock string such as `blake3:<64 hex>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Blake3Digest(pub blake3::Hash);

impl Blake3Digest {
    /// Parse `<prefix><64 lowercase hex>`; anything else is a typed error.
    pub fn parse_prefixed(
        text: &str,
        prefix: &str,
        field: &'static str,
    ) -> Result<Self, BootstrapVerifyError> {
        let hex = text
            .strip_prefix(prefix)
            .ok_or(BootstrapVerifyError::InvalidLockField {
                field,
                reason: "missing digest prefix",
            })?;
        if hex.len() != 64 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(BootstrapVerifyError::InvalidLockField {
                field,
                reason: "digest must be 64 lowercase hex digits",
            });
        }
        blake3::Hash::from_hex(hex).map(Blake3Digest).map_err(|_| {
            BootstrapVerifyError::InvalidLockField {
                field,
                reason: "digest must be 64 lowercase hex digits",
            }
        })
    }
}

const LEAF_DOMAIN: &[u8] = b"vesper.bootstrap.source-leaf.v1\0";
const ROOT_DOMAIN: &[u8] = b"vesper.bootstrap.source-root.v1\0";

/// Canonical root over `(relative path, bytes)` pairs: each leaf is
/// BLAKE3(domain, path length, path, content length, content); the root hashes
/// the leaf count and the leaves in byte-wise path order. Duplicate paths are
/// rejected so two trees cannot share a root by repeating a file.
pub fn source_merkle(files: &[(&str, &[u8])]) -> Result<blake3::Hash, BootstrapVerifyError> {
    let mut sorted: Vec<_> = files.iter().collect();
    sorted.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    if sorted.windows(2).any(|w| w[0].0 == w[1].0) {
        return Err(BootstrapVerifyError::InvalidLockField {
            field: "source_merkle_blake3",
            reason: "duplicate source path",
        });
    }
    let mut root = blake3::Hasher::new();
    root.update(ROOT_DOMAIN);
    root.update(&(sorted.len() as u64).to_le_bytes());
    for (path, bytes) in sorted {
        let mut leaf = blake3::Hasher::new();
        leaf.update(LEAF_DOMAIN);
        leaf.update(&(path.len() as u64).to_le_bytes());
        leaf.update(path.as_bytes());
        leaf.update(&(bytes.len() as u64).to_le_bytes());
        leaf.update(bytes);
        root.update(leaf.finalize().as_bytes());
    }
    Ok(root.finalize())
}
