//! A multi-core work-stealing fiber pool (§12.4).
//!
//! A [`Pool`] runs `workers` scheduler loops. Each worker owns its own
//! completion driver (built on the worker's thread by the caller's factory,
//! so the driver type need not be `Send`), a FIFO run queue that other
//! workers may steal from, a pinned queue that is never stolen (the
//! `spawn_local` half of §12.4) and a cache-hot LIFO slot for fibers spawned
//! from inside a running fiber. Fibers that nobody spawned from a worker go
//! to a shared injector queue.
//!
//! * A fiber's I/O request is submitted to the driver of the worker that ran
//!   it; when it completes, the continuation is queued on that worker, where
//!   an idle sibling can steal it. Requests never move between drivers.
//! * An idle worker steals half of a victim's run queue. Victims are visited
//!   in an order drawn from a per-worker SplitMix64 stream seeded from
//!   [`PoolConfig::seed`].
//! * [`Pool::run`] runs one OS thread per worker. A worker with nothing to
//!   run parks: in its driver (`poll(true)`) when it owns in-flight fiber
//!   I/O, otherwise on a condition variable. Siblings that publish work wake
//!   a parked worker; a worker parked in its driver is woken through a wake
//!   pipe whose read is kept pending in that driver, so wake-ups use the same
//!   completion protocol as every other request.
//! * [`Pool::run_seeded`] runs the same workers interleaved on the calling
//!   thread, choosing which worker steps next from the seed. With a
//!   deterministic driver the whole schedule (who ran which fiber, who stole
//!   from whom) is a function of the seed and the spawned fibers, and is
//!   returned as a [`TraceEntry`] list so a run can be replayed and compared.
//!   With an OS driver, completion timing is the only nondeterminism left.
//!
//! Every continuation is resumed exactly once per event; the pool never
//! drops one silently. Cancellation ([`Spawner::cancel`]) wraps every queued
//! event, and every event delivered after it, in [`Event::Cancelled`], and
//! cancels in-flight I/O in every driver. A driver that keeps failing makes
//! its worker give up; the fibers it still owns are counted in
//! [`PoolOutput::abandoned`] (and by the driver's own `Drop`), and runnable
//! fibers that could no longer run are returned in [`RunError::stranded`].
//!
//! Rust ownership here is runtime evidence only; it is not an implementation
//! of the Vesper source typing rules for portable fibers.
use crate::driver::{CompletionDriver, Request};
use crate::scheduler::{Event, IoEvent, Resume, Step};
use std::{
    cell::Cell,
    collections::VecDeque,
    fmt,
    fs::File,
    io::{self, Write},
    panic::{self, AssertUnwindSafe},
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering::SeqCst},
    },
    time::Duration,
};

/// The identity of one spawned fiber, unique within its pool. IDs are
/// assigned in spawn order, starting at 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FiberId(u64);

impl FiberId {
    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for FiberId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "fiber {}", self.0)
    }
}

/// The continuation type a pool's drivers carry: a fiber, or the pool's own
/// wake-up read. Opaque; name it as the driver's type parameter
/// (`AnyDriver<Task<C>>`).
pub struct Task<C>(TaskKind<C>);

enum TaskKind<C> {
    Fiber {
        id: FiberId,
        home: Option<usize>,
        continuation: C,
    },
    Wake,
}

/// Pool shape and policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolConfig {
    /// Scheduler loops (OS threads under [`Pool::run`]). At least 1.
    pub workers: usize,
    /// Seeds the victim order of every worker and, under
    /// [`Pool::run_seeded`], the interleaving of workers.
    pub seed: u64,
    /// Run fibers spawned from inside a fiber next on the same worker.
    pub lifo_slot: bool,
    /// Keep a wake-up read pending in every driver so a worker can block in
    /// `poll(true)` and still be woken for new work. Disable for drivers
    /// that cannot read a pipe (test doubles); workers with in-flight I/O
    /// then poll every millisecond instead.
    pub driver_wakeup: bool,
}

/// Upper bound on [`PoolConfig::workers`].
pub const MAX_WORKERS: usize = 1024;
/// A worker gives up after this many consecutive failed polls.
pub const MAX_POLL_FAILURES: u32 = 3;
/// Consecutive LIFO-slot runs before the worker serves its queues again.
const MAX_LIFO_STREAK: u32 = 3;

impl PoolConfig {
    pub fn new(workers: usize) -> Self {
        Self {
            workers,
            seed: 0,
            lifo_slot: true,
            driver_wakeup: true,
        }
    }
    pub fn seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }
    pub fn lifo_slot(mut self, on: bool) -> Self {
        self.lifo_slot = on;
        self
    }
    pub fn driver_wakeup(mut self, on: bool) -> Self {
        self.driver_wakeup = on;
        self
    }
}

impl Default for PoolConfig {
    /// One worker per available core.
    fn default() -> Self {
        Self::new(std::thread::available_parallelism().map_or(1, |n| n.get()))
    }
}

/// What a worker did. `stolen` counts fibers this worker took from others.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkerStats {
    pub resumed: u64,
    pub stolen: u64,
    pub steal_attempts: u64,
    pub submitted: u64,
    pub parked: u64,
}

/// What resumed a fiber, in a [`TraceEntry`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceEvent {
    Start,
    Yield,
    Io,
    Cancelled,
}

/// One resumption under [`Pool::run_seeded`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceEntry {
    /// Position in the run's global resumption order, from 0.
    pub step: u64,
    pub worker: usize,
    pub fiber: FiberId,
    pub event: TraceEvent,
    /// The worker this fiber was stolen from just before running.
    pub stolen_from: Option<usize>,
}

/// The result of a finished run.
#[derive(Debug, Default)]
pub struct PoolOutput {
    /// Completed fibers. Under [`Pool::run`] sorted by fiber ID; under
    /// [`Pool::run_seeded`] in completion order.
    pub completed: Vec<(FiberId, Vec<u8>)>,
    pub failures: Vec<(FiberId, String)>,
    pub workers: Vec<WorkerStats>,
    /// Resumptions in order (only under [`Pool::run_seeded`]).
    pub trace: Vec<TraceEntry>,
    /// Fibers lost inside a driver that failed beyond recovery.
    pub abandoned: usize,
}

#[derive(Debug)]
pub enum PoolError {
    /// `workers` is zero or above [`MAX_WORKERS`].
    InvalidWorkers(usize),
    /// `spawn_pinned` named a worker the pool does not have.
    NoSuchWorker(usize),
    /// The pool already finished; spawning is closed.
    Closed,
}

impl fmt::Display for PoolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PoolError::InvalidWorkers(n) => {
                write!(f, "a pool needs 1..={MAX_WORKERS} workers, not {n}")
            }
            PoolError::NoSuchWorker(w) => write!(f, "the pool has no worker {w}"),
            PoolError::Closed => write!(f, "the pool has finished; spawning is closed"),
        }
    }
}

impl std::error::Error for PoolError {}

/// A runnable fiber that could not run because its worker's driver failed.
pub struct Stranded<C> {
    pub id: FiberId,
    pub continuation: C,
    pub event: Event,
}

/// A run that did not finish cleanly.
pub enum RunError<C> {
    /// A worker's driver or wake pipe could not be built. No fiber ran; the
    /// pool comes back with every spawned fiber still queued.
    Setup {
        worker: usize,
        error: io::Error,
        pool: Pool<C>,
    },
    /// A driver failed while fibers ran. The pool cancelled itself and ran
    /// every fiber it could to completion.
    Driver {
        worker: usize,
        error: io::Error,
        output: Box<PoolOutput>,
        stranded: Vec<Stranded<C>>,
    },
}

