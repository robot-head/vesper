//! Scheduler throughput, for the report (ignored by default; run with
//! `cargo test --release -p vesper-pulse --test pool_throughput -- --ignored
//! --nocapture`). Every fiber only yields, so the numbers measure the run
//! queues, stealing and parking, not I/O. Uses only `Pool::run`, so the same
//! file runs against older revisions of the pool for comparison. The
//! fibers share no cache lines while they run.
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};
use vesper_pulse::driver::DriverKind;
use vesper_pulse::pool::{Pool, PoolConfig, Spawner, Task};
use vesper_pulse::scheduler::{Event, Resume, Step};
use vesper_pulse::select::{AnyDriver, host_order};

struct Spin {
    n: u32,
    fan: Option<(Spawner<Spin>, u32)>,
    runs: Arc<AtomicUsize>,
}

impl Resume for Spin {
    fn resume(mut self, _event: Event) -> Step<Self> {
        if let Some((spawner, fan)) = self.fan.take() {
            for _ in 0..fan {
                let child = Spin {
                    n: self.n,
                    fan: None,
                    runs: Arc::clone(&self.runs),
                };
                if spawner.spawn(child).is_err() {
                    panic!("spawn refused");
                }
            }
        }
        if self.n == 0 {
            // Count finished fibers only: a shared counter bumped on every
            // resumption would measure its own cache-line contention.
            self.runs.fetch_add(1, Ordering::Relaxed);
            return Step::Complete(vec![]);
        }
        self.n -= 1;
        Step::Yield(self)
    }
}

fn kind() -> DriverKind {
    *host_order()
        .iter()
        .find(|&&k| AnyDriver::<()>::start(k, 8).is_ok())
        .expect("a host driver")
}

/// `roots` external spawns, each fanning out `fan` children from inside the
/// pool; every fiber yields `yields` times. Returns resumptions per second.
fn measure(workers: usize, roots: u32, fan: u32, yields: u32) -> f64 {
    let kind = kind();
    let runs = Arc::new(AtomicUsize::new(0));
    let pool = Pool::new(PoolConfig::new(workers).seed(1)).unwrap();
    for _ in 0..roots {
        pool.spawn(Spin {
            n: yields,
            fan: Some((pool.spawner(), fan)),
            runs: Arc::clone(&runs),
        });
    }
    let factory = move |_| -> io::Result<AnyDriver<Task<Spin>>> {
        AnyDriver::start(kind, 64).map_err(io::Error::other)
    };
    let t = Instant::now();
    let out = pool.run(factory).unwrap();
    let secs = t.elapsed().as_secs_f64();
    let fibers = runs.load(Ordering::Relaxed);
    assert_eq!(fibers, (roots * (fan + 1)) as usize);
    let total = fibers * (yields as usize + 1);
    assert_eq!(out.completed.len(), (roots * (fan + 1)) as usize);
    total as f64 / secs
}

#[test]
#[ignore = "measurement; run with --ignored --nocapture"]
fn resumptions_per_second() {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    eprintln!("driver {}, {cores} cores", kind());
    for &(workers, roots, fan, yields) in &[
        (1, 64, 63, 400),
        (2, 64, 63, 400),
        (4, 64, 63, 400),
        (8, 64, 63, 400),
        (4, 4096, 0, 400),
        (4, 64, 1023, 4),
    ] {
        let mut best = 0f64;
        for _ in 0..5 {
            best = best.max(measure(workers, roots, fan, yields));
        }
        eprintln!(
            "workers {workers} roots {roots} fan {fan} yields {yields}: {:.2} M resumptions/s",
            best / 1e6
        );
    }
}
