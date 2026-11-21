//! ABBA benchmark of the Rust and Vesper builds of vesper-flux (port wave
//! P3b lane 7, #1069): the `incremental_join` and `checkpoint_resume`
//! examples scaled up.
//!
//! - `join`: a filtered, mapped equijoin of two width-2 inputs, run for
//!   [`JOIN_TICKS`] ticks of 64-row insert/retract deltas over 256 keys;
//!   each tick's delta and the materialized output are digested.
//! - `example`: the `incremental_join` example itself (three one-row ticks
//!   checked against the batch evaluator), [`EXAMPLE_RUNS`] times per batch:
//!   small sets, where per-call costs dominate.
//! - `resume`: the shared-input join of `checkpoint_resume` fed through CDC
//!   for [`RESUME_TICKS`] ticks, checkpointed half way, restored into a
//!   fresh executor and finished there; the final checkpoint is digested.
//! - `ingest`: CDC decoding and accumulation on their own: [`INGEST_TICKS`]
//!   pre-encoded batches of [`INGEST_RECORDS`] width-4 records over 1024
//!   keys (repeats and cancellations), ingested by a fresh executor whose
//!   circuit is its input.
//! - `log`: the input log: [`LOG_EPOCHS`] epochs of two width-3 sets of
//!   [`LOG_ROWS`] rows appended to a fresh log, read back, and the log
//!   reopened (every frame verified). Point `TMPDIR` at a RAM disk so that
//!   `sync_all` does not dominate.
//! - `image`: checkpoint images on their own: an executor of two width-3
//!   inputs holding [`IMAGE_TICKS`] ticks of [`LOG_ROWS`]-row sets is
//!   checkpointed and restored into a fresh executor [`IMAGE_ROUNDS`] times.
//!
//! The two arms are two builds of this example, in separate worker
//! processes: before #1069 replaced the Rust kernels they were the Rust
//! build and the `vesper-port` build (docs/parallel-results/port-p3b-flux.md);
//! now build it at two revisions to compare them. Each worker runs one batch
//! of the workload per request on stdin; the driver times each request with
//! `vesper-bench`'s `run_abba`, so both arms carry the same pipe round trip.
//! Every batch returns a digest of its results, and the driver checks that
//! both builds agree.
//!
//! ```text
//! abba worker <join|example|resume|ingest|log|image>                 # one arm
//! abba drive <join|example|resume|ingest|log|image> <rust-worker> <vesper-worker>
//! ```
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use vesper_bench::{BenchCase, Invocation, RunConfig, analyze_pairs, run_abba};
use vesper_flux::{
    CdcHeader, CdcPolicy, CheckpointLimits, CircuitBuilder, Executor, Field, Frontier, Limits,
    Predicate, SourceBinding, TickInput, WeightPolicy, ZSet,
};

/// Ticks per `join` batch.
const JOIN_TICKS: u64 = 32;
/// Runs of the `incremental_join` example per `example` batch.
const EXAMPLE_RUNS: u64 = 64;
/// Ticks per `resume` batch.
const RESUME_TICKS: u64 = 16;
/// CDC batches per `ingest` batch, and records per CDC batch.
const INGEST_TICKS: u64 = 8;
const INGEST_RECORDS: u64 = 4096;
/// Epochs per `log` batch, and rows per set.
const LOG_EPOCHS: u64 = 16;
const LOG_ROWS: u64 = 1024;
/// Ticks of the `image` executor's state, and checkpoint/restore rounds per
/// `image` batch.
const IMAGE_TICKS: u64 = 8;
const IMAGE_ROUNDS: u64 = 8;

fn fnv(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
    }
    h
}

fn digest_set(h: u64, set: &ZSet) -> u64 {
    let mut h = fnv(h, &(set.len() as u64).to_le_bytes());
    for (row, w) in set.entries() {
        for c in row {
            h = fnv(h, &c.to_le_bytes());
        }
        h = fnv(h, &w.to_le_bytes());
    }
    h
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
}

fn wide() -> Limits {
    Limits {
        max_columns: 16,
        max_input_entries: 1 << 16,
        max_rows: 1 << 18,
        max_nodes: 64,
        max_depth: 32,
        max_work: usize::MAX,
        max_product: usize::MAX,
        max_output: 1 << 18,
    }
}