impl<C> fmt::Debug for RunError<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunError::Setup { worker, error, .. } => f
                .debug_struct("Setup")
                .field("worker", worker)
                .field("error", error)
                .finish_non_exhaustive(),
            RunError::Driver {
                worker,
                error,
                output,
                stranded,
            } => f
                .debug_struct("Driver")
                .field("worker", worker)
                .field("error", error)
                .field("output", output)
                .field("stranded", &stranded.len())
                .finish(),
        }
    }
}

impl<C> fmt::Display for RunError<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunError::Setup { worker, error, .. } => {
                write!(f, "pulse worker {worker} could not start: {error}")
            }
            RunError::Driver { worker, error, .. } => {
                write!(f, "pulse worker {worker}'s driver failed: {error}")
            }
        }
    }
}

struct Ready<C> {
    id: FiberId,
    home: Option<usize>,
    continuation: C,
    event: Event,
}

const RUNNING: u8 = 0;
const PARKED_CV: u8 = 1;
const PARKED_DRIVER: u8 = 2;

struct WorkerShared<C> {
    queue: Mutex<VecDeque<Ready<C>>>,
    pinned: Mutex<VecDeque<Ready<C>>>,
    lifo: Mutex<Option<Ready<C>>>,
    state: AtomicU8,
    wake: Mutex<Option<File>>,
    wake_armed: AtomicBool,
    /// Set (under the `pinned` lock) when this worker's driver failed.
    gone: AtomicBool,
}

struct Shared<C> {
    workers: Vec<WorkerShared<C>>,
    injector: Mutex<VecDeque<Ready<C>>>,
    lifo_slot: bool,
    live: AtomicUsize,
    active: AtomicUsize,
    next_id: AtomicU64,
    started: AtomicBool,
    done: AtomicBool,
    cancelled: AtomicBool,
    parked: AtomicUsize,
    sleep: Mutex<()>,
    cv: Condvar,
    first_error: Mutex<Option<(usize, io::Error)>>,
    trace_step: AtomicU64,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A poisoned lock only means a fiber panicked while the pool held it;
    // the queue itself is still consistent (every operation is one push or
    // pop), and the panic is re-raised by `run`.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

thread_local! {
    /// (pool identity, worker index) while a worker resumes a fiber.
    static CURRENT: Cell<Option<(usize, usize)>> = const { Cell::new(None) };
}

impl<C> Shared<C> {
    fn identity(self: &Arc<Self>) -> usize {
        Arc::as_ptr(self) as *const () as usize
    }
    fn current_worker(self: &Arc<Self>) -> Option<usize> {
        let me = self.identity();
        CURRENT
            .with(|c| c.get())
            .and_then(|(p, w)| (p == me).then_some(w))
    }
    /// Reserve a live slot; fails once a started pool has drained.
    fn reserve(&self) -> bool {
        let mut n = self.live.load(SeqCst);
        loop {
            if n == 0 && self.started.load(SeqCst) {
                return false;
            }
            match self.live.compare_exchange(n, n + 1, SeqCst, SeqCst) {
                Ok(_) => return true,
                Err(now) => n = now,
            }
        }
    }
    fn finish_one(&self) {
        self.finish(1);
    }
    fn finish(&self, n: usize) {
        if n > 0 && self.live.fetch_sub(n, SeqCst) == n {
            self.done.store(true, SeqCst);
            self.wake_all();
        }
    }
    fn push_injector(&self, ready: Ready<C>) {
        lock(&self.injector).push_back(ready);
        self.notify_one();
    }
    fn has_stealable(&self, me: usize) -> bool {
        if !lock(&self.injector).is_empty() {
            return true;
        }
        let _ = me;
        self.workers.iter().any(|w| !lock(&w.queue).is_empty())
    }
    fn has_own(&self, me: usize) -> bool {
        let w = &self.workers[me];
        lock(&w.lifo).is_some() || !lock(&w.pinned).is_empty() || !lock(&w.queue).is_empty()
    }
    fn wake_worker(&self, i: usize) {
        let w = &self.workers[i];
        match w.state.load(SeqCst) {
            PARKED_CV => {
                let _g = lock(&self.sleep);
                self.cv.notify_all();
            }
            PARKED_DRIVER => {
                if !w.wake_armed.swap(true, SeqCst)
                    && let Some(writer) = lock(&w.wake).as_mut()
                {
                    // One byte at most is outstanding per arm, so the pipe
                    // never fills; a failed write leaves the worker to its
                    // own in-flight I/O, which still completes.
                    let _ = writer.write(&[1]);
                }
            }
            _ => {}
        }
    }
    /// Wake one parked worker, if any, to pick up newly published work.
    fn notify_one(&self) {
        if self.parked.load(SeqCst) == 0 {
            return;
        }
        if let Some(i) = self
            .workers
            .iter()
            .position(|w| w.state.load(SeqCst) == PARKED_CV)
            .or_else(|| {
                self.workers
                    .iter()
                    .position(|w| w.state.load(SeqCst) == PARKED_DRIVER)
            })
        {
            self.wake_worker(i);
        }
    }
    fn wake_all(&self) {
        for i in 0..self.workers.len() {
            self.wake_worker(i);
        }
        let _g = lock(&self.sleep);
        self.cv.notify_all();
    }
    fn record_error(&self, worker: usize, error: io::Error) {
        let mut first = lock(&self.first_error);
        if first.is_none() {
            *first = Some((worker, error));
        }
        drop(first);
        self.cancel();
    }
    fn cancel(&self) {
        self.cancelled.store(true, SeqCst);
        self.wake_all();
    }
    fn drain_stranded(&self) -> Vec<Stranded<C>> {
        let mut out = vec![];
        let mut take = |q: &mut VecDeque<Ready<C>>| {
            out.extend(q.drain(..).map(|r| Stranded {
                id: r.id,
                continuation: r.continuation,
                event: r.event,
            }))
        };
        take(&mut lock(&self.injector));
        for w in &self.workers {
            take(&mut lock(&w.queue));
            take(&mut lock(&w.pinned));
            take(&mut lock(&w.lifo).take().into_iter().collect());
        }
        out.sort_by_key(|s| s.id);
        out
    }
}

impl<C> Drop for Shared<C> {
    /// A pool dropped without running (or a run that ended while fibers were
    /// still queued) discards their continuations: count them, like a
    /// dropped driver's, in [`crate::driver::abandoned_requests`].
    fn drop(&mut self) {
        let mut n = lock(&self.injector).len();
        for w in &self.workers {
            n +=
                lock(&w.queue).len() + lock(&w.pinned).len() + usize::from(lock(&w.lifo).is_some());
        }
        crate::driver::note_abandoned(n);
    }
}

/// A handle that spawns into, or cancels, a pool. Cloneable and `Send`, so a
/// running fiber can hold one.
///
/// A spawner holds the pool weakly, so a queued fiber that holds one does
/// not keep its own pool alive: once the pool (and its run) is gone, every
/// call reports [`PoolError::Closed`].
pub struct Spawner<C> {
    shared: std::sync::Weak<Shared<C>>,
}

impl<C> Clone for Spawner<C> {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
        }
    }
}

