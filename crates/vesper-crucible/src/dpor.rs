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
    ExploreConfig, ExploreError, Machine, MachineKey, Next, Proc, SimFiber, StepError, dependent,
    failure_of, may_co_enable,
};
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
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

/// Clock vector: for each process, one plus the index of the latest event
/// of that process known to happen before.
type Clock = BTreeMap<Proc, usize>;

fn join(into: &mut Clock, other: &Clock) {
    for (&p, &v) in other {
        let e = into.entry(p).or_insert(0);
        *e = (*e).max(v);
    }
}

/// Full identity of a state for caching.
type StateKey<K> = (MachineKey, Vec<Failure>, Option<usize>, u32, K);

struct Cached {
    /// The sleep set the state was explored with.
    sleep: BTreeSet<Proc>,
    /// Every transition executed in its explored subtree.
    summary: BTreeSet<(Proc, Next)>,
}

struct Node<K> {
    enabled: Vec<Proc>,
    nexts: Vec<(Proc, Next)>,
    backtrack: BTreeSet<Proc>,
    done: BTreeSet<Proc>,
    sleep: BTreeSet<Proc>,
    /// The sleep set on arrival.
    sleep0: BTreeSet<Proc>,
    chosen: Option<Proc>,
    /// Process clocks in this state.
    proc_clocks: BTreeMap<Proc, Clock>,
    /// The fiber that ran last, and the preemptions so far.
    running: Option<usize>,
    preemptions: u32,
    key: Option<StateKey<K>>,
    /// The state was pruned by the cache (its subtree is `summary`).
    hit: bool,
    summary: BTreeSet<(Proc, Next)>,
}

impl<K> Node<K> {
    fn next_of(&self, p: Proc) -> Option<&Next> {
        self.nexts.iter().find(|(q, _)| *q == p).map(|(_, n)| n)
    }

    /// Whether running `p` here is a preemption: another fiber is chosen
    /// while the fiber that ran last is still ready.
    fn preempts(&self, p: Proc) -> bool {
        match (self.running, p) {
            (Some(c), Proc::Task(j)) => j != c && self.enabled.contains(&Proc::Task(c)),
            _ => false,
        }
    }

    fn allowed(&self, p: Proc, bound: Option<u32>) -> bool {
        bound.is_none_or(|b| self.preemptions + u32::from(self.preempts(p)) <= b)
    }
}

