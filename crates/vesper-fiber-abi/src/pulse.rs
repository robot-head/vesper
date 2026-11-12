//! Adapter from native continuations to `vesper_pulse::scheduler`.
//!
//! [`NativeTask`] implements [`Resume`]. The scheduler's ownership rules carry
//! over unchanged: every queue transition moves the task (and therefore its
//! one-shot [`Parked`] continuation), and `Resume::resume(self, ..)` consumes
//! it. I/O requests move the task's `File` and a fresh buffer into the
//! driver; the completion moves them back.
//!
//! Park operation mapping (prototype op tags):
//! * `OP_YIELD` → `Step::Yield`, resumed with `Value(0)` on `Event::Yield`.
//! * `OP_READ(len, dest)` → `Step::Io(Read)`; the completion copies the bytes
//!   into the data frame at `dest` (bounds-checked) and resumes `Value(n)`.
//!   An I/O error resumes `Error(errno)`.
//! * Any other op resumes `Error(ENOSYS)` immediately.
//!
//! Cancellation (RFC §5). A task receives at most one `Cancelled` resume; a
//! later `Event::Cancelled` for a task whose cancellation was already
//! delivered is unwrapped and handled as the interrupted event.
//! * Before start (`Event::Cancelled(Start)`, or any first event once its
//!   scope is cancelled): no generated code runs; [`Fiber::cancel`] yields a
//!   `CANCELLED` exit with zero parks.
//! * After yield (`Event::Cancelled(Yield)`): the fiber resumes `Cancelled`
//!   once and runs its cancellation edge.
//! * During I/O: the driver owns the request, file, buffer and continuation.
//!   A cancellation *command* (`Driver::cancel_all`, an io_uring
//!   `AsyncCancel` CQE) releases none of them; only the original request's
//!   completion returns them. For a task in a cancelled [`NativeRuntime`]
//!   scope, that completion (ECANCELED, a short read, or a full read that won
//!   the race) recovers the file and buffer and resumes `Cancelled`. A task
//!   created with [`NativeTask::new`] keeps the v1 behaviour: the completion
//!   is delivered as I/O (ECANCELED resumes `Error(125)`).
//! * If the cancellation edge parks again, the task stays scheduled (yield
//!   or I/O) and is driven to its real exit; it is never reported complete
//!   early.
//!
//! Faults (a rejected resume, `Start` delivered to a parked task, I/O
//! delivered to an unstarted task) report `Step::Failed`. Inside a
//! [`NativeRuntime`] the task's live owner is retained, not dropped, and can
//! be recovered with [`NativeRuntime::take_retained`].
use crate::abi::exit;
use crate::fiber::{Exit, Fiber, Outcome, Parked, Resumption};
use crate::prototype::{OP_READ, OP_YIELD};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fs::File;
use std::io;
use std::rc::Rc;
use vesper_pulse::io_driver::{Operation, Request};
use vesper_pulse::scheduler::{Driver, Event, Resume, Scheduler, Step};

pub const EBADF: u64 = 9;
pub const EFAULT: u64 = 14;
pub const EINVAL: u64 = 22;
pub const ENOSYS: u64 = 38;
pub const EIO: u64 = 5;
pub const ECANCELED: i32 = 125;
/// Largest read a prototype task may request.
pub const MAX_READ: u64 = 4096;

enum State {
    Fresh(Fiber),
    Parked(Parked),
}

/// The live owner inside a task, as returned by [`NativeTask::into_owner`].
#[derive(Debug)]
pub enum TaskOwner {
    /// Never started: [`Fiber::cancel`] or [`Fiber::start`] it.
    Created(Fiber),
    /// Parked: answer, [`Parked::cancel`] or (last resort) drop it.
    Parked(Parked),
}

/// Where a task was when its one cancellation was delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelPoint {
    /// Never started; no generated code ran.
    BeforeStart,
    /// Parked on `OP_YIELD` in the ready queue.
    AfterYield,
    /// Parked on `OP_READ`; delivered once the original completion returned
    /// the file and buffer.
    DuringIo,
}

