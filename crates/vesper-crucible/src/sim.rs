//! Concolic SimPulse schedule exploration (§29.2).
//!
//! Fibers are explicit step machines run on the real
//! [`vesper_simpulse::Simulator`]. The schedule seed SimPulse uses to order
//! each round, and a run/defer decision at every dispatch, are drawn from the
//! same choice tape as the property's data. Shrinking that tape therefore
//! shrinks the schedule: deleted fibers, fewer deferrals, seed 0. The
//! SimPulse journal of a run is returned so a counterexample can also be
//! re-validated by SimPulse's own journal replay ([`replay_fibers`]).
//!
//! This explores schedules of Rust step machines. It does not execute Vesper
//! source fibers; integration wires `vesper test --sim` to it.

use crate::source::{Invalid, PropError, Source};
use vesper_simpulse::{Journal, Simulator};

/// What a fiber did in one step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FiberStep {
    /// More steps remain; reschedule.
    Yield,
    /// Finished.
    Done,
}

/// A cooperative fiber over shared world state `W`. Each `step` is atomic
/// with respect to the other fibers.
pub trait Fiber<W> {
    fn step(&mut self, world: &mut W) -> Result<FiberStep, PropError>;
}

/// Exploration limits.
#[derive(Clone, Copy, Debug)]
pub struct SimConfig {
    /// Dispatches (steps plus deferrals) before the run is `Invalid::Overrun`.
    pub max_dispatches: u64,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            max_dispatches: 10_000,
        }
    }
}

/// What one schedule did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimRun {
    pub seed: u64,
    pub journal: Journal,
    /// Dispatches SimPulse made (the schedule length).
    pub dispatches: u64,
    /// Dispatches the tape deferred instead of stepping.
    pub deferrals: u64,
    pub steps: u64,
}

fn simpulse_error(error: impl std::fmt::Display) -> PropError {
    PropError::fail("simpulse", error.to_string())
}

fn drive<W, F: Fiber<W>>(
    src: &mut Source,
    world: &mut W,
    fibers: Vec<F>,
    config: &SimConfig,
    mut sim: Simulator<F>,
    seed: u64,
) -> Result<SimRun, PropError> {
    for fiber in fibers {
        sim.spawn(fiber).map_err(|e| simpulse_error(e.error))?;
    }
    let (mut dispatches, mut deferrals, mut steps) = (0u64, 0u64, 0u64);
    while let Some(mut token) = sim.next_task().map_err(simpulse_error)? {
        dispatches += 1;
        if dispatches > config.max_dispatches {
            return Err(Invalid::Overrun.into());
        }
        if !src.draw_run_now()? {
            deferrals += 1;
            sim.yield_task(token).map_err(|e| simpulse_error(e.error))?;
            continue;
        }
        steps += 1;
        match token.payload_mut().step(world)? {
            FiberStep::Yield => sim.yield_task(token).map_err(|e| simpulse_error(e.error))?,
            FiberStep::Done => {
                sim.finish(token).map_err(|e| simpulse_error(e.error))?;
            }
        }
    }
    if !sim.is_complete() {
        return Err(PropError::fail(
            "simpulse",
            "simulation quiesced with unfinished fibers",
        ));
    }
    sim.finish_replay().map_err(simpulse_error)?;
    Ok(SimRun {
        seed,
        journal: sim.journal().clone(),
        dispatches,
        deferrals,
        steps,
    })
}

/// Run `fibers` to completion under a schedule drawn from `src`.
pub fn run_fibers<W, F: Fiber<W>>(
    src: &mut Source,
    world: &mut W,
    fibers: Vec<F>,
    config: &SimConfig,
) -> Result<SimRun, PropError> {
    let seed = src.draw_u64()?;
    drive(src, world, fibers, config, Simulator::new(seed), seed)
}

/// Re-run under SimPulse's journal replay: every transition is validated
/// against `journal` (a mismatch is a `"simpulse"` failure) and the journal
/// must be fully consumed. The decisions are drawn from `src` exactly as in
/// [`run_fibers`], so replaying the same tape must reproduce the journal.
pub fn replay_fibers<W, F: Fiber<W>>(
    src: &mut Source,
    world: &mut W,
    fibers: Vec<F>,
    journal: Journal,
    config: &SimConfig,
) -> Result<SimRun, PropError> {
    let seed = src.draw_u64()?;
    if seed != journal.seed {
        return Err(PropError::fail(
            "simpulse",
            format!(
                "tape seed {seed} differs from journal seed {}",
                journal.seed
            ),
        ));
    }
    let sim = Simulator::replay(journal).map_err(simpulse_error)?;
    drive(src, world, fibers, config, sim, seed)
}
