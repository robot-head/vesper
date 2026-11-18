//! The long-lived pool ([`Pool::start`]) on every driver this host can
//! start: it stays alive while idle, takes fibers from many outside threads
//! at once, survives contention on the lock-free run queues, and shuts down
//! cleanly. Each test prints the driver it ran so CI logs record which job
//! exercised which driver.
use std::{
    fs::{File, OpenOptions},
    io,
    path::PathBuf,
    sync::{
        Arc, Barrier,
        atomic::{AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use vesper_pulse::driver::{DriverKind, Operation, Request};
use vesper_pulse::pool::{Pool, PoolConfig, PoolError, RunError, Spawner, Task};
use vesper_pulse::scheduler::{Event, Resume, Step};
use vesper_pulse::select::{AnyDriver, SelectError, host_order};

fn kinds() -> Vec<DriverKind> {
    let mut out = vec![];
    for &kind in host_order() {
        match AnyDriver::<()>::start(kind, 8) {
            Ok(_) => out.push(kind),
            Err(SelectError::Unavailable(s)) if kind == DriverKind::IoUring => {
                eprintln!("skipping io_uring on this host: {}", s.error);
            }
            Err(e) => panic!("{kind} failed to start: {e}"),
        }
    }
    assert!(!out.is_empty());
    out
}

fn factory<C: 'static>(
    kind: DriverKind,
) -> impl Fn(usize) -> io::Result<AnyDriver<Task<C>>> + Send + Sync + 'static {
    move |_| AnyDriver::start(kind, 512).map_err(io::Error::other)
}

struct TempFile(PathBuf, File);
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn temp_file(tag: &str) -> TempFile {
    static N: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "vesper-long-pool-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    TempFile(path, file)
}

fn payload(slot: u64) -> Vec<u8> {
    (0..8u64)
        .map(|b| (slot.wrapping_mul(31) + b) as u8)
        .collect()
}

/// Yield `yields` times, write 8 bytes at `slot * 8`, read them back and
/// report `(slot, bytes)` on `done`. With `children`, first spawn that many
/// more jobs through `spawner` (slots `slot + 1 ..`).
struct Job {
    file: Option<File>,
    slot: u64,
    yields: u32,
    stage: u8,
    children: u64,
    spawner: Option<Spawner<Job>>,
    done: mpsc::Sender<(u64, Vec<u8>)>,
}

impl Job {
    fn new(file: &File, slot: u64, yields: u32, done: &mpsc::Sender<(u64, Vec<u8>)>) -> Self {
        Self {
            file: Some(file.try_clone().unwrap()),
            slot,
            yields,
            stage: 0,
            children: 0,
            spawner: None,
            done: done.clone(),
        }
    }
    fn request(&mut self, operation: Operation, buffer: Vec<u8>) -> Request<()> {
        Request {
            file: self
                .file
                .take()
                .expect("a job holds its file between steps"),
            buffer,
            continuation: (),
            operation,
            offset: self.slot * 8,
        }
    }
}

impl Resume for Job {
    fn resume(mut self, event: Event) -> Step<Self> {
        if let Some(spawner) = self.spawner.take() {
            let file = self.file.as_ref().unwrap();
            for k in 1..=self.children {
                let child = Job::new(file, self.slot + k, 1, &self.done);
                spawner.spawn(child).map_err(|(e, _)| e).unwrap();
            }
        }
        match event {
            Event::Start | Event::Yield if self.yields > 0 => {
                self.yields -= 1;
                Step::Yield(self)
            }
            Event::Start | Event::Yield => {
                let r = self.request(Operation::Write, payload(self.slot));
                self.stage = 1;
                Step::Io(with(r, self))
            }
            Event::Io(io) => {
                self.file = Some(io.file);
                match (self.stage, io.result) {
                    (1, Ok(8)) => {
                        let r = self.request(Operation::Read, vec![0; 8]);
                        self.stage = 2;
                        Step::Io(with(r, self))
                    }
                    (2, Ok(8)) => {
                        let _ = self.done.send((self.slot, io.buffer));
                        Step::Complete(vec![])
                    }
                    (stage, other) => Step::Failed(format!("stage {stage}: {other:?}")),
                }
            }
            Event::Cancelled(_) => Step::Failed("cancelled".into()),
        }
    }
}

fn with(r: Request<()>, job: Job) -> Request<Job> {
    Request {
        file: r.file,
        buffer: r.buffer,
        continuation: job,
        operation: r.operation,
        offset: r.offset,
    }
}

fn collect(rx: &mpsc::Receiver<(u64, Vec<u8>)>, n: usize) -> Vec<(u64, Vec<u8>)> {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut got = Vec::with_capacity(n);
    while got.len() < n {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(r) => got.push(r),
            Err(e) => panic!("only {} of {n} jobs reported: {e}", got.len()),
        }
    }
    got.sort();
    got
}