/// Final record of one task in a [`NativeRuntime`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskRecord {
    pub fiber_id: u64,
    /// One of [`exit`].
    pub status: u64,
    pub result: u64,
    pub cancelled: Option<CancelPoint>,
    pub parks: u64,
}

/// A task whose owner was retained instead of dropped on a fault.
#[derive(Debug)]
pub struct RetainedTask {
    pub reason: String,
    pub task: NativeTask,
}

#[derive(Default)]
struct ScopeState {
    cancelled: bool,
    live: BTreeSet<u64>,
    records: Vec<TaskRecord>,
    retained: Vec<RetainedTask>,
    /// Tasks dropped while still owning a live fiber (a driver or caller
    /// discarded them). Their owners are gone; this only reports it.
    dropped: Vec<u64>,
}

type Scope = Rc<RefCell<ScopeState>>;

/// A scheduled native fiber plus the resources its operations use.
pub struct NativeTask {
    /// `None` only transiently inside `resume`, or after `into_owner`.
    state: Option<State>,
    file: Option<File>,
    read_offset: u64,
    cancel: Option<CancelPoint>,
    scope: Option<Scope>,
}

impl std::fmt::Debug for NativeTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeTask")
            .field("fiber_id", &self.fiber_id())
            .field("cancelled", &self.cancel)
            .finish_non_exhaustive()
    }
}

impl Drop for NativeTask {
    fn drop(&mut self) {
        if self.state.is_some()
            && let Some(scope) = &self.scope
            && let Ok(mut scope) = scope.try_borrow_mut()
        {
            let id = self.fiber_id();
            scope.live.remove(&id);
            scope.dropped.push(id);
        }
        // The fiber's own Drop then runs its best-effort fallback.
    }
}

impl NativeTask {
    pub fn new(fiber: Fiber, file: Option<File>) -> Self {
        Self {
            state: Some(State::Fresh(fiber)),
            file,
            read_offset: 0,
            cancel: None,
            scope: None,
        }
    }
    fn scoped(fiber: Fiber, file: Option<File>, scope: &Scope) -> Self {
        let mut task = Self::new(fiber, file);
        scope.borrow_mut().live.insert(task.fiber_id());
        task.scope = Some(scope.clone());
        task
    }
    pub fn fiber_id(&self) -> u64 {
        match &self.state {
            Some(State::Fresh(f)) => f.id(),
            Some(State::Parked(k)) => k.fiber_id(),
            None => 0,
        }
    }
    /// Where this task's cancellation was delivered, if it was.
    pub fn cancelled_at(&self) -> Option<CancelPoint> {
        self.cancel
    }
    /// The suspended continuation, if parked (for relocation in tests).
    pub fn parked_mut(&mut self) -> Option<&mut Parked> {
        match &mut self.state {
            Some(State::Parked(k)) => Some(k),
            _ => None,
        }
    }
    /// Take the live owner and the task's file out of the task. `None` only
    /// for a task that no longer owns a fiber.
    pub fn into_owner(mut self) -> Option<(TaskOwner, Option<File>)> {
        let owner = match self.state.take()? {
            State::Fresh(f) => TaskOwner::Created(f),
            State::Parked(k) => TaskOwner::Parked(k),
        };
        Some((owner, self.file.take()))
    }
    fn scope_cancelled(&self) -> bool {
        self.scope.as_ref().is_some_and(|s| s.borrow().cancelled)
    }
    fn record_exit(&self, e: &Exit) {
        if let Some(scope) = &self.scope {
            let mut scope = scope.borrow_mut();
            scope.live.remove(&e.fiber_id);
            scope.records.push(TaskRecord {
                fiber_id: e.fiber_id,
                status: e.status,
                result: e.result,
                cancelled: self.cancel,
                parks: e.parks,
            });
        }
    }
    /// Report a fault without losing the owner when a scope can hold it.
    fn fault(mut self, state: State, reason: String) -> Step<Self> {
        self.state = Some(state);
        let id = self.fiber_id();
        match self.scope.take() {
            Some(scope) => {
                let mut s = scope.borrow_mut();
                s.live.remove(&id);
                s.retained.push(RetainedTask {
                    reason: reason.clone(),
                    task: self,
                });
                Step::Failed(format!("native fiber {id}: {reason} (owner retained)"))
            }
            None => Step::Failed(format!("native fiber {id}: {reason}")),
        }
    }
    fn deliver_cancel(&mut self, k: Parked, at: CancelPoint) -> Outcome {
        self.cancel = Some(at);
        k.cancel()
    }
}

