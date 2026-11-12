#![allow(dead_code)]

use std::collections::hash_map::DefaultHasher;
use std::hash::BuildHasherDefault;
use vesper_warp_shard::{IsaPath, Kernels};

/// Deterministic index hasher for reproducible tests.
pub type FixedHasher = BuildHasherDefault<DefaultHasher>;

/// SplitMix64: small deterministic PRNG for property-style tests.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    pub fn chance(&mut self, num: u64, den: u64) -> bool {
        self.below(den) < num
    }
}

/// Every kernel path the running CPU supports, scalar first.
pub fn supported_kernels() -> Vec<Kernels> {
    IsaPath::ALL
        .iter()
        .filter_map(|&p| Kernels::new(p).ok())
        .collect()
}
