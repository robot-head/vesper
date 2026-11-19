//! `continue` (P3b gaps lane 7, #1092; manuscript `ContinueExpr`): it
//! starts the next iteration of the innermost `loop` or `for`. The
//! evaluator (optimized and not), the JIT (optimized and not), both object
//! targets and, where a C driver links executables, an AOT executable agree;
//! the JIT leaves no sequence for teardown. Mode rules match `break`'s: a
//! `continue` outside a loop, or one leaving a closure or handled body, is
//! E0213; a value moved on a path that continues is E0312; an unconsumed
//! loop-local linear value is E0302.
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{check, native, parser, runtime::Runtime, seq::native_runtime};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-continue-{}-{}",
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
        "expected {code} '{message}' for {source}: {:?}",
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

fn executable_status(name: &str, source: &str) -> Option<i32> {
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
    Command::new(&exe).output().unwrap().status.code()
}

/// Runs `main` everywhere and checks each result against `expected`.
fn differential(name: &str, source: &str, expected: u64) {
    let c = accepted(source);
    for optimize in [true, false] {
        let mut runtime = Runtime::new(&c.world);
        runtime.optimize = optimize;
        runtime.fuel = 200_000_000;
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
        assert_eq!(
            native_runtime::teardown_frees(),
            0,
            "{name}: JIT (optimize {optimize}) left sequences for teardown"
        );
    }
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        native::object_for_target(&c.world, "main", Some(target))
            .unwrap_or_else(|e| panic!("{name}: {target} object failed: {e}"));
    }
    if links_executables() {
        assert_eq!(
            executable_status(name, source),
            Some((expected & 0xff) as i32),
            "{name}: executable"
        );
    }
}

#[test]
fn continue_in_loop_and_for() {
    // Odd numbers below 20 in a `loop`: 100.
    differential(
        "loop_odd",
        "fn main() -> i64 {
            let mut i = 0;
            let mut s = 0;
            loop {
                i = wrapping_add(i, 1);
                if i >= 20 { break; }
                if i % 2 == 0 { continue; }
                s = wrapping_add(s, i);
            }
            s
        }",
        100,
    );
    // Numbers below 30 that are not multiples of 3: 435 - 135 = 300.
    differential(
        "for_skip",
        "fn main() -> i64 {
            let mut s = 0;
            for i in 0..30 {
                if i % 3 == 0 { continue; }
                s = wrapping_add(s, i);
            }
            s
        }",
        300,
    );
    // `continue` as a match arm and as a block's tail.
    differential(
        "match_arm",
        "fn main() -> i64 {
            let mut s = 0;
            for i in 0..10 {
                let w = match i % 4 { 0 => continue, 1 => 10, _ => 1 };
                if i == 9 { continue }
                s = wrapping_add(s, w);
            }
            s
        }",
        // i = 1, 5 give 10 (9 is skipped); 2, 3, 6, 7 give 1: 24.
        24,
    );
}

#[test]
fn continue_at_the_last_value_of_a_closed_range_does_not_step_past_it() {
    // The step block leaves `a..=b` at its bound before incrementing, so a
    // `continue` at `u8` 255 neither overflows nor wraps around.
    differential(
        "closed_u8",
        "fn main() -> u64 {
            let lo: u8 = 250;
            let hi: u8 = 255;
            let mut s: u64 = 0;
            let mut n: u64 = 0;
            for i in lo..=hi {
                n = wrapping_add(n, 1);
                if i == hi { continue; }
                if i == 252 { continue; }
                s = wrapping_add(s, widen_u64(i));
            }
            wrapping_add(wrapping_mul(s, 10), n)
        }",
        // 250 + 251 + 253 + 254 = 1008; six iterations.
        10086,
    );
    differential(
        "closed_single",
        "fn main() -> i64 {
            let mut n = 0;
            for i in 7..=7 {
                n = wrapping_add(n, i);
                continue;
            }
            n
        }",
        7,
    );
}

#[test]
fn continue_targets_the_innermost_loop() {
    differential(
        "nested",
        "fn main() -> i64 {
            let mut s = 0;
            for i in 0..5 {
                let mut j = 0;
                loop {
                    j = wrapping_add(j, 1);
                    if j > 4 { break; }
                    if j == i { continue; }
                    s = wrapping_add(s, 1);
                }
                if i == 2 { continue; }
                s = wrapping_add(s, 100);
            }
            s
        }",
        // Inner: 4 per i, minus one for i in 1..=4: 20 - 4 = 16; outer: 400.
        416,
    );
}

#[test]
fn continue_drops_loop_locals() {
    // A sequence made in the body and a string built in it are dropped on
    // the `continue` path as on the body's end.
    differential(
        "drops",
        "fn main() -> u64 {
            let mut total: u64 = 0;
            for i in 0..50 {
                let mut s: ChunkSeq[u64] = ChunkSeq::new();
                s.push(wrapping_u64(i));
                s.push(2);
                if i % 2 == 0 { continue; }
                total = wrapping_add(total, s.len());
            }
            total
        }",
        50,
    );
}

#[test]
fn continue_is_a_contextual_word() {
    // A binding or a function named `continue` keeps its meaning.
    differential(
        "names",
        "fn continue(x: i64) -> i64 { wrapping_add(x, 1) }
        fn main() -> i64 {
            let a = continue(1);
            let continue = 4;
            wrapping_add(a, continue)
        }",
        6,
    );
}

#[test]
fn continue_follows_the_mode_rules() {
    rejects(
        "fn main() -> i64 { continue; 1 }",
        "E0213",
        "'continue' outside of a loop",
    );
    rejects(
        "fn main() -> i64 {
            loop {
                let f = || { continue; 1 };
                break f();
            }
        }",
        "E0213",
        "'continue' outside of a loop",
    );
    rejects(
        "linear struct Token { n: i64 }
        fn eat(sink t: Token) -> i64 { let Token { n } = t; n }
        fn main() -> i64 {
            let t = Token { n: 1 };
            let mut i = 0;
            loop {
                i = wrapping_add(i, 1);
                if i < 3 { let _ = eat(t); continue; }
                break 0;
            }
        }",
        "E0312",
        "moved in a previous iteration",
    );
    rejects(
        "linear struct Token { n: i64 }
        fn main() -> i64 {
            for i in 0..3 {
                let t = Token { n: i };
                continue;
            }
            0
        }",
        "E0302",
        "",
    );
}

#[test]
fn continue_formats_and_highlights() {
    let source = "fn main() -> i64 {\n    let mut s = 0;\n    for i in 0..3 {\n        if i == 1 {\n            continue;\n        }\n        s = wrapping_add(s, i);\n    }\n    s\n}\n";
    let formatted = vesper_fmt::format_source(source, vesper_fmt::Limits::default()).unwrap();
    assert_eq!(formatted.output, source);
    assert!(!formatted.changed);
    // Unformatted input reaches the same layout.
    let squeezed = "fn main() -> i64 { let mut s = 0; for i in 0..3 { if i == 1 { continue; } s = wrapping_add(s, i); } s }";
    let formatted = vesper_fmt::format_source(squeezed, vesper_fmt::Limits::default()).unwrap();
    assert_eq!(formatted.output, source);
}