/// Completion payload: `[status, result (8 bytes LE), data frame image...]`.
pub fn encode_exit(e: &Exit) -> Vec<u8> {
    let mut out = Vec::with_capacity(9 + e.data.len());
    out.push(e.status as u8);
    out.extend_from_slice(&e.result.to_le_bytes());
    out.extend_from_slice(&e.data);
    out
}

/// Decoded completion payload.
#[derive(Debug, PartialEq, Eq)]
pub struct Completed {
    pub status: u64,
    pub result: u64,
    pub data: Vec<u8>,
}

impl Completed {
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 9 {
            return None;
        }
        Some(Self {
            status: u64::from(bytes[0]),
            result: u64::from_le_bytes(bytes[1..9].try_into().ok()?),
            data: bytes[9..].to_vec(),
        })
    }
    pub fn word(&self, index: usize) -> u64 {
        u64::from_le_bytes(self.data[index * 8..index * 8 + 8].try_into().unwrap())
    }
}

/// Destination range of a read inside the data frame.
fn destination(k: &Parked, len: usize) -> Result<std::ops::Range<usize>, u64> {
    let dest = usize::try_from(k.request().arg1).map_err(|_| EFAULT)?;
    let end = dest.checked_add(len).ok_or(EFAULT)?;
    if end > k.data().len() {
        return Err(EFAULT);
    }
    Ok(dest..end)
}

fn deliver(k: &mut Parked, bytes: &[u8]) -> Result<(), u64> {
    let range = destination(k, bytes.len())?;
    k.data_mut()[range].copy_from_slice(bytes);
    Ok(())
}

impl NativeTask {
    /// Resume, or hand the rejected owner to `fault`.
    fn resume_or_fault(self, k: Parked, r: Resumption) -> Step<Self> {
        match k.resume(r) {
            Ok(outcome) => self.step(outcome),
            Err(rejected) => {
                let reason = format!("native resume rejected: {}", rejected.error);
                self.fault(State::Parked(rejected.continuation), reason)
            }
        }
    }
    /// Turn an outcome into a scheduler step, resolving operations that need
    /// no scheduler round trip.
    fn step(mut self, mut outcome: Outcome) -> Step<Self> {
        loop {
            let k = match outcome {
                Outcome::Exited(e) => {
                    self.record_exit(&e);
                    return match e.status {
                        exit::COMPLETE | exit::CANCELLED => Step::Complete(encode_exit(&e)),
                        exit::FAILED => Step::Failed(format!(
                            "native fiber {} failed with code {}",
                            e.fiber_id, e.result
                        )),
                        other => Step::Failed(format!(
                            "native fiber {} violated the ABI (status {other})",
                            e.fiber_id
                        )),
                    };
                }
                Outcome::Parked(k) => k,
            };
            let request = k.request();
            let immediate = match request.op {
                OP_YIELD => {
                    self.state = Some(State::Parked(k));
                    return Step::Yield(self);
                }
                OP_READ if request.arg0 > MAX_READ => Resumption::error(request.evidence, EINVAL),
                OP_READ => match self.file.take() {
                    Some(file) => {
                        let len = request.arg0 as usize;
                        if let Err(code) = destination(&k, len) {
                            // Reject the destination before submitting I/O.
                            self.file = Some(file);
                            Resumption::error(request.evidence, code)
                        } else {
                            let offset = self.read_offset;
                            self.state = Some(State::Parked(k));
                            return Step::Io(Request {
                                file,
                                buffer: vec![0; len],
                                continuation: self,
                                operation: Operation::Read,
                                offset,
                            });
                        }
                    }
                    None => Resumption::error(request.evidence, EBADF),
                },
                _ => Resumption::error(request.evidence, ENOSYS),
            };
            outcome = match k.resume(immediate) {
                Ok(o) => o,
                Err(rejected) => {
                    let reason = format!("native resume rejected: {}", rejected.error);
                    return self.fault(State::Parked(rejected.continuation), reason);
                }
            };
        }
    }
    fn io_resumption(
        &mut self,
        k: &mut Parked,
        io: vesper_pulse::scheduler::IoEvent,
    ) -> Resumption {
        self.file = Some(io.file);
        let evidence = k.request().evidence;
        match io.result {
            Ok(n) => {
                // A Driver is an ordinary safe trait implementation. Do not
                // turn a malformed completion into a successful read with
                // more bytes than were requested/delivered.
                if n > io.buffer.len() || n as u64 > k.request().arg0 {
                    Resumption::error(evidence, EIO)
                } else if let Some(offset) = self.read_offset.checked_add(n as u64) {
                    match deliver(k, &io.buffer[..n]) {
                        Ok(()) => {
                            self.read_offset = offset;
                            Resumption::value(evidence, n as u64)
                        }
                        Err(code) => Resumption::error(evidence, code),
                    }
                } else {
                    Resumption::error(evidence, EIO)
                }
            }
            Err(e) => Resumption::error(evidence, e.raw_os_error().map_or(EIO, |c| c as u64)),
        }
    }
}

