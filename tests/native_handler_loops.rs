//! Resuming handlers around loops (P3b gaps lane 10, #1095, part 2). The
//! specializing native backend inlines each clause at its operation and
//! resumes by jumping back into the handled body, saving its return point in
//! one slot per operation site. An operation performed from a loop resumed
//! again before the first resumption returned, overwrote that slot, and the
//! dispatcher then returned to the same point forever:
//! `handle Net in { for i in 0..3 { Net::emit(1) } }` hung natively.
//!
//! A clause that ends in its resumption (`emit(v, k) => k(..)`, also through
//! `if`/`match` arms) now resumes with its own return point, so nothing is
//! saved. The evaluator (optimized and not), the JIT (optimized and not),
//! AArch64 and x86-64 objects and an executable agree on loops, `break`,
//! nested handlers, stateful handlers, looping callees, sequences built in
//! the loop and calls through `dyn`. A clause with work after its resumption
//! is still refused natively at an operation inside a loop (a typed error,
//! not a hang); outside a loop it keeps working.
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};
use vesper::{check, native, native_handlers, parser, runtime::Runtime, seq::native_runtime};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn world(source: &str) -> check::World {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "parse: {:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "check: {:#?}", c.diagnostics);
    c.world
}

/// The JIT result, or a failure (instead of a hang) after a minute.
fn jit(source: &str, optimize: bool) -> Result<String, String> {
    let source = source.to_string();
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let w = world(&source);
        let stray = native_runtime::stray_frees();
        let result = native::jit_with_options(&w, "main", optimize).map(|v| v.to_string());
        let teardown = native_runtime::teardown_frees();
        let strays = native_runtime::stray_frees() - stray;
        let _ = send.send((result, teardown, strays));
    });
    let (result, teardown, strays) = receive
        .recv_timeout(Duration::from_secs(60))
        .unwrap_or_else(|_| panic!("JIT (optimize {optimize}) did not finish: hang"));
    let value = result.map_err(|e| e.to_string())?;
    assert_eq!(teardown, 0, "JIT (optimize {optimize}) leaked sequences");
    assert_eq!(strays, 0, "JIT (optimize {optimize}) freed a dead sequence");
    Ok(value)
}

fn links_executables() -> bool {
    cfg!(unix)
        && Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
}

fn executable_status(source: &str) -> Option<i32> {
    let dir = std::env::temp_dir().join(format!(
        "vesper-handler-loops-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.ves");
    fs::write(&file, source).unwrap();
    let exe: PathBuf = dir.join("main");
    let built = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "vesper build failed\n{}{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    let mut child = Command::new(&exe).spawn().unwrap();
    let mut status = None;
    for _ in 0..600 {
        if let Some(s) = child.try_wait().unwrap() {
            status = Some(s);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let status = status.unwrap_or_else(|| {
        let _ = child.kill();
        panic!("executable did not finish: hang")
    });
    let _ = fs::remove_dir_all(&dir);
    status.code()
}

/// Every backend runs `main` and agrees on `expected`.
fn agree(source: &str, expected: i64) {
    let w = world(source);
    for optimize in [true, false] {
        let mut runtime = Runtime::new(&w);
        runtime.optimize = optimize;
        let value = runtime
            .run("main", vec![])
            .unwrap_or_else(|e| panic!("evaluator (optimize {optimize}): {e}"))
            .to_string();
        assert_eq!(
            value,
            expected.to_string(),
            "evaluator (optimize {optimize})"
        );
        let value = jit(source, optimize)
            .unwrap_or_else(|e| panic!("JIT (optimize {optimize}) failed: {e}"));
        assert_eq!(value, expected.to_string(), "JIT (optimize {optimize})");
    }
    for target in ["aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu"] {
        let (object, _) = native::object_for_target(&w, "main", Some(target))
            .unwrap_or_else(|e| panic!("{target} object: {e}"));
        assert_eq!(&object[..4], b"\x7fELF");
    }
    if links_executables() {
        assert_eq!(
            executable_status(source),
            Some((expected & 0xff) as i32),
            "executable"
        );
    }
}

const NET: &str = "linear effect Net { fn emit(v: i64) -> i64; }
linear effect Log { fn note(v: i64) -> i64; }
";

#[test]
fn the_report_repro() {
    agree(
        &format!(
            "{NET}fn main() -> i64 {{
    handle Net in {{ for i in 0..3 {{ Net::emit(1); }} }} with {{
        return x => 7,
        emit(v, k) => k(v),
    }}
}}
"
        ),
        7,
    );
}

#[test]
fn a_loop_sums_resumed_values() {
    agree(
        &format!(
            "{NET}fn main() -> i64 {{
    let mut total: i64 = 0;
    handle Net in {{
        for i in 0..3 {{ total = wrapping_add(total, Net::emit(1)); }}
        total
    }} with {{
        return x => x,
        emit(v, k) => k(wrapping_add(v, 10)),
    }}
}}
"
        ),
        33,
    );
}