impl<C> Spawner<C> {
    /// Spawn a portable fiber. From inside a running fiber it goes to the
    /// current worker's LIFO slot (or run queue); otherwise to the injector.
    /// Fails, returning the fiber, once a started pool has finished.
    pub fn spawn(&self, continuation: C) -> Result<FiberId, (PoolError, C)> {
        let Some(shared) = self.shared.upgrade() else {
            return Err((PoolError::Closed, continuation));
        };
        let shared = &shared;
        if !shared.reserve() {
            return Err((PoolError::Closed, continuation));
        }
        let id = FiberId(shared.next_id.fetch_add(1, SeqCst));
        let ready = Ready {
            id,
            home: None,
            continuation,
            event: Event::Start,
        };
        match shared.current_worker() {
            Some(w) if shared.lifo_slot => {
                let worker = &shared.workers[w];
                let previous = lock(&worker.lifo).replace(ready);
                if let Some(previous) = previous {
                    lock(&worker.queue).push_back(previous);
                    shared.notify_one();
                }
            }
            Some(w) => {
                lock(&shared.workers[w].queue).push_back(ready);
                shared.notify_one();
            }
            None => shared.push_injector(ready),
        }
        Ok(id)
    }
    /// Spawn a fiber pinned to `worker`: it is never stolen, and all of its
    /// I/O goes through that worker's driver.
    pub fn spawn_pinned(&self, worker: usize, continuation: C) -> Result<FiberId, (PoolError, C)> {
        let Some(shared) = self.shared.upgrade() else {
            return Err((PoolError::Closed, continuation));
        };
        let shared = &shared;
        if worker >= shared.workers.len() {
            return Err((PoolError::NoSuchWorker(worker), continuation));
        }
        let mut pinned = lock(&shared.workers[worker].pinned);
        if shared.workers[worker].gone.load(SeqCst) || !shared.reserve() {
            return Err((PoolError::Closed, continuation));
        }
        let id = FiberId(shared.next_id.fetch_add(1, SeqCst));
        pinned.push_back(Ready {
            id,
            home: Some(worker),
            continuation,
            event: Event::Start,
        });
        drop(pinned);
        shared.wake_worker(worker);
        Ok(id)
    }
    /// Cancel every fiber: queued and later events arrive as
    /// [`Event::Cancelled`], and in-flight I/O is cancelled in every driver.
    pub fn cancel(&self) {
        if let Some(shared) = self.shared.upgrade() {
            shared.cancel();
        }
    }
    pub fn is_cancelled(&self) -> bool {
        self.shared
            .upgrade()
            .is_some_and(|s| s.cancelled.load(SeqCst))
    }
    /// The pool's worker count, or 0 once the pool is gone.
    pub fn workers(&self) -> usize {
        self.shared.upgrade().map_or(0, |s| s.workers.len())
    }
}

/// A pool that has not run yet. Spawn fibers, then [`run`](Pool::run) or
/// [`run_seeded`](Pool::run_seeded).
pub struct Pool<C> {
    shared: Arc<Shared<C>>,
    config: PoolConfig,
}

impl<C> Pool<C> {
    pub fn new(config: PoolConfig) -> Result<Self, PoolError> {
        if config.workers == 0 || config.workers > MAX_WORKERS {
            return Err(PoolError::InvalidWorkers(config.workers));
        }
        let workers = (0..config.workers)
            .map(|_| WorkerShared {
                queue: Mutex::new(VecDeque::new()),
                pinned: Mutex::new(VecDeque::new()),
                lifo: Mutex::new(None),
                state: AtomicU8::new(RUNNING),
                wake: Mutex::new(None),
                wake_armed: AtomicBool::new(false),
                gone: AtomicBool::new(false),
            })
            .collect();
        Ok(Self {
            shared: Arc::new(Shared {
                workers,
                injector: Mutex::new(VecDeque::new()),
                lifo_slot: config.lifo_slot,
                live: AtomicUsize::new(0),
                active: AtomicUsize::new(0),
                next_id: AtomicU64::new(1),
                started: AtomicBool::new(false),
                done: AtomicBool::new(false),
                cancelled: AtomicBool::new(false),
                parked: AtomicUsize::new(0),
                sleep: Mutex::new(()),
                cv: Condvar::new(),
                first_error: Mutex::new(None),
                trace_step: AtomicU64::new(0),
            }),
            config,
        })
    }
    pub fn config(&self) -> &PoolConfig {
        &self.config
    }
    pub fn spawner(&self) -> Spawner<C> {
        Spawner {
            shared: Arc::downgrade(&self.shared),
        }
    }
    pub fn spawn(&self, continuation: C) -> FiberId {
        match self.spawner().spawn(continuation) {
            Ok(id) => id,
            Err(_) => unreachable!("an unstarted pool always accepts fibers"),
        }
    }
    pub fn spawn_pinned(&self, worker: usize, continuation: C) -> Result<FiberId, (PoolError, C)> {
        self.spawner().spawn_pinned(worker, continuation)
    }
    pub fn cancel(&self) {
        self.shared.cancel();
    }
}