impl Resume for NativeTask {
    fn resume(mut self, event: Event) -> Step<Self> {
        let Some(state) = self.state.take() else {
            return Step::Failed("native task resumed without a fiber".into());
        };
        // Unwrap cancellation wrappers; the interrupted event still carries
        // the owned file/buffer of a completed request.
        let mut event = event;
        let mut requested = false;
        while let Event::Cancelled(inner) = event {
            requested = true;
            event = *inner;
        }
        let cancel = (requested || self.scope_cancelled()) && self.cancel.is_none();
        match (state, event) {
            (State::Fresh(fiber), Event::Start) if cancel => {
                self.cancel = Some(CancelPoint::BeforeStart);
                let e = fiber.cancel();
                self.record_exit(&e);
                Step::Complete(encode_exit(&e))
            }
            (State::Fresh(fiber), Event::Start) => {
                let outcome = fiber.start();
                self.step(outcome)
            }
            (State::Fresh(fiber), other) => {
                if let Event::Io(io) = other {
                    self.file = Some(io.file);
                }
                self.fault(State::Fresh(fiber), "resumed before start".into())
            }
            (State::Parked(k), Event::Start) => {
                self.fault(State::Parked(k), "started twice".into())
            }
            (State::Parked(k), Event::Yield) if cancel => {
                let outcome = self.deliver_cancel(k, CancelPoint::AfterYield);
                self.step(outcome)
            }
            (State::Parked(k), Event::Yield) => {
                let r = Resumption::value(k.request().evidence, 0);
                self.resume_or_fault(k, r)
            }
            (State::Parked(k), Event::Io(io)) if cancel => {
                // The original completion returned the request's owners;
                // only now may the cancellation edge run.
                self.file = Some(io.file);
                drop(io.buffer);
                let outcome = self.deliver_cancel(k, CancelPoint::DuringIo);
                self.step(outcome)
            }
            (State::Parked(mut k), Event::Io(io)) => {
                let r = self.io_resumption(&mut k, io);
                self.resume_or_fault(k, r)
            }
            (State::Parked(_), Event::Cancelled(_)) => unreachable!("unwrapped above"),
        }
    }
}

