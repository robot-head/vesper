//! Deterministic SplitMix64, the same mixer SimPulse uses. No library code
//! reads a real clock or OS entropy; every run is a function of its seeds.

/// SplitMix64 pseudo-random generator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitMix64(pub u64);

impl SplitMix64 {
    pub fn next_u64(&mut self) -> u64 {
        #[cfg(vesper_port)]
        {
            self.port(crate::binding::rng_next)
        }
        #[cfg(not(vesper_port))]
        {
            self.next_u64_rust()
        }
    }

    #[cfg(not(vesper_port))]
    fn next_u64_rust(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `0..=max_inclusive`, by rejection sampling (no modulo bias).
    pub fn below_inclusive(&mut self, max_inclusive: u64) -> u64 {
        #[cfg(vesper_port)]
        {
            self.port(|r| crate::binding::rng_below_inclusive(r, max_inclusive))
        }
        #[cfg(not(vesper_port))]
        {
            self.below_inclusive_rust(max_inclusive)
        }
    }

    #[cfg(not(vesper_port))]
    fn below_inclusive_rust(&mut self, max_inclusive: u64) -> u64 {
        let Some(bound) = max_inclusive.checked_add(1) else {
            return self.next_u64();
        };
        // Values below `threshold` would bias the residue; reject them.
        let threshold = bound.wrapping_neg() % bound;
        loop {
            let value = self.next_u64();
            if value >= threshold {
                return value % bound;
            }
        }
    }

    /// Uniform index in `0..len`; `len` must be non-zero.
    pub fn index(&mut self, len: usize) -> usize {
        debug_assert!(len > 0);
        // usize -> u64 is lossless on every supported target.
        self.below_inclusive(len as u64 - 1) as usize
    }

    /// True with probability `numerator / 1024`.
    pub fn chance(&mut self, numerator: u64) -> bool {
        #[cfg(vesper_port)]
        {
            self.port(|r| crate::binding::rng_chance(r, numerator))
        }
        #[cfg(not(vesper_port))]
        {
            self.below_inclusive(1023) < numerator
        }
    }

    /// Run one Vesper generator call on this state (export ABI v3 mirror).
    #[cfg(vesper_port)]
    fn port<R>(&mut self, call: impl FnOnce(&mut crate::binding::repr::Rng) -> R) -> R {
        let mut rng = crate::binding::repr::Rng { word: self.0 };
        let out = call(&mut rng);
        self.0 = rng.word;
        out
    }
}
