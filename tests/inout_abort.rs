//! Keep-writes for abortable effects across stationary `inout` (manuscript
//! §4.3 `PostCall_abort`, issue #935). An abortable effect (`Fail`, `Cancel`,
//! any non-linear effect) may leave a call while an `inout` argument is
//! live, and the caller's place then holds the last value the callee wrote.
//! The checker accepts and rejects the cases below, and the evaluator, the
//! Cranelift JIT, an AOT executable and the extracted Lean model agree on the
//! post-abort value.
use std::process::Command;
use vesper::{check, native, parser, runtime::Runtime};

fn diagnostics(source: &str) -> Vec<String> {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "parse: {:?}", p.diagnostics);
    check::check(&p.module)
        .diagnostics
        .into_iter()
        .map(|d| format!("{} {}", d.code, d.message))
        .collect()
}
fn accepts(source: &str) {
    let d = diagnostics(source);
    assert!(d.is_empty(), "{source}\n{d:#?}");
}
fn rejects(source: &str, code: &str, message: &str) {
    let d = diagnostics(source);
    assert!(
        d.iter().any(|d| d.starts_with(code) && d.contains(message)),
        "expected {code} '{message}' for {source}\n{d:#?}"
    );
}
fn world(source: &str) -> check::World {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{source}\n{:?}", c.diagnostics);
    c.world
}
fn big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}
/// The evaluator and the JIT must both produce `expected`.
fn agree(source: &'static str, expected: &'static str) {
    big_stack(move || {
        let w = world(source);
        let interpreted = Runtime::new(&w).run("main", vec![]).unwrap().to_string();
        assert_eq!(interpreted, expected, "evaluator: {source}");
        let jit = native::jit(&w, "main").unwrap().to_string();
        assert_eq!(jit, expected, "Cranelift JIT: {source}");
    });
}

const BUMP: &str = "fn bump(inout x: i64, limit: i64) eff [Fail[i64]] { x = wrapping_add(x, 1); if x > limit { Fail::raise(x) } x = wrapping_add(x, 10); }";

#[test]
fn callee_writes_survive_a_caught_abort() {
    agree(
        "fn bump(inout x: i64, limit: i64) eff [Fail[i64]] { x = wrapping_add(x, 1); if x > limit { Fail::raise(x) } x = wrapping_add(x, 10); } \
         fn main() -> i64 { let mut x = 5; let r = handle Fail[i64] in { bump(inout x, 0); 0 } with { return v => v, raise(e, k) => e, }; wrapping_add(wrapping_mul(x, 10), r) }",
        "66",
    );
    // The normal path still applies every write.
    agree(
        "fn bump(inout x: i64, limit: i64) eff [Fail[i64]] { x = wrapping_add(x, 1); if x > limit { Fail::raise(x) } x = wrapping_add(x, 10); } \
         fn main() -> i64 { let mut x = 5; let r = handle Fail[i64] in { bump(inout x, 100); 0 } with { return v => v, raise(e, k) => e, }; wrapping_add(wrapping_mul(x, 10), r) }",
        "160",
    );
}

#[test]
fn writes_survive_through_two_fallible_frames() {
    agree(
        "fn bump(inout x: i64, limit: i64) eff [Fail[i64]] { x = wrapping_add(x, 1); if x > limit { Fail::raise(x) } x = wrapping_add(x, 10); } \
         fn outer(inout x: i64) eff [Fail[i64]] { x = wrapping_add(x, 2); bump(inout x, 0); x = 99; } \
         fn main() -> i64 { let mut x = 1; let r = handle Fail[i64] in { outer(inout x); 0 } with { return v => v, raise(e, k) => e, }; wrapping_add(wrapping_mul(x, 10), r) }",
        "44",
    );
}

#[test]
fn projected_struct_writes_survive_and_siblings_stay() {
    agree(
        "copy struct P { a: i64, b: i64 } \
         fn seta(inout p: P) eff [Fail[i64]] { p.a = 7; Fail::raise(1) } \
         fn main() -> i64 { let mut p = P { a: 1, b: 2 }; let r = handle Fail[i64] in { seta(inout p); 0 } with { return v => v, raise(e, k) => e, }; wrapping_add(wrapping_add(wrapping_mul(p.a, 100), wrapping_mul(p.b, 10)), r) }",
        "721",
    );
}

