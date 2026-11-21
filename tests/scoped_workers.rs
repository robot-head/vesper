//! Scoped workers (port wave P3a lane 2, #1037): `parallel_for(in data, n,
//! body)` over the Pulse pool. Each program runs in the evaluator and the
//! JIT (optimized and not), builds for both native targets and, where a C
//! driver links executables, runs as one; the JIT really runs iterations at
//! the same time; a nested call inside a worker finishes; the runtime joins
//! every iteration before it returns; and the checker refuses data shared
//! by `inout` (`E0824`) and bodies the pool cannot run (`E0823`).
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use vesper::{atomics::workers::vesper_parallel_for, check, native, parser, runtime::Runtime};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-workers-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn checked(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{source}\n{:?}", p.diagnostics);
    check::check(&p.module)
}

fn accepted(source: &str) -> check::Checked {
    let c = checked(source);
    let shown: Vec<_> = c
        .diagnostics
        .iter()
        .map(|d| format!("{}: {} at `{}`", d.code, d.message, &source[d.span.clone()]))
        .collect();
    assert!(c.diagnostics.is_empty(), "{source}\n{shown:#?}");
    c
}

fn rejects(source: &str, code: &str, message: &str) {
    let c = checked(source);
    assert!(
        c.diagnostics
            .iter()
            .any(|d| d.code == code && d.message.contains(message)),
        "expected {code} `{message}` for {source}: {:#?}",
        c.diagnostics
    );
}

fn links_executables() -> bool {
    cfg!(unix)
        && Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
}

fn parallel_hardware() -> bool {
    std::thread::available_parallelism().is_ok_and(|n| n.get() >= 2)
}

/// Runs `main` everywhere and checks each result against `expected`.
fn differential(name: &str, source: &str, expected: u64) {
    let c = accepted(source);
    for optimize in [true, false] {
        let mut runtime = Runtime::new(&c.world);
        runtime.optimize = optimize;
        let value = runtime
            .run("main", vec![])
            .unwrap_or_else(|e| panic!("{name}: evaluator (optimize {optimize}) failed: {e}"))
            .to_string();
        assert_eq!(
            value,
            expected.to_string(),
            "{name}: evaluator (optimize {optimize})"
        );
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap_or_else(|e| panic!("{name}: JIT (optimize {optimize}) failed: {e}"))
            .to_string();
        assert_eq!(
            jit,
            expected.to_string(),
            "{name}: JIT (optimize {optimize})"
        );
    }
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        native::object_for_target(&c.world, "main", Some(target))
            .unwrap_or_else(|e| panic!("{name}: {target} object failed: {e}"));
    }
    if links_executables() {
        let temp = Temp::new();
        let file = temp.0.join(format!("{name}.ves"));
        fs::write(&file, source).unwrap();
        let exe = temp.0.join(name);
        let built = Command::new(env!("CARGO_BIN_EXE_vesper"))
            .arg("build")
            .arg(&file)
            .arg("-o")
            .arg(&exe)
            .output()
            .unwrap();
        assert!(
            built.status.success(),
            "{name}: vesper build failed\n{}{}",
            String::from_utf8_lossy(&built.stdout),
            String::from_utf8_lossy(&built.stderr)
        );
        let status = Command::new(&exe).output().unwrap().status.code();
        assert_eq!(status, Some((expected & 0xff) as i32), "{name}: executable");
    }
}

const PARALLEL_SUM: &str = include_str!("fixtures/scoped_workers/parallel_sum.ves");

#[test]
fn a_parallel_sum_agrees_everywhere() {
    differential("parallel_sum", PARALLEL_SUM, (0..4000u64).sum());
}

/// Scalar data is passed by value and an empty loop runs nothing.
#[test]
fn scalar_data_and_empty_loops() {
    differential(
        "scalar_data",
        "\
struct Totals { sum: Atomic[u64], calls: Atomic[u64] }
fn add(in t: Totals, i: u64) {
    t.sum.fetch_add(wrapping_mul(i, i), MemoryOrder::Relaxed);
    t.calls.fetch_add(1, MemoryOrder::Relaxed);
}
fn bump(in seed: u64, i: u64) {}
fn main() -> u64 {
    let t = Totals { sum: Atomic::new(0), calls: Atomic::new(0) };
    parallel_for(in t, 100, add);
    parallel_for(in t, 0, add);
    parallel_for(in t, 1, add);
    let seed: u64 = 7;
    parallel_for(in seed, 50, bump);
    wrapping_add(t.sum.load(MemoryOrder::SeqCst), wrapping_mul(t.calls.load(MemoryOrder::SeqCst), 1000000))
}
",
        (0..100u64).map(|i| i * i).sum::<u64>() + 101 * 1_000_000,
    );
}

