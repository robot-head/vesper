//! Systematic schedule exploration of the SimPulse step machine: exhaustive
//! enumeration and dynamic partial-order reduction (§29.2).
//!
//! Both explorers are stateless: every execution re-runs `setup` and
//! replays the recorded prefix, so worlds and fibers need not be `Clone`.
//!
//! [`explore_dpor`] is persistent-set DPOR (Flanagan and Godefroid, POPL
//! 2005) with clock-vector happens-before and sleep sets. Two transitions
//! are dependent when their declared footprints conflict, when they belong
//! to the same fiber (a fiber's step and the delivery of its I/O), or when
//! one is a clock advance, which every ready fiber disables. Races are only
//! reversed between transitions that can be enabled together (a fiber step
//! and a clock advance never are). Source-set DPOR (Abdulla et al. 2014) was
//! not used: its source-set argument assumes transitions do not disable one
//! another, and SimPulse's clock advance is disabled by every I/O delivery
//! that readies a fiber; the persistent-set algorithm handles disabling by
//! backtracking to every enabled process when the racing one is not
//! enabled.
//!
//! The guarantee (given accurate footprints and deterministic fibers): every
//! Mazurkiewicz trace of the step machine is represented by at least one
//! explored execution, so every reachable final state and every reachable
//! fiber failure is found. `tests/dpor.rs` checks this differentially
//! against [`explore_exhaustive`] (same set of distinct outcomes, fewer
//! executions).
//!
//! Wave-seven extensions:
//!
//! * **Preemption bounding** (`ExploreConfig::preemption_bound`): only
//!   schedules with at most `k` preemptions are explored (Musuvathi and
//!   Qadeer's iterative context bounding). Bounded DPOR follows Coons,
//!   Musuvathi and McKinley's bounded POR (OOPSLA 2013): a race is also
//!   reversed at the start of the scheduling block of its first event, every
//!   awake member of FG05's candidate set is added, and a node that takes a
//!   preemption also explores letting the running fiber continue. It runs
//!   without sleep sets, which are unsound under a bound (the explored member
//!   of a trace class may need more preemptions than a pruned one). The
//!   guarantee is relative to the bound: every outcome of a schedule within
//!   it is found, checked differentially against bounded exhaustive search.
//! * **State caching** ([`explore_cached`]): a caller-supplied key of the
//!   world and fibers, together with the machine's own schedule-independent
//!   state, the failures so far and the preemption state, identifies a
//!   state. A state whose subtree was fully explored is not explored again.
//!   Under DPOR a cached state is pruned only if it was explored with a sleep
//!   set contained in the current one, and every next transition seen in its
//!   subtree is race-checked against the current prefix (every racing event,
//!   not only the latest), so the prefix still gets the backtracking points
//!   the pruned subtree would have added. Bounded DPOR does not prune on
//!   cache hits: the differential stress found a bounded case it missed, so
//!   caching applies to exhaustive search (bounded or not) and to unbounded
//!   DPOR.
//! * **Spawning, cancellation and chosen I/O results**: a spawned fiber is a
//!   new process that happens after its spawning step; spawns are mutually
//!   dependent (one index counter); a cancellation request is dependent with
//!   every step of its target; each declared result of an external
//!   completion is its own delivery process, and choosing one adds its
//!   sibling results as backtracking points.

#[cfg(vesper_port)]
use crate::explore::Footprint;
#[cfg(not(vesper_port))]
use crate::explore::Proc;
use crate::explore::{ExploreConfig, ExploreError, MachineKey, SimFiber};
#[cfg(vesper_port)]
use crate::explore::{Machine, Next, Proc, StepError, failure_of};
use crate::source::{Failure, PropError};
use std::collections::{BTreeMap, BTreeSet};

/// The final outcome of one execution: the observer's value, or every
/// failure that happened in it (sorted). A fiber whose step fails is
/// retired and the other fibers run on, so the set of failures is the same
/// for every interleaving of one Mazurkiewicz trace; reporting only the
/// first failure would depend on which of two independent failing steps
/// happened to run first.
pub type Outcome<O> = Result<O, Vec<Failure>>;

