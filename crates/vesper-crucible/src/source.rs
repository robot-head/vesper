//! `FuzzSource` (§29.2): the draw API generators use, backed by a choice tape,
//! and the `Probe` through which a target reports coverage and comparisons.

use crate::rng::SplitMix64;
use crate::tape::{ChoiceTape, MAX_TAPE_LEN};
use std::collections::BTreeMap;
use std::fmt;

/// Why a run did not produce a meaningful verdict. An invalid run is neither
/// a pass nor a failure: the shrinker and fuzzer discard it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Invalid {
    /// The replayed tape ran out (strict replay), or the run exceeded its
    /// draw budget.
    Overrun,
    /// `assume` rejected the generated value.
    Rejected,
    /// A choice among zero alternatives was requested.
    EmptyChoice,
    /// A range with `lo > hi` was requested.
    EmptyRange,
}

impl fmt::Display for Invalid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Overrun => "choice tape overrun or draw budget exhausted",
            Self::Rejected => "generated value rejected by assume",
            Self::EmptyChoice => "choice among zero alternatives",
            Self::EmptyRange => "empty integer range",
        })
    }
}

impl std::error::Error for Invalid {}

/// A property failure. `kind` identifies the bug: the shrinker only accepts
/// candidates that fail with the same kind, so shrinking cannot slip from one
/// bug to a different one. `detail` is free text for the report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub kind: String,
    pub detail: String,
}

impl Failure {
    pub fn new(kind: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            detail: detail.into(),
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.detail)
    }
}

/// What a property returns on a non-passing run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PropError {
    Fail(Failure),
    Invalid(Invalid),
}

impl PropError {
    pub fn fail(kind: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::Fail(Failure::new(kind, detail))
    }
}

impl From<Invalid> for PropError {
    fn from(value: Invalid) -> Self {
        Self::Invalid(value)
    }
}

impl From<Failure> for PropError {
    fn from(value: Failure) -> Self {
        Self::Fail(value)
    }
}

/// `Ok(())` when `cond` holds, else a failure of `kind`.
pub fn check(cond: bool, kind: &str, detail: impl FnOnce() -> String) -> Result<(), PropError> {
    if cond {
        Ok(())
    } else {
        Err(PropError::fail(kind, detail()))
    }
}

/// What a tape position was drawn as. The fuzzer's mutators and comparison
/// solver use it; the shrinker does not need it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawKind {
    /// An integer in `0..=max`.
    Value { max: u64 },
    /// One byte of a byte string or a `u8` draw.
    Byte,
    /// A collection continuation flag (0 = stop, 1 = another element).
    Flag,
    /// A SimPulse scheduling decision (0 = run the dispatched fiber).
    Schedule,
}

impl DrawKind {
    pub fn max(self) -> u64 {
        match self {
            Self::Value { max } => max,
            Self::Byte => u64::from(u8::MAX),
            Self::Flag | Self::Schedule => 1,
        }
    }
}

/// Comparison operators a target can report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpKind {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl CmpKind {
    fn eval(self, lhs: u64, rhs: u64) -> bool {
        match self {
            Self::Eq => lhs == rhs,
            Self::Ne => lhs != rhs,
            Self::Lt => lhs < rhs,
            Self::Le => lhs <= rhs,
            Self::Gt => lhs > rhs,
            Self::Ge => lhs >= rhs,
        }
    }
}

/// Operands of one executed comparison.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CmpOperands {
    Int {
        kind: CmpKind,
        lhs: u64,
        rhs: u64,
    },
    /// Byte-string equality; both sides truncated to [`MAX_CMP_BYTES`].
    Bytes {
        lhs: Vec<u8>,
        rhs: Vec<u8>,
    },
}

/// One executed comparison and its outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CmpRecord {
    pub site: u32,
    pub operands: CmpOperands,
    pub taken: bool,
}

/// Longest byte-string operand kept per comparison.
pub const MAX_CMP_BYTES: usize = 32;
/// Comparisons kept per run (later ones still count as coverage).
pub const MAX_CMP_RECORDS: usize = 4096;

