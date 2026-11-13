//! The SimPulse step machine with timers and I/O completions (§29.2), and a
//! choice-tape driver over it.
//!
//! Fibers are Rust step machines ([`SimFiber`]) whose steps may yield,
//! finish, sleep on a SimPulse timer, submit I/O that SimPulse completes
//! after a virtual delay, or park on *external* I/O whose completion the
//! explorer delivers. In every state the machine offers a set of enabled
//! [`Proc`]s:
//!
//! * [`Proc::Task`]`(i)`: run one step of ready fiber `i`;
//! * [`Proc::Io`]: deliver the `seq`-th external I/O completion of fiber
//!   `task` (any time after it was submitted);
//! * [`Proc::Timer`]: when no fiber is ready, fire the pending timer or
//!   scheduled I/O with the earliest deadline (the lowest `(task, seq)` among
//!   equal deadlines): SimPulse advances virtual time to it and fires
//!   everything due then. Each timer is its own process, created by the step
//!   that armed it.
//!
//! So a schedule can deliver a reply before or after a timeout fires, which
//! a yield/finish-only explorer cannot express. Everything runs on the real
//! [`vesper_simpulse::Simulator`]: SimPulse has no "dispatch this task" call,
//! so the machine steers it by yielding tokens it did not want back to
//! SimPulse (recorded as dispatch/yield pairs in the journal), and every run
//! returns SimPulse's journal, which [`replay_schedule`] re-validates with
//! `Simulator::replay`.
//!
//! Zero-delay sleeps and scheduled I/O are rejected ([`ExploreError::ZeroDelay`]):
//! SimPulse would fire them inside the next dispatch, which is not a separate
//! schedulable event.

use crate::source::{Failure, Invalid, PropError, Source};
use std::collections::BTreeMap;
use std::fmt;
use vesper_simpulse::{CompletionId, IoResult, Journal, Simulator, TaskId, TaskState, WakeReason};

/// Why a fiber was woken, with schedule-independent identities.
///
/// SimPulse numbers completions from one counter shared by all fibers, so
/// its `CompletionId`s depend on the interleaving and would make otherwise
/// independent steps observably order-dependent. Steps see per-fiber
/// sequence numbers instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    Start,
    Yield,
    /// The fiber's `seq`-th timer or scheduled I/O fired as a timer.
    Timer {
        seq: u32,
    },
    /// The fiber's `seq`-th timer-backed (scheduled) I/O completed.
    ScheduledIo {
        seq: u32,
        result: IoResult,
    },
    /// The fiber's `seq`-th external I/O was delivered.
    ExternalIo {
        seq: u32,
        result: IoResult,
    },
}

/// A shared-state object a fiber step reads or writes (chosen by the test).
pub type ObjId = u64;

/// One access.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Access {
    Read(ObjId),
    Write(ObjId),
}

/// The objects a step may read and write. A fiber declares the footprint of
/// its *next* step before the step runs; the machine checks that the step's
/// recorded accesses stay inside it. Partial-order reduction is only as
/// sound as these footprints, so a violation aborts exploration.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Footprint {
    reads: Vec<ObjId>,
    writes: Vec<ObjId>,
}

fn insert_sorted(v: &mut Vec<ObjId>, obj: ObjId) {
    if let Err(at) = v.binary_search(&obj) {
        v.insert(at, obj);
    }
}

fn intersects(a: &[ObjId], b: &[ObjId]) -> bool {
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => return true,
        }
    }
    false
}

impl Footprint {
    /// No shared accesses (a purely local step).
    pub fn none() -> Self {
        Self::default()
    }

    pub fn read(mut self, obj: ObjId) -> Self {
        insert_sorted(&mut self.reads, obj);
        self
    }

    pub fn write(mut self, obj: ObjId) -> Self {
        insert_sorted(&mut self.writes, obj);
        self
    }

    pub fn reads(&self) -> &[ObjId] {
        &self.reads
    }

    pub fn writes(&self) -> &[ObjId] {
        &self.writes
    }

    /// Whether two steps with these footprints may not commute: one writes
    /// an object the other reads or writes.
    pub fn conflicts(&self, other: &Self) -> bool {
        intersects(&self.writes, &other.writes)
            || intersects(&self.writes, &other.reads)
            || intersects(&self.reads, &other.writes)
    }