/// What a systematic exploration found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exploration<O: Ord> {
    /// Each distinct outcome with the first schedule that produced it.
    pub outcomes: BTreeMap<Outcome<O>, Vec<Proc>>,
    /// Executions run to a final state (completed or failed).
    pub executions: u64,
    /// Executions abandoned because every enabled process was in the sleep
    /// set (DPOR only; such a prefix is equivalent to an explored one).
    pub sleep_blocked: u64,
    /// Transitions executed in total, including prefix re-execution.
    pub transitions: u64,
    /// States reached that were already explored ([`explore_cached`]); the
    /// execution through them is abandoned.
    pub cache_hits: u64,
    /// Choices skipped because they would exceed
    /// `ExploreConfig::preemption_bound`.
    pub bound_pruned: u64,
}

impl<O: Ord> Exploration<O> {
    /// The distinct outcomes.
    pub fn outcome_set(&self) -> BTreeSet<&Outcome<O>> {
        self.outcomes.keys().collect()
    }

    /// A failure found, with the schedule of its execution.
    pub fn first_failure(&self) -> Option<(&Failure, &[Proc])> {
        self.outcomes.iter().find_map(|(o, s)| {
            o.as_ref()
                .err()
                .and_then(|f| f.first())
                .map(|f| (f, s.as_slice()))
        })
    }
}

pub(crate) type StateKey<K> = (MachineKey, Vec<Failure>, Option<usize>, u32, K);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Exhaustive,
    Dpor,
}

/// Which systematic explorer [`explore_cached`] runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Search {
    /// Every schedule (within the preemption bound).
    Exhaustive,
    /// Persistent-set DPOR with sleep sets.
    Dpor,
}

/// Explore every schedule of the machine built by `setup` (within
/// `config.preemption_bound`, if set).
pub fn explore_exhaustive<W, F, S, V, O>(
    config: &ExploreConfig,
    setup: S,
    observe: V,
) -> Result<Exploration<O>, ExploreError>
where
    F: SimFiber<W>,
    S: FnMut() -> (W, Vec<F>),
    V: FnMut(&W) -> Result<O, PropError>,
    O: Ord,
{
    explore(
        Mode::Exhaustive,
        config,
        setup,
        observe,
        None::<fn(&W, &[F])>,
    )
}

/// Explore one schedule per Mazurkiewicz trace (at least), by DPOR (within
/// `config.preemption_bound`, if set).
pub fn explore_dpor<W, F, S, V, O>(
    config: &ExploreConfig,
    setup: S,
    observe: V,
) -> Result<Exploration<O>, ExploreError>
where
    F: SimFiber<W>,
    S: FnMut() -> (W, Vec<F>),
    V: FnMut(&W) -> Result<O, PropError>,
    O: Ord,
{
    explore(Mode::Dpor, config, setup, observe, None::<fn(&W, &[F])>)
}

/// [`explore_exhaustive`] or [`explore_dpor`] with state caching. `key`
/// must identify the world and every fiber's state completely (two states
/// with equal keys must have the same future); the machine adds its own
/// state (task states, timers, pending I/O, cancellation flags, virtual
/// time), the failures so far and the preemption state.
pub fn explore_cached<W, F, S, V, O, K, KF>(
    search: Search,
    config: &ExploreConfig,
    setup: S,
    observe: V,
    key: KF,
) -> Result<Exploration<O>, ExploreError>
where
    F: SimFiber<W>,
    S: FnMut() -> (W, Vec<F>),
    V: FnMut(&W) -> Result<O, PropError>,
    O: Ord,
    K: Ord,
    KF: FnMut(&W, &[F]) -> K,
{
    let mode = match search {
        Search::Exhaustive => Mode::Exhaustive,
        Search::Dpor => Mode::Dpor,
    };
    explore(mode, config, setup, observe, Some(key))
}