/// A `parallel_for` inside a worker runs on that worker, so nesting never
/// waits on a pool whose workers are all waiting too.
#[test]
fn nested_loops_finish() {
    differential(
        "nested",
        "\
struct Grid { cells: Atomic[u64] }
fn inner(in g: Grid, j: u64) {
    g.cells.fetch_add(wrapping_add(j, 1), MemoryOrder::Relaxed);
}
fn outer(in g: Grid, i: u64) {
    parallel_for(in g, 64, inner);
}
fn main() -> u64 {
    let g = Grid { cells: Atomic::new(0) };
    parallel_for(in g, 64, outer);
    g.cells.load(MemoryOrder::SeqCst)
}
",
        64 * (1..=64u64).sum::<u64>(),
    );
}

/// Two iterations wait (boundedly) for each other: the JIT runs them at the
/// same time, so the first sees the second arrive; the evaluator runs them
/// in order, so it never does.
#[test]
fn the_jit_runs_iterations_at_the_same_time() {
    let c = accepted(
        "\
struct Meet { arrived: Atomic[u64], met: Atomic[u64] }
fn wait(in m: Meet, i: u64) {
    m.arrived.fetch_add(1, MemoryOrder::AcqRel);
    let mut seen: u64 = 0;
    let spins: u64 = 2000000000;
    for k in 0..spins {
        if m.arrived.load(MemoryOrder::Acquire) == 2 { seen = 1; break; } else {}
    }
    m.met.fetch_add(seen, MemoryOrder::Relaxed);
}
fn main() -> u64 {
    let m = Meet { arrived: Atomic::new(0), met: Atomic::new(0) };
    parallel_for(in m, 2, wait);
    m.met.load(MemoryOrder::SeqCst)
}
",
    );
    if !parallel_hardware() {
        return;
    }
    for optimize in [true, false] {
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap()
            .to_string();
        assert_eq!(
            jit, "2",
            "both iterations saw each other (optimize {optimize})"
        );
    }
}

/// A sequence fault in an iteration on a pool worker fails the JIT run
/// like one on the calling thread, and does not leak into the next run.
#[test]
fn a_fault_on_a_worker_fails_the_run() {
    let c = accepted(
        "\
fn total(in s: Str) -> u64 { s.len() }
fn view(in s: String, i: u64) {
    if i == 0 { total(s[1..9]); } else {}
}
fn main() -> u64 {
    let s = \"a\u{e9}b\";
    parallel_for(in s, 64, view);
    0
}
",
    );
    for optimize in [true, false] {
        let e = native::jit_with_options(&c.world, "main", optimize).unwrap_err();
        assert!(e.contains("out of bounds"), "optimize {optimize}: {e}");
    }
    let clean = accepted(PARALLEL_SUM);
    for _ in 0..4 {
        assert_eq!(
            native::jit(&clean.world, "main").unwrap().to_string(),
            (0..4000u64).sum::<u64>().to_string()
        );
    }
}

/// `TiledSeq` handles live in a per-thread JIT registry, so a loop that
/// reaches one runs on the calling thread and still sees the values.
#[test]
fn thread_affine_data_runs_on_the_caller() {
    let source = "\
tiled(8) copy struct P { hot mass: u64 }
struct Job { s: TiledSeq[P], total: Atomic[u64] }
fn add(in job: Job, i: u64) {
    job.total.fetch_add(job.s.col_get[mass](wrapping_add(i, 0) % 3), MemoryOrder::Relaxed);
}
fn main() -> u64 {
    let mut s: TiledSeq[P] = TiledSeq::with_capacity[P](4);
    s.push(P { mass: 1 });
    s.push(P { mass: 2 });
    s.push(P { mass: 3 });
    let job = Job { s: s, total: Atomic::new(0) };
    parallel_for(in job, 9, add);
    job.total.load(MemoryOrder::SeqCst)
}
";
    let c = accepted(source);
    let mut runtime = Runtime::new(&c.world);
    assert_eq!(runtime.run("main", vec![]).unwrap().to_string(), "18");
    for optimize in [true, false] {
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap()
            .to_string();
        assert_eq!(jit, "18", "optimize {optimize}");
    }
}

static SEEN: Mutex<Vec<std::thread::ThreadId>> = Mutex::new(Vec::new());
static HITS: AtomicU64 = AtomicU64::new(0);

