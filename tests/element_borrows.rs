//! Element views (§13, self-hosting SH2 lane 1, #1173): an element of a
//! `ChunkSeq[T]` or `Slice[T]` whose type is not copy is borrowed in place,
//! never copied out. `f(in s[i])` and `f(inout s[i])` lend it to a call (and
//! `s[i].m()` to a method), `swap` and `replace` exchange it, and
//! `match s.try_get(i)` / `match s.try_get_mut(i)` bind `Some(x)` to it
//! totally. The evaluator, both JIT configurations and an executable agree,
//! and the JIT frees every sequence exactly once.
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
            "vesper-element-borrows-{}-{}",
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
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    check::check(&p.module)
}

fn accepted(source: &str) -> check::Checked {
    let c = checked(source);
    assert!(c.diagnostics.is_empty(), "{:#?}", c.diagnostics);
    let program = vesper::ir::lower_world(&c.world);
    let d = vesper::ir_check::validate(&c.world, &program);
    assert!(d.is_empty(), "Place IR check: {d:#?}");
    c
}

fn links() -> bool {
    cfg!(unix)
        && Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
}

/// Runs `main` in the evaluator, the JIT (optimizer on and off) and, where
/// a C compiler links, an executable; all must agree. The JIT must free
/// every sequence itself and never free one that is not live.
fn differential(name: &str, source: &str) -> u64 {
    let c = accepted(source);
    let evaluated = Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap_or_else(|e| panic!("{name}: evaluator failed: {e}"))
        .to_string();
    for optimize in [true, false] {
        let stray = native_runtime::stray_frees();
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap_or_else(|e| panic!("{name}: JIT (optimize {optimize}) failed: {e}"))
            .to_string();
        assert_eq!(
            jit, evaluated,
            "{name}: JIT (optimize {optimize}) vs evaluator"
        );
        assert_eq!(
            native_runtime::teardown_frees(),
            0,
            "{name}: JIT (optimize {optimize}) left sequences for teardown"
        );
        assert_eq!(
            native_runtime::stray_frees(),
            stray,
            "{name}: JIT (optimize {optimize}) freed a sequence that was not live"
        );
    }
    let expected: u64 = evaluated.parse().unwrap();
    if links() {
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
        let run = Command::new(&exe).output().unwrap();
        assert_eq!(
            run.status.code(),
            Some((expected & 0xff) as i32),
            "{name}: executable stderr={}",
            String::from_utf8_lossy(&run.stderr)
        );
    }
    expected
}

/// `main` that runs `body` under a `Fail` handler (element indexing raises
/// `IndexOutOfBounds`).
fn handled(items: &str) -> String {
    format!(
        "{items}
fn main() -> u64 {{ handle Fail[RuntimeError] in {{ body() }} with {{ return x => x, raise(e, k) => 999, }} }}
"
    )
}

const STATE: &str = "struct State { slots: ChunkSeq[u32], n: u64 }
fn fresh(n: u64) -> State { State { slots: ChunkSeq::new[u32](), n: n } }
";

/// The census repro (gap C5): a struct that owns a sequence, read out of a
/// sequence with `try_get`. It was `E1522`; the `match` now borrows the
/// element in place.
#[test]
fn census_repro_reads_a_state_in_place() {
    let source = format!(
        "{STATE}fn first_len(in states: ChunkSeq[State]) -> u64 {{
    match states.try_get(0) {{ Some(s) => s.slots.len(), None => 0 }}
}}
fn main() -> u64 {{
    let mut states = ChunkSeq::new[State]();
    let empty = first_len(in states);
    let mut s = fresh(0);
    s.slots.push(4);
    s.slots.push(5);
    states.push(s);
    wrapping_add(wrapping_mul(empty, 100), first_len(in states))
}}
"
    );
    assert_eq!(differential("census_repro", &source), 2);
}