/// One `join` batch: inserts with occasional retractions of earlier rows.
fn join_batch(seed: u64) -> u64 {
    let l = wide();
    let mut b = CircuitBuilder::new(l).expect("builder");
    let x = b.input(2).expect("input");
    let y = b.input(2).expect("input");
    let fx = b
        .filter(
            x,
            Predicate::AtLeast {
                column: 1,
                value: 8,
            },
        )
        .expect("filter");
    let my = b
        .map(
            y,
            vec![Field::Column(0), Field::Column(1), Field::Constant(1)],
        )
        .expect("map");
    let j = b.equijoin(fx, my, 0, 0).expect("join");
    let circuit = b.finish(j).expect("finish");
    let mut exec = circuit.executor();
    let mut rng = Lcg(seed);
    let mut live: [Vec<Vec<i64>>; 2] = [Vec::new(), Vec::new()];
    let mut h = 0xcbf2_9ce4_8422_2325;
    for _ in 0..JOIN_TICKS {
        let mut deltas = Vec::new();
        for side in &mut live {
            let mut raw = Vec::new();
            for _ in 0..64 {
                if !side.is_empty() && rng.next().is_multiple_of(4) {
                    let i = (rng.next() as usize) % side.len();
                    raw.push((side.swap_remove(i), -1));
                } else {
                    let row = vec![(rng.next() % 256) as i64, (rng.next() % 64) as i64];
                    side.push(row.clone());
                    raw.push((row, 1));
                }
            }
            deltas.push(ZSet::consolidate(2, &raw, l).expect("consolidate"));
        }
        let out = exec.tick(&TickInput { deltas }).expect("tick");
        h = digest_set(h, &out);
    }
    digest_set(h, exec.materialized())
}

/// One `incremental_join` run (examples/incremental_join.rs without output).
fn example_run(h: u64) -> u64 {
    let l = Limits::default();
    let mut b = CircuitBuilder::new(l).expect("builder");
    let a = b.input(1).expect("input");
    let c = b.input(1).expect("input");
    let root = b.equijoin(a, c, 0, 0).expect("join");
    let circuit = b.finish(root).expect("finish");
    let mut executor = circuit.clone().executor();
    let mut input = vec![ZSet::empty(1, l).expect("empty"); 2];
    let mut output = ZSet::empty(2, l).expect("empty");
    let mut frontier = Frontier::new(0, 2).expect("frontier");
    let mut ticket = frontier.acquire(0).expect("acquire");
    let mut h = h;
    for (tick, (wa, wb)) in [(2, 3), (-1, -2), (-1, -1)].into_iter().enumerate() {
        let deltas = vec![
            ZSet::consolidate(1, &[(vec![7], wa)], l).expect("consolidate"),
            ZSet::consolidate(1, &[(vec![7], wb)], l).expect("consolidate"),
        ];
        for i in 0..2 {
            input[i] = input[i].plus(&deltas[i], l).expect("plus");
        }
        let delta = executor.tick(&TickInput { deltas }).expect("tick");
        output = output.plus(&delta, l).expect("plus");
        assert_eq!(output, circuit.batch(&input).expect("batch"));
        ticket = frontier
            .advance(ticket, (tick + 1) as i64)
            .map_err(|e| e.error)
            .expect("advance");
        h = digest_set(h, &delta);
        h = fnv(h, &frontier.watermark().to_le_bytes());
    }
    frontier
        .retire(ticket)
        .map_err(|e| e.error)
        .expect("retire");
    assert!(output.is_empty());
    h
}

fn example_batch() -> u64 {
    (0..EXAMPLE_RUNS).fold(0xcbf2_9ce4_8422_2325, |h, _| example_run(h))
}

fn resume_executor() -> Executor {
    let mut b = CircuitBuilder::new(wide()).expect("builder");
    let a = b.input(1).expect("input");
    let c = b.input(1).expect("input");
    let shared = b.union(a, a).expect("union");
    let root = b.equijoin(shared, c, 0, 0).expect("join");
    let mut e = b.finish(root).expect("finish").executor();
    e.bind_sources(&CdcPolicy {
        bindings: (0..2)
            .map(|p| SourceBinding {
                partition_id: p,
                schema_id: p as u16 + 1,
                input: p as usize,
                first_lsn: 0,
                weights: WeightPolicy::Unit,
            })
            .collect(),
        max_batch_bytes: 1 << 20,
        max_records: 1 << 12,
    })
    .expect("bind");
    e
}