struct Event {
    proc: Proc,
    next: Next,
    clock: Clock,
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

/// Add `p`'s backtracking points for a race at event `i`, at the state
/// before event `at` (`at <= i`). FG05's candidate set `E` there is `p`
/// itself if enabled, and every process enabled there that runs a later
/// event which happens before `p`. A candidate in the sleep set is useless
/// (its exploration is pruned). Unbounded, the first awake member of `E` is
/// added, `p` first; bounded, every awake member is. If none is awake, or
/// `E` is empty, every enabled process is added.
fn add_at<K>(
    pre: &mut Node<K>,
    at: usize,
    p: Proc,
    events: &[Event],
    hb_p: &dyn Fn(usize, &Event) -> bool,
    bounded: bool,
) {
    let mut candidates: Vec<Proc> = Vec::new();
    if pre.enabled.contains(&p) {
        candidates.push(p);
    }
    for (j, e) in events.iter().enumerate().skip(at + 1) {
        if hb_p(j, e) && pre.enabled.contains(&e.proc) && !candidates.contains(&e.proc) {
            candidates.push(e.proc);
        }
    }
    let mut awake = candidates.into_iter().filter(|q| !pre.sleep.contains(q));
    if bounded {
        let awake: Vec<Proc> = awake.collect();
        if awake.is_empty() {
            let all = pre.enabled.clone();
            pre.backtrack.extend(all);
        } else {
            pre.backtrack.extend(awake);
        }
    } else {
        match awake.next() {
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

/// FG05's backtracking step for one process `p` whose next transition is
/// `next` and whose clock is `p_clock`: find the latest earlier event `i`
/// that races with it (is dependent, can be co-enabled, and does not happen
/// before `p`) and add backtracking points before it. `all_races` handles
/// every racing event instead (used for transitions of a cached subtree,
/// whose clocks are only known approximately). `bounded` also adds points
/// at the start of the scheduling block containing `i` (bounded POR).
fn backtrack_for<K>(
    head: &mut [Node<K>],
    events: &[Event],
    p: Proc,
    next: &Next,
    p_clock: Option<&Clock>,
    all_races: bool,
    bounded: bool,
) {
    let hb_p = |j: usize, e: &Event| p_clock.and_then(|c| c.get(&e.proc)).copied().unwrap_or(0) > j;
    for (i, e) in events.iter().enumerate().rev() {
        let races = e.proc != p
            && may_co_enable(e.proc, p)
            && dependent((e.proc, &e.next), (p, next))
            && !hb_p(i, e);
        if !races {
            continue;
        }
        add_at(&mut head[i], i, p, events, &hb_p, bounded);
        if bounded {
            // The start of the scheduling block containing `i`: the latest
            // event at or before `i` that switched the running fiber.
            let switches = |k: usize| match events[k].proc {
                Proc::Task(j) => head[k].running != Some(j),
                _ => false,
            };
            let mut b = i;
            while b > 0 && !switches(b) {
                b -= 1;
            }
            if b < i {
                add_at(&mut head[b], b, p, events, &hb_p, bounded);
            }
        }
        if !all_races {
            break;
        }
    }
}

fn add_backtrack_points<K>(nodes: &mut [Node<K>], events: &[Event], bounded: bool) {
    let (head, tail) = nodes.split_at_mut(events.len());
    let current = &tail[0];
    for (p, next) in &current.nexts {
        backtrack_for(
            head,
            events,
            *p,
            next,
            current.proc_clocks.get(p),
            false,
            bounded,
        );
    }
}

/// Race-check the transitions of a cached subtree against the prefix.
fn add_summary_points<K>(
    nodes: &mut [Node<K>],
    events: &[Event],
    summary: &BTreeSet<(Proc, Next)>,
    bounded: bool,
) {
    let (head, tail) = nodes.split_at_mut(events.len());
    let current = &tail[0];
    for (p, next) in summary {
        backtrack_for(
            head,
            events,
            *p,
            next,
            current.proc_clocks.get(p),
            true,
            bounded,
        );
    }
}

/// Choosing a delivery with one declared result also schedules its sibling
/// results here: they are alternative transitions of the same completion.
fn add_siblings<K>(node: &mut Node<K>, p: Proc) {
    if let Proc::Io { task, seq, .. } = p {
        for &q in &node.enabled {
            if matches!(q, Proc::Io { task: t, seq: s, .. } if t == task && s == seq) {
                node.backtrack.insert(q);
            }
        }
    }
}

fn new_node<W, F: SimFiber<W>, K>(
    m: &Machine<W, F>,
    proc_clocks: BTreeMap<Proc, Clock>,
    running: Option<usize>,
    preemptions: u32,
) -> Node<K> {
    Node {
        enabled: m.enabled(),
        nexts: m.nexts(),
        backtrack: BTreeSet::new(),
        done: BTreeSet::new(),
        sleep: BTreeSet::new(),
        sleep0: BTreeSet::new(),
        chosen: None,
        proc_clocks,
        running,
        preemptions,
        key: None,
        hit: false,
        summary: BTreeSet::new(),
    }
}

/// Pick the next unexplored, awake, allowed process at `node`, from its
/// enabled list (exhaustive) or its backtrack set (DPOR). Processes the
/// bound forbids are marked done and counted.
fn pick<K>(
    node: &mut Node<K>,
    mode: Mode,
    from_backtrack: bool,
    bound: Option<u32>,
    pruned: &mut u64,
) -> Option<Proc> {
    let candidates: Vec<Proc> = if from_backtrack {
        node.backtrack.iter().copied().collect()
    } else {
        node.enabled.clone()
    };
    for p in candidates {
        if node.done.contains(&p) || (mode == Mode::Dpor && node.sleep.contains(&p)) {
            continue;
        }
        if !node.allowed(p, bound) {
            node.done.insert(p);
            *pruned += 1;
            continue;
        }
        node.chosen = Some(p);
        node.done.insert(p);
        node.backtrack.insert(p);
        if mode == Mode::Dpor {
            add_siblings(node, p);
            // Bounded: a preemption is also explored without it (the
            // running fiber continues), because a trace-equivalent schedule
            // that spends the preemption later may be the only one within
            // the bound (see `tests/dpor_rest.rs`).
            if bound.is_some()
                && node.preempts(p)
                && let Some(c) = node.running
            {
                node.backtrack.insert(Proc::Task(c));
            }
        }
        return Some(p);
    }
    None
}

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
    let bounded = bound.is_some() && mode == Mode::Dpor;
    // Sleep sets are unsound under a preemption bound: the explored member
    // of a trace class may need more preemptions than a pruned one (Coons
    // et al. 2013, and `tests/dpor_rest.rs` found it), so bounded DPOR runs
    // without them.
    let sleeping = mode == Mode::Dpor && !bounded;
    let mut out = Exploration {
        outcomes: BTreeMap::new(),
        executions: 0,
        sleep_blocked: 0,
        transitions: 0,
        cache_hits: 0,
        bound_pruned: 0,
    };
    let mut cache: BTreeMap<StateKey<K>, Cached> = BTreeMap::new();
    let mut nodes: Vec<Node<K>> = Vec::new();
    let mut events: Vec<Event> = Vec::new();
    let mut state_key = |m: &Machine<W, F>,
                         failures: &[Failure],
                         running: Option<usize>,
                         preemptions: u32|
     -> Result<Option<StateKey<K>>, ExploreError> {
        let Some(user) = key.as_mut() else {
            return Ok(None);
        };
        let mut failures = failures.to_vec();
        failures.sort();
        let (running, preemptions) = if bound.is_some() {
            (running, preemptions)
        } else {
            (None, 0)
        };
        Ok(Some((
            m.key()?,
            failures,
            running,
            preemptions,
            user(&m.world, m.fibers()),
        )))
    };
    loop {
        // Re-execute the recorded prefix.
        let (world, fibers) = setup();
        let mut m = Machine::new(world, fibers, config.seed)?.with_max_fibers(config.max_fibers)?;
        let mut failures: Vec<Failure> = Vec::new();
        for e in &events {
            match m.execute(e.proc) {
                Ok(()) => {}
                Err(StepError::Explore(err)) => return Err(err),
                Err(StepError::Prop(p)) => failures.push(failure_of(p)?),
            }
            out.transitions += 1;
        }
        if nodes.is_empty() {
            let mut root = new_node(&m, BTreeMap::new(), None, 0);
            root.key = state_key(&m, &failures, None, 0)?;
            nodes.push(root);
        } else if m.enabled() != nodes[events.len()].enabled {
            return Err(ExploreError::Nondeterministic(
                "re-executed prefix reached a state with different enabled processes".into(),
            ));
        }
        // Extend to a final state.
        let leaf: Option<Outcome<O>> = loop {
            let d = events.len();
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
            if d >= config.max_depth {
                return Err(ExploreError::DepthExceeded {
                    limit: config.max_depth,
                });
            }
            let node = &mut nodes[d];
            let choice = match node.chosen {
                Some(p) => Some(p),
                None => pick(node, mode, false, bound, &mut out.bound_pruned),
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
            node.summary.insert((p, next.clone()));
            // Child sleep set: sleeping processes independent of `p`.
            let child_sleep: BTreeSet<Proc> = if sleeping {
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
            let running = match p {
                Proc::Task(j) => Some(j),
                _ => node.running,
            };
            let preemptions = node.preemptions + u32::from(node.preempts(p));
            // Happens-before clock of the new event.
            let mut clock = node.proc_clocks.get(&p).cloned().unwrap_or_default();
            for e in &events {
                if dependent((e.proc, &e.next), (p, &next)) {
                    join(&mut clock, &e.clock);
                }
            }
            clock.insert(p, d + 1);
            let mut proc_clocks = node.proc_clocks.clone();
            let before: BTreeSet<Proc> = node.nexts.iter().map(|(q, _)| *q).collect();
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
                Err(StepError::Prop(e)) => failures.push(failure_of(e)?),
            }
            // A newly created process (a spawned fiber, a delivery, a
            // timer) starts after the step that created it.
            for (q, _) in m.nexts() {
                if !before.contains(&q) && !proc_clocks.contains_key(&q) {
                    proc_clocks.insert(q, clock.clone());
                }
            }
            let mut child = new_node(&m, proc_clocks, running, preemptions);
            child.sleep0.clone_from(&child_sleep);
            child.sleep = child_sleep;
            child.key = state_key(&m, &failures, running, preemptions)?;
            let hit = child
                .key
                .as_ref()
                .and_then(|k| cache.get(k))
                .filter(|c| mode == Mode::Exhaustive || c.sleep.is_subset(&child.sleep));
            if let Some(cached) = hit {
                child.hit = true;
                child.summary = cached.summary.clone();
            }
            let hit = child.hit;
            nodes.push(child);
            if mode == Mode::Dpor {
                add_backtrack_points(&mut nodes, &events, bounded);
            }
            if hit {
                out.cache_hits += 1;
                if mode == Mode::Dpor {
                    let summary = std::mem::take(&mut nodes[events.len()].summary);
                    add_summary_points(&mut nodes, &events, &summary, bounded);
                    nodes[events.len()].summary = summary;
                }
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
                && sleeping
            {
                node.sleep.insert(prev);
            }
            let found = if node.hit {
                None
            } else {
                pick(node, mode, mode == Mode::Dpor, bound, &mut out.bound_pruned)
            };
            if found.is_some() {
                // Re-run with `events` truncated to this node's depth.
                events.truncate(nodes.len() - 1);
                break;
            }
            let done = nodes.pop().expect("a last node");
            if let Some(parent) = nodes.last_mut() {
                parent.summary.extend(done.summary.iter().cloned());
            }
            if let Some(k) = done.key
                && !done.hit
            {
                let replace = cache
                    .get(&k)
                    .is_none_or(|c| done.sleep0.is_subset(&c.sleep));
                if replace {
                    cache.insert(
                        k,
                        Cached {
                            sleep: done.sleep0,
                            summary: done.summary,
                        },
                    );
                }
            }
            events.truncate(nodes.len().saturating_sub(1));
        }
    }
}
