//! Reset-per-execution virtual arenas for generated values (§29.1, §29.2
//! `Arbitrary[r]` over a `Region`).
//!
//! A [`VirtualArena`] owns one bounded `vesper-alloc` [`Region`] (64 KiB by
//! default) and one reusable [`Source`]. Every execution draws its inputs
//! into the region through [`ArbitraryIn`] and the region is reset when the
//! execution ends, so every handle minted by one execution is stale in the
//! next (the region's checked handles report `HandleError::Stale`). The
//! source's tape buffers are re-armed rather than reallocated. After the
//! first executions have grown the region's chunk and slot table and the
//! source's buffers to their steady-state sizes, an execution performs no
//! heap allocation (`tests/arena_alloc.rs` counts this with a global
//! allocator).
//!
//! A value that does not fit the arena is [`Invalid::ArenaExhausted`]: the
//! run is discarded like any other invalid run, never truncated.
//!
//! This is the host-runtime arena. Lowering source-level `Arbitrary[r]`
//! instances onto it is compiler work this crate does not do.

use crate::exec::Verdict;
use crate::source::{Arbitrary, Invalid, PropError, Source};
use std::fmt;
use vesper_alloc::{ConfigError, Region, RegionBytes, RegionConfig, RegionError, RegionVec};

/// Default arena size: one 64 KiB chunk, retained across resets.
pub const VIRTUAL_ARENA_BYTES: usize = 64 * 1024;

/// Default upper bound on allocations tracked per execution.
pub const VIRTUAL_ARENA_MAX_ALLOCATIONS: usize = 4096;

/// Failure kind reported when an execution left a linear region payload
/// live. The arena is rebuilt (the payload is finalized, not leaked) and the
/// leak is counted in [`ArenaStats::linear_leaks`].
pub const ARENA_LINEAR_LEAK_KIND: &str = "arena-linear-leak";

/// Why an arena could not be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArenaSetupError {
    /// The size is zero.
    ZeroSize,
    /// `vesper-alloc` rejected the derived region configuration.
    Config(ConfigError),
}

impl fmt::Display for ArenaSetupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroSize => f.write_str("virtual arena size is zero"),
            Self::Config(e) => write!(f, "virtual arena region configuration: {e:?}"),
        }
    }
}

impl std::error::Error for ArenaSetupError {}

/// Arena accounting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ArenaStats {
    /// Executions run so far.
    pub executions: u64,
    /// Region bytes in use when the last execution ended (before reset).
    pub last_used_bytes: usize,
    /// Largest `last_used_bytes` seen.
    pub high_water_bytes: usize,
    /// Executions that ended with a live linear region payload.
    pub linear_leaks: u64,
    /// Executions whose input did not fit ([`Invalid::ArenaExhausted`]).
    pub exhausted: u64,
}

/// A reset-per-execution input arena and reusable choice source.
pub struct VirtualArena {
    config: RegionConfig,
    region: Region,
    src: Source,
    stats: ArenaStats,
}

