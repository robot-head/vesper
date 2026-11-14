//! Clone entropy for `reseed_after_fork` (§37.1.2): hardware RNG output
//! (`RDRAND` on x86_64, `RNDR` on AArch64) mixed with the operating system's
//! generator.
//!
//! A forked clone's key is derived from the root key, its generation id, its
//! fork epoch *and* 32 bytes of fresh entropy gathered after the fork, so two
//! clones of one snapshot get unrelated keys even if the generation id or
//! epoch bookkeeping were wrong. Nonce uniqueness does not depend on this (it
//! comes from distinct epochs, see [`crate::fork`]). Hardware output is never
//! used alone when the OS generator is available: both are absorbed into one
//! BLAKE3 `derive_key` context, so a biased or backdoored instruction cannot
//! make the result weaker than the OS source.
#![forbid(unsafe_code)]

use crate::hw_rng;

const CONTEXT: &str = "vesper.aether.clone-entropy/1";

/// Which sources contributed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntropySources {
    /// `Some("RDRAND")`/`Some("RNDR")` if the instruction produced output.
    pub hardware: Option<&'static str>,
    /// The OS generator (`/dev/urandom`) produced output.
    pub os: bool,
    /// Caller-supplied bytes (deterministic tests only).
    pub supplied: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntropyError {
    /// Neither a hardware nor an OS source was available.
    NoSource,
}

/// 32 bytes of clone entropy. Not `Clone` (use once) and never printed.
pub struct CloneEntropy {
    bytes: [u8; 32],
    sources: EntropySources,
}
impl std::fmt::Debug for CloneEntropy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloneEntropy")
            .field("sources", &self.sources)
            .finish_non_exhaustive()
    }
}
impl Drop for CloneEntropy {
    fn drop(&mut self) {
        self.bytes = [0; 32];
        std::hint::black_box(&self.bytes);
    }
}

fn os_entropy() -> Option<[u8; 32]> {
    #[cfg(unix)]
    {
        use std::io::Read;
        let mut b = [0u8; 32];
        let mut f = std::fs::File::open("/dev/urandom").ok()?;
        f.read_exact(&mut b).ok()?;
        Some(b)
    }
    #[cfg(not(unix))]
    {
        None
    }
}

impl CloneEntropy {
    /// Gathers 4 hardware words and 32 OS bytes and mixes them. Fails only if
    /// both sources are unavailable.
    pub fn gather() -> Result<Self, EntropyError> {
        let mut hw = [0u8; 32];
        let mut have_hw = hw_rng::instruction().is_some();
        if have_hw {
            for chunk in hw.as_chunks_mut::<8>().0 {
                match hw_rng::word() {
                    Some(w) => chunk.copy_from_slice(&w.to_le_bytes()),
                    None => {
                        have_hw = false;
                        break;
                    }
                }
            }
        }
        let os = os_entropy();
        if !have_hw && os.is_none() {
            return Err(EntropyError::NoSource);
        }
        let mut h = blake3::Hasher::new_derive_key(CONTEXT);
        h.update(&[u8::from(have_hw)]);
        if have_hw {
            h.update(&hw);
        }
        h.update(&[u8::from(os.is_some())]);
        if let Some(os) = os {
            h.update(&os);
        }
        hw = [0; 32];
        std::hint::black_box(&hw);
        Ok(Self {
            bytes: *h.finalize().as_bytes(),
            sources: EntropySources {
                hardware: if have_hw { hw_rng::instruction() } else { None },
                os: os.is_some(),
                supplied: false,
            },
        })
    }
    /// Fixed bytes, for deterministic tests. Marked `supplied` in the sources.
    pub fn supplied(bytes: [u8; 32]) -> Self {
        Self {
            bytes,
            sources: EntropySources {
                hardware: None,
                os: false,
                supplied: true,
            },
        }
    }
    pub fn sources(&self) -> EntropySources {
        self.sources
    }
    pub(crate) fn bytes(&self) -> &[u8; 32] {
        &self.bytes
    }
}