extern "C" fn record(data: u64, i: u64) -> u64 {
    HITS.fetch_add(data + i, Ordering::Relaxed);
    let id = std::thread::current().id();
    let mut seen = SEEN.lock().unwrap();
    if !seen.contains(&id) {
        seen.push(id);
    }
    drop(seen);
    // Long enough that other workers pick up chunks.
    std::thread::sleep(std::time::Duration::from_micros(200));
    0
}

/// The runtime runs every iteration exactly once before it returns, on
/// more than one thread where the host has them.
#[test]
fn the_runtime_joins_every_iteration() {
    let body = record as extern "C" fn(u64, u64) -> u64 as usize as u64;
    for n in [0u64, 1, 2, 7, 1000] {
        HITS.store(0, Ordering::SeqCst);
        assert_eq!(vesper_parallel_for(body, 3, n), 0);
        assert_eq!(
            HITS.load(Ordering::SeqCst),
            (0..n).map(|i| 3 + i).sum::<u64>(),
            "n = {n}"
        );
    }
    if parallel_hardware() {
        let threads = SEEN.lock().unwrap().len();
        assert!(threads >= 2, "iterations ran on {threads} thread(s)");
    }
}

#[test]
fn data_shared_by_inout_is_refused() {
    let fixture = include_str!("fixtures/scoped_workers/shared_inout.ves");
    rejects(fixture, "E0824", "share their data only by 'in'");
    let temp = Temp::new();
    let file = temp.0.join("shared_inout.ves");
    fs::write(&file, fixture).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg("check")
        .arg(&file)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("E0824"), "{text}");
    // A body that would take the shared data by `inout` is refused too.
    rejects(
        "\
struct Tally { count: u64 }
fn count_one(inout tally: Tally, i: u64) { tally.count = i; }
fn main() -> u64 {
    let t = Tally { count: 0 };
    parallel_for(in t, 8, count_one);
    0
}
",
        "E0824",
        "share their data only by 'in'",
    );
}

#[test]
fn bodies_the_pool_cannot_run_are_refused() {
    // A closure.
    rejects(
        "\
fn main() -> u64 {
    let seed: u64 = 1;
    parallel_for(in seed, 4, |s, i| ());
    0
}
",
        "E0823",
        "named function whose effects need no handler",
    );
    // A body with an effect.
    rejects(
        "\
fn risky(in s: u64, i: u64) eff [Fail[u64]] { if i == 3 { raise(i) } else {} }
fn main() -> u64 {
    let seed: u64 = 1;
    parallel_for(in seed, 4, risky);
    0
}
",
        "E0823",
        "named function whose effects need no handler",
    );
}

/// Bodies may carry stateless capabilities (§15.4): a capability effect
/// runs on the pool, and an `Alloc` marker without operations is accepted. A user effect with operations, or a
/// labelled instance, still needs a handler and is refused.
#[test]
fn capability_bodies_run_on_workers() {
    differential(
        "realtime_body",
        "\
fn mix(in seed: u64, i: u64) eff [Realtime] {
    let _ = wrapping_mul(wrapping_add(seed, i), 2654435761);
}
fn main() -> u64 {
    let seed: u64 = 7;
    parallel_for(in seed, 100, mix);
    seed
}
",
        7,
    );
    // An entry cannot carry a declared effect, so the `Alloc` body is
    // checked only.
    accepted(
        "\
effect Alloc {}
struct Totals { sum: Atomic[u64] }
fn add(in t: Totals, i: u64) eff [Alloc] {
    t.sum.fetch_add(i, MemoryOrder::Relaxed);
}
fn main() -> u64 eff [Alloc] {
    let t = Totals { sum: Atomic::new(0) };
    parallel_for(in t, 10, add);
    t.sum.load(MemoryOrder::SeqCst)
}
",
    );
    rejects(
        "\
effect Alloc { fn grab(n: u64) -> u64; }
fn take(in s: u64, i: u64) eff [Alloc] { let _ = Alloc::grab(i); }
fn main() -> u64 {
    let seed: u64 = 1;
    parallel_for(in seed, 4, take);
    0
}
",
        "E0823",
        "named function whose effects need no handler",
    );
}

#[test]
fn a_user_parallel_for_takes_precedence() {
    differential(
        "user_parallel_for",
        "\
fn parallel_for(in n: u64) -> u64 { wrapping_add(n, 1) }
fn main() -> u64 {
    let n: u64 = 41;
    parallel_for(in n)
}
",
        42,
    );
}

