//! The Rust build of the Toeplitz hashes (frozen; the Vesper build is
//! `vesper/src/rss.ves`).

use crate::rss::{MAX_RSS_INPUT, RssError};
use core::fmt;

/// Bit-serial Toeplitz hash, straight from the definition: for every set input
/// bit (most significant first) XOR in the 32-bit key window starting at that
/// bit. Used as the reference for [`Toeplitz`].
pub fn toeplitz_reference(key: &[u8; 40], input: &[u8]) -> Result<u32, RssError> {
    if input.len() > MAX_RSS_INPUT {
        return Err(RssError::InputTooLong { len: input.len() });
    }
    let key_bit = |k: usize| u32::from((key[k / 8] >> (7 - k % 8)) & 1);
    let mut window = u32::from_be_bytes([key[0], key[1], key[2], key[3]]);
    let mut result = 0u32;
    for (i, &byte) in input.iter().enumerate() {
        for b in (0..8).rev() {
            if (byte >> b) & 1 == 1 {
                result ^= window;
            }
            // Next key bit: index 32 + bits consumed so far, at most 319.
            window = (window << 1) | key_bit(32 + i * 8 + (7 - b));
        }
    }
    Ok(result)
}

/// Table-driven Toeplitz hasher: one 256-entry XOR table per input byte
/// position, precomputed from the key at construction (36 KiB).
#[derive(Clone)]
pub struct Toeplitz {
    table: Box<[[u32; 256]; MAX_RSS_INPUT]>,
}

impl fmt::Debug for Toeplitz {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Toeplitz { .. }")
    }
}

impl Toeplitz {
    /// Precomputes the tables for `key`.
    pub fn new(key: &[u8; 40]) -> Toeplitz {
        let mut table = Box::new([[0u32; 256]; MAX_RSS_INPUT]);
        for (pos, row) in table.iter_mut().enumerate() {
            for bit in 0..8 {
                // Window for input bit (pos, bit from MSB) starts at key bit
                // pos * 8 + bit; read 32 bits from a 64-bit big-endian view.
                let start = pos * 8 + bit;
                let byte = start / 8;
                let mut w = 0u64;
                for k in 0..8 {
                    w = (w << 8) | u64::from(key.get(byte + k).copied().unwrap_or(0));
                }
                // Truncation keeps exactly the 32-bit window.
                let window = (w >> (32 - start % 8)) as u32;
                for (v, entry) in row.iter_mut().enumerate() {
                    if (v >> (7 - bit)) & 1 == 1 {
                        *entry ^= window;
                    }
                }
            }
        }
        Toeplitz { table }
    }

    /// Hash of `input` (at most 36 bytes).
    pub fn hash(&self, input: &[u8]) -> Result<u32, RssError> {
        if input.len() > MAX_RSS_INPUT {
            return Err(RssError::InputTooLong { len: input.len() });
        }
        Ok(input
            .iter()
            .zip(self.table.iter())
            .fold(0, |h, (&b, row)| h ^ row[usize::from(b)]))
    }
}