/// The coverage and comparison-operand feedback channel (§29.1.1). Targets
/// call it by hand today; integration wires it to Cranelift instrumentation.
/// Edge identifiers and comparison sites are arbitrary `u32` labels chosen
/// by the target.
#[derive(Clone, Debug, Default)]
pub struct Probe {
    edges: BTreeMap<u32, u32>,
    cmp_outcomes: BTreeMap<(u32, bool), u32>,
    /// Best operand closeness seen per comparison site (number of equal low
    /// bits for integers, equal leading bytes for byte strings).
    closeness: BTreeMap<u32, u32>,
    cmps: Vec<CmpRecord>,
}

impl Probe {
    /// Record one hit of coverage edge `id`.
    pub fn edge(&mut self, id: u32) {
        let hits = self.edges.entry(id).or_insert(0);
        *hits = hits.saturating_add(1);
    }

    /// Evaluate and record an integer comparison; returns its result.
    pub fn cmp(&mut self, site: u32, kind: CmpKind, lhs: u64, rhs: u64) -> bool {
        let taken = kind.eval(lhs, rhs);
        let close = (lhs ^ rhs).trailing_zeros();
        self.note(site, taken, close);
        if self.cmps.len() < MAX_CMP_RECORDS {
            self.cmps.push(CmpRecord {
                site,
                operands: CmpOperands::Int { kind, lhs, rhs },
                taken,
            });
        }
        taken
    }

    /// `cmp(site, CmpKind::Eq, lhs, rhs)`.
    pub fn eq(&mut self, site: u32, lhs: u64, rhs: u64) -> bool {
        self.cmp(site, CmpKind::Eq, lhs, rhs)
    }

    /// Evaluate and record byte-string equality; returns its result.
    pub fn eq_bytes(&mut self, site: u32, lhs: &[u8], rhs: &[u8]) -> bool {
        let taken = lhs == rhs;
        let common = lhs.iter().zip(rhs).take_while(|(a, b)| a == b).count();
        // `common` <= MAX_TAPE_LEN-ish slice lengths; saturate for the feature.
        self.note(site, taken, u32::try_from(common).unwrap_or(u32::MAX));
        if self.cmps.len() < MAX_CMP_RECORDS {
            let keep = |s: &[u8]| s[..s.len().min(MAX_CMP_BYTES)].to_vec();
            self.cmps.push(CmpRecord {
                site,
                operands: CmpOperands::Bytes {
                    lhs: keep(lhs),
                    rhs: keep(rhs),
                },
                taken,
            });
        }
        taken
    }

    fn note(&mut self, site: u32, taken: bool, close: u32) {
        let hits = self.cmp_outcomes.entry((site, taken)).or_insert(0);
        *hits = hits.saturating_add(1);
        let best = self.closeness.entry(site).or_insert(0);
        *best = (*best).max(close);
    }

    pub fn edges(&self) -> &BTreeMap<u32, u32> {
        &self.edges
    }

    pub fn comparisons(&self) -> &[CmpRecord] {
        &self.cmps
    }

    /// Coverage features of this run: edges and comparison outcomes with
    /// AFL-style hit-count buckets, plus (when `value_profile`) per-site
    /// operand closeness. A run with a feature no corpus entry had is kept.
    pub(crate) fn features(&self, value_profile: bool) -> Vec<Feature> {
        let mut out = Vec::with_capacity(self.edges.len() + self.cmp_outcomes.len());
        out.extend(
            self.edges
                .iter()
                .map(|(&id, &hits)| Feature::Edge(id, bucket(hits))),
        );
        out.extend(
            self.cmp_outcomes
                .iter()
                .map(|(&(site, taken), &hits)| Feature::Cmp(site, taken, bucket(hits))),
        );
        if value_profile {
            out.extend(
                self.closeness
                    .iter()
                    .map(|(&site, &close)| Feature::Closeness(site, close)),
            );
        }
        out
    }
}