#[test]
fn a_stateful_handler_counts_every_iteration() {
    // t = 2 * (0 + 1 + 2 + 3 + 4) = 20; five emits.
    agree(
        &format!(
            "{NET}fn main() -> i64 {{
    handle Net state sent = 0 in {{
        let mut t: i64 = 0;
        for i in 0..5 {{ t = wrapping_add(t, Net::emit(i)); }}
        t
    }} with {{
        return (x, sent) => wrapping_add(wrapping_mul(x, 100), sent),
        emit(v, sent, k) => k(wrapping_mul(v, 2), wrapping_add(sent, 1)),
    }}
}}
"
        ),
        2005,
    );
}

#[test]
fn break_leaves_the_loop_and_arms_resume_in_tail_position() {
    // 0 + 1 + 102 + 103 = 206.
    agree(
        &format!(
            "{NET}fn main() -> i64 {{
    handle Net in {{
        let mut t: i64 = 0;
        let mut i: i64 = 0;
        loop {{
            if i == 4 {{ break; }}
            t = wrapping_add(t, Net::emit(i));
            i = wrapping_add(i, 1);
        }}
        t
    }} with {{
        return x => x,
        emit(v, k) => if v > 1 {{ k(wrapping_add(v, 100)) }} else {{ k(v) }},
    }}
}}
"
        ),
        206,
    );
}

#[test]
fn nested_handlers_inside_an_outer_loop() {
    // Each inner run: 3 * (0 + 1 + 2 + 3) = 18, four notes; three runs.
    agree(
        &format!(
            "{NET}fn main() -> i64 {{
    handle Log state notes = 0 in {{
        let mut t: i64 = 0;
        for j in 0..3 {{
            let inner = handle Net in {{
                let mut u: i64 = 0;
                for i in 0..4 {{ u = wrapping_add(u, Net::emit(i)); Log::note(i); }}
                u
            }} with {{
                return x => x,
                emit(v, k) => k(wrapping_mul(v, 3)),
            }};
            t = wrapping_add(t, inner);
        }}
        t
    }} with {{
        return (x, notes) => wrapping_add(wrapping_mul(x, 1000), notes),
        note(v, notes, k) => k(v, wrapping_add(notes, 1)),
    }}
}}
"
        ),
        54 * 1000 + 12,
    );
}

#[test]
fn a_looping_callee_performs_the_operation() {
    // r = 0, 1, 2: pump(2) = 1 + 2 = 3, pump(3) = 6, pump(4) = 10.
    agree(
        &format!(
            "{NET}fn pump(n: i64) -> i64 eff [Net] {{
    let mut s: i64 = 0;
    for i in 0..n {{ s = wrapping_add(s, Net::emit(i)); }}
    s
}}
fn main() -> i64 {{
    let mut e: i64 = 0;
    for r in 0..3 {{
        let f = handle Net in {{ pump(wrapping_add(r, 2)) }} with {{
            return x => x,
            emit(v, k) => k(wrapping_add(v, 1)),
        }};
        e = wrapping_add(wrapping_mul(e, 100), f);
    }}
    e
}}
"
        ),
        30610,
    );
}