/// Why a drain or shutdown stopped before every owner was resolved. The
/// runtime, and with it every pending owner, is returned in
/// [`IncompleteShutdown::runtime`].
#[derive(Debug)]
pub enum Incomplete {
    /// The tick budget ran out. `pending` lists the live fiber ids.
    BudgetExhausted { pending: Vec<u64> },
    /// The driver failed to cancel or poll. Harvested completions were
    /// already queued by the scheduler; nothing was dropped.
    Driver(io::Error),
    /// Faulted tasks are held in the runtime; take them with
    /// [`NativeRuntime::take_retained`] and resolve them explicitly.
    Retained { fiber_ids: Vec<u64> },
    /// The scheduler is idle but these fibers never exited: their task is
    /// still held by something that does not report as pending (for
    /// example a driver whose `pending` undercounts).
    Stalled { pending: Vec<u64> },
    /// Tasks were dropped while owning a live fiber (for example by a
    /// driver discarding a request). Their owners are gone; only their
    /// best-effort drop cancellation ran.
    Lost { fiber_ids: Vec<u64> },
}

impl std::fmt::Display for Incomplete {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Incomplete::BudgetExhausted { pending } => {
                write!(
                    f,
                    "drain budget exhausted with {} live fibers",
                    pending.len()
                )
            }
            Incomplete::Driver(e) => write!(f, "driver error during drain: {e}"),
            Incomplete::Retained { fiber_ids } => {
                write!(f, "{} faulted owners retained", fiber_ids.len())
            }
            Incomplete::Stalled { pending } => {
                write!(f, "scheduler idle with {} unfinished fibers", pending.len())
            }
            Incomplete::Lost { fiber_ids } => {
                write!(f, "{} live tasks were dropped", fiber_ids.len())
            }
        }
    }
}

/// A drain/shutdown that could not finish. Nothing was released.
pub struct IncompleteShutdown<D> {
    pub reason: Incomplete,
    pub runtime: NativeRuntime<D>,
}

impl<D> std::fmt::Debug for IncompleteShutdown<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IncompleteShutdown")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

/// Every task resolved. The driver is handed back (still owning nothing the
/// runtime tracks); call its own shutdown if it has one.
pub struct Drained<D> {
    pub driver: D,
    pub completed: Vec<Completed>,
    pub failures: Vec<String>,
    /// One record per task, in exit order.
    pub records: Vec<TaskRecord>,
}

impl<D> std::fmt::Debug for Drained<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Drained")
            .field("completed", &self.completed)
            .field("failures", &self.failures)
            .field("records", &self.records)
            .finish_non_exhaustive()
    }
}

/// A [`Scheduler`] of native tasks with an explicit, owned lifecycle.
///
/// * [`Self::cancel`] marks the scope cancelled and cancels the scheduler
///   (queued events are wrapped, the driver is told to cancel in-flight
///   requests). Each task then receives its one cancellation as described
///   in the module docs.
/// * [`Self::drain`] runs at most `budget` ticks. On success every task has
///   exited; otherwise [`IncompleteShutdown`] returns the runtime with every
///   pending owner so the caller can drain again, retry the driver, or
///   recover retained tasks.
/// * Dropping a runtime instead is the unrecoverable fallback: queued tasks
///   and the driver are dropped, each live fiber gets its best-effort drop
///   cancellation, and `IoDriver`'s own `Drop` keeps kernel-visible buffers
///   alive until their completions.
///
/// The scheduler and its interface are used unchanged.
pub struct NativeRuntime<D> {
    scheduler: Scheduler<NativeTask, D>,
    scope: Scope,
    completed: Vec<Completed>,
    failures: Vec<String>,
}

impl<D> std::fmt::Debug for NativeRuntime<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let scope = self.scope.borrow();
        f.debug_struct("NativeRuntime")
            .field("cancelled", &scope.cancelled)
            .field("live", &scope.live)
            .field("retained", &scope.retained.len())
            .finish_non_exhaustive()
    }
}