/// A coverage feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Feature {
    Edge(u32, u8),
    Cmp(u32, bool, u8),
    Closeness(u32, u32),
}

fn bucket(hits: u32) -> u8 {
    match hits {
        0 => 0,
        1 => 1,
        2 => 2,
        3 => 3,
        4..=7 => 4,
        8..=15 => 5,
        16..=31 => 6,
        32..=127 => 7,
        _ => 8,
    }
}

/// How a source obtains choices beyond what it has recorded.
#[derive(Clone, Debug)]
enum Extend {
    /// Strict replay: running past the tape is `Invalid::Overrun`.
    Strict,
    /// Prefix replay, then fresh pseudo-random choices.
    Random(SplitMix64),
}

/// Generation-time distribution of one draw. Only affects choices drawn past
/// the tape prefix; replayed values are taken from the tape as they are.
#[derive(Clone, Copy, Debug)]
enum Gen {
    Uniform,
    /// 1 with probability `n / 1024`.
    Bernoulli(u64),
}

/// The choice source a property draws from: Vesper's `FuzzSource` effect.
/// Replayed values greater than the requested bound are clamped to it, so a
/// mutated or shrunk tape always decodes to an in-range value; the recorded
/// (realized) tape holds the clamped values.
#[derive(Debug)]
pub struct Source {
    prefix: Vec<u64>,
    extend: Extend,
    realized: Vec<u64>,
    kinds: Vec<DrawKind>,
    max_draws: usize,
    probe: Probe,
}

/// Default per-run draw budget.
pub const DEFAULT_MAX_DRAWS: usize = 1 << 16;

impl Source {
    /// Strictly replay `tape`. Strict replay can never draw past the tape,
    /// so its budget is the tape-length cap ([`MAX_TAPE_LEN`]), not the
    /// default per-run budget: a counterexample found with a raised
    /// `max_draws` still replays.
    pub fn replay(tape: &ChoiceTape) -> Self {
        Self::build(tape.values.clone(), Extend::Strict, MAX_TAPE_LEN)
    }

    /// Replay `prefix`, then continue with pseudo-random choices from `seed`.
    pub fn with_prefix(prefix: Vec<u64>, seed: u64) -> Self {
        Self::build(prefix, Extend::Random(SplitMix64(seed)), DEFAULT_MAX_DRAWS)
    }

    /// Fresh pseudo-random choices from `seed`.
    pub fn random(seed: u64) -> Self {
        Self::with_prefix(Vec::new(), seed)
    }

    pub(crate) fn build_strict(prefix: Vec<u64>, max_draws: usize) -> Self {
        Self::build(prefix, Extend::Strict, max_draws)
    }

    pub(crate) fn build_random(prefix: Vec<u64>, seed: u64, max_draws: usize) -> Self {
        Self::build(prefix, Extend::Random(SplitMix64(seed)), max_draws)
    }

    fn build(prefix: Vec<u64>, extend: Extend, max_draws: usize) -> Self {
        Self {
            prefix,
            extend,
            realized: Vec::new(),
            kinds: Vec::new(),
            max_draws: max_draws.min(MAX_TAPE_LEN),
            probe: Probe::default(),
        }
    }

    fn raw(&mut self, kind: DrawKind, generation: Gen) -> Result<u64, Invalid> {
        let pos = self.realized.len();
        if pos >= self.max_draws {
            return Err(Invalid::Overrun);
        }
        let max = kind.max();
        let value = match self.prefix.get(pos) {
            Some(&v) => v.min(max),
            None => match &mut self.extend {
                Extend::Strict => return Err(Invalid::Overrun),
                Extend::Random(rng) => match generation {
                    Gen::Uniform => rng.below_inclusive(max),
                    Gen::Bernoulli(n) => u64::from(rng.chance(n)).min(max),
                },
            },
        };
        self.realized.push(value);
        self.kinds.push(kind);
        Ok(value)
    }