#[test]
fn sibling_argument_abort_writes_nothing() {
    agree(
        "fn set(inout x: i64, v: i64) { x = v; } fn fails() -> i64 eff [Fail[i64]] { Fail::raise(3) } \
         fn main() -> i64 { let mut x = 1; let r = handle Fail[i64] in { set(inout x, fails()); 0 } with { return v => v, raise(e, k) => e, }; wrapping_add(wrapping_mul(x, 10), r) }",
        "13",
    );
}

#[test]
fn a_handler_inside_the_callee_catches_before_return() {
    agree(
        "fn local(inout x: i64) -> i64 { x = 3; handle Fail[i64] in { x = 4; Fail::raise(9) } with { return v => v, raise(e, k) => e, } } \
         fn main() -> i64 { let mut x = 0; let r = local(inout x); wrapping_add(wrapping_mul(x, 10), r) }",
        "49",
    );
}

#[test]
fn cancel_like_abortable_effects_behave_like_fail() {
    agree(
        "effect Stop { fn stop() -> Never; } \
         fn halt(inout x: i64) eff [Stop] { x = 7; Stop::stop() } \
         fn main() -> i64 { let mut x = 0; let r = handle Stop in { halt(inout x); 0 } with { return v => v, stop(k) => 1, }; wrapping_add(wrapping_mul(x, 10), r) }",
        "71",
    );
}

#[test]
fn checked_arithmetic_keeps_the_last_write() {
    agree(
        "fn inc(inout x: u8) eff [Fail[ArithOverflow]] { x = 254; x += 1; x += 1; } \
         fn main() -> u8 { let mut x: u8 = 0; let r = handle Fail[ArithOverflow] in { inc(inout x); 0 } with { return v => v, raise(e, k) => 1, }; x }",
        "255",
    );
}

#[test]
fn a_linear_place_may_be_borrowed_across_an_abort_its_owner_handles() {
    agree(
        "linear struct Tx { n: i64 } \
         fn append(inout tx: Tx, v: i64) eff [Fail[i64]] { tx.n = v; Fail::raise(v) } \
         fn main() -> i64 { let mut tx = Tx { n: 0 }; let r = handle Fail[i64] in { append(inout tx, 5); 0 } with { return v => v, raise(e, k) => e, }; let Tx { n } = tx; wrapping_add(wrapping_mul(n, 10), r) }",
        "55",
    );
}

#[test]
fn checker_accepts_abortable_effects_across_stationary_inout() {
    accepts(BUMP);
    accepts("fn bad(inout x: i64) eff [Fail[ArithOverflow]] { x += 1; }");
    accepts(
        "effect Ask { fn ask() -> i64; } fn both(inout x: i64, in y: i64) {} fn ok(inout x: i64) eff [Ask] { both(inout x, Ask::ask()); }",
    );
    // An open row may abort; the same rule applies.
    accepts("fn each[r: eff](inout x: i64, sink f: fn() eff [r]) eff [r] { x = 1; f(); }");
}

#[test]
fn checker_keeps_linear_ownership_rules() {
    let tx = "linear struct Tx { n: i64 } fn append(inout tx: Tx, v: i64) eff [Fail[i64]] { tx.n = v; Fail::raise(v) } ";
    // The owner cannot let the abort unwind its own frame.
    rejects(
        &format!(
            "{tx} fn caller() eff [Fail[i64]] {{ let mut tx = Tx {{ n: 0 }}; append(inout tx, 1); let Tx {{ n }} = tx; }}"
        ),
        "E0402",
        "may discard live linear value 'tx'",
    );
    // A handled body may not consume an outer linear value that an abort it
    // catches would leave live.
    rejects(
        &format!(
            "{tx} fn main() -> i64 {{ let mut tx = Tx {{ n: 0 }}; handle Fail[i64] in {{ append(inout tx, 5); let Tx {{ n }} = tx; n }} with {{ return v => v, raise(e, k) => e, }} }}"
        ),
        "E0402",
        "consumed inside a handled body",
    );
    // A linear value declared inside the handled body is still discarded.
    rejects(
        &format!(
            "{tx} fn main() -> i64 {{ handle Fail[i64] in {{ let mut tx = Tx {{ n: 0 }}; append(inout tx, 5); let Tx {{ n }} = tx; n }} with {{ return v => v, raise(e, k) => e, }} }}"
        ),
        "E0402",
        "may discard live linear value 'tx'",
    );
    // Linear handler state and sink linear values are unchanged.
    rejects(
        "linear struct Tx {} fn bad(sink tx: Tx) eff [Fail[i64]] { Fail::raise(1); let Tx {} = tx; }",
        "E0402",
        "may discard live linear value 'tx'",
    );
}