#[cfg(not(vesper_port))]
fn explore<W, F, S, V, O, K, KF>(
    mode: Mode,
    config: &ExploreConfig,
    setup: S,
    observe: V,
    key: Option<KF>,
) -> Result<Exploration<O>, ExploreError>
where
    F: SimFiber<W>,
    S: FnMut() -> (W, Vec<F>),
    V: FnMut(&W) -> Result<O, PropError>,
    O: Ord,
    K: Ord,
    KF: FnMut(&W, &[F]) -> K,
{
    crate::engine::dpor::explore(mode, config, setup, observe, key)
}

/// The `none` word of the Vesper search machine (no running fiber, no key).
#[cfg(vesper_port)]
const NONE: u64 = u64::MAX;

/// A process as the order-keeping word of the Vesper search machine.
#[cfg(vesper_port)]
fn encode_proc(p: Proc) -> u64 {
    match p {
        Proc::Task(i) => {
            assert!(
                i < 1 << 22,
                "fiber index {i} too large for the Vesper search"
            );
            i as u64
        }
        Proc::Io { task, seq, choice } => {
            assert!(
                task < 1 << 22,
                "fiber index {task} too large for the Vesper search"
            );
            1 << 62 | (task as u64) << 40 | u64::from(seq) << 8 | u64::from(choice)
        }
        Proc::Timer { task, seq } => {
            assert!(
                task < 1 << 30,
                "fiber index {task} too large for the Vesper search"
            );
            2 << 62 | (task as u64) << 32 | u64::from(seq)
        }
    }
}

#[cfg(vesper_port)]
fn decode_proc(w: u64) -> Proc {
    match w >> 62 {
        0 => Proc::Task(w as usize),
        1 => Proc::Io {
            task: ((w >> 40) & 0x3f_ffff) as usize,
            seq: (w >> 8) as u32,
            choice: w as u8,
        },
        _ => Proc::Timer {
            task: ((w >> 32) & 0x3fff_ffff) as usize,
            seq: w as u32,
        },
    }
}

/// The enabled processes and the `(process, transition)` words of a state.
/// A fiber's transition is its footprint's id, registered on first sight.
#[cfg(vesper_port)]
fn encode_state<W, F: SimFiber<W>>(
    search: &mut crate::binding::Search,
    footprints: &mut BTreeMap<Footprint, u64>,
    m: &Machine<W, F>,
    enabled: &mut Vec<u64>,
    nexts: &mut Vec<u64>,
) {
    enabled.clear();
    enabled.extend(m.enabled().into_iter().map(encode_proc));
    nexts.clear();
    for (p, next) in m.nexts() {
        let code = match next {
            Next::Task(fp) => match footprints.get(&fp) {
                Some(&id) => id,
                None => {
                    let cancels: Vec<u64> = fp.cancels().iter().map(|&t| t as u64).collect();
                    let id = search.footprint(fp.reads(), fp.writes(), fp.spawns(), &cancels);
                    footprints.insert(fp, id);
                    id
                }
            },
            Next::Io | Next::Timer => 0,
        };
        nexts.push(encode_proc(p));
        nexts.push(code);
    }
}