/// One `resume` batch: ticks 0..RESUME_TICKS/2 on one executor, a
/// checkpoint, the rest on a restored one.
fn resume_batch(seed: u64) -> u64 {
    let limits = CheckpointLimits::default();
    let mut live = resume_executor();
    let mut rng = Lcg(seed);
    let mut h = 0xcbf2_9ce4_8422_2325;
    let mut lsns = [0u64; 2];
    for tick in 0..RESUME_TICKS {
        if tick == RESUME_TICKS / 2 {
            let image = live.checkpoint(limits).expect("checkpoint");
            h = fnv(h, &image);
            let mut resumed = resume_executor();
            resumed.restore_checkpoint(&image, limits).expect("restore");
            live = resumed;
        }
        let mut bytes = Vec::new();
        for partition in 0..2u32 {
            for _ in 0..24 {
                let lsn = &mut lsns[partition as usize];
                *lsn += 1;
                let header = CdcHeader {
                    source_lsn: *lsn - 1,
                    event_ts_ms: tick as i64,
                    z_weight: 1,
                    partition_id: partition,
                    schema_id: partition as u16 + 1,
                    flags: 0,
                };
                let key = (rng.next() % 512) as i64;
                bytes.extend(live.encode_cdc_record(header, &[key]).expect("encode"));
            }
        }
        let delta = live.ingest_cdc(&bytes).expect("ingest").delta;
        h = digest_set(h, &delta);
    }
    fnv(h, &live.checkpoint(limits).expect("checkpoint"))
}

fn ingest_executor() -> Executor {
    let mut b = CircuitBuilder::new(wide()).expect("builder");
    let a = b.input(4).expect("input");
    let mut e = b.finish(a).expect("finish").executor();
    e.bind_sources(&CdcPolicy {
        bindings: vec![SourceBinding {
            partition_id: 7,
            schema_id: 1,
            input: 0,
            first_lsn: 0,
            weights: WeightPolicy::NonZero,
        }],
        max_batch_bytes: 1 << 20,
        max_records: INGEST_RECORDS as usize,
    })
    .expect("bind");
    e
}

/// The `ingest` workload's CDC batches, encoded once per worker.
fn ingest_batches() -> Vec<Vec<u8>> {
    let e = ingest_executor();
    let mut rng = Lcg(0x1d6e57);
    let mut lsn = 0;
    (0..INGEST_TICKS)
        .map(|tick| {
            let mut bytes = Vec::new();
            for _ in 0..INGEST_RECORDS {
                let key = (rng.next() % 1024) as i64;
                let header = CdcHeader {
                    source_lsn: lsn,
                    event_ts_ms: tick as i64,
                    z_weight: [1, 1, 2, -1][(rng.next() % 4) as usize],
                    partition_id: 7,
                    schema_id: 1,
                    flags: 0,
                };
                lsn += 1;
                let row = [key % 16, key, -key, key * 1_000_003];
                bytes.extend(e.encode_cdc_record(header, &row).expect("encode"));
            }
            bytes
        })
        .collect()
}

/// One `ingest` batch: every pre-encoded CDC batch into a fresh executor.
fn ingest_batch(batches: &[Vec<u8>]) -> u64 {
    let mut e = ingest_executor();
    let mut h = 0xcbf2_9ce4_8422_2325;
    for bytes in batches {
        let ingested = e.ingest_cdc(bytes).expect("ingest");
        h = digest_set(fnv(h, &ingested.records.to_le_bytes()), &ingested.delta);
    }
    h
}

/// The `log` workload's epochs, built once per worker.
fn log_inputs() -> Vec<TickInput> {
    let mut rng = Lcg(0x10_6e57);
    (0..LOG_EPOCHS)
        .map(|_| TickInput {
            deltas: (0..2)
                .map(|_| {
                    let entries: Vec<(Vec<i64>, i64)> = (0..LOG_ROWS)
                        .map(|_| {
                            let k = rng.next() as i64;
                            (vec![k % 1000, k, -k], 1 + (k % 3))
                        })
                        .collect();
                    ZSet::consolidate(3, &entries, wide()).expect("set")
                })
                .collect(),
        })
        .collect()
}

