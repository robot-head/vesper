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

use crate::explore::{
    ExploreConfig, ExploreError, Machine, Next, Proc, SimFiber, StepError, dependent, failure_of,
    may_co_enable,
};
use crate::source::{Failure, PropError};
use std::collections::{BTreeMap, BTreeSet};

/// The final outcome of one execution: the observer's value for a completed
/// run, or the failure that stopped it.
pub type Outcome<O> = Result<O, Failure>;

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
}

impl<O: Ord> Exploration<O> {
    /// The distinct outcomes.
    pub fn outcome_set(&self) -> BTreeSet<&Outcome<O>> {
        self.outcomes.keys().collect()
    }

    /// The first failure found, with its schedule.
    pub fn first_failure(&self) -> Option<(&Failure, &[Proc])> {
        self.outcomes
            .iter()
            .find_map(|(o, s)| o.as_ref().err().map(|f| (f, s.as_slice())))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Exhaustive,
    Dpor,
}

/// Clock vector: for each process, one plus the index of the latest event
/// of that process known to happen before.
type Clock = BTreeMap<Proc, usize>;

fn join(into: &mut Clock, other: &Clock) {
    for (&p, &v) in other {
        let e = into.entry(p).or_insert(0);
        *e = (*e).max(v);
    }
}

struct Node {
    enabled: Vec<Proc>,
    nexts: Vec<(Proc, Next)>,
    backtrack: BTreeSet<Proc>,
    done: BTreeSet<Proc>,
    sleep: BTreeSet<Proc>,
    chosen: Option<Proc>,
    /// Process clocks in this state.
    proc_clocks: BTreeMap<Proc, Clock>,
}

impl Node {
    fn next_of(&self, p: Proc) -> Option<&Next> {
        self.nexts.iter().find(|(q, _)| *q == p).map(|(_, n)| n)
    }
}

struct Event {
    proc: Proc,
    next: Next,
    clock: Clock,
}

/// Explore every schedule of the machine built by `setup`.
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
    explore(Mode::Exhaustive, config, setup, observe)
}

/// Explore one schedule per Mazurkiewicz trace (at least), by DPOR.
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
    explore(Mode::Dpor, config, setup, observe)
}

fn new_node<W, F: SimFiber<W>>(m: &Machine<W, F>, proc_clocks: BTreeMap<Proc, Clock>) -> Node {
    Node {
        enabled: m.enabled(),
        nexts: m.nexts(),
        backtrack: BTreeSet::new(),
        done: BTreeSet::new(),
        sleep: BTreeSet::new(),
        chosen: None,
        proc_clocks,
    }
}

/// FG05's backtracking step at the newest node: for every process `p` with
/// a next transition, find the latest earlier event `i` that races with it
/// (is dependent, can be co-enabled, and does not happen before `p`). At the
/// state before `i`, FG05's candidate set is `E`: `p` itself if enabled
/// there, and every process enabled there that runs a later event which
/// happens before `p`. A candidate in that state's sleep set is useless
/// (its exploration is pruned), so the first awake member of `E` is added,
/// `p` first; if every member is asleep or `E` is empty, every enabled
/// process is added.
fn add_backtrack_points(nodes: &mut [Node], events: &[Event]) {
    let (head, tail) = nodes.split_at_mut(events.len());
    let current = &tail[0];
    for (p, next) in &current.nexts {
        let p_clock = current.proc_clocks.get(p);
        let hb_p =
            |j: usize, e: &Event| p_clock.and_then(|c| c.get(&e.proc)).copied().unwrap_or(0) > j;
        let race = events.iter().enumerate().rev().find(|&(j, e)| {
            e.proc != *p
                && may_co_enable(e.proc, *p)
                && dependent((e.proc, &e.next), (*p, next))
                && !hb_p(j, e)
        });
        let Some((i, _)) = race else { continue };
        let pre = &mut head[i];
        let mut candidates: Vec<Proc> = Vec::new();
        if pre.enabled.contains(p) {
            candidates.push(*p);
        }
        for (j, e) in events.iter().enumerate().skip(i + 1) {
            if hb_p(j, e) && pre.enabled.contains(&e.proc) && !candidates.contains(&e.proc) {
                candidates.push(e.proc);
            }
        }
        match candidates.into_iter().find(|q| !pre.sleep.contains(q)) {
            Some(q) => {
                pre.backtrack.insert(q);
            }
            None => {
                let all = pre.enabled.clone();
                pre.backtrack.extend(all);
            }
        }
    }
}