    /// Whether `access` is permitted (a declared write also permits reads).
    pub fn permits(&self, access: Access) -> bool {
        match access {
            Access::Read(o) => {
                self.reads.binary_search(&o).is_ok() || self.writes.binary_search(&o).is_ok()
            }
            Access::Write(o) => self.writes.binary_search(&o).is_ok(),
        }
    }
}

/// How a parked fiber's I/O completes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IoRequest {
    /// SimPulse completes it with `result` `delay_ns` after submission
    /// (`Simulator::schedule_io`); it fires on a [`Proc::Timer`] transition.
    Scheduled { delay_ns: u64, result: IoResult },
    /// The environment completes it with `result` at a point the explorer
    /// chooses (`Simulator::park_io` + `complete_io`): a [`Proc::Io`]
    /// transition.
    External { result: IoResult },
}

/// What a fiber step did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimStep {
    /// Stay ready.
    Yield,
    /// Finished.
    Done,
    /// Park on a SimPulse timer for `ns` (> 0) virtual nanoseconds.
    Sleep { ns: u64 },
    /// Park on an I/O completion.
    Io(IoRequest),
}

/// What a step sees, and where it records its accesses.
#[derive(Debug)]
pub struct StepCx {
    task: usize,
    reason: Wake,
    now_ns: u64,
    accesses: Vec<Access>,
}

impl StepCx {
    /// Index of the running fiber (its position in the spawn vector).
    pub fn task(&self) -> usize {
        self.task
    }

    /// Why SimPulse woke this fiber: start, yield, its timer, or its I/O
    /// completion with the result, identified per fiber (see [`Wake`]).
    pub fn reason(&self) -> Wake {
        self.reason
    }

    /// SimPulse virtual time.
    pub fn now_ns(&self) -> u64 {
        self.now_ns
    }

    /// Record a read of `obj` (must be inside the declared footprint).
    pub fn read(&mut self, obj: ObjId) {
        self.accesses.push(Access::Read(obj));
    }

    /// Record a write of `obj` (must be inside the declared footprint).
    pub fn write(&mut self, obj: ObjId) {
        self.accesses.push(Access::Write(obj));
    }
}

/// A cooperative fiber over world `W`. Steps are atomic.
pub trait SimFiber<W> {
    /// Footprint of the next step. It must depend only on the fiber's own
    /// state (it is queried while the fiber is parked or not yet chosen) and
    /// must cover every wake reason the next step can see.
    fn footprint(&self) -> Footprint;

    fn step(&mut self, world: &mut W, cx: &mut StepCx) -> Result<SimStep, PropError>;
}

/// A schedulable process; the machine's transitions are "the next action of
/// process p".
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Proc {
    /// Step fiber `i`.
    Task(usize),
    /// Deliver the `seq`-th (from 0) external I/O completion of fiber `task`.
    Io { task: usize, seq: u32 },
    /// Fire the `seq`-th (from 0) timer or scheduled I/O armed by fiber
    /// `task`: SimPulse advances virtual time to its deadline.
    Timer { task: usize, seq: u32 },
}

impl fmt::Display for Proc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Task(i) => write!(f, "step({i})"),
            Self::Io { task, seq } => write!(f, "deliver({task}#{seq})"),
            Self::Timer { task, seq } => write!(f, "tick({task}#{seq})"),
        }
    }
}

/// Exploration errors: the harness, not the property, went wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExploreError {
    /// A step recorded an access outside its declared footprint.
    Footprint { task: usize, access: Access },
    /// A sleep or scheduled I/O with zero delay.
    ZeroDelay { task: usize },
    /// SimPulse rejected a transition (including a journal replay mismatch).
    SimPulse(String),
    /// A schedule named a process that is not enabled.
    NotEnabled(Proc),
    /// Re-executing a recorded prefix behaved differently (a fiber or setup
    /// is nondeterministic).
    Nondeterministic(String),
    /// An execution exceeded `ExploreConfig::max_depth` transitions.
    DepthExceeded { limit: usize },
    /// Exploration exceeded `ExploreConfig::max_executions`.
    ExecutionLimit { limit: u64 },
    /// A fiber or observer returned `Invalid` during systematic exploration,
    /// where there is no tape to reject.
    Invalid(Invalid),
}