/// One `log` batch: append every epoch, read each back, reopen the log.
fn log_batch(inputs: &[TickInput], n: u64) -> u64 {
    let path = std::env::temp_dir().join(format!("vesper-flux-abba-{}-{n}", std::process::id()));
    let limits = vesper_flux::LogLimits {
        max_frame_bytes: 1 << 24,
        max_rows_per_frame: 1 << 16,
        ..vesper_flux::LogLimits::default()
    };
    let mut log = vesper_flux::InputLog::create(&path, &[3, 3], limits).expect("create");
    let mut h = 0xcbf2_9ce4_8422_2325;
    for input in inputs {
        log.append(input).expect("append");
    }
    for epoch in 0..log.len() {
        for d in &log.read(epoch).expect("read").deltas {
            h = digest_set(h, d);
        }
    }
    drop(log);
    let (log, recovery) = vesper_flux::InputLog::open(&path, limits).expect("open");
    h = fnv(h, &recovery.epochs.to_le_bytes());
    drop(log);
    std::fs::remove_file(&path).expect("remove");
    h
}

fn image_executor() -> Executor {
    let mut b = CircuitBuilder::new(wide()).expect("builder");
    let a = b.input(3).expect("input");
    b.input(3).expect("input");
    b.finish(a).expect("finish").executor()
}

/// The `image` workload's executor, built once per worker.
fn image_state() -> Executor {
    let mut e = image_executor();
    let mut rng = Lcg(0x1a_6e57);
    for _ in 0..IMAGE_TICKS {
        let deltas = (0..2)
            .map(|_| {
                let entries: Vec<(Vec<i64>, i64)> = (0..LOG_ROWS)
                    .map(|_| {
                        let k = rng.next() as i64;
                        (vec![k % 1000, k, -k], 1 + (k % 3))
                    })
                    .collect();
                ZSet::consolidate(3, &entries, wide()).expect("set")
            })
            .collect();
        e.tick(&TickInput { deltas }).expect("tick");
    }
    e
}

/// One `image` batch: checkpoint and restore into a fresh executor.
fn image_batch(e: &Executor) -> u64 {
    let limits = CheckpointLimits {
        max_bytes: 1 << 24,
        max_total_rows: 1_000_000,
        max_total_cells: 4_000_000,
        max_work: 16_000_000,
        ..CheckpointLimits::default()
    };
    let mut h = 0xcbf2_9ce4_8422_2325;
    for _ in 0..IMAGE_ROUNDS {
        let image = e.checkpoint(limits).expect("checkpoint");
        let mut fresh = image_executor();
        fresh.restore_checkpoint(&image, limits).expect("restore");
        h = fnv(h, &image[image.len() - 32..]);
        h = fnv(h, &fresh.ticks().to_le_bytes());
    }
    h
}

/// Serve batches: each stdin line `go <n>` runs batch `n` and answers with
/// its digest.
fn worker(workload: &str) {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    let batches = if workload == "ingest" {
        ingest_batches()
    } else {
        Vec::new()
    };
    let epochs = if workload == "log" {
        log_inputs()
    } else {
        Vec::new()
    };
    let image = (workload == "image").then(image_state);
    for line in stdin.lock().lines() {
        let line = line.expect("read request");
        let n: u64 = line
            .strip_prefix("go ")
            .and_then(|n| n.parse().ok())
            .expect("request `go <n>`");
        let digest = match workload {
            "join" => join_batch(n + 1),
            "example" => example_batch(),
            "resume" => resume_batch(n + 1),
            "ingest" => ingest_batch(&batches),
            "log" => log_batch(&epochs, n),
            "image" => image_batch(image.as_ref().expect("image state")),
            other => panic!("unknown workload {other}"),
        };
        writeln!(out, "{digest}").expect("answer");
        out.flush().expect("flush");
    }
}

/// One arm: a worker process timed per request.
struct Arm {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    digests: Vec<(usize, u64)>,
}

impl Arm {
    fn spawn(binary: &str, workload: &str) -> Arm {
        let mut child = Command::new(binary)
            .args(["worker", workload])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("spawn {binary}: {e}"));
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Arm {
            child,
            stdin,
            stdout,
            digests: Vec::new(),
        }
    }
}