#[test]
fn an_idle_pool_stays_alive_and_takes_spawns_from_many_threads() {
    for kind in kinds() {
        eprintln!("long-lived pool on {kind}");
        let temp = temp_file("outside");
        let config = PoolConfig::new(4).seed(11).keep_results(false);
        let pool = Pool::new(config)
            .unwrap()
            .start(factory::<Job>(kind))
            .unwrap();
        let (tx, rx) = mpsc::channel();
        // Idle first: nothing spawned, the pool must neither end nor spin.
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(pool.live(), 0);
        let threads = 6u64;
        let per = 150u64;
        let barrier = Arc::new(Barrier::new(threads as usize));
        std::thread::scope(|s| {
            for t in 0..threads {
                let spawner = pool.spawner();
                let barrier = Arc::clone(&barrier);
                let tx = tx.clone();
                let file = &temp.1;
                s.spawn(move || {
                    barrier.wait();
                    for k in 0..per {
                        let slot = t * per + k;
                        let job = Job::new(file, slot, (slot % 4) as u32, &tx);
                        spawner.spawn(job).map_err(|(e, _)| e).unwrap();
                    }
                });
            }
        });
        let first = collect(&rx, (threads * per) as usize);
        for (slot, bytes) in &first {
            assert_eq!(bytes, &payload(*slot), "{kind}: slot {slot}");
        }
        // Idle again, then a second burst: the same workers pick it up.
        std::thread::sleep(Duration::from_millis(120));
        let base = threads * per;
        for k in 0..50 {
            let job = Job::new(&temp.1, base + k, 2, &tx);
            pool.spawn(job).map_err(|(e, _)| e).unwrap();
        }
        let second = collect(&rx, 50);
        assert_eq!(second.first().map(|r| r.0), Some(base));
        let spawner = pool.spawner();
        let out = pool.shutdown().unwrap();
        assert!(out.completed.is_empty(), "keep_results(false) keeps none");
        assert!(out.failures.is_empty());
        assert_eq!(out.abandoned, 0);
        assert_eq!(out.workers.len(), 4);
        let resumed: u64 = out.workers.iter().map(|w| w.resumed).sum();
        assert!(resumed >= threads * per * 3, "{kind}: resumed {resumed}");
        // After shutdown, spawning is closed and the fiber comes back.
        let late = Job::new(&temp.1, 0, 0, &tx);
        assert!(matches!(spawner.spawn(late), Err((PoolError::Closed, _))));
    }
}