/// The search loop of the port build: the Vesper machine (`vesper/src/
/// lib.ves`, "Systematic schedule search") makes every choice; this loop
/// runs the SimPulse machine, the fibers and the observer.
#[cfg(vesper_port)]
fn explore<W, F, S, V, O, K, KF>(
    mode: Mode,
    config: &ExploreConfig,
    mut setup: S,
    mut observe: V,
    mut key: Option<KF>,
) -> Result<Exploration<O>, ExploreError>
where
    F: SimFiber<W>,
    S: FnMut() -> (W, Vec<F>),
    V: FnMut(&W) -> Result<O, PropError>,
    O: Ord,
    K: Ord,
    KF: FnMut(&W, &[F]) -> K,
{
    let bound = config.preemption_bound;
    let mut search = crate::binding::Search::new(
        mode == Mode::Dpor,
        bound.is_some(),
        bound.map_or(0, u64::from),
        key.is_some(),
    );
    let mut out = Exploration {
        outcomes: BTreeMap::new(),
        executions: 0,
        sleep_blocked: 0,
        transitions: 0,
        cache_hits: 0,
        bound_pruned: 0,
    };
    let mut footprints: BTreeMap<Footprint, u64> = BTreeMap::new();
    let mut keys: BTreeMap<StateKey<K>, u64> = BTreeMap::new();
    let mut state_key = |m: &Machine<W, F>,
                         failures: &[Failure],
                         running: Option<usize>,
                         preemptions: u32|
     -> Result<u64, ExploreError> {
        let Some(user) = key.as_mut() else {
            return Ok(NONE);
        };
        let mut failures = failures.to_vec();
        failures.sort();
        let (running, preemptions) = if bound.is_some() {
            (running, preemptions)
        } else {
            (None, 0)
        };
        let k = (
            m.key()?,
            failures,
            running,
            preemptions,
            user(&m.world, m.fibers()),
        );
        let fresh = keys.len() as u64;
        Ok(*keys.entry(k).or_insert(fresh))
    };
    let (mut enabled, mut nexts) = (Vec::new(), Vec::new());
    loop {
        // Re-execute the recorded prefix.
        let mut schedule: Vec<Proc> = search.prefix().into_iter().map(decode_proc).collect();
        let (world, fibers) = setup();
        let mut m = Machine::new(world, fibers, config.seed)?.with_max_fibers(config.max_fibers)?;
        let mut failures: Vec<Failure> = Vec::new();
        for &p in &schedule {
            match m.execute(p) {
                Ok(()) => {}
                Err(StepError::Explore(err)) => return Err(err),
                Err(StepError::Prop(p)) => failures.push(failure_of(p)?),
            }
            out.transitions += 1;
        }
        encode_state(&mut search, &mut footprints, &m, &mut enabled, &mut nexts);
        if search.nodes() == 0 {
            let k = state_key(&m, &failures, None, 0)?;
            search.root(&enabled, &nexts, k);
        } else if !search.same_enabled(&enabled) {
            return Err(ExploreError::Nondeterministic(
                "re-executed prefix reached a state with different enabled processes".into(),
            ));
        }
        // Extend to a final state.
        let leaf: Option<Outcome<O>> = loop {
            if m.complete() {
                if !failures.is_empty() {
                    failures.sort();
                    break Some(Err(std::mem::take(&mut failures)));
                }
                break Some(match observe(&m.world) {
                    Ok(o) => Ok(o),
                    Err(e) => Err(vec![failure_of(e)?]),
                });
            }
            if schedule.len() >= config.max_depth {
                return Err(ExploreError::DepthExceeded {
                    limit: config.max_depth,
                });
            }
            let choice = search.choose();
            match choice.code {
                0 => {}
                1 => break None,
                _ => {
                    return Err(ExploreError::Nondeterministic(
                        "machine quiesced with unfinished fibers".into(),
                    ));
                }
            }
            let p = decode_proc(choice.value);
            let result = m.execute(p);
            out.transitions += 1;
            schedule.push(p);
            match result {
                Ok(()) => {}
                Err(StepError::Explore(err)) => return Err(err),
                Err(StepError::Prop(e)) => failures.push(failure_of(e)?),
            }
            // The fiber that just stepped stays "running" only if it
            // yielded (is still ready).
            let still_ready = match p {
                Proc::Task(j) => m.enabled().contains(&Proc::Task(j)),
                _ => false,
            };
            encode_state(&mut search, &mut footprints, &m, &mut enabled, &mut nexts);
            search.child(still_ready, &enabled, &nexts);
            let running = search.running();
            let running = (running != NONE).then_some(running as usize);
            let preemptions = search.preemptions() as u32;
            let k = state_key(&m, &failures, running, preemptions)?;
            if search.key(k) {
                break None;
            }
        };
        if let Some(outcome) = leaf {
            out.executions += 1;
            if out.executions > config.max_executions {
                return Err(ExploreError::ExecutionLimit {
                    limit: config.max_executions,
                });
            }
            out.outcomes.entry(outcome).or_insert(schedule);
        }
        if !search.backtrack() {
            let stats = search.stats();
            out.sleep_blocked = stats.sleep_blocked;
            out.cache_hits = stats.cache_hits;
            out.bound_pruned = stats.bound_pruned;
            return Ok(out);
        }
    }
}