/// SplitMix64: a tiny, well-mixed, seedable stream.
struct Rng(u64);
impl Rng {
    fn new(seed: u64, stream: u64) -> Self {
        let mut r = Rng(seed ^ stream.wrapping_mul(0xA076_1D64_78BD_642F));
        r.next();
        r
    }
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

struct Worker<C, D> {
    index: usize,
    driver: Option<D>,
    rng: Rng,
    ticks: u64,
    lifo_streak: u32,
    /// The wake pipe's read end while no read is in flight.
    wake_reader: Option<File>,
    wake_inflight: bool,
    cancel_seen: bool,
    poll_failures: u32,
    gave_up: bool,
    stats: WorkerStats,
    completed: Vec<(FiberId, Vec<u8>)>,
    failures: Vec<(FiberId, String)>,
    trace: Option<Vec<TraceEntry>>,
    abandoned: usize,
    _fibers: std::marker::PhantomData<fn(C)>,
}

const WAKE_BUFFER: usize = 16;

struct WorkerResult {
    stats: WorkerStats,
    completed: Vec<(FiberId, Vec<u8>)>,
    failures: Vec<(FiberId, String)>,
    trace: Vec<TraceEntry>,
    abandoned: usize,
}

impl<C: Resume, D: CompletionDriver<Task<C>>> Worker<C, D> {
    fn new(index: usize, driver: D, seed: u64, trace: bool) -> Self {
        Self {
            index,
            driver: Some(driver),
            rng: Rng::new(seed, index as u64 + 1),
            ticks: 0,
            lifo_streak: 0,
            wake_reader: None,
            wake_inflight: false,
            cancel_seen: false,
            poll_failures: 0,
            gave_up: false,
            stats: WorkerStats::default(),
            completed: vec![],
            failures: vec![],
            trace: trace.then(Vec::new),
            abandoned: 0,
            _fibers: std::marker::PhantomData,
        }
    }
    fn driver(&mut self) -> &mut D {
        self.driver
            .as_mut()
            .expect("a running worker owns its driver")
    }
    /// In-flight fiber requests (the wake read excluded).
    fn fiber_io(&self) -> usize {
        let d = self
            .driver
            .as_ref()
            .expect("a running worker owns its driver");
        d.pending() - usize::from(self.wake_inflight)
    }
    fn observe_cancel(&mut self, shared: &Shared<C>) {
        if self.cancel_seen || !shared.cancelled.load(SeqCst) {
            return;
        }
        self.cancel_seen = true;
        // The wake read is cancelled too; it is simply re-armed.
        if let Err(e) = self.driver().cancel_all() {
            shared.record_error(self.index, e);
        }
    }
    fn arm_wake(&mut self, shared: &Shared<C>) {
        if self.wake_inflight {
            return;
        }
        let Some(file) = self.wake_reader.take() else {
            return;
        };
        let request = Request {
            file,
            buffer: vec![0; WAKE_BUFFER],
            continuation: Task(TaskKind::Wake),
            operation: crate::driver::Operation::Read,
            offset: 0,
        };
        match self.driver().submit(request) {
            Ok(_) => self.wake_inflight = true,
            Err((e, request)) => {
                self.wake_reader = Some(request.file);
                // A driver full of fiber I/O is not a failure: this park
                // falls back to timed polling and the wake read is retried
                // at the next park.
                if e.kind() != io::ErrorKind::WouldBlock {
                    shared.record_error(self.index, e);
                }
            }
        }
    }
    fn place(&self, shared: &Shared<C>, ready: Ready<C>) {
        match ready.home {
            Some(h) => lock(&shared.workers[h].pinned).push_back(ready),
            None => lock(&shared.workers[self.index].queue).push_back(ready),
        }
    }
    fn harvest(&mut self, shared: &Shared<C>, wait: bool) {
        if self.gave_up {
            return;
        }
        let batch = self.driver().poll(wait);
        let (events, error) = match batch {
            Ok(events) => (events, None),
            Err(e) => (e.completed, Some(e.error)),
        };
        let mut fibers = 0;
        for done in events {
            let Request {
                file,
                buffer,
                continuation: Task(kind),
                ..
            } = done.request;
            match kind {
                TaskKind::Wake => {
                    self.wake_inflight = false;
                    shared.workers[self.index].wake_armed.store(false, SeqCst);
                    if done.result.is_ok_and(|n| n == 0) {
                        // Every writer is gone (the pool is ending): never
                        // re-arm a read that would complete at once forever.
                        drop(file);
                    } else {
                        self.wake_reader = Some(file);
                    }
                }
                TaskKind::Fiber {
                    id,
                    home,
                    continuation,
                } => {
                    fibers += 1;
                    self.place(
                        shared,
                        Ready {
                            id,
                            home,
                            continuation,
                            event: Event::Io(IoEvent {
                                file,
                                buffer,
                                result: done.result,
                            }),
                        },
                    );
                }
            }
        }
        if fibers > 1 {
            shared.notify_one();
        }
        match error {
            None => self.poll_failures = 0,
            Some(e) => {
                self.poll_failures += 1;
                shared.record_error(self.index, e);
                if self.poll_failures >= MAX_POLL_FAILURES {
                    self.give_up(shared);
                }
            }
        }
    }
    /// The driver cannot make progress: hand runnable fibers to the others
    /// and count the ones the driver still owns as abandoned.
    fn give_up(&mut self, shared: &Shared<C>) {
        self.gave_up = true;
        let lost = self.fiber_io();
        self.abandoned += lost;
        let me = &shared.workers[self.index];
        let mut moved: Vec<Ready<C>> = lock(&me.queue).drain(..).collect();
        moved.extend(lock(&me.lifo).take());
        // Pinned fibers cannot move and nobody else may run them: they stay
        // queued, come back as stranded, and no longer count as live.
        let pinned = {
            let q = lock(&me.pinned);
            me.gone.store(true, SeqCst);
            q.len()
        };
        let others = shared.active.fetch_sub(1, SeqCst) > 1;
        if others {
            lock(&shared.injector).extend(moved);
            shared.finish(lost + pinned);
        } else {
            lock(&me.queue).extend(moved);
            // Nobody is left to run anything: end the run.
            shared.done.store(true, SeqCst);
        }
        shared.wake_all();
    }
    fn take_stolen(&mut self, shared: &Shared<C>) -> Option<(Ready<C>, Option<usize>)> {
        let n = shared.workers.len();
        if n < 2 {
            return None;
        }
        self.stats.steal_attempts += 1;
        let start = self.rng.below(n);
        for k in 0..n {
            let victim = (start + k) % n;
            if victim == self.index {
                continue;
            }
            let mut q = lock(&shared.workers[victim].queue);
            if q.is_empty() {
                continue;
            }
            let take = q.len().div_ceil(2);
            let mut taken: VecDeque<Ready<C>> = q.drain(..take).collect();
            drop(q);
            let first = taken.pop_front().expect("took at least one");
            self.stats.stolen += 1 + taken.len() as u64;
            if !taken.is_empty() {
                lock(&shared.workers[self.index].queue).extend(taken);
            }
            return Some((first, Some(victim)));
        }
        None
    }
    fn next_job(&mut self, shared: &Shared<C>) -> Option<(Ready<C>, Option<usize>)> {
        self.ticks += 1;
        let me = &shared.workers[self.index];
        // Every 61st pick serves the injector first, so external spawns are
        // never starved by a busy local queue.
        if self.ticks.is_multiple_of(61)
            && let Some(r) = lock(&shared.injector).pop_front()
        {
            return Some((r, None));
        }
        if self.lifo_streak < MAX_LIFO_STREAK
            && let Some(r) = lock(&me.lifo).take()
        {
            self.lifo_streak += 1;
            return Some((r, None));
        }
        self.lifo_streak = 0;
        let local = || lock(&me.queue).pop_front();
        let pinned = || lock(&me.pinned).pop_front();
        let own = if self.ticks.is_multiple_of(2) {
            pinned().or_else(local)
        } else {
            local().or_else(pinned)
        };
        if let Some(r) = own.or_else(|| lock(&me.lifo).take()) {
            return Some((r, None));
        }
        if let Some(r) = lock(&shared.injector).pop_front() {
            return Some((r, None));
        }
        self.take_stolen(shared)
    }
    fn run(&mut self, shared: &Arc<Shared<C>>, ready: Ready<C>, stolen_from: Option<usize>) {
        let Ready {
            id,
            home,
            continuation,
            mut event,
        } = ready;
        let cancelled = shared.cancelled.load(SeqCst);
        if cancelled && !matches!(event, Event::Cancelled(_)) {
            event = Event::Cancelled(Box::new(event));
        }
        if let Some(trace) = &mut self.trace {
            trace.push(TraceEntry {
                step: shared.trace_step.fetch_add(1, SeqCst),
                worker: self.index,
                fiber: id,
                event: match &event {
                    Event::Start => TraceEvent::Start,
                    Event::Yield => TraceEvent::Yield,
                    Event::Io(_) => TraceEvent::Io,
                    Event::Cancelled(_) => TraceEvent::Cancelled,
                },
                stolen_from,
            });
        }
        self.stats.resumed += 1;
        CURRENT.with(|c| c.set(Some((shared.identity(), self.index))));
        let step = continuation.resume(event);
        CURRENT.with(|c| c.set(None));
        match step {
            Step::Complete(value) => {
                self.completed.push((id, value));
                shared.finish_one();
            }
            Step::Failed(error) => {
                self.failures.push((id, error));
                shared.finish_one();
            }
            Step::Yield(next) => {
                self.place(
                    shared,
                    Ready {
                        id,
                        home,
                        continuation: next,
                        event: Event::Yield,
                    },
                );
                if lock(&shared.workers[self.index].queue).len() > 1 {
                    shared.notify_one();
                }
            }
            Step::Io(request) => {
                let Request {
                    file,
                    buffer,
                    continuation,
                    operation,
                    offset,
                } = request;
                let request = Request {
                    file,
                    buffer,
                    continuation: Task(TaskKind::Fiber {
                        id,
                        home,
                        continuation,
                    }),
                    operation,
                    offset,
                };
                self.stats.submitted += 1;
                match self.driver().submit(request) {
                    Ok(rid) => {
                        if shared.cancelled.load(SeqCst)
                            && let Err(e) = self.driver().cancel(rid)
                        {
                            shared.record_error(self.index, e);
                        }
                    }
                    Err((error, request)) => {
                        let Request {
                            file,
                            buffer,
                            continuation: Task(kind),
                            ..
                        } = request;
                        let TaskKind::Fiber { continuation, .. } = kind else {
                            unreachable!("a rejected fiber request carries its fiber")
                        };
                        self.place(
                            shared,
                            Ready {
                                id,
                                home,
                                continuation,
                                event: Event::Io(IoEvent {
                                    file,
                                    buffer,
                                    result: Err(error),
                                }),
                            },
                        );
                    }
                }
            }
        }
    }
    /// Run one fiber if this worker can find one. Never blocks.
    fn step(&mut self, shared: &Arc<Shared<C>>) -> bool {
        if self.gave_up {
            return false;
        }
        self.observe_cancel(shared);
        if self.driver().pending() > 0 {
            self.harvest(shared, false);
            // Three failed polls in a row retire the worker: it must not
            // run anything after `give_up` handed its fibers on.
            if self.gave_up {
                return false;
            }
        }
        match self.next_job(shared) {
            Some((ready, from)) => {
                self.run(shared, ready, from);
                true
            }
            None => false,
        }
    }
    /// Park until there may be work. Returns after a wake-up, a completion or
    /// a timeout; the caller re-checks everything.
    fn park(&mut self, shared: &Shared<C>, driver_wakeup: bool) {
        self.stats.parked += 1;
        let me = &shared.workers[self.index];
        let io = self.fiber_io() > 0;
        if io && driver_wakeup {
            self.arm_wake(shared);
            if self.wake_inflight {
                me.state.store(PARKED_DRIVER, SeqCst);
                shared.parked.fetch_add(1, SeqCst);
                if !self.should_wake(shared) {
                    self.harvest(shared, true);
                }
                shared.parked.fetch_sub(1, SeqCst);
                me.state.store(RUNNING, SeqCst);
                return;
            }
        }
        let guard = lock(&shared.sleep);
        me.state.store(PARKED_CV, SeqCst);
        shared.parked.fetch_add(1, SeqCst);
        if !self.should_wake(shared) {
            // The timeout is a safety net only; wake-ups are explicit. With
            // in-flight I/O and no driver wake-up, it is the poll interval.
            let timeout = if io {
                Duration::from_millis(1)
            } else {
                Duration::from_millis(50)
            };
            let _ = shared.cv.wait_timeout(guard, timeout);
        } else {
            drop(guard);
        }
        shared.parked.fetch_sub(1, SeqCst);
        me.state.store(RUNNING, SeqCst);
    }
    fn should_wake(&self, shared: &Shared<C>) -> bool {
        shared.done.load(SeqCst)
            || (shared.cancelled.load(SeqCst) && !self.cancel_seen)
            || shared.has_own(self.index)
            || shared.has_stealable(self.index)
    }
    /// Release the driver (cancel the wake read, drain whatever is left)
    /// and keep only the results.
    fn close(mut self, shared: &Shared<C>) -> WorkerResult {
        *lock(&shared.workers[self.index].wake) = None;
        if let Some(driver) = self.driver.take() {
            self.release(driver);
        }
        WorkerResult {
            stats: std::mem::take(&mut self.stats),
            completed: std::mem::take(&mut self.completed),
            failures: std::mem::take(&mut self.failures),
            trace: self.trace.take().unwrap_or_default(),
            abandoned: self.abandoned,
        }
    }
    fn release(&mut self, driver: D) {
        if self.gave_up {
            drop(driver);
            return;
        }
        match driver.drain() {
            Ok(rest) => {
                // Only the wake read can be left after every fiber finished;
                // any fiber here was abandoned by an aborted run.
                self.abandoned += rest
                    .iter()
                    .filter(|c| matches!(c.request.continuation.0, TaskKind::Fiber { .. }))
                    .count();
            }
            Err(e) => {
                self.abandoned += e.driver.pending();
                drop(e);
            }
        }
    }
}

fn wake_pipe() -> io::Result<(File, File)> {
    #[cfg(unix)]
    {
        let (r, w) = io::pipe()?;
        Ok((
            File::from(std::os::fd::OwnedFd::from(r)),
            File::from(std::os::fd::OwnedFd::from(w)),
        ))
    }
    #[cfg(windows)]
    {
        crate::iocp::wake_pipe()
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no wake pipe on this OS",
        ))
    }
}

