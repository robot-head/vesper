//! `?` on `Option[T]` (P3b gaps lane 7, #1092; manuscript `TryExpr`): in a
//! function returning `Option`, `e?` continues with the payload of
//! `Option::Some` and returns `Option::None` otherwise. Every program runs
//! in the evaluator and JIT (optimized and not), both object targets and an
//! executable, and leaves no sequence for teardown.
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
            "vesper-try-option-{}-{}",
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

const NUM: &str = "fn num(in s: Slice[u8], i: u64) -> Option[u64] {
    match s.try_get(i) { Option::Some(b) => Option::Some(widen_u64(b)), Option::None => Option::None }
}
fn bytes(n: u64) -> ChunkSeq[u8] {
    let mut c: ChunkSeq[u8] = ChunkSeq::new();
    for i in 0..n { c.push(wrapping_u8(wrapping_add(i, 1))); }
    c
}
";

#[test]
fn some_continues_and_none_returns() {
    // Aegis gap 5: `num(in s, 0, 4)?` in an `Option`-returning function.
    let source = format!(
        "{NUM}fn pair(in s: Slice[u8], i: u64) -> Option[u64] {{
            let a = num(in s, i)?;
            let b = num(in s, wrapping_add(i, 1))?;
            Option::Some(wrapping_add(wrapping_mul(a, 10), b))
        }}
        fn or(o: Option[u64], d: u64) -> u64 {{ match o {{ Option::Some(v) => v, Option::None => d }} }}
        fn main() -> u64 {{
            let c = bytes(4);
            // 12 + 34 + 200 (index 3 has no successor) = 246.
            wrapping_add(wrapping_add(or(pair(in c.as_slice(), 0), 1), or(pair(in c.as_slice(), 2), 1)), or(pair(in c.as_slice(), 3), 200))
        }}"
    );
    differential("pair", &source, 246);
}

#[test]
fn try_option_in_a_loop_and_nested_expressions() {
    let source = format!(
        "{NUM}fn total(in s: Slice[u8], n: u64) -> Option[u64] {{
            let mut t: u64 = 0;
            for i in 0..n {{ t = wrapping_add(t, num(in s, i)?); }}
            Option::Some(t)
        }}
        fn or(o: Option[u64], d: u64) -> u64 {{ match o {{ Option::Some(v) => v, Option::None => d }} }}
        fn main() -> u64 {{
            let c = bytes(5);
            // 1+2+3+4+5 = 15; reading 6 bytes returns None (100).
            wrapping_add(or(total(in c.as_slice(), 5), 0), or(total(in c.as_slice(), 6), 100))
        }}"
    );
    differential("loop", &source, 115);
}

#[test]
fn try_option_needs_an_option_result() {
    rejects(
        &format!("{NUM}fn f(in s: Slice[u8]) -> u64 {{ num(in s, 0)? }}\nfn main() -> u64 {{ 0 }}"),
        "E0200",
        "requires a function returning Option",
    );
    rejects(
        "fn f(x: u64) -> Option[u64] { x? }\nfn main() -> u64 { 0 }",
        "E0200",
        "'?' requires Result[T, E], or Option[T]",
    );
}