impl fmt::Display for ExploreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Footprint { task, access } => {
                write!(f, "fiber {task} made undeclared access {access:?}")
            }
            Self::ZeroDelay { task } => write!(f, "fiber {task} parked with zero delay"),
            Self::SimPulse(e) => write!(f, "simpulse: {e}"),
            Self::NotEnabled(p) => write!(f, "{p} is not enabled"),
            Self::Nondeterministic(e) => write!(f, "nondeterministic re-execution: {e}"),
            Self::DepthExceeded { limit } => write!(f, "execution exceeded {limit} transitions"),
            Self::ExecutionLimit { limit } => write!(f, "exploration exceeded {limit} executions"),
            Self::Invalid(i) => write!(f, "invalid during exploration: {i}"),
        }
    }
}

impl std::error::Error for ExploreError {}

/// Failure kind used when an [`ExploreError`] surfaces inside a property
/// run by [`explore_random`].
pub const EXPLORE_KIND: &str = "explore";

impl From<ExploreError> for PropError {
    fn from(e: ExploreError) -> Self {
        match e {
            ExploreError::Invalid(i) => PropError::Invalid(i),
            ExploreError::DepthExceeded { .. } => PropError::Invalid(Invalid::Overrun),
            other => PropError::fail(EXPLORE_KIND, other.to_string()),
        }
    }
}

/// Limits and SimPulse seed.
#[derive(Clone, Copy, Debug)]
pub struct ExploreConfig {
    /// Seed of the SimPulse simulator. It only orders SimPulse's internal
    /// round-robin, which the machine overrides; outcomes do not depend on it.
    pub seed: u64,
    /// Transitions per execution.
    pub max_depth: usize,
    /// Executions per systematic exploration.
    pub max_executions: u64,
}

impl Default for ExploreConfig {
    fn default() -> Self {
        Self {
            seed: 0,
            max_depth: 10_000,
            max_executions: 1_000_000,
        }
    }
}

/// Result of a transition the explorer must react to.
#[derive(Debug)]
pub(crate) enum StepError {
    /// The property failed (or was invalid) inside a fiber step.
    Prop(PropError),
    Explore(ExploreError),
}

impl From<ExploreError> for StepError {
    fn from(e: ExploreError) -> Self {
        Self::Explore(e)
    }
}

fn sim_err(e: impl fmt::Display) -> ExploreError {
    ExploreError::SimPulse(e.to_string())
}

/// What the explorer needs to know about a process's next transition to
/// decide dependence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Next {
    Task(Footprint),
    Io,
    Timer,
}

/// Dependence of two transitions: they may not commute, or one may enable
/// or disable the other. A timer is dependent with everything: it is
/// disabled by any ready fiber and by an earlier deadline, and it changes
/// the time steps observe.
pub(crate) fn dependent(a: (Proc, &Next), b: (Proc, &Next)) -> bool {
    match (a, b) {
        ((Proc::Timer { .. }, _), _) | (_, (Proc::Timer { .. }, _)) => true,
        ((Proc::Task(i), Next::Task(fa)), (Proc::Task(j), Next::Task(fb))) => {
            i == j || fa.conflicts(fb)
        }
        ((Proc::Task(i), _), (Proc::Io { task, .. }, _))
        | ((Proc::Io { task, .. }, _), (Proc::Task(i), _)) => i == task,
        ((Proc::Io { task: a, .. }, _), (Proc::Io { task: b, .. }, _)) => a == b,
        _ => true,
    }
}

/// Whether two transitions can ever be enabled in the same state. A fiber
/// step and a timer cannot (a timer fires only when every fiber is parked); a
/// fiber's step and the delivery of its own pending I/O cannot (it is
/// parked until the delivery).
pub(crate) fn may_co_enable(a: Proc, b: Proc) -> bool {
    match (a, b) {
        (Proc::Task(_), Proc::Timer { .. }) | (Proc::Timer { .. }, Proc::Task(_)) => false,
        (Proc::Task(i), Proc::Io { task, .. }) | (Proc::Io { task, .. }, Proc::Task(i)) => {
            i != task
        }
        _ => true,
    }
}