impl<D: Driver<NativeTask>> NativeRuntime<D> {
    pub fn new(driver: D) -> Self {
        Self {
            scheduler: Scheduler::new(driver),
            scope: Rc::default(),
            completed: vec![],
            failures: vec![],
        }
    }
    /// Schedule a fiber. Returns its id. A fiber spawned after
    /// [`Self::cancel`] is cancelled before it starts.
    pub fn spawn(&mut self, fiber: Fiber, file: Option<File>) -> u64 {
        let task = NativeTask::scoped(fiber, file, &self.scope);
        let id = task.fiber_id();
        self.scheduler.spawn(task);
        id
    }
    pub fn scheduler(&mut self) -> &mut Scheduler<NativeTask, D> {
        &mut self.scheduler
    }
    pub fn driver(&mut self) -> &mut D {
        self.scheduler.driver()
    }
    /// Run one scheduler tick and collect its results.
    pub fn tick(&mut self, wait: bool) -> io::Result<bool> {
        let ran = self.scheduler.tick(wait);
        self.collect();
        ran
    }
    fn collect(&mut self) {
        while let Some(bytes) = self.scheduler.take_completed() {
            // encode_exit always produces at least 9 bytes.
            self.completed
                .push(Completed::decode(&bytes).expect("adapter-encoded completion"));
        }
        while let Some(f) = self.scheduler.take_failure() {
            self.failures.push(f);
        }
    }
    pub fn is_cancelled(&self) -> bool {
        self.scope.borrow().cancelled
    }
    /// Fiber ids that have not exited and are still owned by this runtime.
    pub fn live(&self) -> Vec<u64> {
        self.scope.borrow().live.iter().copied().collect()
    }
    /// Records of tasks that exited so far.
    pub fn records(&self) -> Vec<TaskRecord> {
        self.scope.borrow().records.clone()
    }
    /// Recover tasks whose owners were retained after a fault.
    pub fn take_retained(&mut self) -> Vec<RetainedTask> {
        std::mem::take(&mut self.scope.borrow_mut().retained)
    }
    /// Cancel every task (idempotent per task: each receives at most one
    /// cancellation). A driver error is returned with the runtime intact.
    pub fn cancel(&mut self) -> io::Result<()> {
        self.scope.borrow_mut().cancelled = true;
        self.scheduler.cancel_all()
    }
    /// Run at most `budget` ticks until every task has exited.
    pub fn drain(
        mut self,
        budget: usize,
        wait: bool,
    ) -> Result<Drained<D>, Box<IncompleteShutdown<D>>> {
        let mut ticks = 0;
        while !self.scheduler.is_idle() {
            if ticks == budget {
                let pending = self.live();
                return Err(self.incomplete(Incomplete::BudgetExhausted { pending }));
            }
            ticks += 1;
            if let Err(e) = self.tick(wait) {
                return Err(self.incomplete(Incomplete::Driver(e)));
            }
        }
        self.collect();
        let (retained, dropped, live) = {
            let s = self.scope.borrow();
            let retained: Vec<u64> = s.retained.iter().map(|r| r.task.fiber_id()).collect();
            (retained, s.dropped.clone(), self.live())
        };
        if !dropped.is_empty() {
            return Err(self.incomplete(Incomplete::Lost { fiber_ids: dropped }));
        }
        if !retained.is_empty() {
            return Err(self.incomplete(Incomplete::Retained {
                fiber_ids: retained,
            }));
        }
        if !live.is_empty() {
            return Err(self.incomplete(Incomplete::Stalled { pending: live }));
        }
        let records = std::mem::take(&mut self.scope.borrow_mut().records);
        Ok(Drained {
            driver: self.scheduler.into_parts().driver,
            completed: self.completed,
            failures: self.failures,
            records,
        })
    }
    /// [`Self::cancel`] then [`Self::drain`].
    pub fn shutdown(
        mut self,
        budget: usize,
        wait: bool,
    ) -> Result<Drained<D>, Box<IncompleteShutdown<D>>> {
        if let Err(e) = self.cancel() {
            return Err(self.incomplete(Incomplete::Driver(e)));
        }
        self.drain(budget, wait)
    }
    fn incomplete(self, reason: Incomplete) -> Box<IncompleteShutdown<D>> {
        Box::new(IncompleteShutdown {
            reason,
            runtime: self,
        })
    }
}
