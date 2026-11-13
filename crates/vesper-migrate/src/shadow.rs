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
//! * [`run`] executes the legacy side in-process as a Rust closure with no
//!   isolation ([`Isolation::Unavailable`]);
//! * [`run_isolated`] (design §29.1 item 5, §13.4) runs the whole loop in a
//!   contained child process and executes each legacy call with an
//!   [`IsolationDomain`]'s regions (the Vesper-owned buffers) write-protected
//!   by PKU (`WRPKRU`) or POE (`POR_EL0`). A stray legacy write is a typed
//!   [`IsolationFailure`]. On hosts without PKU/POE it degrades to [`run`]
//!   and says so. Protection keys are per-thread rights inside one address
//!   space: legacy code that rewrites PKRU itself is not contained.
//!
//! A legacy C function is driven through a Rust closure over a C-ABI shim;
//! this crate compiles no C.

use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;

use vesper_ffi::isolation::{
    self, ChildExit, IsolationDomain, Mechanism, Revoke, Unavailable, UnavailableReason,
};

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
    Unavailable {
        reason: &'static str,
    },
    /// Every legacy call ran with the domain's regions write-protected by
    /// this mechanism, inside a contained child process.
    Hardware {
        mechanism: Mechanism,
    },
}

/// The legacy side broke isolation, or the contained child died.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolationFailure<I> {
    /// The case in progress (`None` if the child died before any case).
    pub case: Option<u64>,
    pub input: Option<I>,
    pub exit: ChildExit,
}

impl<I> IsolationFailure<I> {
    /// A hardware protection-key fault: the legacy side wrote (or touched)
    /// Vesper-owned memory.
    pub fn is_protection_violation(&self) -> bool {
        matches!(
            self.exit,
            ChildExit::Fault {
                protection_key: true,
                ..
            }
        )
    }
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
    /// Set by [`run_isolated`] when the legacy side faulted or the child died.
    pub isolation_failure: Option<IsolationFailure<I>>,
}

impl<I, O> ShadowReport<I, O> {
    pub const LIMITATION: &'static str =
        "finite differential testing over generated inputs is not an equivalence proof";
    pub fn agreed(&self) -> bool {
        self.divergence.is_none() && self.isolation_failure.is_none()
    }
}

impl<I: fmt::Debug, O: fmt::Debug> fmt::Display for ShadowReport<I, O> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(v) = &self.isolation_failure {
            write!(
                f,
                "isolation failure at case {:?} (seed {:#x}): input {:?}: {:?}; ",
                v.case, self.seed, v.input, v.exit
            )?;
        }
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
        match self.isolation {
            Isolation::Unavailable { reason } => write!(f, "; isolation unavailable: {reason}")?,
            Isolation::Hardware { mechanism } => write!(
                f,
                "; legacy side ran with Vesper buffers write-protected ({mechanism:?}, same address space)"
            )?,
        }
        write!(f, "; {}", Self::LIMITATION)
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
                isolation_failure: None,
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
                isolation_failure: None,
            };
        }
        case += 1;
    }
    ShadowReport {
        seed: config.seed,
        cases_run: case,
        divergence: None,
        isolation,
        isolation_failure: None,
    }
}

fn unavailable_text(u: &Unavailable) -> &'static str {
    match u.reason {
        UnavailableReason::UnsupportedTarget => {
            "hardware isolation needs Linux x86_64 (PKU) or Linux AArch64 (POE); legacy code ran in-process"
        }
        UnavailableReason::CpuLacksFeature => {
            "this CPU lacks PKU/POE; legacy code ran in-process without isolation"
        }
        UnavailableReason::KernelLacksSupport => {
            "the kernel does not enable protection keys; legacy code ran in-process without isolation"
        }
        UnavailableReason::KeysExhausted => {
            "no free protection key; legacy code ran in-process without isolation"
        }
        UnavailableReason::Os(_) => {
            "protection keys failed with an OS error; legacy code ran in-process without isolation"
        }
    }
}

// Child-to-parent progress messages: value * 4 + tag.
const START: u64 = 0;
const DIVERGED: u64 = 1;
const DONE: u64 = 2;

