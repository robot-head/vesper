//! Deterministic xorshift input generation shared by the fusion tests.
#![allow(dead_code)]

pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    /// Uniform in `[-scale, scale)`.
    pub fn f32(&mut self, scale: f32) -> f32 {
        ((self.next() >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0) * scale
    }
    pub fn vec(&mut self, n: usize, scale: f32) -> Vec<f32> {
        (0..n).map(|_| self.f32(scale)).collect()
    }
    pub fn i32(&mut self, bound: i32) -> i32 {
        (self.next() % (2 * bound as u64 + 1)) as i32 - bound
    }
}
