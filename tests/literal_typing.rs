//! Integer literals typed from their use (P3b gaps lane 7, #1092;
//! manuscript ch. 3, "Integer literal typing") and `wrapping_neg_k`. A
//! `let` initialized from unsuffixed literals and a `for` over a literal
//! range take the kind of the binding's later uses; without a constraining
//! use they stay `i64`. Every program runs in the evaluator and JIT
//! (optimized and not), both object targets and an executable.
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
            "vesper-literal-typing-{}-{}",
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
fn ranges_take_the_kind_of_the_loop_variable() {
    // Kinetic gap 2 / Prism H3 gap 3: `0..3` indexing a `Slice[u64]`.
    differential(
        "range_index",
        "fn get(in s: Slice[u64], i: u64) -> u64 {
            match s.try_get(i) { Option::Some(v) => v, Option::None => 0 }
        }
        fn sum(in s: Slice[u64]) -> u64 {
            let mut t: u64 = 0;
            for a in 0..3 { t = wrapping_add(t, get(in s, a)); }
            t
        }
        fn main() -> u64 {
            let mut c: ChunkSeq[u64] = ChunkSeq::new();
            c.push(5); c.push(6); c.push(7); c.push(100);
            sum(in c.as_slice())
        }",
        18,
    );
    // The bound is an open binding too.
    differential(
        "range_bound_binding",
        "fn main() -> u64 {
            let n = 10;
            let mut t: u64 = 0;
            for i in 0..n { t = wrapping_add(i, t); }
            t
        }",
        45,
    );
    // A closed `u8` range of literals ends at 255 without overflow.
    differential(
        "range_u8",
        "fn main() -> u64 {
            let mut t: u64 = 0;
            for i in 250..=255 { let b: u8 = i; t = wrapping_add(t, widen_u64(b)); }
            t
        }",
        1515,
    );
}

#[test]
fn lets_take_the_kind_of_their_uses() {
    // Pact gap 4 / Chorus gap 4: `let x = if c { 1 } else { 2 }; x` in a
    // `u64` function.
    differential(
        "if_branches",
        "fn f(c: bool) -> u64 { let x = if c { 1 } else { 2 }; x }
        fn main() -> u64 { wrapping_add(wrapping_mul(f(true), 10), f(false)) }",
        12,
    );
    // Prism H3 gap 9: a tuple of literals passed as `u8`.
    differential(
        "tuple",
        "fn h(c: u64) -> u8 {
            let (lo, hi) = if c == 224 { (160, 191) } else { (128, 191) };
            wrapping_add(lo, hi)
        }
        fn main() -> u64 { widen_u64(h(224)) }",
        (160 + 191) & 0xff,
    );
    // A `let mut` accumulator and a chain of literal bindings.
    differential(
        "mut_and_chain",
        "fn take(x: u32) -> u32 { x }
        fn main() -> u64 {
            let a = 7;
            let b = a;
            let mut acc = 0;
            for i in 0..4 { acc = wrapping_add(acc, take(b)); }
            widen_u64(acc)
        }",
        28,
    );
}

#[test]
fn unconstrained_literals_stay_i64() {
    // No use constrains `x`: it is `i64`, so -1 stays representable.
    differential(
        "default",
        "fn main() -> u64 { let x = 5; let y = wrapping_sub(x, 6); wrapping_u64(y) }",
        u64::MAX,
    );
    // Uses that disagree leave the binding `i64`; the program is still
    // rejected as before.
    rejects(
        "fn a(x: u8) -> u8 { x }
        fn b(x: u16) -> u16 { x }
        fn main() -> u64 { let t = 5; let _ = a(t); let _ = b(t); 0 }",
        "E0200",
        "expected u8",
    );
    // A literal binding hinted to a kind it does not fit is E0209 there.
    rejects(
        "fn a(x: u8) -> u8 { x }
        fn main() -> u64 { let t = 300; widen_u64(a(t)) }",
        "E0209",
        "outside the range of u8",
    );
}

#[test]
fn wrapping_neg_for_every_kind() {
    use vesper::numeric::INTEGER_KINDS;
    for k in INTEGER_KINDS {
        let name = k.name();
        let (lo, hi) = (k.min(), k.max());
        let mut sum: u64 = 0;
        let mut body = String::new();
        for v in [0i128, 1, 2, lo, hi, lo + 1, hi - 1] {
            let neg = k.wrap(-v);
            sum = sum.wrapping_mul(31).wrapping_add(neg as u64);
            let init = if v < 0 && v == lo {
                format!(
                    "let v: {name} = {}; let v: {name} = wrapping_sub(v, 1);",
                    lo + 1
                )
            } else {
                format!("let v: {name} = {v};")
            };
            body.push_str(&format!(
                "{init} acc = wrapping_add(wrapping_mul(acc, 31), wrapping_u64(wrapping_neg_{name}(v)));\n"
            ));
        }
        let source = format!("fn main() -> u64 {{ let mut acc: u64 = 0;\n{body} acc }}");
        differential(&format!("neg_{name}"), &source, sum);
    }
}