    /// FuzzSource::draw_u64_bounded: an integer in `0..=max_inclusive`.
    /// Shrinks toward 0.
    pub fn draw_u64_bounded(&mut self, max_inclusive: u64) -> Result<u64, Invalid> {
        self.raw(DrawKind::Value { max: max_inclusive }, Gen::Uniform)
    }

    pub fn draw_u64(&mut self) -> Result<u64, Invalid> {
        self.draw_u64_bounded(u64::MAX)
    }

    pub fn draw_u32(&mut self) -> Result<u32, Invalid> {
        let v = self.draw_u64_bounded(u64::from(u32::MAX))?;
        Ok(u32::try_from(v).expect("bounded draw"))
    }

    pub fn draw_u16(&mut self) -> Result<u16, Invalid> {
        let v = self.draw_u64_bounded(u64::from(u16::MAX))?;
        Ok(u16::try_from(v).expect("bounded draw"))
    }

    pub fn draw_u8(&mut self) -> Result<u8, Invalid> {
        let v = self.raw(DrawKind::Byte, Gen::Uniform)?;
        Ok(u8::try_from(v).expect("bounded draw"))
    }

    pub fn draw_bool(&mut self) -> Result<bool, Invalid> {
        Ok(self.draw_u64_bounded(1)? == 1)
    }

    /// An integer in `lo..=hi`; shrinks toward `lo`.
    pub fn draw_range_u64(&mut self, lo: u64, hi: u64) -> Result<u64, Invalid> {
        if lo > hi {
            return Err(Invalid::EmptyRange);
        }
        // hi - lo cannot underflow; lo + offset <= hi cannot overflow.
        let offset = self.draw_u64_bounded(hi - lo)?;
        Ok(lo + offset)
    }

    /// An integer in `lo..=hi`; shrinks toward `lo`.
    pub fn draw_range_i64(&mut self, lo: i64, hi: i64) -> Result<i64, Invalid> {
        if lo > hi {
            return Err(Invalid::EmptyRange);
        }
        // Two's-complement distance; exact because hi >= lo.
        let span = hi.wrapping_sub(lo) as u64;
        let offset = self.draw_u64_bounded(span)?;
        Ok(lo.wrapping_add(offset as i64))
    }

    /// An index in `0..n`; shrinks toward 0.
    pub fn draw_index(&mut self, n: usize) -> Result<usize, Invalid> {
        if n == 0 {
            return Err(Invalid::EmptyChoice);
        }
        let v = self.draw_u64_bounded(n as u64 - 1)?;
        Ok(usize::try_from(v).expect("bounded draw"))
    }

    /// One of `choices`; shrinks toward the first.
    pub fn draw_enum<T: Clone>(&mut self, choices: &[T]) -> Result<T, Invalid> {
        let i = self.draw_index(choices.len())?;
        Ok(choices[i].clone())
    }

    /// A collection continuation flag. Elements are drawn after a `1` flag and
    /// the collection ends at a `0` flag, so deleting a `(1, element...)` span
    /// from the tape removes exactly one element: the shrinker shortens
    /// collections without a type-specific shrinker.
    fn more(&mut self, len: usize, max_len: usize, average: usize) -> Result<bool, Invalid> {
        if len >= max_len {
            return Ok(false);
        }
        // Continue with probability average / (average + 1).
        let average = average.max(1) as u64;
        let p = (1024 * average) / (average + 1);
        Ok(self.raw(DrawKind::Flag, Gen::Bernoulli(p))? == 1)
    }

    /// A vector of at most `max_len` elements drawn by `element`.
    pub fn draw_vec<T>(
        &mut self,
        max_len: usize,
        mut element: impl FnMut(&mut Self) -> Result<T, Invalid>,
    ) -> Result<Vec<T>, Invalid> {
        let mut out = Vec::new();
        while self.more(out.len(), max_len, 8)? {
            out.push(element(self)?);
        }
        Ok(out)
    }