/// The machine: a SimPulse simulator, the world, and the fibers.
pub(crate) struct Machine<W, F> {
    sim: Simulator<usize>,
    pub(crate) world: W,
    fibers: Vec<F>,
    ids: Vec<TaskId>,
    reasons: Vec<Option<WakeReason>>,
    io_seq: Vec<u32>,
    external: BTreeMap<(usize, u32), (CompletionId, IoResult)>,
    waiting_external: Vec<bool>,
    /// Per-fiber identity of every completion SimPulse allocated.
    completions: BTreeMap<CompletionId, (u32, bool)>,
    /// The pending timer or scheduled I/O of each fiber: (seq, deadline).
    timers: Vec<Option<(u32, u64)>>,
    timer_seq: Vec<u32>,
    pub(crate) deferrals: u64,
    pub(crate) steps: u64,
}

impl<W, F: SimFiber<W>> Machine<W, F> {
    pub(crate) fn new(world: W, fibers: Vec<F>, seed: u64) -> Result<Self, ExploreError> {
        Self::with_sim(world, fibers, Simulator::new(seed))
    }

    pub(crate) fn replaying(
        world: W,
        fibers: Vec<F>,
        journal: Journal,
    ) -> Result<Self, ExploreError> {
        Self::with_sim(world, fibers, Simulator::replay(journal).map_err(sim_err)?)
    }

    fn with_sim(world: W, fibers: Vec<F>, mut sim: Simulator<usize>) -> Result<Self, ExploreError> {
        let n = fibers.len();
        let mut ids = Vec::with_capacity(n);
        for i in 0..n {
            ids.push(sim.spawn(i).map_err(|e| sim_err(e.error))?);
        }
        Ok(Self {
            sim,
            world,
            fibers,
            ids,
            reasons: vec![None; n],
            io_seq: vec![0; n],
            external: BTreeMap::new(),
            waiting_external: vec![false; n],
            completions: BTreeMap::new(),
            timers: vec![None; n],
            timer_seq: vec![0; n],
            deferrals: 0,
            steps: 0,
        })
    }

    fn state(&self, i: usize) -> TaskState {
        self.sim
            .task_state(self.ids[i])
            .expect("spawned task is known")
    }

    /// The timer that fires next: earliest deadline, then lowest fiber.
    fn next_timer(&self) -> Option<Proc> {
        self.timers
            .iter()
            .enumerate()
            .filter_map(|(task, t)| t.map(|(seq, deadline)| (deadline, task, seq)))
            .min()
            .map(|(_, task, seq)| Proc::Timer { task, seq })
    }

    pub(crate) fn complete(&self) -> bool {
        self.sim.is_complete()
    }

    pub(crate) fn now_ns(&self) -> u64 {
        self.sim.now_ns()
    }

    pub(crate) fn journal(&self) -> &Journal {
        self.sim.journal()
    }

    pub(crate) fn finish_replay(&self) -> Result<(), ExploreError> {
        self.sim.finish_replay().map_err(sim_err)
    }

    /// Enabled processes, in canonical order (fibers, deliveries, clock).
    pub(crate) fn enabled(&self) -> Vec<Proc> {
        let mut out: Vec<Proc> = (0..self.fibers.len())
            .filter(|&i| self.state(i) == TaskState::Ready)
            .map(Proc::Task)
            .collect();
        let any_ready = !out.is_empty();
        out.extend(
            self.external
                .keys()
                .map(|&(task, seq)| Proc::Io { task, seq }),
        );
        if !any_ready && let Some(t) = self.next_timer() {
            out.push(t);
        }
        out
    }

    /// Every process that still has a next transition, enabled or not, with
    /// its dependence information.
    pub(crate) fn nexts(&self) -> Vec<(Proc, Next)> {
        let mut out = Vec::new();
        for (i, fiber) in self.fibers.iter().enumerate() {
            if self.state(i) != TaskState::Finished {
                out.push((Proc::Task(i), Next::Task(fiber.footprint())));
            }
        }
        out.extend(
            self.external
                .keys()
                .map(|&(task, seq)| (Proc::Io { task, seq }, Next::Io)),
        );
        out.extend(
            self.timers
                .iter()
                .enumerate()
                .filter_map(|(task, t)| t.map(|(seq, _)| (Proc::Timer { task, seq }, Next::Timer))),
        );
        out
    }

    fn observe(&mut self, token: &vesper_simpulse::RunToken<usize>) {
        let i = *token.payload();
        if self.reasons[i].is_none() {
            self.reasons[i] = Some(token.reason());
        }
    }