#[test]
fn fibers_spawned_inside_a_running_pool_finish_before_shutdown_returns() {
    for kind in kinds() {
        eprintln!("fan-out under shutdown on {kind}");
        let temp = temp_file("fanout");
        let pool = Pool::new(PoolConfig::new(3).seed(5))
            .unwrap()
            .start(factory::<Job>(kind))
            .unwrap();
        let (tx, rx) = mpsc::channel();
        let parents = 40u64;
        let children = 9u64;
        for p in 0..parents {
            let mut job = Job::new(&temp.1, p * (children + 1), 3, &tx);
            job.children = children;
            job.spawner = Some(pool.spawner());
            pool.spawn(job).map_err(|(e, _)| e).unwrap();
        }
        // Shut down at once: children spawned while draining still run.
        let out = pool.shutdown().unwrap();
        let total = (parents * (children + 1)) as usize;
        assert_eq!(out.completed.len(), total, "{kind}");
        assert!(out.failures.is_empty(), "{kind}: {:?}", out.failures);
        let got = collect(&rx, total);
        for (slot, bytes) in &got {
            assert_eq!(bytes, &payload(*slot));
        }
        let stolen: u64 = out.workers.iter().map(|w| w.stolen).sum();
        eprintln!("{kind}: {stolen} fibers stolen");
    }
}

/// A pure compute fiber: yield `n` times, then finish. It holds a count of
/// how often it ran, so a double resumption or a lost fiber shows up.
struct Spin {
    n: u32,
    runs: Arc<AtomicUsize>,
    spawner: Option<(Spawner<Spin>, u32)>,
}

impl Resume for Spin {
    fn resume(mut self, _event: Event) -> Step<Self> {
        self.runs.fetch_add(1, Ordering::Relaxed);
        if let Some((spawner, fan)) = self.spawner.take() {
            for _ in 0..fan {
                let child = Spin {
                    n: self.n,
                    runs: Arc::clone(&self.runs),
                    spawner: None,
                };
                spawner.spawn(child).map_err(|(e, _)| e).unwrap();
            }
        }
        if self.n == 0 {
            return Step::Complete(vec![]);
        }
        self.n -= 1;
        Step::Yield(self)
    }
}

#[test]
fn contended_queues_resume_every_fiber_exactly_as_often_as_it_yields() {
    for kind in kinds() {
        eprintln!("contention on {kind}");
        let workers = 8;
        let pool = Pool::new(PoolConfig::new(workers).seed(3).keep_results(false))
            .unwrap()
            .start(factory::<Spin>(kind))
            .unwrap();
        let runs = Arc::new(AtomicUsize::new(0));
        let roots = 64u32;
        let fan = 31u32;
        let yields = 20u32;
        let feeders = 4;
        std::thread::scope(|s| {
            for _ in 0..feeders {
                let spawner = pool.spawner();
                let runs = Arc::clone(&runs);
                s.spawn(move || {
                    for _ in 0..roots / feeders {
                        let root = Spin {
                            n: yields,
                            runs: Arc::clone(&runs),
                            spawner: Some((spawner.clone(), fan)),
                        };
                        spawner.spawn(root).map_err(|(e, _)| e).unwrap();
                    }
                });
            }
        });
        let out = pool.shutdown().unwrap();
        let fibers = (roots * (fan + 1)) as usize;
        assert_eq!(runs.load(Ordering::Relaxed), fibers * (yields as usize + 1));
        let resumed: u64 = out.workers.iter().map(|w| w.resumed).sum();
        assert_eq!(resumed as usize, fibers * (yields as usize + 1));
        let stolen: u64 = out.workers.iter().map(|w| w.stolen).sum();
        eprintln!(
            "{kind}: {fibers} fibers, {resumed} resumptions, {stolen} stolen, per worker {:?}",
            out.workers.iter().map(|w| w.resumed).collect::<Vec<_>>()
        );
    }
}

#[test]
fn pinned_workers_report_their_core_where_the_os_allows_it() {
    let kind = kinds()[0];
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let workers = cores.clamp(1, 4);
    let pool = Pool::new(PoolConfig::new(workers).pin_cores(true))
        .unwrap()
        .start(factory::<Spin>(kind))
        .unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    for _ in 0..32 {
        let fiber = Spin {
            n: 4,
            runs: Arc::clone(&runs),
            spawner: None,
        };
        pool.spawn(fiber).map_err(|(e, _)| e).unwrap();
    }
    let out = pool.shutdown().unwrap();
    assert_eq!(out.completed.len(), 32);
    let pinned: Vec<_> = out.workers.iter().map(|w| w.pinned_core).collect();
    eprintln!("{kind}: pinned cores {pinned:?}");
    if cfg!(any(target_os = "linux", windows)) {
        // Linux and Windows honour hard affinity; distinct workers get
        // distinct cores while there are enough of them.
        let mut got: Vec<usize> = pinned.iter().map(|p| p.expect("pinned")).collect();
        got.sort();
        got.dedup();
        assert_eq!(got.len(), workers);
    }
}