impl<C: Resume> Pool<C> {
    fn output(
        workers: Vec<WorkerResult>,
        sort: bool,
        order: Option<Vec<(FiberId, Vec<u8>)>>,
    ) -> PoolOutput {
        let mut out = PoolOutput::default();
        for mut w in workers {
            out.completed.append(&mut w.completed);
            out.failures.append(&mut w.failures);
            out.trace.append(&mut w.trace);
            out.abandoned += w.abandoned;
            out.workers.push(w.stats);
        }
        // Each worker records its own resumptions; restore the global order.
        out.trace.sort_by_key(|t| t.step);
        if let Some(order) = order {
            out.completed = order;
        }
        if sort {
            out.completed.sort_by_key(|(id, _)| *id);
            out.failures.sort_by_key(|(id, _)| *id);
        }
        out
    }
    fn finish_run(self, out: PoolOutput) -> Result<PoolOutput, RunError<C>> {
        let error = lock(&self.shared.first_error).take();
        let stranded = self.shared.drain_stranded();
        match error {
            None if stranded.is_empty() => Ok(out),
            None => Err(RunError::Driver {
                worker: 0,
                error: io::Error::other("fibers were left runnable with no worker"),
                output: Box::new(out),
                stranded,
            }),
            Some((worker, error)) => Err(RunError::Driver {
                worker,
                error,
                output: Box::new(out),
                stranded,
            }),
        }
    }