/// `cargo test --release --test scoped_workers -- --ignored --nocapture`:
/// a 4-worker parallel sum of 2^24 `u64`s in optimized JIT code against the
/// same split in Rust on four scoped threads, and each against its own
/// sequential loop. A Vesper run's time per sum is the difference between
/// `REPS` sums and none, so the fill and the JIT compile cancel out.
#[test]
#[ignore]
fn measure_parallel_sum_against_rust() {
    // SAFETY: the variable is read once, when the pool starts, and no other
    // test in this binary reads or writes it.
    unsafe { std::env::set_var("VESPER_WORKERS", "4") };
    const N: u64 = 1 << 24;
    const REPS: u64 = 20;
    const MIX: u64 = 2654435761;
    let program = |reps: u64, parallel: bool| {
        let call = if parallel {
            "parallel_for(in job, 4, sum_chunk);"
        } else {
            "sequential(in job, 4);"
        };
        format!(
            "\
struct Job {{ xs: ChunkSeq[u64], chunk: u64, total: Atomic[u64] }}
fn sum_chunk(in job: Job, i: u64) {{
    let mut s: u64 = 0;
    let lo: u64 = wrapping_mul(i, job.chunk);
    for k in 0..job.chunk {{
        match job.xs.try_get(wrapping_add(lo, k)) {{
            Option::Some(v) => {{ s = wrapping_add(s, v); }},
            Option::None => {{}},
        }}
    }}
    job.total.fetch_add(s, MemoryOrder::Relaxed);
}}
fn sequential(in job: Job, ways: u64) {{
    for i in 0..ways {{ sum_chunk(in job, i); }}
}}
fn main() -> u64 {{
    let mut xs: ChunkSeq[u64] = ChunkSeq::new[u64]();
    let n: u64 = {N};
    for i in 0..n {{ xs.push(wrapping_mul(i, {MIX})); }}
    let job = Job {{ xs: xs, chunk: {chunk}, total: Atomic::new(0) }};
    let reps: u64 = {reps};
    for r in 0..reps {{ {call} }}
    job.total.load(MemoryOrder::SeqCst)
}}
",
            chunk = N / 4
        )
    };
    let one = (0..N).fold(0u64, |a, i| a.wrapping_add(i.wrapping_mul(MIX)));
    let run = |reps: u64, parallel: bool| {
        let c = accepted(&program(reps, parallel));
        let start = std::time::Instant::now();
        let got = native::jit_with_options(&c.world, "main", true)
            .unwrap()
            .to_string();
        let elapsed = start.elapsed().as_secs_f64();
        assert_eq!(got, one.wrapping_mul(reps).to_string());
        elapsed
    };
    let xs: Vec<u64> = (0..N).map(|i| i.wrapping_mul(MIX)).collect();
    let rust = |parallel: bool| {
        let start = std::time::Instant::now();
        for _ in 0..REPS {
            let total = AtomicU64::new(0);
            let xs = std::hint::black_box(&xs);
            if parallel {
                std::thread::scope(|s| {
                    for part in xs.chunks((N / 4) as usize) {
                        let total = &total;
                        s.spawn(move || {
                            let sum = part.iter().fold(0u64, |a, &v| a.wrapping_add(v));
                            total.fetch_add(sum, Ordering::Relaxed);
                        });
                    }
                });
            } else {
                for part in xs.chunks((N / 4) as usize) {
                    let sum = part.iter().fold(0u64, |a, &v| a.wrapping_add(v));
                    total.fetch_add(sum, Ordering::Relaxed);
                }
            }
            assert_eq!(total.load(Ordering::SeqCst), one);
        }
        start.elapsed().as_secs_f64() / REPS as f64
    };
    for round in 0..3 {
        let base = run(0, true);
        let vesper_par = (run(REPS, true) - base) / REPS as f64;
        let vesper_seq = (run(REPS, false) - base) / REPS as f64;
        let rust_par = rust(true);
        let rust_seq = rust(false);
        println!(
            "sum of {N} u64, round {round}: vesper 4 workers {:.2} ms (1 thread {:.2} ms, {:.2}x), \
             rust 4 threads {:.2} ms (1 thread {:.2} ms, {:.2}x), 4-way ratio {:.2}",
            vesper_par * 1e3,
            vesper_seq * 1e3,
            vesper_seq / vesper_par,
            rust_par * 1e3,
            rust_seq * 1e3,
            rust_seq / rust_par,
            vesper_par / rust_par
        );
    }
}