fn explore<W, F, S, V, O>(
    mode: Mode,
    config: &ExploreConfig,
    mut setup: S,
    mut observe: V,
) -> Result<Exploration<O>, ExploreError>
where
    F: SimFiber<W>,
    S: FnMut() -> (W, Vec<F>),
    V: FnMut(&W) -> Result<O, PropError>,
    O: Ord,
{
    let mut out = Exploration {
        outcomes: BTreeMap::new(),
        executions: 0,
        sleep_blocked: 0,
        transitions: 0,
    };
    let mut nodes: Vec<Node> = Vec::new();
    let mut events: Vec<Event> = Vec::new();
    loop {
        // Re-execute the recorded prefix.
        let (world, fibers) = setup();
        let mut m = Machine::new(world, fibers, config.seed)?;
        for e in &events {
            match m.execute(e.proc) {
                Ok(()) => {}
                Err(StepError::Explore(err)) => return Err(err),
                Err(StepError::Prop(p)) => {
                    return Err(ExploreError::Nondeterministic(format!(
                        "recorded prefix step {} failed on re-execution: {p:?}",
                        e.proc
                    )));
                }
            }
            out.transitions += 1;
        }
        if nodes.is_empty() {
            nodes.push(new_node(&m, BTreeMap::new()));
        } else if m.enabled() != nodes[events.len()].enabled {
            return Err(ExploreError::Nondeterministic(
                "re-executed prefix reached a state with different enabled processes".into(),
            ));
        }
        // Extend to a final state.
        let leaf: Option<Outcome<O>> = loop {
            let d = events.len();
            if m.complete() {
                break Some(match observe(&m.world) {
                    Ok(o) => Ok(o),
                    Err(e) => Err(failure_of(e)?),
                });
            }
            if d >= config.max_depth {
                return Err(ExploreError::DepthExceeded {
                    limit: config.max_depth,
                });
            }
            let node = &mut nodes[d];
            let choice = match node.chosen {
                Some(p) => Some(p),
                None => {
                    let pick = node.enabled.iter().copied().find(|p| {
                        !node.done.contains(p)
                            && (mode == Mode::Exhaustive || !node.sleep.contains(p))
                    });
                    if let Some(p) = pick {
                        node.chosen = Some(p);
                        node.done.insert(p);
                        node.backtrack.insert(p);
                    }
                    pick
                }
            };
            let Some(p) = choice else {
                if node.enabled.is_empty() {
                    return Err(ExploreError::Nondeterministic(
                        "machine quiesced with unfinished fibers".into(),
                    ));
                }
                out.sleep_blocked += 1;
                break None;
            };
            let next = node
                .next_of(p)
                .cloned()
                .expect("an enabled process has a next transition");
            // Child sleep set: sleeping processes independent of `p`.
            let child_sleep: BTreeSet<Proc> = if mode == Mode::Dpor {
                node.sleep
                    .iter()
                    .copied()
                    .filter(|&q| {
                        q != p
                            && node
                                .next_of(q)
                                .is_some_and(|nq| !dependent((q, nq), (p, &next)))
                    })
                    .collect()
            } else {
                BTreeSet::new()
            };
            // Happens-before clock of the new event.
            let mut clock = node.proc_clocks.get(&p).cloned().unwrap_or_default();
            for e in &events {
                if dependent((e.proc, &e.next), (p, &next)) {
                    join(&mut clock, &e.clock);
                }
            }
            clock.insert(p, d + 1);
            let mut proc_clocks = node.proc_clocks.clone();
            let result = m.execute(p);
            out.transitions += 1;
            proc_clocks.insert(p, clock.clone());
            events.push(Event {
                proc: p,
                next,
                clock: clock.clone(),
            });
            match result {
                Ok(()) => {}
                Err(StepError::Explore(err)) => return Err(err),
                Err(StepError::Prop(e)) => break Some(Err(failure_of(e)?)),
            }
            // A newly created delivery or timer process starts after the
            // step that submitted or armed it.
            for (q, _) in m.nexts() {
                if !matches!(q, Proc::Task(_)) && !proc_clocks.contains_key(&q) {
                    proc_clocks.insert(q, clock.clone());
                }
            }
            let mut child = new_node(&m, proc_clocks);
            child.sleep = child_sleep;
            nodes.push(child);
            if mode == Mode::Dpor {
                add_backtrack_points(&mut nodes, &events);
            }
        };
        if let Some(outcome) = leaf {
            out.executions += 1;
            if out.executions > config.max_executions {
                return Err(ExploreError::ExecutionLimit {
                    limit: config.max_executions,
                });
            }
            out.outcomes
                .entry(outcome)
                .or_insert_with(|| events.iter().map(|e| e.proc).collect());
        }
        // Backtrack to the deepest node with an unexplored alternative.
        nodes.truncate(events.len() + 1);
        loop {
            let Some(node) = nodes.last_mut() else {
                return Ok(out);
            };
            if let Some(prev) = node.chosen.take()
                && mode == Mode::Dpor
            {
                node.sleep.insert(prev);
            }
            let pick = match mode {
                Mode::Dpor => node
                    .backtrack
                    .iter()
                    .copied()
                    .find(|p| !node.done.contains(p) && !node.sleep.contains(p)),
                Mode::Exhaustive => node
                    .enabled
                    .iter()
                    .copied()
                    .find(|p| !node.done.contains(p)),
            };
            if let Some(p) = pick {
                node.chosen = Some(p);
                node.done.insert(p);
                // Re-run with `events` truncated to this node's depth.
                events.truncate(nodes.len() - 1);
                break;
            }
            nodes.pop();
            events.truncate(nodes.len().saturating_sub(1));
        }
    }
}
