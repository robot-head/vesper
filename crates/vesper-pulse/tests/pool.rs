//! The work-stealing pool on every driver this host can start: many fibers
//! across worker threads doing real file and pipe I/O, cross-worker wake-ups,
//! cancellation of parked reads, and seeded runs. Each test prints the driver
//! it ran so CI logs record which job exercised which driver.
use std::{
    fs::{File, OpenOptions},
    io,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper_pulse::driver::{DriverKind, Operation, Request, is_cancelled};
use vesper_pulse::pool::{Pool, PoolConfig, PoolOutput, Spawner, Task};
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

fn start<C>(kind: DriverKind) -> impl Fn(usize) -> io::Result<AnyDriver<Task<C>>> + Sync {
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
        "vesper-pool-{tag}-{}-{}",
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

fn stream() -> (File, File) {
    #[cfg(unix)]
    {
        let (r, w) = io::pipe().unwrap();
        (
            File::from(std::os::fd::OwnedFd::from(r)),
            File::from(std::os::fd::OwnedFd::from(w)),
        )
    }
    #[cfg(windows)]
    {
        vesper_pulse::iocp::overlapped_pipe().unwrap()
    }
}

fn io<T>(file: File, operation: Operation, buffer: Vec<u8>, offset: u64, k: T) -> Request<T> {
    Request {
        file,
        buffer,
        continuation: k,
        operation,
        offset,
    }
}

/// The stress workload.
enum Job {
    /// Write 8 bytes at `slot * 8`, read them back, yield `yields` times.
    File {
        file: Option<File>,
        slot: u64,
        yields: u32,
        stage: u8,
    },
    /// Spawn `n` file jobs from inside the pool (they land on this worker).
    FanOut {
        file: File,
        first: u64,
        n: u64,
        spawner: Spawner<Job>,
    },
    /// Start reading `file` until `len` bytes arrived.
    Listen(Option<File>, usize),
    /// Read a pipe until `len` bytes arrived.
    Reader { got: Vec<u8>, len: usize },
    /// Yield `yields` times, then write `payload`.
    Writer {
        file: Option<File>,
        payload: Vec<u8>,
        yields: u32,
    },
}

fn payload(slot: u64) -> Vec<u8> {
    (slot * 8..slot * 8 + 8).map(|b| b as u8).collect()
}

impl Resume for Job {
    fn resume(self, event: Event) -> Step<Self> {
        match (self, event) {
            (_, Event::Cancelled(_)) => Step::Failed("unexpected cancellation".into()),
            (
                Job::File {
                    file,
                    slot,
                    yields,
                    stage: 0,
                },
                Event::Start,
            ) => Step::Io(io(
                file.unwrap(),
                Operation::Write,
                payload(slot),
                slot * 8,
                Job::File {
                    file: None,
                    slot,
                    yields,
                    stage: 1,
                },
            )),
            (
                Job::File {
                    slot,
                    yields,
                    stage: 1,
                    ..
                },
                Event::Io(e),
            ) => match e.result {
                Ok(8) => Step::Io(io(
                    e.file,
                    Operation::Read,
                    vec![0; 8],
                    slot * 8,
                    Job::File {
                        file: None,
                        slot,
                        yields,
                        stage: 2,
                    },
                )),
                other => Step::Failed(format!("slot {slot} write: {other:?}")),
            },
            (
                Job::File {
                    slot,
                    yields,
                    stage: 2,
                    ..
                },
                Event::Io(e),
            ) => match e.result {
                Ok(8) if e.buffer == payload(slot) => Step::Yield(Job::File {
                    file: None,
                    slot,
                    yields,
                    stage: 3,
                }),
                other => Step::Failed(format!("slot {slot} read: {other:?} {:?}", e.buffer)),
            },
            (
                Job::File {
                    slot,
                    yields,
                    stage: 3,
                    ..
                },
                Event::Yield,
            ) => {
                if yields == 0 {
                    Step::Complete(slot.to_le_bytes().to_vec())
                } else {
                    Step::Yield(Job::File {
                        file: None,
                        slot,
                        yields: yields - 1,
                        stage: 3,
                    })
                }
            }
            (
                Job::FanOut {
                    file,
                    first,
                    n,
                    spawner,
                },
                Event::Start,
            ) => {
                for slot in first..first + n {
                    let job = Job::File {
                        file: Some(file.try_clone().unwrap()),
                        slot,
                        yields: (slot % 3) as u32,
                        stage: 0,
                    };
                    if spawner.spawn(job).is_err() {
                        return Step::Failed("spawn refused".into());
                    }
                }
                Step::Complete(b"fanout".to_vec())
            }
            (Job::Listen(file, len), Event::Start) => Step::Io(io(
                file.unwrap(),
                Operation::Read,
                vec![0; 64],
                0,
                Job::Reader { got: vec![], len },
            )),
            (Job::Reader { mut got, len }, Event::Io(e)) => match e.result {
                Ok(0) => Step::Failed("unexpected end of pipe".into()),
                Ok(_) => {
                    got.extend_from_slice(&e.buffer);
                    if got.len() >= len {
                        Step::Complete(got)
                    } else {
                        Step::Io(io(
                            e.file,
                            Operation::Read,
                            vec![0; 64],
                            0,
                            Job::Reader { got, len },
                        ))
                    }
                }
                Err(err) => Step::Failed(format!("pipe read: {err}")),
            },
            (
                Job::Writer {
                    file,
                    payload,
                    yields,
                },
                Event::Start | Event::Yield,
            ) => {
                if yields > 0 {
                    return Step::Yield(Job::Writer {
                        file,
                        payload,
                        yields: yields - 1,
                    });
                }
                let n = payload.len();
                Step::Io(io(
                    file.unwrap(),
                    Operation::Write,
                    payload,
                    0,
                    Job::Writer {
                        file: None,
                        payload: vec![],
                        yields: n as u32,
                    },
                ))
            }
            (Job::Writer { yields, .. }, Event::Io(e)) => match e.result {
                Ok(n) if n == yields as usize => Step::Complete(b"wrote".to_vec()),
                other => Step::Failed(format!("pipe write: {other:?}")),
            },
            _ => Step::Failed("a continuation received the wrong event".into()),
        }
    }
}

fn check_stress(out: &PoolOutput, files: u64, pipes: usize, kind: DriverKind) {
    // file jobs + the fan-out + a reader and a writer per pipe + the tail.
    let fibers = files as usize + 1 + 2 * pipes + 1;
    assert!(out.failures.is_empty(), "{kind}: {:?}", out.failures);
    assert_eq!(out.completed.len(), fibers, "{kind}");
    let mut slots: Vec<u64> = vec![];
    let mut messages = 0;
    for (_, v) in &out.completed {
        if v.starts_with(b"pulse-") {
            messages += 1;
        } else if v.len() == 8 {
            slots.push(u64::from_le_bytes(v[..].try_into().unwrap()));
        }
    }
    slots.sort();
    assert_eq!(slots, (0..files).collect::<Vec<_>>(), "{kind}");
    assert_eq!(messages, pipes, "{kind}");
    assert_eq!(out.abandoned, 0);
}

fn stress_pool(workers: usize, seed: u64, files: u64, pipes: usize, temp: &TempFile) -> Pool<Job> {
    let pool = Pool::new(PoolConfig::new(workers).seed(seed)).unwrap();
    // Half the file jobs come from outside (the injector), half from one
    // fan-out fiber (one worker's queue, so the others must steal them).
    let outside = files / 2;
    for slot in 0..outside {
        pool.spawn(Job::File {
            file: Some(temp.1.try_clone().unwrap()),
            slot,
            yields: (slot % 4) as u32,
            stage: 0,
        });
    }
    pool.spawn(Job::FanOut {
        file: temp.1.try_clone().unwrap(),
        first: outside,
        n: files - outside,
        spawner: pool.spawner(),
    });
    for p in 0..pipes {
        let (r, w) = stream();
        let msg = format!("pulse-{p:04}").into_bytes();
        let len = msg.len();
        // The reader is pinned to one worker and the writer to another, so
        // the reader's worker must be woken out of its driver.
        let rw = p % workers;
        let ww = (p + 1) % workers;
        pool.spawn_pinned(rw, Job::Listen(Some(r), len))
            .map_err(|_| ())
            .unwrap();
        pool.spawn_pinned(
            ww,
            Job::Writer {
                file: Some(w),
                payload: msg,
                yields: (p % 7) as u32,
            },
        )
        .map_err(|_| ())
        .unwrap();
    }
    // A tail fiber that yields a while and writes one byte to a pipe nobody
    // reads (it has room), so the run outlives the pipe pairs.
    let (keep, tail) = stream();
    std::mem::forget(keep);
    pool.spawn(Job::Writer {
        file: Some(tail),
        payload: b"x".to_vec(),
        yields: 50,
    });
    pool
}

#[test]
fn many_fibers_across_threads_on_every_host_driver() {
    let workers = 4;
    let (files, pipes) = (1200, 24);
    for kind in kinds() {
        let temp = temp_file("stress");
        let started = std::time::Instant::now();
        let out = stress_pool(workers, 42, files, pipes, &temp)
            .run(start(kind))
            .unwrap();
        check_stress(&out, files, pipes, kind);
        let busy = out.workers.iter().filter(|w| w.resumed > 0).count();
        let stolen: u64 = out.workers.iter().map(|w| w.stolen).sum();
        eprintln!(
            "{kind}: {} fibers on {workers} threads in {:?}; workers that ran fibers: {busy}; stolen: {stolen}; stats: {:?}",
            out.completed.len(),
            started.elapsed(),
            out.workers
        );
        assert!(busy >= 2, "{kind}: work never spread: {:?}", out.workers);
        assert!(out.workers.iter().map(|w| w.submitted).sum::<u64>() >= files * 2);
    }
}

#[test]
fn seeded_runs_finish_the_same_workload_on_every_host_driver() {
    for kind in kinds() {
        let temp = temp_file("seeded");
        let a = stress_pool(3, 9, 120, 6, &temp)
            .run_seeded(start(kind))
            .unwrap();
        check_stress(&a, 120, 6, kind);
        assert!(a.trace.iter().any(|t| t.stolen_from.is_some()), "{kind}");
        let b = stress_pool(3, 9, 120, 6, &temp)
            .run_seeded(start(kind))
            .unwrap();
        let mut x = a.completed.clone();
        let mut y = b.completed.clone();
        x.sort();
        y.sort();
        assert_eq!(x, y, "{kind}");
        eprintln!("{kind}: seeded run of {} resumptions ok", a.trace.len());
    }
}

/// Readers parked on empty pipes on every worker are cancelled by a fiber on
/// another worker; each completes exactly once with the cancellation error.
enum Cancel {
    Read(Option<File>),
    Parked,
    Trigger(Spawner<Cancel>, u32),
}
impl Resume for Cancel {
    fn resume(self, event: Event) -> Step<Self> {
        match (self, event) {
            (Cancel::Read(f), Event::Start) => Step::Io(io(
                f.unwrap(),
                Operation::Read,
                vec![0; 8],
                0,
                Cancel::Parked,
            )),
            (Cancel::Parked, Event::Cancelled(inner)) => match *inner {
                Event::Io(e) => match e.result {
                    Err(err) if is_cancelled(&err) => Step::Complete(b"cancelled".to_vec()),
                    other => Step::Failed(format!("expected cancellation: {other:?}")),
                },
                _ => Step::Failed("cancelled without its read".into()),
            },
            // Cancelled before its worker started it: also exactly once.
            (Cancel::Read(_), Event::Cancelled(inner)) if matches!(*inner, Event::Start) => {
                Step::Complete(b"never started".to_vec())
            }
            (Cancel::Trigger(s, n), Event::Start | Event::Yield) if n > 0 => {
                Step::Yield(Cancel::Trigger(s, n - 1))
            }
            (Cancel::Trigger(s, _), _) => {
                s.cancel();
                Step::Complete(b"trigger".to_vec())
            }
            _ => Step::Failed("wrong event".into()),
        }
    }
}

#[test]
fn cancelling_the_pool_completes_parked_reads_on_every_worker() {
    for kind in kinds() {
        let workers = 3;
        let pool = Pool::new(PoolConfig::new(workers)).unwrap();
        let mut writers = vec![];
        for w in 0..workers * 2 {
            let (r, keep) = stream();
            writers.push(keep);
            pool.spawn_pinned(w % workers, Cancel::Read(Some(r)))
                .map_err(|_| ())
                .unwrap();
        }
        pool.spawn_pinned(0, Cancel::Trigger(pool.spawner(), 20))
            .map_err(|_| ())
            .unwrap();
        let out = pool.run(start(kind)).unwrap();
        assert!(out.failures.is_empty(), "{kind}: {:?}", out.failures);
        assert_eq!(out.completed.len(), workers * 2 + 1);
        let parked = out
            .completed
            .iter()
            .filter(|(_, v)| v == b"cancelled")
            .count();
        eprintln!(
            "{kind}: cancelled {parked} parked reads and {} unstarted readers",
            workers * 2 - parked
        );
    }
}