    /// Execute `p`, which must be enabled.
    pub(crate) fn execute(&mut self, p: Proc) -> Result<(), StepError> {
        if !self.enabled().contains(&p) {
            return Err(ExploreError::NotEnabled(p).into());
        }
        match p {
            Proc::Task(i) => self.step_task(i),
            Proc::Io { task, seq } => {
                let (completion, result) = self
                    .external
                    .remove(&(task, seq))
                    .expect("enabled delivery is pending");
                self.waiting_external[task] = false;
                self.sim.complete_io(completion, result).map_err(sim_err)?;
                Ok(())
            }
            Proc::Timer { .. } => {
                let token = self
                    .sim
                    .next_task()
                    .map_err(sim_err)?
                    .ok_or_else(|| sim_err("clock advance woke no task"))?;
                let now = self.sim.now_ns();
                for t in &mut self.timers {
                    if t.is_some_and(|(_, deadline)| deadline <= now) {
                        *t = None;
                    }
                }
                self.observe(&token);
                self.deferrals += 1;
                self.sim.yield_task(token).map_err(|e| sim_err(e.error))?;
                Ok(())
            }
        }
    }

    fn arm(&mut self, i: usize, delay_ns: u64) -> Result<u32, ExploreError> {
        let deadline = self
            .sim
            .now_ns()
            .checked_add(delay_ns)
            .ok_or_else(|| sim_err("virtual clock overflow"))?;
        let seq = self.timer_seq[i];
        self.timer_seq[i] = seq
            .checked_add(1)
            .ok_or_else(|| sim_err("timer sequence exhausted"))?;
        self.timers[i] = Some((seq, deadline));
        Ok(seq)
    }

    fn step_task(&mut self, i: usize) -> Result<(), StepError> {
        let token = loop {
            let token = self
                .sim
                .next_task()
                .map_err(sim_err)?
                .ok_or_else(|| sim_err("no runnable task while a fiber is ready"))?;
            self.observe(&token);
            if *token.payload() == i {
                break token;
            }
            self.deferrals += 1;
            self.sim.yield_task(token).map_err(|e| sim_err(e.error))?;
        };
        let raw = self.reasons[i].take().unwrap_or(token.reason());
        let ident = |c: &CompletionId| {
            self.completions
                .get(c)
                .copied()
                .ok_or_else(|| sim_err("wake for an unknown completion"))
        };
        let reason = match raw {
            WakeReason::Start => Wake::Start,
            WakeReason::Yield => Wake::Yield,
            WakeReason::Timer(c) => Wake::Timer { seq: ident(&c)?.0 },
            WakeReason::Io { completion, result } => match ident(&completion)? {
                (seq, true) => Wake::ExternalIo { seq, result },
                (seq, false) => Wake::ScheduledIo { seq, result },
            },
        };
        let declared = self.fibers[i].footprint();
        let mut cx = StepCx {
            task: i,
            reason,
            now_ns: self.sim.now_ns(),
            accesses: Vec::new(),
        };
        let result = self.fibers[i].step(&mut self.world, &mut cx);
        self.steps += 1;
        if let Some(&access) = cx.accesses.iter().find(|&&a| !declared.permits(a)) {
            return Err(ExploreError::Footprint { task: i, access }.into());
        }
        let next = match result {
            Ok(next) => next,
            Err(e) => {
                // Retire the failed fiber so the other fibers can go on.
                self.sim.finish(token).map_err(|e| sim_err(e.error))?;
                return Err(StepError::Prop(e));
            }
        };
        let transition = match next {
            SimStep::Yield => self.sim.yield_task(token),
            SimStep::Done => self.sim.finish(token).map(drop),
            SimStep::Sleep { ns: 0 } | SimStep::Io(IoRequest::Scheduled { delay_ns: 0, .. }) => {
                return Err(ExploreError::ZeroDelay { task: i }.into());
            }
            SimStep::Sleep { ns } => {
                let seq = self.arm(i, ns)?;
                self.sim.sleep(token, ns).map(|c| {
                    self.completions.insert(c, (seq, false));
                })
            }
            SimStep::Io(IoRequest::Scheduled { delay_ns, result }) => {
                let seq = self.arm(i, delay_ns)?;
                self.sim.schedule_io(token, delay_ns, result).map(|c| {
                    self.completions.insert(c, (seq, false));
                })
            }
            SimStep::Io(IoRequest::External { result }) => match self.sim.park_io(token) {
                Ok(completion) => {
                    let seq = self.io_seq[i];
                    self.io_seq[i] = seq
                        .checked_add(1)
                        .ok_or_else(|| sim_err("external I/O sequence exhausted"))?;
                    self.external.insert((i, seq), (completion, result));
                    self.completions.insert(completion, (seq, true));
                    self.waiting_external[i] = true;
                    Ok(())
                }
                Err(e) => Err(e),
            },
        };
        transition.map_err(|e| sim_err(e.error))?;
        Ok(())
    }
}