/// [`run`], with every legacy call executed under `domain` with its regions
/// write-protected, inside a contained child process that is killed after
/// `timeout`. Put the replacement's (Vesper-owned) buffers in `domain`'s
/// regions: a legacy write into them is a hardware fault, reported as an
/// [`IsolationFailure`] with the case and its regenerated input.
///
/// When `domain` is `Err` (no PKU/POE) this is exactly [`run`] and the report
/// says isolation was unavailable. Both sides and `generate` must be
/// deterministic: a reported divergence is re-executed in the caller's
/// process (without isolation) to recover its outcomes, and inputs are
/// regenerated from the seed. See [`isolation::contain`] for what the child
/// closure may do.
#[allow(clippy::too_many_arguments)]
pub fn run_isolated<I: Clone, O: PartialEq>(
    config: ShadowConfig,
    boundary: Vec<I>,
    mut generate: impl FnMut(&mut SplitMix64) -> I,
    mut legacy: impl FnMut(&I) -> O,
    mut replacement: impl FnMut(&I) -> O,
    domain: Result<&IsolationDomain, &Unavailable>,
    timeout: Duration,
) -> ShadowReport<I, O> {
    let domain = match domain {
        Ok(d) => d,
        Err(u) => {
            let mut r = run(config, boundary, generate, legacy, replacement);
            r.isolation = Isolation::Unavailable {
                reason: unavailable_text(u),
            };
            return r;
        }
    };
    let boundary_len = boundary.len() as u64;
    let total = boundary_len + config.cases;
    let contained = isolation::contain(timeout, |tx| {
        let mut rng = SplitMix64::new(config.seed);
        for case in 0..total {
            let input = match boundary.get(case as usize) {
                Some(b) => b.clone(),
                None => generate(&mut rng),
            };
            tx.send(case * 4 + START);
            let l = domain.with_revoked(Revoke::Write, || observe(&mut legacy, &input));
            let r = observe(&mut replacement, &input);
            if l != r {
                tx.send(case * 4 + DIVERGED);
                return;
            }
        }
        tx.send(total * 4 + DONE);
    });
    let contained = match contained {
        Ok(c) => c,
        Err(e) => {
            let mut r = run(config, boundary, generate, legacy, replacement);
            r.isolation = Isolation::Unavailable {
                reason: match e {
                    isolation::DomainError::Unavailable(ref u) => unavailable_text(u),
                    _ => {
                        "the contained child could not be started; legacy code ran in-process without isolation"
                    }
                },
            };
            return r;
        }
    };
    // Regenerate input `k` deterministically from the seed.
    let mut input_at = |k: u64| -> I {
        if let Some(b) = boundary.get(k as usize) {
            return b.clone();
        }
        let mut rng = SplitMix64::new(config.seed);
        let mut last = None;
        for _ in boundary_len..=k {
            last = Some(generate(&mut rng));
        }
        last.expect("k is at least the boundary length")
    };
    let mut report = ShadowReport {
        seed: config.seed,
        cases_run: 0,
        divergence: None,
        isolation: Isolation::Hardware {
            mechanism: domain.mechanism(),
        },
        isolation_failure: None,
    };
    let last = contained.messages.last().copied();
    match (last.map(|m| (m / 4, m % 4)), &contained.exit) {
        (Some((n, DONE)), ChildExit::Returned) => report.cases_run = n,
        (Some((k, DIVERGED)), ChildExit::Returned) => {
            let input = input_at(k);
            let l = observe(&mut legacy, &input);
            let r = observe(&mut replacement, &input);
            report.cases_run = k + 1;
            report.divergence = Some(Divergence {
                case: k,
                input,
                legacy: l,
                replacement: r,
            });
        }
        (started, exit) => {
            let case = started.filter(|(_, tag)| *tag == START).map(|(k, _)| k);
            report.cases_run = case.map_or(0, |k| k + 1);
            report.isolation_failure = Some(IsolationFailure {
                case,
                input: case.map(&mut input_at),
                exit: exit.clone(),
            });
        }
    }
    report
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