impl BenchCase for Arm {
    type Input = usize;
    type Output = (usize, u64);
    type Error = std::io::Error;
    fn setup(&mut self, i: Invocation) -> Result<usize, std::io::Error> {
        Ok(i.operation)
    }
    fn run(&mut self, n: usize) -> Result<(usize, u64), std::io::Error> {
        writeln!(self.stdin, "go {n}")?;
        self.stdin.flush()?;
        let mut line = String::new();
        self.stdout.read_line(&mut line)?;
        let digest = line
            .trim()
            .parse()
            .map_err(|e| std::io::Error::other(format!("worker answered {line:?}: {e}")))?;
        Ok((n, digest))
    }
    fn teardown(&mut self, output: (usize, u64)) -> Result<(), std::io::Error> {
        self.digests.push(output);
        Ok(())
    }
}

fn drive(workload: &str, rust: &str, vesper: &str) {
    let (operations, per_batch) = match workload {
        "join" => (4, JOIN_TICKS),
        "example" => (4, EXAMPLE_RUNS * 3),
        "resume" => (4, RESUME_TICKS),
        "ingest" => (4, INGEST_TICKS),
        "log" => (4, LOG_EPOCHS),
        "image" => (4, IMAGE_ROUNDS),
        other => panic!("unknown workload {other}"),
    };
    let config = RunConfig {
        epochs: 12,
        warmup_epochs: 1,
        operations_per_window: operations,
        max_elapsed_nanos: 600_000_000_000,
        ..RunConfig::default()
    };
    let mut baseline = Arm::spawn(rust, workload);
    let mut candidate = Arm::spawn(vesper, workload);
    let report = run_abba(config, &mut baseline, &mut candidate).expect("ABBA run");
    let mut a = baseline.digests.clone();
    let mut b = candidate.digests.clone();
    a.sort_unstable();
    b.sort_unstable();
    assert_eq!(
        a, b,
        "the Rust and Vesper builds computed different results"
    );
    let mut ratios: Vec<f64> = report
        .pairs()
        .iter()
        .map(|p| p.candidate_total() as f64 / p.baseline_total() as f64)
        .collect();
    ratios.sort_by(f64::total_cmp);
    let per_unit = |total: u64| total as f64 / (2 * operations) as f64 / per_batch as f64;
    let mut rust_ns: Vec<f64> = report
        .pairs()
        .iter()
        .map(|p| per_unit(p.baseline_total()))
        .collect();
    let mut vesper_ns: Vec<f64> = report
        .pairs()
        .iter()
        .map(|p| per_unit(p.candidate_total()))
        .collect();
    rust_ns.sort_by(f64::total_cmp);
    vesper_ns.sort_by(f64::total_cmp);
    let median = |v: &[f64]| (v[(v.len() - 1) / 2] + v[v.len() / 2]) / 2.0;
    let unit = "tick";
    println!(
        "{workload}: {} measured epochs x A/B/B/A x {operations} requests; \
         Rust median {:.0} ns/{unit}, Vesper median {:.0} ns/{unit}, \
         Vesper/Rust median {:.2}x (min {:.2}x, max {:.2}x)",
        report.pairs().len(),
        median(&rust_ns),
        median(&vesper_ns),
        median(&ratios),
        ratios[0],
        ratios[ratios.len() - 1],
    );
    let analysis = analyze_pairs(report.pairs(), 10_000).expect("analysis");
    println!(
        "{workload}: 2x gate (candidate at most 10000 bp slower): {:?}; digests agree over {} batches",
        analysis.decision,
        a.len()
    );
    // `ratios` is sorted; the interval's ranks are 1-based.
    if let Some(interval) = analysis.interval {
        println!(
            "{workload}: 95% median interval of Vesper/Rust: {:.3}x..{:.3}x",
            ratios[interval.lower_rank - 1],
            ratios[interval.upper_rank - 1]
        );
    }
    for arm in [&mut baseline, &mut candidate] {
        let _ = arm.child.kill();
        let _ = arm.child.wait();
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>()[1..] {
        ["worker", workload] => worker(workload),
        ["drive", workload, rust, vesper] => drive(workload, rust, vesper),
        _ => {
            eprintln!(
                "usage: abba worker <join|example|resume|ingest|log|image>| abba drive <join|example|resume|ingest|log|image> <rust-worker> <vesper-worker>"
            );
            std::process::exit(2);
        }
    }
}