    /// Run every fiber to completion on `workers` OS threads. `factory(i)`
    /// builds worker `i`'s driver on that worker's thread.
    pub fn run<D, F>(self, factory: F) -> Result<PoolOutput, RunError<C>>
    where
        C: Send,
        D: CompletionDriver<Task<C>>,
        F: Fn(usize) -> io::Result<D> + Sync,
    {
        let n = self.config.workers;
        let shared = &self.shared;
        let wakeup = self.config.driver_wakeup;
        let seed = self.config.seed;
        let barrier = std::sync::Barrier::new(n);
        let setup: Mutex<Option<(usize, io::Error)>> = Mutex::new(None);
        shared.active.store(n, SeqCst);
        shared.started.store(true, SeqCst);
        if shared.live.load(SeqCst) == 0 {
            shared.done.store(true, SeqCst);
        }
        let results: Vec<_> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..n)
                .map(|i| {
                    let barrier = &barrier;
                    let setup = &setup;
                    let factory = &factory;
                    std::thread::Builder::new()
                        .name(format!("pulse-worker-{i}"))
                        .spawn_scoped(scope, move || {
                            // A panicking factory must still reach the
                            // barrier, or its siblings would wait forever.
                            let built = panic::catch_unwind(AssertUnwindSafe(|| {
                                factory(i).and_then(|driver| {
                                    let reader = if wakeup {
                                        let (r, w) = wake_pipe()?;
                                        *lock(&shared.workers[i].wake) = Some(w);
                                        Some(r)
                                    } else {
                                        None
                                    };
                                    Ok((driver, reader))
                                })
                            }));
                            let (built, panicked) = match built {
                                Ok(Ok(b)) => (Some(b), None),
                                Ok(Err(e)) => {
                                    lock(setup).get_or_insert((i, e));
                                    (None, None)
                                }
                                Err(payload) => {
                                    lock(setup).get_or_insert((
                                        i,
                                        io::Error::other("the driver factory panicked"),
                                    ));
                                    (None, Some(payload))
                                }
                            };
                            barrier.wait();
                            if lock(setup).is_some() {
                                *lock(&shared.workers[i].wake) = None;
                                drop(built);
                                if let Some(payload) = panicked {
                                    panic::resume_unwind(payload);
                                }
                                return None;
                            }
                            let (driver, reader) = built?;
                            let mut worker = Worker::new(i, driver, seed, false);
                            worker.wake_reader = reader;
                            let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
                                while !shared.done.load(SeqCst) && !worker.gave_up {
                                    if !worker.step(shared) && !shared.done.load(SeqCst) {
                                        worker.park(shared, wakeup);
                                    }
                                }
                            }));
                            if let Err(payload) = outcome {
                                shared.done.store(true, SeqCst);
                                shared.wake_all();
                                // Let the driver's own Drop protocol release
                                // what it still owns.
                                drop(worker);
                                panic::resume_unwind(payload);
                            }
                            Some(worker.close(shared))
                        })
                        .expect("spawn a pulse worker thread")
                })
                .collect();
            handles.into_iter().map(|h| h.join()).collect::<Vec<_>>()
        });
        let mut workers = vec![];
        let mut panicked = None;
        for r in results {
            match r {
                Ok(Some(w)) => workers.push(w),
                Ok(None) => {}
                Err(payload) => {
                    panicked.get_or_insert(payload);
                }
            }
        }
        if let Some(payload) = panicked {
            panic::resume_unwind(payload);
        }
        if let Some((worker, error)) = lock(&setup).take() {
            shared.started.store(false, SeqCst);
            shared.done.store(false, SeqCst);
            return Err(RunError::Setup {
                worker,
                error,
                pool: self,
            });
        }
        let out = Self::output(workers, true, None);
        self.finish_run(out)
    }

    /// Run every fiber to completion with all workers interleaved on this
    /// thread. Which worker steps next, and which victim a thief tries
    /// first, come from [`PoolConfig::seed`]; with a deterministic driver
    /// the returned trace and output are identical for identical seeds.
    pub fn run_seeded<D, F>(self, mut factory: F) -> Result<PoolOutput, RunError<C>>
    where
        D: CompletionDriver<Task<C>>,
        F: FnMut(usize) -> io::Result<D>,
    {
        let n = self.config.workers;
        let mut workers = Vec::with_capacity(n);
        for i in 0..n {
            match factory(i) {
                Ok(d) => workers.push(Worker::new(i, d, self.config.seed, true)),
                Err(error) => {
                    return Err(RunError::Setup {
                        worker: i,
                        error,
                        pool: self,
                    });
                }
            }
        }
        let shared = self.shared.clone();
        shared.active.store(n, SeqCst);
        shared.started.store(true, SeqCst);
        if shared.live.load(SeqCst) == 0 {
            shared.done.store(true, SeqCst);
        }
        let mut chooser = Rng::new(self.config.seed, 0);
        let mut order = vec![];
        let mut idle_rounds = 0usize;
        while !shared.done.load(SeqCst) {
            let w = chooser.below(n);
            let before = workers[w].completed.len();
            if workers[w].step(&shared) {
                idle_rounds = 0;
                if workers[w].completed.len() > before {
                    order.push(workers[w].completed.last().cloned().expect("pushed"));
                }
                continue;
            }
            idle_rounds += 1;
            if idle_rounds < n * 4 {
                continue;
            }
            idle_rounds = 0;
            // Nobody found a fiber in a full round: wait for I/O.
            let busy: Vec<usize> = (0..n)
                .filter(|&i| !workers[i].gave_up && workers[i].fiber_io() > 0)
                .collect();
            let runnable = (0..n).any(|i| shared.has_own(i)) || shared.has_stealable(0);
            if runnable {
                continue;
            }
            match busy.as_slice() {
                [] => {
                    // Live fibers but nothing runnable or in flight: only a
                    // worker that gave up can cause this; end the run.
                    break;
                }
                [only] => workers[*only].harvest(&shared, true),
                _ => {
                    for &i in &busy {
                        workers[i].harvest(&shared, false);
                    }
                    std::thread::sleep(Duration::from_micros(100));
                }
            }
        }
        let results = workers.into_iter().map(|w| w.close(&shared)).collect();
        let out = Self::output(results, false, Some(order));
        self.finish_run(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::{
        Completion, DriverKind, Operation, PollError, Rejected, RequestId, cancelled_error,
        is_cancelled,
    };
    use std::collections::BTreeMap;
    use std::sync::atomic::AtomicUsize;

    fn null_file() -> File {
        File::open(if cfg!(windows) { "NUL" } else { "/dev/null" }).unwrap()
    }

    /// A deterministic driver: each request completes on the `delay`-th poll
    /// after its submission; reads fill the buffer with `offset as u8`.
    struct LoopDriver<T> {
        polls: u64,
        delay: u64,
        next: u64,
        fail_polls: u32,
        queue: BTreeMap<RequestId, (u64, Request<T>, bool)>,
    }
    impl<T> LoopDriver<T> {
        fn new(delay: u64) -> Self {
            Self {
                polls: 0,
                delay,
                next: 1,
                fail_polls: 0,
                queue: BTreeMap::new(),
            }
        }
    }
    impl<T> CompletionDriver<T> for LoopDriver<T> {
        fn kind(&self) -> DriverKind {
            DriverKind::Epoll
        }
        fn pending(&self) -> usize {
            self.queue.len()
        }
        fn submit(&mut self, request: Request<T>) -> Result<RequestId, Rejected<T>> {
            let id = RequestId(self.next);
            self.next += 1;
            self.queue
                .insert(id, (self.polls + self.delay, request, false));
            Ok(id)
        }
        fn cancel(&mut self, id: RequestId) -> io::Result<bool> {
            Ok(match self.queue.get_mut(&id) {
                Some((due, _, c)) if !*c => {
                    *c = true;
                    *due = 0;
                    true
                }
                _ => false,
            })
        }
        fn cancel_all(&mut self) -> io::Result<()> {
            let ids: Vec<_> = self.queue.keys().copied().collect();
            for id in ids {
                self.cancel(id)?;
            }
            Ok(())
        }
        fn poll(&mut self, _wait: bool) -> Result<Vec<Completion<T>>, PollError<T>> {
            self.polls += 1;
            let due: Vec<_> = self
                .queue
                .iter()
                .filter(|(_, (d, _, _))| *d <= self.polls)
                .map(|(id, _)| *id)
                .collect();
            let completed = due
                .into_iter()
                .map(|id| {
                    let (_, mut request, cancelled) = self.queue.remove(&id).unwrap();
                    let result = if cancelled {
                        Err(cancelled_error())
                    } else {
                        if request.operation == Operation::Read {
                            let b = request.offset as u8;
                            request.buffer.iter_mut().for_each(|x| *x = b);
                        }
                        Ok(request.buffer.len())
                    };
                    Completion {
                        id,
                        request,
                        result,
                    }
                })
                .collect();
            if self.fail_polls > 0 {
                self.fail_polls -= 1;
                return Err(PollError {
                    error: io::Error::other("injected poll failure"),
                    completed,
                });
            }
            Ok(completed)
        }
    }

    /// Yields `yields` times, reads one byte at offset `tag`, then completes
    /// with `[tag, byte read]`. Optionally spawns `children` first.
    struct Fiber {
        tag: u8,
        yields: u32,
        stage: u32,
        children: u32,
        spawner: Option<Spawner<Fiber>>,
        cancelled_seen: u32,
    }
    impl Fiber {
        fn new(tag: u8, yields: u32) -> Self {
            Self {
                tag,
                yields,
                stage: 0,
                children: 0,
                spawner: None,
                cancelled_seen: 0,
            }
        }
    }
    impl Resume for Fiber {
        fn resume(mut self, event: Event) -> Step<Self> {
            if let Event::Cancelled(inner) = event {
                self.cancelled_seen += 1;
                assert_eq!(self.cancelled_seen, 1, "cancelled twice");
                let code = match *inner {
                    Event::Io(io) => {
                        assert!(io.result.is_err() || io.result.is_ok());
                        2
                    }
                    _ => 1,
                };
                return Step::Complete(vec![self.tag, code]);
            }
            if let Some(spawner) = &self.spawner {
                while self.children > 0 {
                    self.children -= 1;
                    let tag = self.tag.wrapping_add(self.children as u8 + 1);
                    spawner.spawn(Fiber::new(tag, 2)).map_err(|_| ()).unwrap();
                }
            }
            match event {
                Event::Start | Event::Yield if self.stage < self.yields => {
                    self.stage += 1;
                    Step::Yield(self)
                }
                Event::Start | Event::Yield => Step::Io(Request {
                    file: null_file(),
                    buffer: vec![0],
                    operation: Operation::Read,
                    offset: self.tag as u64,
                    continuation: self,
                }),
                Event::Io(io) => match io.result {
                    Ok(1) => Step::Complete(vec![self.tag, io.buffer[0]]),
                    other => Step::Failed(format!("read failed: {other:?}")),
                },
                Event::Cancelled(_) => unreachable!(),
            }
        }
    }

    fn seeded(seed: u64, workers: usize, fibers: u8) -> PoolOutput {
        let pool = Pool::new(PoolConfig::new(workers).seed(seed)).unwrap();
        let spawner = pool.spawner();
        for t in 0..fibers {
            let mut f = Fiber::new(t, (t % 4) as u32);
            if t % 8 == 0 {
                f.children = 3;
                f.spawner = Some(spawner.clone());
            }
            pool.spawn(f);
        }
        pool.run_seeded(|_| Ok(LoopDriver::new(2))).unwrap()
    }

    #[test]
    fn seeded_runs_replay_identically_and_steal() {
        let a = seeded(7, 4, 64);
        let b = seeded(7, 4, 64);
        assert_eq!(a.trace, b.trace);
        assert_eq!(a.completed, b.completed);
        assert_eq!(a.workers, b.workers);
        // 64 spawned + 3 children for every 8th fiber.
        assert_eq!(a.completed.len(), 64 + 8 * 3);
        assert!(a.failures.is_empty());
        for (_, v) in &a.completed {
            assert_eq!(v[0], v[1], "each fiber read its own offset");
        }
        assert!(a.trace.iter().any(|t| t.stolen_from.is_some()));
        assert!(a.workers.iter().all(|w| w.resumed > 0));
        let c = seeded(8, 4, 64);
        assert_ne!(
            a.trace, c.trace,
            "a different seed gives a different schedule"
        );
        let mut x: Vec<_> = a.completed.clone();
        let mut y: Vec<_> = c.completed.clone();
        x.sort();
        y.sort();
        assert_eq!(x, y, "the results do not depend on the schedule");
    }

    #[test]
    fn pinned_fibers_stay_home_and_children_use_the_lifo_slot() {
        let pool = Pool::new(PoolConfig::new(3).seed(3)).unwrap();
        let mut pinned = vec![];
        for w in 0..3 {
            for t in 0..4u8 {
                pinned.push((
                    pool.spawn_pinned(w, Fiber::new(100 + t, 3))
                        .map_err(|_| ())
                        .unwrap(),
                    w,
                ));
            }
        }
        let mut parent = Fiber::new(10, 0);
        parent.children = 2;
        parent.spawner = Some(pool.spawner());
        let parent_id = pool.spawn(parent);
        assert!(matches!(
            pool.spawn_pinned(3, Fiber::new(0, 0)),
            Err((PoolError::NoSuchWorker(3), _))
        ));
        let out = pool.run_seeded(|_| Ok(LoopDriver::new(1))).unwrap();
        for (id, home) in pinned {
            assert!(
                out.trace
                    .iter()
                    .filter(|t| t.fiber == id)
                    .all(|t| t.worker == home && t.stolen_from.is_none())
            );
        }
        // The parent's first run spawns two children; the last one spawned
        // is in the LIFO slot and runs next on the same worker.
        let first = out.trace.iter().position(|t| t.fiber == parent_id).unwrap();
        let worker = out.trace[first].worker;
        let next_here = out.trace[first + 1..]
            .iter()
            .find(|t| t.worker == worker)
            .unwrap();
        assert!(
            next_here.fiber > parent_id,
            "a child ran next: {next_here:?}"
        );
        assert_eq!(out.completed.len(), 12 + 3);
    }

    #[test]
    fn cancellation_reaches_queued_and_in_flight_fibers_once() {
        for threaded in [false, true] {
            let pool = Pool::new(PoolConfig::new(2).driver_wakeup(false)).unwrap();
            for t in 0..20 {
                pool.spawn(Fiber::new(t, 1));
            }
            pool.cancel();
            let out = if threaded {
                pool.run(|_| Ok(LoopDriver::new(3))).unwrap()
            } else {
                pool.run_seeded(|_| Ok(LoopDriver::new(3))).unwrap()
            };
            assert_eq!(out.completed.len(), 20);
            assert!(out.completed.iter().all(|(_, v)| v[1] == 1));
        }
        // Cancel while reads are in flight: they complete cancelled.
        struct Canceller(Spawner<Fiber2>, u32);
        enum Fiber2 {
            Reader(u8),
            Cancel(Canceller),
        }
        impl Resume for Fiber2 {
            fn resume(self, event: Event) -> Step<Self> {
                match (self, event) {
                    (Fiber2::Reader(t), Event::Start) => Step::Io(Request {
                        file: null_file(),
                        buffer: vec![0],
                        operation: Operation::Read,
                        offset: 0,
                        continuation: Fiber2::Reader(t),
                    }),
                    (Fiber2::Reader(t), Event::Cancelled(inner)) => match *inner {
                        Event::Io(io) => {
                            assert!(is_cancelled(io.result.as_ref().unwrap_err()));
                            assert_eq!(io.buffer.len(), 1);
                            Step::Complete(vec![t])
                        }
                        _ => Step::Failed("cancelled before reading".into()),
                    },
                    (Fiber2::Reader(_), _) => Step::Failed("read finished uncancelled".into()),
                    (Fiber2::Cancel(Canceller(s, n)), _) if n > 0 => {
                        Step::Yield(Fiber2::Cancel(Canceller(s, n - 1)))
                    }
                    (Fiber2::Cancel(Canceller(s, _)), _) => {
                        s.cancel();
                        Step::Complete(vec![255])
                    }
                }
            }
        }
        let pool = Pool::new(PoolConfig::new(3).seed(1)).unwrap();
        for t in 0..6 {
            pool.spawn(Fiber2::Reader(t));
        }
        pool.spawn(Fiber2::Cancel(Canceller(pool.spawner(), 4)));
        // The reads never finish on their own (delay is effectively infinite).
        let out = pool
            .run_seeded(|_| Ok(LoopDriver::new(u64::MAX / 2)))
            .unwrap();
        assert_eq!(out.completed.len(), 7, "{:?}", out.failures);
    }

    #[test]
    fn a_failing_driver_strands_nothing_silently() {
        let pool = Pool::new(PoolConfig::new(2).seed(5).driver_wakeup(false)).unwrap();
        for t in 0..10 {
            pool.spawn(Fiber::new(t, 0));
        }
        let err = pool
            .run_seeded(|i| {
                let mut d = LoopDriver::new(2);
                if i == 0 {
                    d.fail_polls = u32::MAX;
                }
                Ok(d)
            })
            .unwrap_err();
        let RunError::Driver {
            worker,
            output,
            stranded,
            ..
        } = err
        else {
            panic!("expected a driver error")
        };
        assert_eq!(worker, 0);
        // Every fiber is accounted for: finished (cancelled or not), lost in
        // the failed driver, or handed back.
        assert_eq!(
            output.completed.len() + output.failures.len() + output.abandoned + stranded.len(),
            10
        );
    }

    #[test]
    fn threaded_mock_pool_runs_every_fiber() {
        let pool = Pool::new(PoolConfig::new(4).driver_wakeup(false)).unwrap();
        let spawner = pool.spawner();
        for t in 0..200u8 {
            let mut f = Fiber::new(t, (t % 5) as u32);
            if t % 50 == 0 {
                f.children = 4;
                f.spawner = Some(spawner.clone());
            }
            pool.spawn(f);
        }
        let out = pool.run(|_| Ok(LoopDriver::new(1))).unwrap();
        assert_eq!(out.completed.len(), 200 + 16);
        assert!(out.completed.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(matches!(
            spawner.spawn(Fiber::new(0, 0)),
            Err((PoolError::Closed, _))
        ));
    }

    #[test]
    fn a_dropped_unrun_pool_counts_its_fibers() {
        let before = crate::driver::abandoned_requests();
        let pool = Pool::new(PoolConfig::new(2)).unwrap();
        pool.spawn(Fiber::new(1, 0));
        pool.spawn_pinned(1, Fiber::new(2, 0))
            .map_err(|_| ())
            .unwrap();
        drop(pool);
        assert!(crate::driver::abandoned_requests() >= before + 2);
    }

    #[test]
    fn seeded_traces_keep_the_global_resumption_order() {
        let pool = Pool::new(PoolConfig::new(2).seed(11)).unwrap();
        for w in 0..2 {
            pool.spawn_pinned(w, Fiber::new(w as u8, 3))
                .map_err(|_| ())
                .unwrap();
        }
        let out = pool.run_seeded(|_| Ok(LoopDriver::new(1))).unwrap();
        let steps: Vec<u64> = out.trace.iter().map(|t| t.step).collect();
        assert_eq!(steps, (0..steps.len() as u64).collect::<Vec<_>>());
        // Both workers ran, interleaved rather than grouped by worker.
        let workers: Vec<usize> = out.trace.iter().map(|t| t.worker).collect();
        let switches = workers.windows(2).filter(|w| w[0] != w[1]).count();
        assert!(switches > 1, "{workers:?}");
    }

    #[test]
    fn a_panicking_factory_does_not_hang_the_other_workers() {
        let pool = Pool::new(PoolConfig::new(3).driver_wakeup(false)).unwrap();
        pool.spawn(Fiber::new(1, 0));
        let r = panic::catch_unwind(AssertUnwindSafe(|| {
            pool.run(|i| {
                if i == 0 {
                    panic!("factory 0 panics");
                }
                Ok(LoopDriver::new(1))
            })
        }));
        assert!(r.is_err(), "the factory's panic propagates");
    }

    #[test]
    fn a_queued_fiber_holding_a_spawner_does_not_keep_its_pool_alive() {
        struct Guard(Arc<AtomicUsize>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.fetch_add(1, SeqCst);
            }
        }
        struct Holder(
            #[allow(dead_code)] Spawner<Holder>,
            #[allow(dead_code)] Guard,
        );
        impl Resume for Holder {
            fn resume(self, _: Event) -> Step<Self> {
                Step::Complete(vec![])
            }
        }
        let dropped = Arc::new(AtomicUsize::new(0));
        let pool = Pool::new(PoolConfig::new(1)).unwrap();
        pool.spawn(Holder(pool.spawner(), Guard(dropped.clone())));
        let spawner = pool.spawner();
        drop(pool);
        assert_eq!(dropped.load(SeqCst), 1, "the queued fiber was released");
        assert_eq!(spawner.workers(), 0);
        assert!(matches!(
            spawner.spawn(Holder(spawner.clone(), Guard(dropped.clone()))),
            Err((PoolError::Closed, _))
        ));
    }

    #[test]
    fn a_retired_worker_runs_nothing_after_giving_up() {
        // Worker 0 fails every poll while it owns I/O and has pinned work
        // queued; worker 1 must still finish every portable fiber.
        for seed in 0..32 {
            let pool = Pool::new(PoolConfig::new(2).seed(seed).driver_wakeup(false)).unwrap();
            for t in 0..4 {
                pool.spawn_pinned(0, Fiber::new(t, 0))
                    .map_err(|_| ())
                    .unwrap();
            }
            for t in 10..20 {
                pool.spawn(Fiber::new(t, 2));
            }
            let err = pool
                .run_seeded(|i| {
                    let mut d = LoopDriver::new(2);
                    if i == 0 {
                        d.fail_polls = u32::MAX;
                    }
                    Ok(d)
                })
                .unwrap_err();
            let RunError::Driver {
                output, stranded, ..
            } = err
            else {
                panic!("expected a driver error")
            };
            assert_eq!(
                output.completed.len() + output.failures.len() + output.abandoned + stranded.len(),
                14,
                "seed {seed}"
            );
            assert!(
                stranded.iter().all(|s| s.id.get() <= 4),
                "seed {seed}: only pinned fibers may strand: {:?}",
                stranded.iter().map(|s| s.id).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn configuration_and_setup_errors_are_typed() {
        assert!(matches!(
            Pool::<Fiber>::new(PoolConfig::new(0)),
            Err(PoolError::InvalidWorkers(0))
        ));
        assert!(matches!(
            Pool::<Fiber>::new(PoolConfig::new(MAX_WORKERS + 1)),
            Err(PoolError::InvalidWorkers(_))
        ));
        let pool = Pool::new(PoolConfig::new(2).driver_wakeup(false)).unwrap();
        pool.spawn(Fiber::new(1, 0));
        let err = pool
            .run(|i| {
                if i == 1 {
                    Err(io::Error::other("no driver"))
                } else {
                    Ok(LoopDriver::new(1))
                }
            })
            .unwrap_err();
        let RunError::Setup { worker, pool, .. } = err else {
            panic!("expected a setup error")
        };
        assert_eq!(worker, 1);
        // Nothing ran; the pool runs fine once drivers start.
        let out = pool.run(|_| Ok(LoopDriver::new(1))).unwrap();
        assert_eq!(out.completed, vec![(FiberId(1), vec![1, 1])]);
        let empty = Pool::<Fiber>::new(PoolConfig::new(2)).unwrap();
        assert!(
            empty
                .run_seeded(|_| Ok(LoopDriver::new(1)))
                .unwrap()
                .completed
                .is_empty()
        );
    }
}