#[test]
fn a_sequence_built_in_the_loop_is_freed() {
    // [10, 11, 12, 13]: sum 46, length 4.
    agree(
        &format!(
            "{NET}fn main() -> i64 {{
    handle Net in {{
        let mut s: ChunkSeq[i64] = ChunkSeq::new[i64]();
        for i in 0..4 {{ s.push(Net::emit(i)); }}
        let mut t: i64 = 0;
        for i in 0..s.len() {{
            t = match s.try_get(i) {{
                Option::Some(x) => wrapping_add(t, x),
                Option::None => t,
            }};
        }}
        wrapping_add(wrapping_mul(t, 10), wrapping_i64(s.len()))
    }} with {{
        return x => x,
        emit(v, k) => k(wrapping_add(v, 10)),
    }}
}}
"
        ),
        464,
    );
}

#[test]
fn calls_through_dyn_in_a_loop() {
    // Echo: emit(i + 1) doubled, i = 0..3: 2 * (1 + 2 + 3) = 12; three emits.
    agree(
        &format!(
            "{NET}struct Echo {{ k: i64 }}
trait Transport {{ fn send(inout self, v: i64) -> i64 eff [Net]; }}
impl Transport for Echo {{
    fn send(inout self, v: i64) -> i64 eff [Net] {{ Net::emit(wrapping_add(v, self.k)) }}
}}
fn pump(inout t: dyn Transport) -> i64 eff [Net] {{
    let mut s: i64 = 0;
    for i in 0..3 {{ s = wrapping_add(s, t.send(i)); }}
    s
}}
fn main() -> i64 {{
    let mut e = Echo {{ k: 1 }};
    handle Net state sent = 0 in {{ pump(inout e) }} with {{
        return (x, sent) => wrapping_add(wrapping_mul(x, 10), sent),
        emit(v, sent, k) => k(wrapping_mul(v, 2), wrapping_add(sent, 1)),
    }}
}}
"
        ),
        123,
    );
}

const NON_TAIL: &str = "linear effect Net { fn emit(v: i64) -> i64; }
fn main() -> i64 {
    handle Net in {
        let mut t: i64 = 0;
        for i in 0..3 { t = wrapping_add(t, Net::emit(i)); }
        t
    } with {
        return x => x,
        emit(v, k) => wrapping_add(k(v), 1000),
    }
}
";

/// Work after the resumption, at an operation inside a loop: refused
/// natively with a typed error, never a hang; the evaluator runs it.
#[test]
fn a_non_tail_resumption_in_a_loop_is_refused_not_hung() {
    let w = world(NON_TAIL);
    let value = Runtime::new(&w).run("main", vec![]).unwrap().to_string();
    assert_eq!(value, "3003");
    for optimize in [true, false] {
        let error = jit(NON_TAIL, optimize).unwrap_err();
        assert!(
            error.contains(native_handlers::LOOP_RESUME_REFUSAL),
            "optimize {optimize}: {error}"
        );
    }
}

/// The same clause outside a loop keeps its saved return point.
#[test]
fn a_non_tail_resumption_outside_a_loop_still_runs() {
    agree(
        &format!(
            "{NET}fn main() -> i64 {{
    let r = handle Net in {{
        let a = Net::emit(1);
        let b = Net::emit(2);
        wrapping_add(a, b)
    }} with {{
        return x => x,
        emit(v, k) => wrapping_add(k(v), 1000),
    }};
    let mut t: i64 = 0;
    for i in 0..3 {{
        t = wrapping_add(t, handle Net in {{ Net::emit(i) }} with {{
            return x => x,
            emit(v, k) => wrapping_add(k(v), 10),
        }});
    }}
    wrapping_add(wrapping_mul(r, 100), t)
}}
"
        ),
        2003 * 100 + 33,
    );
}