#[test]
fn a_failed_start_returns_the_pool_with_its_fibers() {
    let kind = kinds()[0];
    let pool = Pool::new(PoolConfig::new(3)).unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    for _ in 0..5 {
        let fiber = Spin {
            n: 1,
            runs: Arc::clone(&runs),
            spawner: None,
        };
        pool.spawn(fiber);
    }
    let failing = move |i: usize| -> io::Result<AnyDriver<Task<Spin>>> {
        if i == 2 {
            Err(io::Error::other("no driver for worker 2"))
        } else {
            AnyDriver::start(kind, 64).map_err(io::Error::other)
        }
    };
    let pool = match pool.start(failing) {
        Err(RunError::Setup { worker, pool, .. }) => {
            assert_eq!(worker, 2);
            pool
        }
        Ok(_) => panic!("a failing factory must not start the pool"),
        Err(e) => panic!("expected a setup error, got {e}"),
    };
    assert_eq!(runs.load(Ordering::Relaxed), 0, "no fiber ran");
    // The same pool, with its queued fibers, runs once a factory works.
    let out = pool
        .start(factory::<Spin>(kind))
        .unwrap()
        .shutdown()
        .unwrap();
    assert_eq!(out.completed.len(), 5);
    assert_eq!(runs.load(Ordering::Relaxed), 10);
}

#[test]
fn dropping_a_running_pool_shuts_it_down() {
    let kind = kinds()[0];
    let pool = Pool::new(PoolConfig::new(2))
        .unwrap()
        .start(factory::<Spin>(kind))
        .unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let spawner = pool.spawner();
    for _ in 0..10 {
        let fiber = Spin {
            n: 3,
            runs: Arc::clone(&runs),
            spawner: None,
        };
        spawner.spawn(fiber).map_err(|(e, _)| e).unwrap();
    }
    drop(pool);
    // Drop waited for the live fibers, then closed spawning.
    assert_eq!(runs.load(Ordering::Relaxed), 40);
    let late = Spin {
        n: 0,
        runs,
        spawner: None,
    };
    assert!(matches!(spawner.spawn(late), Err((PoolError::Closed, _))));
}

#[test]
fn a_spawn_racing_shutdown_either_runs_or_is_refused() {
    let kind = kinds()[0];
    for round in 0..200 {
        let pool = Pool::new(PoolConfig::new(2).seed(round))
            .unwrap()
            .start(factory::<Spin>(kind))
            .unwrap();
        let runs = Arc::new(AtomicUsize::new(0));
        let spawner = pool.spawner();
        let accepted = std::thread::scope(|s| {
            let feeder = s.spawn(|| {
                let mut accepted = 0;
                loop {
                    let fiber = Spin {
                        n: 0,
                        runs: Arc::clone(&runs),
                        spawner: None,
                    };
                    match spawner.spawn(fiber) {
                        Ok(_) => accepted += 1,
                        Err((PoolError::Closed, _)) => return accepted,
                        Err((e, _)) => panic!("{e}"),
                    }
                    if accepted % 3 == 0 {
                        std::thread::yield_now();
                    }
                }
            });
            // Let the pool go idle now and then, so the race hits the
            // zero-to-one reservation.
            if round % 2 == 0 {
                std::thread::sleep(Duration::from_micros(200));
            }
            let out = pool.shutdown().unwrap();
            assert_eq!(out.abandoned, 0);
            feeder.join().unwrap()
        });
        assert_eq!(
            runs.load(Ordering::Relaxed),
            accepted,
            "round {round}: every accepted spawn ran before shutdown returned"
        );
    }
}
