//! Shared test helpers: a deterministic synthetic llama2.c checkpoint large
//! enough for every block format (row widths that are multiples of 256).
#![allow(dead_code)]

use vesper_tensor::llama::Model;

pub struct Rng(pub u64);
impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    /// Uniform in [-1, 1).
    pub fn unit(&mut self) -> f32 {
        ((self.next_u64() >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }
    pub fn vec(&mut self, n: usize, scale: f32) -> Vec<f32> {
        (0..n).map(|_| self.unit() * scale).collect()
    }
}

/// Header `(dim, hidden, layers, heads, kv_heads, vocab, seq_len)`.
pub fn synthetic_checkpoint(shape: [usize; 7], seed: u64) -> Vec<u8> {
    let [dim, hidden, layers, heads, kv_heads, vocab, seq_len] = shape;
    let kv_dim = kv_heads * dim / heads;
    let mut rng = Rng(seed);
    let mut out = Vec::new();
    for v in shape {
        out.extend_from_slice(&(v as i32).to_le_bytes());
    }
    let mut put = |v: Vec<f32>| {
        for x in v {
            out.extend_from_slice(&x.to_le_bytes());
        }
    };
    let s = |fan_in: usize| 1.7 / (fan_in as f32).sqrt();
    put(rng.vec(vocab * dim, 1.0));
    put((0..layers * dim).map(|_| 1.0 + 0.1 * rng.unit()).collect());
    put(rng.vec(layers * dim * dim, s(dim)));
    put(rng.vec(layers * kv_dim * dim, s(dim)));
    put(rng.vec(layers * kv_dim * dim, s(dim)));
    put(rng.vec(layers * dim * dim, s(dim)));
    put((0..layers * dim).map(|_| 1.0 + 0.1 * rng.unit()).collect());
    put(rng.vec(layers * hidden * dim, s(dim)));
    put(rng.vec(layers * dim * hidden, s(hidden)));
    put(rng.vec(layers * hidden * dim, s(dim)));
    put((0..dim).map(|_| 1.0 + 0.1 * rng.unit()).collect());
    put(vec![0.0; seq_len * (dim / heads)]);
    out
}

/// dim 256, hidden 256, 1 layer, 8 query / 4 KV heads, vocab 96, context 64.
pub fn synthetic() -> Model {
    Model::from_llama2c(&synthetic_checkpoint([256, 256, 1, 8, 4, 96, 64], 0x5157)).unwrap()
}

pub fn stories() -> Model {
    Model::from_llama2c(include_bytes!("../../models/stories260K.bin")).unwrap()
}