#[test]
fn clauses_see_entry_snapshots_and_joins_forget_overwritten_facts() {
    // A clause captures `x` as it was when the handler was entered.
    agree(
        "fn zero(inout x: i64) eff [Fail[i64]] { x = 0; Fail::raise(1) } \
         fn main() -> i64 { let mut x = 5; let r = handle Fail[i64] in { zero(inout x); 0 } with { return v => v, raise(e, k) => 100 / x, }; wrapping_add(wrapping_mul(x, 100), r) }",
        "20",
    );
    // After an abortable handler, the abort edge may skip the body's writes,
    // or keep a write an `inout` callee made before failing.
    rejects(
        "fn main() -> i64 { let mut x = 0; let c = true; handle Fail[i64] in { if c { Fail::raise(1) } x = 5; 0 } with { return v => v, raise(e, k) => e, }; 100 / x }",
        "E0401",
        "",
    );
    rejects(
        "fn zero(inout x: i64) eff [Fail[i64]] { x = 0; Fail::raise(1) } \
         fn main() -> i64 { let mut x = 5; handle Fail[i64] in { zero(inout x); x = 5; 0 } with { return v => v, raise(e, k) => e, }; 100 / x }",
        "E0401",
        "",
    );
    // Facts about places the body leaves alone survive the join.
    accepts(
        "fn main() -> i64 { let x = 5; let mut y = 0; let r = handle Fail[i64] in { y = 1; if x > 0 { Fail::raise(1) } 0 } with { return v => v, raise(e, k) => e, }; wrapping_add(100 / x, r) }",
    );
}

fn lean() -> Option<String> {
    let compiler = std::env::var("VESPER_LEAN").unwrap_or_else(|_| "lean".into());
    Command::new(&compiler)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|_| compiler)
}

/// The extracted model keeps writes on the `.error` edge: run at
/// `ExceptT E (StateM Σ)` with `$sync := set`, and inside the extracted
/// `main`, whose local handler observes the callee's last write.
#[test]
fn lean_model_agrees_on_the_post_abort_value() {
    let source = "fn bump(inout x: i64, limit: i64) eff [Fail[i64]] { x = wrapping_add(x, 1); if x > limit { Fail::raise(x) } x = wrapping_add(x, 10); } \
                  fn outer(inout x: i64) eff [Fail[i64]] { x = wrapping_add(x, 2); bump(inout x, 0); x = 99; } \
                  fn main() -> i64 { let mut x = 1; let r = handle Fail[i64] in { outer(inout x); 0 } with { return v => v, raise(e, k) => e, }; wrapping_add(wrapping_mul(x, 10), r) }";
    let w = world(source);
    let evaluated = Runtime::new(&w).run("main", vec![]).unwrap().to_string();
    assert_eq!(evaluated, "44");
    let text = vesper::lean::extract(&w).unwrap();
    assert!(text.contains("«$sync»"), "{text}");
    let Some(compiler) = lean() else {
        eprintln!("Lean validation skipped: set VESPER_LEAN or install the pinned toolchain");
        return;
    };
    let file = format!(
        "{text}\n\
         #eval Vesper.main\n\
         #eval ((Vesper.bump (m := ExceptT Int64 (StateM Int64)) 5 0 («$sync» := set)).run.run 0)\n\
         #eval ((Vesper.bump (m := ExceptT Int64 (StateM Int64)) 5 100 («$sync» := set)).run.run 0)\n\
         #eval ((Vesper.outer (m := ExceptT Int64 (StateM Int64)) 1 («$sync» := set)).run.run 0)\n"
    );
    let path = std::env::temp_dir().join(format!("vesper-inout-abort-{}.lean", std::process::id()));
    std::fs::write(&path, &file).unwrap();
    let result = Command::new(&compiler).arg(&path).output().unwrap();
    let _ = std::fs::remove_file(&path);
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        result.status.success(),
        "{stdout}\n{}\n{file}",
        String::from_utf8_lossy(&result.stderr)
    );
    let lines = stdout
        .lines()
        .filter(|l| !l.contains("warning") && !l.trim().is_empty())
        .collect::<Vec<_>>();
    assert!(lines.contains(&"44"), "{stdout}");
    // The abort keeps x = 6; a normal return gives 16 and leaves the state
    // as the last sync before the final write.
    assert!(lines.contains(&"(Except.error 6, 6)"), "{stdout}");
    assert!(
        lines.iter().any(|l| l.starts_with("(Except.ok (")),
        "{stdout}"
    );
    assert!(lines.contains(&"(Except.error 4, 4)"), "{stdout}");
}