/// One completed schedule.
#[derive(Debug)]
pub struct SimTrace<W> {
    /// The final world.
    pub world: W,
    /// The transitions taken, in order.
    pub schedule: Vec<Proc>,
    /// SimPulse's journal of the run.
    pub journal: Journal,
    /// Fiber steps executed.
    pub steps: u64,
    /// Tokens the machine handed back to SimPulse to steer it.
    pub deferrals: u64,
    /// Final virtual time.
    pub now_ns: u64,
}

fn finish<W, F: SimFiber<W>>(m: Machine<W, F>, schedule: Vec<Proc>) -> SimTrace<W> {
    SimTrace {
        journal: m.journal().clone(),
        steps: m.steps,
        deferrals: m.deferrals,
        now_ns: m.now_ns(),
        world: m.world,
        schedule,
    }
}

fn lift(e: StepError) -> PropError {
    match e {
        StepError::Prop(p) => p,
        StepError::Explore(e) => e.into(),
    }
}

/// Run `fibers` to completion, drawing every scheduling decision (which
/// enabled process goes next, including I/O deliveries and clock advances)
/// from `src` as an index into the canonical enabled list. Shrinking the
/// tape therefore shrinks toward "first enabled process", i.e. fibers in
/// index order, deliveries before timers.
pub fn explore_random<W, F: SimFiber<W>>(
    src: &mut Source,
    world: W,
    fibers: Vec<F>,
    config: &ExploreConfig,
) -> Result<SimTrace<W>, PropError> {
    let mut m = Machine::new(world, fibers, config.seed)?;
    let mut schedule = Vec::new();
    while !m.complete() {
        if schedule.len() >= config.max_depth {
            return Err(Invalid::Overrun.into());
        }
        let enabled = m.enabled();
        if enabled.is_empty() {
            return Err(PropError::fail(
                EXPLORE_KIND,
                "machine quiesced with unfinished fibers",
            ));
        }
        let p = enabled[src.draw_index(enabled.len())?];
        m.execute(p).map_err(lift)?;
        schedule.push(p);
    }
    Ok(finish(m, schedule))
}

/// Re-run `schedule` exactly. With `journal`, SimPulse replays it and
/// validates every transition, and the journal must be fully consumed.
pub fn replay_schedule<W, F: SimFiber<W>>(
    world: W,
    fibers: Vec<F>,
    schedule: &[Proc],
    journal: Option<Journal>,
    config: &ExploreConfig,
) -> Result<SimTrace<W>, PropError> {
    let mut m = match journal {
        Some(j) => Machine::replaying(world, fibers, j)?,
        None => Machine::new(world, fibers, config.seed)?,
    };
    for &p in schedule {
        m.execute(p).map_err(lift)?;
    }
    if !m.complete() {
        return Err(PropError::fail(
            EXPLORE_KIND,
            "schedule ended with unfinished fibers",
        ));
    }
    m.finish_replay()?;
    Ok(finish(m, schedule.to_vec()))
}

/// Render a schedule compactly, e.g. `step(0) deliver(0#0) tick`.
pub fn format_schedule(schedule: &[Proc]) -> String {
    let parts: Vec<String> = schedule.iter().map(ToString::to_string).collect();
    parts.join(" ")
}

/// The failure reported for a fiber step that failed.
pub(crate) fn failure_of(e: PropError) -> Result<Failure, ExploreError> {
    match e {
        PropError::Fail(f) => Ok(f),
        PropError::Invalid(i) => Err(ExploreError::Invalid(i)),
    }
}
