//! Differential shadow harness (design §29.3, `@differential_shadow`).
//!
//! Runs a legacy implementation and its replacement over the same generated
//! inputs and reports the first input on which their outcomes differ, where an
//! outcome is either a returned value or a panic. Inputs come from a seeded
//! SplitMix64 stream plus caller-supplied boundary cases, so every report is
//! reproducible from `(seed, case index)`.
//!
//! Limits, stated in every [`ShadowReport`]:
//!
//! * finite differential testing is not an equivalence proof;
//! * the legacy side runs in-process as a Rust closure. MPK/PKRU (or
//!   POR_EL0) domain isolation is **not** exercised, so
//!   [`ShadowReport::isolation`] is always [`Isolation::Unavailable`]. A
//!   legacy C function is driven through a Rust closure over a C-ABI shim;
//!   this crate compiles no C.

use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// Deterministic SplitMix64 input source.
#[derive(Debug, Clone)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }
    pub fn next_u64(&mut self) -> u64 {
        // Wrapping arithmetic is the SplitMix64 definition.
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    /// Uniform in `0..=max` (bias is irrelevant for test generation).
    pub fn below_or_eq(&mut self, max: u64) -> u64 {
        match max.checked_add(1) {
            Some(n) => self.next_u64() % n,
            None => self.next_u64(),
        }
    }
    /// A byte vector of length `0..=max_len`. One draw in four copies a
    /// caller-provided prefix (for example a valid header magic) so the
    /// generator reaches past early rejections.
    pub fn bytes(&mut self, max_len: usize, prefix: &[u8]) -> Vec<u8> {
        let len = usize::try_from(self.below_or_eq(max_len as u64)).unwrap_or(max_len);
        let mut out = Vec::with_capacity(len);
        let keep_prefix = self.next_u64().is_multiple_of(4);
        for i in 0..len {
            let b = match prefix.get(i) {
                Some(p) if keep_prefix => *p,
                _ => (self.next_u64() >> 56) as u8,
            };
            out.push(b);
        }
        out
    }
}

/// Isolation actually used for the legacy side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Isolation {
    Unavailable { reason: &'static str },
}

/// The observed result of running one side on one input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome<O> {
    Returned(O),
    Panicked(String),
}

/// The first input on which the two sides disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence<I, O> {
    /// 0-based case index (boundary cases first, then generated ones).
    pub case: u64,
    pub input: I,
    pub legacy: Outcome<O>,
    pub replacement: Outcome<O>,
}

/// Harness configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShadowConfig {
    pub seed: u64,
    /// Generated cases after the boundary cases.
    pub cases: u64,
}

impl Default for ShadowConfig {
    fn default() -> Self {
        Self {
            seed: 0x5eed_f00d,
            cases: 10_000,
        }
    }
}

/// What a shadow run established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowReport<I, O> {
    pub seed: u64,
    pub cases_run: u64,
    pub divergence: Option<Divergence<I, O>>,
    pub isolation: Isolation,
}

impl<I, O> ShadowReport<I, O> {
    pub const LIMITATION: &'static str =
        "finite differential testing over generated inputs is not an equivalence proof";
    pub fn agreed(&self) -> bool {
        self.divergence.is_none()
    }
}

impl<I: fmt::Debug, O: fmt::Debug> fmt::Display for ShadowReport<I, O> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.divergence {
            None => write!(
                f,
                "no divergence in {} cases (seed {:#x})",
                self.cases_run, self.seed
            )?,
            Some(d) => write!(
                f,
                "divergence at case {} (seed {:#x}): input {:?}: legacy {:?}, replacement {:?}",
                d.case, self.seed, d.input, d.legacy, d.replacement
            )?,
        }
        let Isolation::Unavailable { reason } = self.isolation;
        write!(f, "; isolation unavailable: {reason}; {}", Self::LIMITATION)
    }
}

fn observe<I, O>(f: &mut impl FnMut(&I) -> O, input: &I) -> Outcome<O> {
    match catch_unwind(AssertUnwindSafe(|| f(input))) {
        Ok(v) => Outcome::Returned(v),
        Err(payload) => Outcome::Panicked(
            payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "non-string panic payload".into()),
        ),
    }
}

/// Run `legacy` and `replacement` on every boundary case, then on
/// `config.cases` inputs drawn by `generate`, stopping at the first
/// divergence.
pub fn run<I: Clone, O: PartialEq>(
    config: ShadowConfig,
    boundary: impl IntoIterator<Item = I>,
    mut generate: impl FnMut(&mut SplitMix64) -> I,
    mut legacy: impl FnMut(&I) -> O,
    mut replacement: impl FnMut(&I) -> O,
) -> ShadowReport<I, O> {
    let mut rng = SplitMix64::new(config.seed);
    let mut case = 0u64;
    let isolation = Isolation::Unavailable {
        reason: "legacy code ran in-process; MPK/PKRU and POR_EL0 domains were not exercised",
    };
    let mut check = |input: I, case: u64| -> Option<Divergence<I, O>> {
        let l = observe(&mut legacy, &input);
        let r = observe(&mut replacement, &input);
        (l != r).then(|| Divergence {
            case,
            input,
            legacy: l,
            replacement: r,
        })
    };
    for input in boundary {
        if let Some(d) = check(input, case) {
            return ShadowReport {
                seed: config.seed,
                cases_run: case + 1,
                divergence: Some(d),
                isolation,
            };
        }
        case += 1;
    }
    for _ in 0..config.cases {
        let input = generate(&mut rng);
        if let Some(d) = check(input, case) {
            return ShadowReport {
                seed: config.seed,
                cases_run: case + 1,
                divergence: Some(d),
                isolation,
            };
        }
        case += 1;
    }
    ShadowReport {
        seed: config.seed,
        cases_run: case,
        divergence: None,
        isolation,
    }
}

/// Greedy shrink of a diverging byte input: delete chunks, then lower bytes,
/// keeping any candidate on which `diverges` still holds. Terminates because
/// every accepted step strictly decreases (length, byte sum) in shortlex
/// order. This is a convenience for reports; ChoiceTape shrinking lives in
/// Crucible.
pub fn shrink_bytes(input: &[u8], mut diverges: impl FnMut(&[u8]) -> bool) -> Vec<u8> {
    let mut best = input.to_vec();
    let mut chunk = best.len().div_ceil(2).max(1);
    while chunk > 0 && !best.is_empty() {
        let mut i = 0;
        while i + chunk <= best.len() {
            let mut c = best.clone();
            c.drain(i..i + chunk);
            if diverges(&c) {
                best = c;
            } else {
                i += 1;
            }
        }
        chunk /= 2;
    }
    for i in 0..best.len() {
        let (mut lo, mut hi) = (0u8, best[i]);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let mut c = best.clone();
            c[i] = mid;
            if diverges(&c) {
                best[i] = mid;
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
    }
    best
}
