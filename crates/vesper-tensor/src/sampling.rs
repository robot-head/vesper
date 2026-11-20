//! Temperature and top-p (nucleus) sampling with llama2.c `run.c` semantics.
//!
//! [`Sampler::sample`]:
//!
//! * temperature `0` is greedy [`argmax`] (first index on ties);
//! * otherwise the logits are divided by the temperature and passed through
//!   [`softmax`], and a coin `u` in `[0, 1)` is drawn from `run.c`'s
//!   xorshift64* generator (`random_u32() >> 8` over `2^24`);
//! * with `top_p` outside `(0, 1)` the token is the first index whose
//!   cumulative probability exceeds `u` (`sample_mult`);
//! * otherwise (`sample_topp`) tokens with probability below
//!   `(1 - top_p) / (n - 1)` are dropped, the rest are sorted by descending
//!   probability (ties by ascending id; `run.c`'s `qsort` leaves tie order
//!   unspecified), truncated after the first prefix whose cumulative
//!   probability exceeds `top_p`, and sampled with `u * cumulative`.
//!
//! The sampler preallocates its buffers, so [`Sampler::sample`] performs no
//! heap allocation.
// The port-backed build (src/port.rs) replaces some Rust bodies, leaving
// their helpers unused there.
#![cfg_attr(vesper_port, allow(dead_code, unused_imports))]
#[cfg(not(vesper_port))]
use crate::ops::{argmax, softmax};
use crate::Error;

/// The xorshift64* output multiplier (Vigna, "An experimental exploration of
/// Marsaglia's xorshift generators", 2016), as in `run.c`.
#[cfg_attr(vesper_port, allow(dead_code))]
const XORSHIFT64_STAR_MUL: u64 = 0x2545_f491_4f6c_dd1d;
/// `next_f32` keeps 24 random bits and divides by `2^24`.
#[cfg_attr(vesper_port, allow(dead_code))]
const F32_UNIT_SCALE: f32 = 16_777_216.0;

