//! Two-phase borrows (§4.3, port gaps lane 3, #986): the `inout` receiver
//! loan of a builtin or method call is reserved while the call's arguments
//! are evaluated and activated only at the call. Arguments may read the
//! receiver's place and see its value before the call; writes, moves and
//! `inout` borrows of it in the arguments stay refused (`E0410` in the Place
//! IR check, `E0301` in the checker). The evaluator, both JIT
//! configurations and an executable agree.
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
            "vesper-two-phase-{}-{}",
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

#[test]
fn arguments_read_the_reserved_receiver() {
    let source = "fn inc(inout s: ChunkSeq[u64], i: u64) -> bool eff [Fail[RuntimeError]] {
    s.try_set(i, wrapping_add(s.get(i), 1))
}
fn seq_u64(in s: ChunkSeq[u64], i: u64) -> u64 {
    match s.try_get(i) {
        Option::Some(x) => x,
        Option::None => 0,
    }
}
fn bump_all(inout s: ChunkSeq[u64]) {
    for i in 0..s.len() {
        let ok = s.try_set(i, wrapping_mul(seq_u64(in s, i), 3));
    }
    s.push(s.len());
    s.push(seq_u64(in s, 0));
}
struct P { x: u64, y: u64 }
impl P {
    fn bump(inout self, v: u64) { self.x = wrapping_add(self.x, v); }
    fn get(in self) -> u64 { self.x }
}
fn twice(in p: P) -> u64 { wrapping_mul(p.x, 2) }
fn run() -> u64 eff [Fail[RuntimeError]] {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    s.push(41);
    s.push(9);
    let ok = inc(inout s, 0);
    bump_all(inout s);
    s.set(1, wrapping_add(s.get(0), s.len()));
    let mut p = P { x: 2, y: 5 };
    p.bump(wrapping_add(p.y, twice(in p)));
    p.bump(p.get());
    let mut acc: u64 = if ok { 1 } else { 0 };
    for i in 0..s.len() {
        acc = wrapping_add(wrapping_mul(acc, 131), s.get(i));
    }
    wrapping_add(wrapping_mul(acc, 1000), p.x)
}
fn main() -> u64 {
    handle Fail[RuntimeError] in {
        run()
    } with {
        return x => x,
        raise(e, k) => 7777,
    }
}
";
    // s = [42, 9] -> [126, 27] -> push 2 -> [126, 27, 2] -> push 126;
    // s[1] = 126 + 4. p.x = 2 + (5 + 4) = 11, then 11 + 11.
    let s = [126u64, 130, 2, 126];
    let acc = s
        .iter()
        .fold(1u64, |acc, x| acc.wrapping_mul(131).wrapping_add(*x));
    assert_eq!(differential("reads", source), acc * 1000 + 22);
}

#[test]
fn writes_moves_and_inout_borrows_stay_refused() {
    for (body, code, needle) in [
        (
            "fn f(inout s: ChunkSeq[u64]) -> bool {
    s.try_set(0, match s.pop() { Option::Some(x) => x, Option::None => 0 })
}",
            "E0410",
            "overlaps active loan",
        ),
        (
            "fn f(inout s: ChunkSeq[u64]) {
    s.push({ s.clear(); 1 });
}",
            "E0410",
            "overlaps active loan",
        ),
        (
            "fn take(sink s: ChunkSeq[u64]) -> u64 { s.len() }
fn f(inout s: ChunkSeq[u64]) {
    s.push(take(s));
}",
            "E0305",
            "",
        ),
        (
            "fn grow(inout s: ChunkSeq[u64]) -> u64 { s.push(1); 1 }
fn f(inout s: ChunkSeq[u64]) {
    s.push(grow(inout s));
}",
            "E0410",
            "overlaps active loan",
        ),
        (
            "fn f(inout s: ChunkSeq[u64]) {
    s.extend(in s[0..1]);
}",
            "E0301",
            "cannot read from the sequence it writes",
        ),
        (
            "struct P { x: u64 }
impl P { fn bump(inout self, v: u64) { self.x = wrapping_add(self.x, v); } }
fn clear(inout p: P) -> u64 { p.x = 0; 1 }
fn f(inout p: P) { p.bump(clear(inout p)); }",
            "E0301",
            "conflicts with active argument loan",
        ),
        (
            "struct P { x: u64 }
impl P { fn add(inout self, in o: P) { self.x = wrapping_add(self.x, o.x); } }
fn f(inout p: P) { p.add(in p); }",
            "E0301",
            "overlapping arguments",
        ),
        // Every `inout` argument is reserved (#1094), so a plain call
        // still refuses writes of its place in the other arguments.
        (
            "fn put(inout s: ChunkSeq[u64], v: u64) { s.push(v); }
fn f(inout s: ChunkSeq[u64]) { put(inout s, { s.clear(); 1 }); }",
            "E0301",
            "conflicts with active argument loan",
        ),
    ] {
        let c = checked(&format!("{body}\nfn main() -> u64 {{ 0 }}"));
        assert!(
            c.diagnostics
                .iter()
                .any(|d| d.code == code && d.message.contains(needle)),
            "{body}: expected {code} ({needle}): {:#?}",
            c.diagnostics
        );
    }
}

/// Lean extraction reads the receiver for the argument before it applies
/// the update, so it agrees with the backends.
#[test]
fn lean_extraction_reads_before_the_update() {
    let temp = Temp::new();
    let file = temp.0.join("lean.ves");
    fs::write(
        &file,
        "fn inc(inout s: ChunkSeq[u64], i: u64) -> bool eff [Fail[RuntimeError]] {
    s.try_set(i, wrapping_add(s.get(i), 1))
}
fn main() -> u64 { 0 }
",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg("emit")
        .arg(&file)
        .arg("--emit")
        .arg("lean")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lean = String::from_utf8_lossy(&out.stdout);
    let def = lean
        .lines()
        .find(|l| l.starts_with("def «inc»"))
        .expect("inc is extracted");
    let read = def.find("getElem!").expect("the argument reads s");
    let write = def.find("setIfInBounds").expect("the call writes s");
    assert!(read < write, "{def}");
}