impl fmt::Debug for VirtualArena {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VirtualArena")
            .field("bytes", &self.config.max_reserved_bytes)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl VirtualArena {
    /// A [`VIRTUAL_ARENA_BYTES`] arena.
    pub fn new() -> Result<Self, ArenaSetupError> {
        Self::with_bytes(VIRTUAL_ARENA_BYTES)
    }

    /// An arena of exactly `bytes` bytes: one chunk of that size, which is
    /// also the reservation bound and is retained across resets.
    pub fn with_bytes(bytes: usize) -> Result<Self, ArenaSetupError> {
        if bytes == 0 {
            return Err(ArenaSetupError::ZeroSize);
        }
        let config = RegionConfig {
            initial_chunk_bytes: bytes,
            max_chunk_bytes: bytes,
            max_reserved_bytes: bytes,
            max_align: 64,
            max_allocations: VIRTUAL_ARENA_MAX_ALLOCATIONS,
            max_checkpoints: 16,
            retained_bytes: bytes,
        };
        let region = Region::new(config).map_err(ArenaSetupError::Config)?;
        Ok(Self {
            config,
            region,
            src: Source::replay(&crate::ChoiceTape::default()),
            stats: ArenaStats::default(),
        })
    }

    /// The arena's size in bytes.
    pub fn bytes(&self) -> usize {
        self.config.max_reserved_bytes
    }

    pub fn stats(&self) -> ArenaStats {
        self.stats
    }

    /// Bytes currently allocated in the region (0 between executions).
    pub fn used_bytes(&self) -> usize {
        self.region.stats().used_bytes
    }

    /// Reset epoch of the underlying region; it advances once per execution.
    pub fn epoch(&self) -> u64 {
        self.region.stats().epoch
    }

    /// The choices the last execution consumed.
    pub fn realized(&self) -> &[u64] {
        self.src.realized_values()
    }

    /// Run `prop` once, strictly replaying `tape`.
    pub fn run_replay<P>(&mut self, tape: &[u64], prop: P) -> Verdict
    where
        P: FnOnce(&mut Source, &mut Region) -> Result<(), PropError>,
    {
        self.src.rearm_strict(tape);
        self.execute(prop)
    }

    /// Run `prop` once on `prefix` followed by pseudo-random choices from
    /// `seed` (at most `max_draws` choices in all).
    pub fn run_random<P>(&mut self, prefix: &[u64], seed: u64, max_draws: usize, prop: P) -> Verdict
    where
        P: FnOnce(&mut Source, &mut Region) -> Result<(), PropError>,
    {
        self.src.rearm_random(prefix, seed, max_draws);
        self.execute(prop)
    }

    fn execute<P>(&mut self, prop: P) -> Verdict
    where
        P: FnOnce(&mut Source, &mut Region) -> Result<(), PropError>,
    {
        let result = prop(&mut self.src, &mut self.region);
        let used = self.region.stats().used_bytes;
        self.stats.executions += 1;
        self.stats.last_used_bytes = used;
        self.stats.high_water_bytes = self.stats.high_water_bytes.max(used);
        let mut verdict = match result {
            Ok(()) => Verdict::Pass,
            Err(PropError::Fail(f)) => Verdict::Fail(f),
            Err(PropError::Invalid(i)) => {
                if i == Invalid::ArenaExhausted {
                    self.stats.exhausted += 1;
                }
                Verdict::Invalid(i)
            }
        };
        if let Err(RegionError::LinearObligationsLive { count }) = self.region.reset() {
            // The region refuses to discard live linear payloads. Rebuild
            // it: dropping the old region finalizes them (no silent leak of
            // memory), and the protocol violation is reported and counted.
            self.stats.linear_leaks += 1;
            let fresh = Region::new(self.config).expect("configuration validated at construction");
            drop(std::mem::replace(&mut self.region, fresh));
            verdict = Verdict::Fail(crate::Failure::new(
                ARENA_LINEAR_LEAK_KIND,
                format!("{count} linear region payload(s) live at the end of the execution"),
            ));
        }
        verdict
    }
}

fn exhausted<E>(_: E) -> Invalid {
    Invalid::ArenaExhausted
}

/// Types generated directly into an execution's region (`Arbitrary[r]`).
pub trait ArbitraryIn: Sized {
    fn arbitrary_in(src: &mut Source, region: &mut Region) -> Result<Self, Invalid>;
}

/// Longest collection [`ArbitraryIn`] generates for `RegionBytes` and
/// `RegionVec<T>`. Use [`draw_region_bytes`] / [`draw_region_vec`] for
/// other bounds.
pub const ARBITRARY_IN_MAX_LEN: usize = 1024;

/// A byte string of at most `max_len` bytes, drawn straight into `region`
/// (same tape encoding as [`Source::draw_bytes`]: a continuation flag before
/// each byte). Bytes are staged in a 256-byte stack buffer, so no heap
/// allocation happens.
pub fn draw_region_bytes(
    src: &mut Source,
    region: &mut Region,
    max_len: usize,
) -> Result<RegionBytes, Invalid> {
    let mut out = region.alloc_bytes(0, 1).map_err(exhausted)?;
    let mut stage = [0u8; 256];
    let (mut staged, mut len) = (0usize, 0usize);
    while src.more(len, max_len, 24)? {
        stage[staged] = src.draw_u8()?;
        staged += 1;
        len += 1;
        if staged == stage.len() {
            region.extend_bytes(&mut out, &stage).map_err(exhausted)?;
            staged = 0;
        }
    }
    if staged > 0 {
        region
            .extend_bytes(&mut out, &stage[..staged])
            .map_err(exhausted)?;
    }
    Ok(out)
}

/// A vector of at most `max_len` elements drawn by `T::arbitrary` and pushed
/// into `region` (same tape encoding as [`Source::draw_vec`]).
pub fn draw_region_vec<T: Arbitrary + 'static>(
    src: &mut Source,
    region: &mut Region,
    max_len: usize,
) -> Result<RegionVec<T>, Invalid> {
    let mut out = region.vec_new::<T>().map_err(exhausted)?;
    let mut len = 0usize;
    while src.more(len, max_len, 8)? {
        let value = T::arbitrary(src)?;
        region.vec_push(&mut out, value).map_err(exhausted)?;
        len += 1;
    }
    Ok(out)
}

impl ArbitraryIn for RegionBytes {
    fn arbitrary_in(src: &mut Source, region: &mut Region) -> Result<Self, Invalid> {
        draw_region_bytes(src, region, ARBITRARY_IN_MAX_LEN)
    }
}

impl<T: Arbitrary + 'static> ArbitraryIn for RegionVec<T> {
    fn arbitrary_in(src: &mut Source, region: &mut Region) -> Result<Self, Invalid> {
        draw_region_vec(src, region, ARBITRARY_IN_MAX_LEN)
    }
}

impl<A: ArbitraryIn, B: ArbitraryIn> ArbitraryIn for (A, B) {
    fn arbitrary_in(src: &mut Source, region: &mut Region) -> Result<Self, Invalid> {
        Ok((A::arbitrary_in(src, region)?, B::arbitrary_in(src, region)?))
    }
}