/// `run.c`'s xorshift64* generator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rng(pub u64);
impl Rng {
    #[cfg(not(vesper_port))]
    pub fn next_u32(&mut self) -> u32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(XORSHIFT64_STAR_MUL) >> 32) as u32
    }
    #[cfg(vesper_port)]
    pub fn next_u32(&mut self) -> u32 {
        let (seed, value) = crate::port::must(crate::port::binding::rng_next_u32(self.0));
        self.0 = seed;
        value
    }
    /// A float in `[0, 1)` with 24 random bits.
    #[cfg(not(vesper_port))]
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 / F32_UNIT_SCALE
    }
    #[cfg(vesper_port)]
    pub fn next_f32(&mut self) -> f32 {
        let (seed, value) = crate::port::must(crate::port::binding::rng_next_f32(self.0));
        self.0 = seed;
        f32::from_bits(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SamplerConfig {
    pub temperature: f32,
    pub top_p: f32,
    pub seed: u64,
}
impl SamplerConfig {
    pub const GREEDY: Self = Self {
        temperature: 0.0,
        top_p: 1.0,
        seed: 1,
    };
}

#[derive(Debug)]
pub struct Sampler {
    config: SamplerConfig,
    rng: Rng,
    probs: Vec<f32>,
    #[cfg(not(vesper_port))]
    candidates: Vec<(f32, u32)>,
    /// The port's top-p candidates, packed as `(!bits(p) << 32) | id`.
    #[cfg(vesper_port)]
    candidates: Vec<u64>,
}

impl Sampler {
    /// `temperature >= 0` and `top_p` finite (values outside `(0, 1)` disable
    /// top-p); `seed` must be non-zero (xorshift has a zero fixed point).
    pub fn new(vocab: usize, config: SamplerConfig) -> Result<Self, Error> {
        if vocab == 0 || vocab > u32::MAX as usize {
            return Err(Error::Shape("sampler vocabulary"));
        }
        if !(config.temperature.is_finite() && config.temperature >= 0.0) {
            return Err(Error::Shape("sampling temperature must be finite and >= 0"));
        }
        if !config.top_p.is_finite() {
            return Err(Error::Shape("top_p must be finite"));
        }
        if config.seed == 0 {
            return Err(Error::Shape("sampler seed must be non-zero"));
        }
        Ok(Self {
            config,
            rng: Rng(config.seed),
            probs: vec![0.0; vocab],
            #[cfg(not(vesper_port))]
            candidates: Vec::with_capacity(vocab),
            #[cfg(vesper_port)]
            candidates: vec![0; vocab],
        })
    }
    pub fn config(&self) -> SamplerConfig {
        self.config
    }
    pub fn rng(&self) -> Rng {
        self.rng
    }
    /// Probabilities from the last non-greedy [`Sampler::sample`] call.
    pub fn probabilities(&self) -> &[f32] {
        &self.probs
    }
    /// Draws the next token from `logits` (length `vocab`).
    #[cfg(not(vesper_port))]
    pub fn sample(&mut self, logits: &[f32]) -> Result<u32, Error> {
        if logits.len() != self.probs.len() {
            return Err(Error::Shape(
                "logits length differs from the sampler vocabulary",
            ));
        }
        if self.config.temperature == 0.0 {
            return Ok(argmax(logits)? as u32);
        }
        for (p, l) in self.probs.iter_mut().zip(logits) {
            *p = l / self.config.temperature;
        }
        softmax(&mut self.probs)?;
        let coin = self.rng.next_f32();
        let top_p = self.config.top_p;
        if top_p <= 0.0 || top_p >= 1.0 {
            return Ok(sample_mult(&self.probs, coin));
        }
        Ok(sample_top_p(&self.probs, top_p, coin, &mut self.candidates))
    }
    #[cfg(vesper_port)]
    pub fn sample(&mut self, logits: &[f32]) -> Result<u32, Error> {
        if logits.len() != self.probs.len() {
            return Err(Error::Shape(
                "logits length differs from the sampler vocabulary",
            ));
        }
        let (token, seed) = crate::port::check(
            crate::port::binding::sample(
                crate::port::bits_mut(&mut self.probs),
                crate::port::bits(logits),
                self.config.temperature.to_bits(),
                self.config.top_p.to_bits(),
                self.rng.0,
                &mut self.candidates,
            ),
            |fault| match fault {
                crate::port::Fault::Shape if self.config.temperature == 0.0 => {
                    Error::Shape("argmax of empty vector")
                }
                crate::port::Fault::Shape => Error::Shape("softmax of empty vector"),
                _ => Error::NonFinite,
            },
        )?;
        self.rng.0 = seed;
        Ok(token as u32)
    }
}

/// `run.c` `sample_mult`: first index whose CDF exceeds `coin`.
#[cfg(not(vesper_port))]
pub fn sample_mult(probs: &[f32], coin: f32) -> u32 {
    let mut cdf = 0.0f32;
    for (i, p) in probs.iter().enumerate() {
        cdf += p;
        if coin < cdf {
            return i as u32;
        }
    }
    (probs.len() - 1) as u32 // rounding: the CDF may end just below 1
}
#[cfg(vesper_port)]
pub fn sample_mult(probs: &[f32], coin: f32) -> u32 {
    crate::port::must(crate::port::binding::sample_mult(
        crate::port::bits(probs),
        coin.to_bits(),
    )) as u32
}

/// `run.c` `sample_topp` with a deterministic tie order. `scratch` is reused.
#[cfg(not(vesper_port))]
pub fn sample_top_p(probs: &[f32], top_p: f32, coin: f32, scratch: &mut Vec<(f32, u32)>) -> u32 {
    let n = probs.len();
    if n == 1 {
        return 0;
    }
    let cutoff = (1.0 - top_p) / (n - 1) as f32;
    scratch.clear();
    for (i, &p) in probs.iter().enumerate() {
        if p >= cutoff {
            scratch.push((p, i as u32));
        }
    }
    if scratch.is_empty() {
        // Every token is below the cutoff only when `top_p < 1/n`: then the
        // largest probability (at least `1/n`) alone exceeds `top_p`, so the
        // nucleus is exactly that token (first index on ties). `run.c` reads
        // out of bounds here.
        let mut best = 0;
        for (i, &p) in probs.iter().enumerate() {
            if p > probs[best] {
                best = i;
            }
        }
        return best as u32;
    }
    scratch.sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut cumulative = 0.0f32;
    let mut last = scratch.len() - 1;
    for (i, (p, _)) in scratch.iter().enumerate() {
        cumulative += p;
        if cumulative > top_p {
            last = i;
            break;
        }
    }
    let r = coin * cumulative;
    let mut cdf = 0.0f32;
    for &(p, id) in &scratch[..=last] {
        cdf += p;
        if r < cdf {
            return id;
        }
    }
    scratch[last].1
}
#[cfg(vesper_port)]
pub fn sample_top_p(probs: &[f32], top_p: f32, coin: f32, scratch: &mut Vec<(f32, u32)>) -> u32 {
    // The port sorts packed candidate words (see `sample_top_p` in
    // vesper/src/sampling.ves). `scratch` keeps the Rust build's type; the
    // words live in a per-thread buffer that is reused across calls, so a
    // sampling loop allocates only when the vocabulary grows.
    thread_local! {
        static WORDS: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
    }
    scratch.clear();
    WORDS.with(|words| {
        let mut words = words.borrow_mut();
        if words.len() < probs.len() {
            words.resize(probs.len(), 0);
        }
        crate::port::must(crate::port::binding::sample_top_p(
            crate::port::bits(probs),
            top_p.to_bits(),
            coin.to_bits(),
            &mut words[..probs.len()],
        )) as u32
    })
}
