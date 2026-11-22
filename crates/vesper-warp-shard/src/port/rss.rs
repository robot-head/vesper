//! The Vesper build of the Toeplitz hashes (`vesper/src/rss.ves`).

use crate::port::binding::{self as b, ToeplitzCore};
use crate::rss::RssError;
use core::fmt;

/// Bit-serial Toeplitz hash, straight from the definition: for every set input
/// bit (most significant first) XOR in the 32-bit key window starting at that
/// bit. Used as the reference for [`Toeplitz::hash`].
pub fn toeplitz_reference(key: &[u8; 40], input: &[u8]) -> Result<u32, RssError> {
    b::toeplitz_reference_hash(key, input)
        .map_err(|len| RssError::InputTooLong { len: len as usize })
}

/// Table-driven Toeplitz hasher: one 256-entry table per input byte
/// position, so hashing is one lookup and XOR per byte.
pub struct Toeplitz {
    core: ToeplitzCore,
    key: [u8; 40],
}

impl Clone for Toeplitz {
    fn clone(&self) -> Toeplitz {
        Toeplitz::new(&self.key)
    }
}

impl fmt::Debug for Toeplitz {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Toeplitz { .. }")
    }
}

impl Toeplitz {
    /// Precomputes the tables for `key`.
    pub fn new(key: &[u8; 40]) -> Toeplitz {
        Toeplitz {
            core: ToeplitzCore::new(key),
            key: *key,
        }
    }

    /// Hash of `input` (at most 36 bytes).
    pub fn hash(&self, input: &[u8]) -> Result<u32, RssError> {
        self.core
            .hash(input)
            .map_err(|len| RssError::InputTooLong { len: len as usize })
    }
}
