//! The JIT runtime of `parallel_for` (scoped workers, [`super`]): a
//! process-wide Pulse pool ([`vesper_pulse::pool::RunningPool`]) runs the
//! iterations in chunks and the caller waits for every chunk before it
//! returns, so no worker outlives the borrow of `data`.
//!
//! The compiled body is `extern "C" fn(data: u64, i: u64) -> u64` (an `in`
//! aggregate is its address). A call from inside a pool worker (a nested
//! `parallel_for`) runs on the calling thread, so a worker never blocks
//! waiting for siblings that might all be blocked the same way.
use std::sync::{
    Arc, Condvar, Mutex, OnceLock,
    atomic::{AtomicUsize, Ordering},
};
use vesper_pulse::{
    pool::{Pool, PoolConfig, RunningPool, Spawner, Task},
    scheduler::{Event, Resume, Step},
    select::{AnyDriver, host_order},
};

type Body = extern "C" fn(u64, u64) -> u64;

/// Counts the chunks still running and wakes the caller at zero.
struct Latch {
    remaining: AtomicUsize,
    lock: Mutex<()>,
    zero: Condvar,
    /// The first sequence fault a worker recorded (the runtime's fault
    /// state is per thread), moved to the caller after the join.
    fault: Mutex<Option<String>>,
}
impl Latch {
    fn new(chunks: usize) -> Self {
        Self {
            remaining: AtomicUsize::new(chunks),
            lock: Mutex::new(()),
            zero: Condvar::new(),
            fault: Mutex::new(None),
        }
    }
    fn done(&self) {
        if self.remaining.fetch_sub(1, Ordering::AcqRel) == 1 {
            let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
            self.zero.notify_all();
        }
    }
    fn wait(&self) {
        let mut guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        while self.remaining.load(Ordering::Acquire) != 0 {
            guard = self.zero.wait(guard).unwrap_or_else(|e| e.into_inner());
        }
    }
}

thread_local! {
    static IN_WORKER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Iterations `lo..hi` of one `parallel_for`.
struct Chunk {
    body: usize,
    data: u64,
    lo: u64,
    hi: u64,
    latch: Arc<Latch>,
}
impl Chunk {
    fn run(&self) {
        // SAFETY: `body` is the address of a compiled word function the
        // caller of `vesper_parallel_for` keeps alive until every chunk is
        // done (it waits on the latch), and `data` is the caller's `in`
        // borrow, which outlives the wait.
        let body: Body = unsafe { std::mem::transmute::<usize, Body>(self.body) };
        for i in self.lo..self.hi {
            body(self.data, i);
        }
    }
}
impl Resume for Chunk {
    fn resume(self, event: Event) -> Step<Self> {
        // A cancelled chunk still runs: the scope's join waits for every
        // iteration, and nothing in this runtime cancels the pool.
        let _ = event;
        IN_WORKER.with(|w| w.set(true));
        self.run();
        if let Some(fault) = crate::seq::native_runtime::take_fault() {
            self.latch
                .fault
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get_or_insert(fault);
        }
        self.latch.done();
        Step::Complete(vec![])
    }
}

struct Workers {
    spawner: Mutex<Spawner<Chunk>>,
    count: usize,
    _pool: RunningPool<Chunk>,
}

fn start() -> Option<Workers> {
    // `VESPER_WORKERS` sets the pool size (measurements); the default is
    // one worker per hardware thread.
    let count = std::env::var("VESPER_WORKERS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()));
    if count < 2 {
        return None;
    }
    let kind = host_order()
        .iter()
        .copied()
        .find(|k| AnyDriver::<Task<Chunk>>::start(*k, 16).is_ok())?;
    let pool = Pool::new(PoolConfig::new(count).keep_results(false)).ok()?;
    let running = pool
        .start(move |_| AnyDriver::<Task<Chunk>>::start(kind, 16).map_err(std::io::Error::other))
        .ok()?;
    Some(Workers {
        spawner: Mutex::new(running.spawner()),
        count,
        _pool: running,
    })
}

fn workers() -> Option<&'static Workers> {
    static WORKERS: OnceLock<Option<Workers>> = OnceLock::new();
    WORKERS.get_or_init(start).as_ref()
}

/// `parallel_for(in data, n, body)` in JIT code: every `body(data, i)` for
/// `i` in `0..n`, spread over the pool, joined before the return.
pub extern "C" fn vesper_parallel_for(body: u64, data: u64, n: u64) -> u64 {
    let whole = Chunk {
        body: body as usize,
        data,
        lo: 0,
        hi: n,
        latch: Arc::new(Latch::new(0)),
    };
    let pool = match workers() {
        Some(pool) if n > 1 && !IN_WORKER.with(|w| w.get()) => pool,
        _ => {
            whole.run();
            return 0;
        }
    };
    let chunks = n.min(pool.count as u64 * 4);
    let latch = Arc::new(Latch::new(chunks as usize));
    let spawner = pool
        .spawner
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let chunk = |c: u64| Chunk {
        body: body as usize,
        data,
        lo: (u128::from(n) * u128::from(c) / u128::from(chunks)) as u64,
        hi: (u128::from(n) * u128::from(c + 1) / u128::from(chunks)) as u64,
        latch: Arc::clone(&latch),
    };
    // The caller runs the last chunk itself instead of idling on the latch.
    for c in 0..chunks - 1 {
        if let Err((_, refused)) = spawner.spawn(chunk(c)) {
            // The pool refused the fiber: run it here.
            refused.run();
            refused.latch.done();
        }
    }
    let last = chunk(chunks - 1);
    last.run();
    last.latch.done();
    latch.wait();
    if let Some(fault) = latch.fault.lock().unwrap_or_else(|e| e.into_inner()).take() {
        crate::seq::native_runtime::record_fault(fault);
    }
    0
}

/// The JIT symbols of the scoped workers.
pub(crate) fn symbols() -> Vec<(&'static str, *const u8)> {
    vec![("vesper_parallel_for", vesper_parallel_for as *const u8)]
}