/// Failure points that do not go through an effect call (checked arithmetic,
/// `?`), a shadowed parameter name, and a return clause that aborts after
/// the handled body wrote an `inout` parameter: all keep the last write in
/// the evaluator, the JIT and the extracted Lean model. (One handler per
/// function: the extractor does not yet support two sequential handlers in
/// one function body, independently of this change.)
#[test]
fn lean_model_keeps_writes_on_primitive_failures_shadows_and_return_clauses() {
    const SOURCE: &str = "effect Ask { fn ask() -> i64; } \
        fn inc(inout x: u8) eff [Fail[ArithOverflow]] { x = 254; x += 1; x += 1; } \
        fn tryq(inout x: i64) eff [Fail[i64]] { x = 3; let r: Result[i64, i64] = Err(1); r?; x = 4; } \
        fn shadow(inout x: i64) eff [Fail[i64]] { x = 7; let x: i64 = 99; Fail::raise(x) } \
        fn retc(inout x: i64) -> i64 eff [Fail[i64]] { handle Ask in { x = 8; 0 } with { return v => Fail::raise(1), ask(k) => k(0), } } \
        fn ta() -> u8 { let mut a: u8 = 0; let r = handle Fail[ArithOverflow] in { inc(inout a); 0 } with { return v => v, raise(e, k) => 1, }; a } \
        fn tb() -> i64 { let mut b = 0; let r = handle Fail[i64] in { tryq(inout b); 0 } with { return v => v, raise(e, k) => e, }; b } \
        fn tc() -> i64 { let mut c = 0; let r = handle Fail[i64] in { shadow(inout c); 0 } with { return v => v, raise(e, k) => e, }; c } \
        fn td() -> i64 { let mut d = 0; let r = handle Fail[i64] in { retc(inout d) } with { return v => v, raise(e, k) => e, }; d } \
        fn main() -> (u8, i64, i64, i64) { (ta(), tb(), tc(), td()) }";
    big_stack(|| {
        let w = world(SOURCE);
        let interpreted = Runtime::new(&w).run("main", vec![]).unwrap().to_string();
        let jit = native::jit(&w, "main").unwrap().to_string();
        assert_eq!(jit, interpreted);
        assert!(
            interpreted.contains("255")
                && interpreted.contains('3')
                && interpreted.contains('7')
                && interpreted.contains('8'),
            "{interpreted}"
        );
        let text = vesper::lean::extract(&w).unwrap();
        let Some(compiler) = lean() else {
            eprintln!("Lean validation skipped: set VESPER_LEAN or install the pinned toolchain");
            return;
        };
        let file = format!("{text}\n#eval Vesper.main\n");
        let path = std::env::temp_dir().join(format!(
            "vesper-inout-abort-primitive-{}.lean",
            std::process::id()
        ));
        std::fs::write(&path, &file).unwrap();
        let result = Command::new(&compiler).arg(&path).output().unwrap();
        let _ = std::fs::remove_file(&path);
        let stdout = String::from_utf8_lossy(&result.stdout);
        assert!(
            result.status.success(),
            "{stdout}\n{}\n{file}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(stdout.contains("(255, 3, 7, 8)"), "{stdout}");
    });
}