    /// FuzzSource::draw_bytes: a byte string of at most `max_len` bytes.
    pub fn draw_bytes(&mut self, max_len: usize) -> Result<Vec<u8>, Invalid> {
        let mut out = Vec::new();
        while self.more(out.len(), max_len, 24)? {
            out.push(self.draw_u8()?);
        }
        Ok(out)
    }

    /// FuzzSource::assume_guard: reject this run unless `cond`.
    pub fn assume(&mut self, cond: bool) -> Result<(), Invalid> {
        if cond { Ok(()) } else { Err(Invalid::Rejected) }
    }

    /// A SimPulse scheduling decision: `true` runs the fiber SimPulse
    /// dispatched, `false` defers it to a later round. Shrinks toward
    /// "run" (0), so shrunk schedules have the fewest deferrals.
    pub fn draw_run_now(&mut self) -> Result<bool, Invalid> {
        // Defer with probability 1/4 when generating.
        Ok(self.raw(DrawKind::Schedule, Gen::Bernoulli(256))? == 0)
    }

    pub fn probe(&mut self) -> &mut Probe {
        &mut self.probe
    }

    /// Number of choices drawn so far.
    pub fn draws(&self) -> usize {
        self.realized.len()
    }

    /// The choices actually consumed (after clamping), as a tape.
    pub fn realized(&self) -> ChoiceTape {
        ChoiceTape::new(self.realized.clone())
    }

    pub(crate) fn into_parts(self) -> (Vec<u64>, Vec<DrawKind>, Probe) {
        (self.realized, self.kinds, self.probe)
    }
}

/// Types with a canonical generator over a choice source (Vesper's
/// `Arbitrary` trait).
pub trait Arbitrary: Sized {
    fn arbitrary(src: &mut Source) -> Result<Self, Invalid>;
}

impl Arbitrary for bool {
    fn arbitrary(src: &mut Source) -> Result<Self, Invalid> {
        src.draw_bool()
    }
}
impl Arbitrary for u8 {
    fn arbitrary(src: &mut Source) -> Result<Self, Invalid> {
        src.draw_u8()
    }
}
impl Arbitrary for u16 {
    fn arbitrary(src: &mut Source) -> Result<Self, Invalid> {
        src.draw_u16()
    }
}
impl Arbitrary for u32 {
    fn arbitrary(src: &mut Source) -> Result<Self, Invalid> {
        src.draw_u32()
    }
}
impl Arbitrary for u64 {
    fn arbitrary(src: &mut Source) -> Result<Self, Invalid> {
        src.draw_u64()
    }
}
impl Arbitrary for i64 {
    /// Zig-zag decoded so the tape value 0 is 0, 1 is -1, 2 is 1, ...: shrinks
    /// toward zero from either side.
    fn arbitrary(src: &mut Source) -> Result<Self, Invalid> {
        let z = src.draw_u64()?;
        Ok(((z >> 1) as i64) ^ -((z & 1) as i64))
    }
}
impl<T: Arbitrary> Arbitrary for Option<T> {
    fn arbitrary(src: &mut Source) -> Result<Self, Invalid> {
        if src.draw_bool()? {
            Ok(Some(T::arbitrary(src)?))
        } else {
            Ok(None)
        }
    }
}
impl<T: Arbitrary> Arbitrary for Vec<T> {
    fn arbitrary(src: &mut Source) -> Result<Self, Invalid> {
        src.draw_vec(64, T::arbitrary)
    }
}
impl<A: Arbitrary, B: Arbitrary> Arbitrary for (A, B) {
    fn arbitrary(src: &mut Source) -> Result<Self, Invalid> {
        Ok((A::arbitrary(src)?, B::arbitrary(src)?))
    }
}
impl<A: Arbitrary, B: Arbitrary, C: Arbitrary> Arbitrary for (A, B, C) {
    fn arbitrary(src: &mut Source) -> Result<Self, Invalid> {
        Ok((A::arbitrary(src)?, B::arbitrary(src)?, C::arbitrary(src)?))
    }
}
