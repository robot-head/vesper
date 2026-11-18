//! Native drops of function-local and by-value `ChunkSeq[T]` and `String`
//! values (§13, Stage 1 perf lane 2, #932).
//!
//! Every program runs in the evaluator, the Cranelift JIT (optimizer on and
//! off) and, where a C compiler links, an executable and a C-ABI library.
//! The JIT must free every sequence itself (nothing is left for its
//! teardown) and never free one that is not live; the library's live
//! allocation counter must return to its start.
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
            "vesper-seq-drop-test-{}-{}",
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

fn accepted(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
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

fn vesper(args: &[&std::ffi::OsStr]) {
    let out = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "vesper {args:?} failed\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Runs `main` of `source` everywhere and returns its value. The JIT must
/// leave nothing for teardown and drop nothing twice.
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
        assert_eq!(jit, evaluated, "{name}: JIT (optimize {optimize})");
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
        vesper(&[
            "build".as_ref(),
            file.as_os_str(),
            "-o".as_ref(),
            exe.as_os_str(),
        ]);
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

/// Builds `source` (whose `pub fn`s take no arguments and return `u64`) as a
/// C-ABI library, calls each of `calls` `rounds` times from C and returns
/// the live sequence allocations before and after.
fn library_live(name: &str, source: &str, calls: &[&str], rounds: u32) -> Option<(i64, i64)> {
    if !links() {
        return None;
    }
    let temp = Temp::new();
    let file = temp.0.join(format!("{name}.ves"));
    // A sequence parameter makes the library use export ABI v2, which
    // exports the live allocation counter.
    fs::write(
        &file,
        format!("{source}\npub fn probe(in s: ChunkSeq[u64]) -> u64 {{ s.len() }}\n"),
    )
    .unwrap();
    let object = temp.0.join(format!("{name}.o"));
    vesper(&[
        "build".as_ref(),
        "--lib".as_ref(),
        file.as_os_str(),
        "-o".as_ref(),
        object.as_os_str(),
    ]);
    let mut driver = String::from("#include <stdio.h>\n#include <stdint.h>\n");
    for f in calls {
        driver.push_str(&format!("uint64_t vesper_{name}_{f}(void);\n"));
    }
    driver.push_str(&format!(
        "int64_t vesper_{name}_live_allocations(void);\nint main(void) {{\n  int64_t before = vesper_{name}_live_allocations();\n  uint64_t sum = 0;\n  for (int r = 0; r < {rounds}; r++) {{\n"
    ));
    for f in calls {
        driver.push_str(&format!("    sum += vesper_{name}_{f}();\n"));
    }
    driver.push_str(&format!(
        "  }}\n  printf(\"%lld %lld %llu\\n\", (long long)before, (long long)vesper_{name}_live_allocations(), (unsigned long long)sum);\n  return 0;\n}}\n"
    ));
    let c = temp.0.join("driver.c");
    fs::write(&c, driver).unwrap();
    let exe = temp.0.join("driver");
    let cc = Command::new("cc")
        .arg(&c)
        .arg(&object)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        cc.status.success(),
        "{name}: cc failed: {}",
        String::from_utf8_lossy(&cc.stderr)
    );
    let out = Command::new(&exe).output().unwrap();
    assert!(out.status.success(), "{name}: driver failed");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let words: Vec<i64> = text
        .split_whitespace()
        .take(2)
        .map(|w| w.parse().unwrap())
        .collect();
    Some((words[0], words[1]))
}

/// Exit kinds a function-local sequence must be freed on.
const EXITS: &str = r#"
fn u(x: u64) -> u64 { x }
pub fn normal() -> u64 {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    for i in u(0)..u(40) { s.push(i); }
    s.len()
}

pub fn early() -> u64 {
    let mut s: ChunkSeq[u64] = ChunkSeq::with_capacity[u64](2);
    let mut t: ChunkSeq[u8] = ChunkSeq::new[u8]();
    t.push(1);
    let mut i: u64 = 0;
    loop {
        s.push(i);
        if i == 9 { return s.len(); }
        i = wrapping_add(i, 1);
    }
}

pub fn rebound() -> u64 {
    let mut total: u64 = 0;
    for i in u(0)..u(25) {
        let mut t: ChunkSeq[u64] = ChunkSeq::new[u64]();
        t.push(i);
        t.push(i);
        total = wrapping_add(total, t.len());
    }
    total
}

pub fn assigned() -> u64 {
    let mut a: ChunkSeq[u64] = ChunkSeq::new[u64]();
    a.push(1);
    a = ChunkSeq::new[u64]();
    a.push(2);
    a.push(3);
    a = ChunkSeq::with_capacity[u64](8);
    a.push(4);
    a.len()
}

fn raise_at(n: u64) -> u64 eff [Fail[RuntimeError]] {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    for i in u(0)..u(5) { s.push(i); }
    s.get(n)
}

pub fn failed() -> u64 {
    handle Fail[RuntimeError] in { raise_at(7) } with {
        return x => x,
        raise(e, k) => 11,
    }
}

pub fn not_failed() -> u64 {
    handle Fail[RuntimeError] in { raise_at(3) } with {
        return x => x,
        raise(e, k) => 11,
    }
}

pub fn returned_from_handled() -> u64 {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    s.push(5);
    s.push(6);
    handle Fail[RuntimeError] in {
        return 7;
    } with {
        return x => x,
        raise(e, k) => 0,
    }
}

pub fn finished() -> u64 {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    s.push(1);
    let n = s.len();
    s.clear();
    handle Fail[RuntimeError] in { ChunkSeq::finish(s); n } with {
        return x => x,
        raise(e, k) => 0,
    }
}
"#;

const EXIT_CALLS: [&str; 8] = [
    "normal",
    "early",
    "rebound",
    "assigned",
    "failed",
    "not_failed",
    "returned_from_handled",
    "finished",
];

#[test]
fn locals_are_freed_on_every_exit() {
    let main = format!(
        "{EXITS}\nfn main() -> u64 {{ wrapping_add(wrapping_add(wrapping_add(normal(), early()), wrapping_add(rebound(), assigned())), wrapping_add(wrapping_add(failed(), not_failed()), wrapping_add(returned_from_handled(), finished()))) }}\n"
    );
    assert_eq!(
        differential("exits", &main),
        40 + 10 + 50 + 1 + 11 + 3 + 7 + 1
    );
    if let Some((before, after)) = library_live("exits", EXITS, &EXIT_CALLS, 100) {
        assert_eq!(before, after, "the library leaked sequences");
    }
}

/// Ownership moves: by value into a callee (which frees it), out through a
/// return, into an aggregate that is dropped by its own consumer, and a
/// local moved then dropped (freed once).
const MOVES: &str = r#"
fn u(x: u64) -> u64 { x }
struct Holder { items: ChunkSeq[u64], tag: u64 }

fn consume(sink s: ChunkSeq[u64]) -> u64 { s.len() }
fn pass_on(sink s: ChunkSeq[u64]) -> ChunkSeq[u64] { s }
fn build(n: u64) -> ChunkSeq[u64] {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    for i in 0..n { s.push(i); }
    s
}
fn unwrap(sink h: Holder) -> ChunkSeq[u64] {
    let Holder { items, tag } = h;
    items
}

pub fn by_value() -> u64 {
    let mut a: ChunkSeq[u64] = ChunkSeq::new[u64]();
    a.push(1);
    a.push(2);
    consume(a)
}

pub fn moved_then_dropped() -> u64 {
    let mut a: ChunkSeq[u64] = ChunkSeq::new[u64]();
    a.push(1);
    let b = a;
    let c = pass_on(b);
    c.len()
}

pub fn returned() -> u64 {
    let s = build(6);
    let t = build(3);
    wrapping_add(s.len(), consume(t))
}

pub fn through_aggregate() -> u64 {
    let s = build(4);
    let h = Holder { items: s, tag: 9 };
    let back = unwrap(h);
    back.len()
}

pub fn moved_in_handled() -> u64 {
    let s = build(5);
    let t = build(2);
    handle Fail[RuntimeError] in { wrapping_add(consume(s), t.len()) } with {
        return x => x,
        raise(e, k) => 0,
    }
}

// With the optimizer on, the inliner splices `consume` and `build` into the
// handled body, whose own locals then own sequences.
pub fn local_in_handled() -> u64 {
    let t = build(2);
    handle Fail[RuntimeError] in {
        let v = build(3);
        wrapping_add(wrapping_add(consume(v), consume(build(4))), t.len())
    } with {
        return x => x,
        raise(e, k) => 0,
    }
}

pub fn branches() -> u64 {
    let mut total: u64 = 0;
    for i in u(0)..u(6) {
        let s = build(i);
        if i % 2 == 0 {
            total = wrapping_add(total, consume(s));
        } else {
            total = wrapping_add(total, s.len());
        }
    }
    total
}
"#;

const MOVE_CALLS: [&str; 7] = [
    "by_value",
    "local_in_handled",
    "moved_in_handled",
    "moved_then_dropped",
    "returned",
    "through_aggregate",
    "branches",
];

#[test]
fn moved_and_by_value_sequences_are_freed_once() {
    let main = format!(
        "{MOVES}\nfn main() -> u64 {{ wrapping_add(wrapping_add(by_value(), moved_then_dropped()), wrapping_add(wrapping_add(returned(), through_aggregate()), wrapping_add(branches(), wrapping_add(moved_in_handled(), local_in_handled())))) }}\n"
    );
    assert_eq!(differential("moves", &main), 2 + 1 + 9 + 4 + 15 + 7 + 9);
    if let Some((before, after)) = library_live("moves", MOVES, &MOVE_CALLS, 100) {
        assert_eq!(before, after, "the library leaked sequences");
    }
}

const STRINGS: &str = r#"
fn u(x: u64) -> u64 { x }
fn shout(sink s: String) -> u64 {
    let mut t = s;
    t.push_str(in "!");
    t.len()
}
fn grow(sink s: String) -> u64 {
    let mut s = s;
    s.push_str(in " and more bytes than a literal");
    s.len()
}
fn owned_param(sink s: String) -> u64 {
    s.len()
}

pub fn local_strings() -> u64 {
    let mut total: u64 = 0;
    for i in u(0)..u(20) {
        let mut s = String::new();
        s.push_str(in "a string long enough to allocate");
        s.push_char(65);
        total = wrapping_add(total, s.len());
    }
    let literal = "borrowed";
    wrapping_add(total, literal.len())
}

pub fn string_args() -> u64 {
    let mut s = String::new();
    s.push_str(in "owned");
    let a = owned_param(s);
    let b = shout(String::new());
    let c = grow(String::new());
    wrapping_add(wrapping_add(a, b), c)
}

pub fn early_string() -> u64 {
    let mut s = String::new();
    for i in u(0)..u(10) {
        s.push_str(in "xy");
        if s.len() > 9 { return s.len(); }
    }
    0
}
"#;

#[test]
fn strings_are_freed() {
    let main = format!(
        "{STRINGS}\nfn main() -> u64 {{ wrapping_add(wrapping_add(local_strings(), string_args()), early_string()) }}\n"
    );
    differential("strings", &main);
    if let Some((before, after)) = library_live(
        "strs",
        STRINGS,
        &["local_strings", "string_args", "early_string"],
        100,
    ) {
        assert_eq!(before, after, "the library leaked string buffers");
    }
}

fn rejected(source: &str, code: &str) {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(
        c.diagnostics.iter().any(|d| d.code == code),
        "expected {code}, got {:#?}",
        c.diagnostics
    );
}

/// The checker refuses every program that could reach a dropped sequence,
/// so a drop never frees memory still in use: a use after a move or after a
/// by-value call, a slice that outlives its statement, and a returned view.
#[test]
fn the_checker_refuses_use_after_drop() {
    let moved = rejected_code(
        "fn main() -> u64 { let mut a: ChunkSeq[u64] = ChunkSeq::new[u64](); let b = a; a.len() }",
    );
    let consumed = rejected_code(
        "fn take(sink s: ChunkSeq[u64]) -> u64 { 0 }\nfn main() -> u64 { let mut a: ChunkSeq[u64] = ChunkSeq::new[u64](); let n = take(a); wrapping_add(n, a.len()) }",
    );
    assert_eq!(moved, consumed, "both are uses of a moved sequence");
    rejected(
        "fn main() -> u64 { let mut s: ChunkSeq[u64] = ChunkSeq::new[u64](); let v = s.as_slice(); 0 }",
        "E1521",
    );
    rejected("fn f(in x: Slice[u64]) -> Slice[u64] { x }", "E1520");
    let string_moved =
        rejected_code("fn main() -> u64 { let s = String::new(); let t = s; s.len() }");
    assert_eq!(
        string_moved, moved,
        "a moved string is refused like a sequence"
    );
}

fn rejected_code(source: &str) -> String {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    c.diagnostics
        .first()
        .unwrap_or_else(|| panic!("expected a diagnostic for {source}"))
        .code
        .clone()
}

/// Sequences a function does not own stay alive: `in` and `inout`
/// parameters, and a sequence returned to the caller.
#[test]
fn borrowed_sequences_are_not_freed() {
    let source = r#"
fn u(x: u64) -> u64 { x }
fn count(in s: ChunkSeq[u64]) -> u64 { s.len() }
fn add(inout s: ChunkSeq[u64], v: u64) { s.push(v) }
fn replace(inout s: ChunkSeq[u64]) {
    let mut fresh: ChunkSeq[u64] = ChunkSeq::new[u64]();
    fresh.push(7);
    fresh.push(8);
    s.extend(in fresh.as_slice());
}
fn main() -> u64 {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    for i in u(0)..u(10) { add(inout s, i); }
    replace(inout s);
    let a = count(in s);
    let b = count(in s);
    wrapping_add(a, b)
}
"#;
    assert_eq!(differential("borrowed", source), 24);
}

/// Export ABI v2 by-value parameters: a sequence the caller makes with
/// `seq_new` and a string lent as borrowed bytes are owned by the callee,
/// which frees them (and any buffer it grows), so the live count returns to
/// its start.
#[test]
fn by_value_sequences_and_strings_cross_the_export_abi() {
    if !links() {
        return;
    }
    let temp = Temp::new();
    let file = temp.0.join("bv.ves");
    fs::write(
        &file,
        "pub fn take(sink s: ChunkSeq[u64]) -> u64 { let mut t = s; t.push(9); t.len() }\n\
         pub fn words(sink s: String) -> u64 { let mut t = s; t.push_str(in \" grown well past the borrowed bytes\"); t.len() }\n\
         pub fn keep(sink s: String) -> u64 { s.len() }\n",
    )
    .unwrap();
    let object = temp.0.join("bv.o");
    vesper(&[
        "build".as_ref(),
        "--lib".as_ref(),
        file.as_os_str(),
        "-o".as_ref(),
        object.as_os_str(),
    ]);
    let driver = r#"#include <stdio.h>
#include <stdint.h>
typedef struct { uint64_t len, cap, data; } H;
int64_t vesper_bv_seq_new(int64_t, int64_t);
int64_t vesper_bv_live_allocations(void);
uint64_t vesper_bv_take(uint64_t);
uint64_t vesper_bv_words(uint64_t *);
uint64_t vesper_bv_keep(uint64_t *);
int main(void) {
  int64_t before = vesper_bv_live_allocations();
  uint64_t sum = 0;
  for (int r = 0; r < 100; r++) {
    H *h = (H *)vesper_bv_seq_new(4, 8);
    uint64_t *d = (uint64_t *)h->data; d[0] = 1; d[1] = 2; h->len = 2;
    sum += vesper_bv_take((uint64_t)h);
    uint64_t w[3] = {5, 0, (uint64_t)"hello"};
    sum += vesper_bv_words(w);
    uint64_t k[3] = {5, 0, (uint64_t)"hello"};
    sum += vesper_bv_keep(k);
  }
  printf("%lld %lld %llu\n", (long long)before, (long long)vesper_bv_live_allocations(), (unsigned long long)sum);
  return 0;
}
"#;
    let c = temp.0.join("driver.c");
    fs::write(&c, driver).unwrap();
    let exe = temp.0.join("driver");
    let cc = Command::new("cc")
        .arg(&c)
        .arg(&object)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        cc.status.success(),
        "{}",
        String::from_utf8_lossy(&cc.stderr)
    );
    let out = Command::new(&exe).output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "0 0 4800");
}

/// Struct locals that hold sequences in (nested) fields free them on the
/// optimize path (#972): at every exit, on rebinding in a loop, on
/// assignment, and not after a move.
const FIELDS: &str = r#"
struct W {
    s: ChunkSeq[u64],
    k: u64
}

struct O {
    inner: W,
    t: ChunkSeq[u64]
}

fn fill(n: u64) -> ChunkSeq[u64] {
    let mut s: ChunkSeq[u64] = ChunkSeq::new();
    for i in 0..n {
        s.push(i);
    }
    s
}

fn raise_at(n: u64) -> u64 eff [Fail[RuntimeError]] {
    if n == 0 {
        Fail::raise(RuntimeError::DivisionByZero)
    }
    n
}

pub fn plain() -> u64 {
    let w = W { s: fill(3), k: 4 };
    wrapping_add(w.s.len(), w.k)
}

pub fn looped() -> u64 {
    let mut total: u64 = 0;
    let n: u64 = 4;
    for i in 0..n {
        let w = W { s: fill(i), k: i };
        total = wrapping_add(total, w.s.len());
    }
    total
}

pub fn nested() -> u64 {
    let o = O { inner: W { s: fill(2), k: 1 }, t: fill(5) };
    wrapping_add(o.inner.s.len(), o.t.len())
}

pub fn assigned() -> u64 {
    let mut w = W { s: fill(2), k: 0 };
    w = W { s: fill(6), k: 1 };
    w.s.len()
}

fn make(n: u64) -> W {
    let w = W { s: fill(n), k: n };
    w
}

pub fn moved_out() -> u64 {
    let w = make(3);
    wrapping_add(w.s.len(), w.k)
}

pub fn early() -> u64 {
    let w = W { s: fill(3), k: 2 };
    if w.k == 2 {
        return w.s.len();
    }
    0
}

fn failing(n: u64) -> u64 eff [Fail[RuntimeError]] {
    let w = W { s: fill(3), k: n };
    raise_at(w.k)
}

pub fn failed() -> u64 {
    handle Fail[RuntimeError] in {
        failing(0)
    } with {
        return v => v,
        raise(e, k) => 7,
    }
}
"#;

const FIELD_CALLS: [&str; 7] = [
    "plain",
    "looped",
    "nested",
    "assigned",
    "moved_out",
    "early",
    "failed",
];

#[test]
fn struct_fields_holding_sequences_are_freed() {
    let main = format!(
        "{FIELDS}\nfn main() -> u64 {{ wrapping_add(wrapping_add(wrapping_add(plain(), looped()), wrapping_add(nested(), assigned())), wrapping_add(wrapping_add(moved_out(), early()), failed())) }}\n"
    );
    let c = accepted(&main);
    let evaluated = Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap()
        .to_string();
    assert_eq!(evaluated, "42");
    // The unoptimized path keeps today's drops (these locals go to
    // teardown); the optimize path frees them all, once.
    for optimize in [true, false] {
        let stray = native_runtime::stray_frees();
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap()
            .to_string();
        assert_eq!(jit, evaluated, "optimize {optimize}");
        let teardown = native_runtime::teardown_frees();
        if optimize {
            assert_eq!(teardown, 0, "struct fields left for teardown");
        }
        assert_eq!(native_runtime::stray_frees(), stray, "optimize {optimize}");
    }
    if let Some((before, after)) = library_live("fields", FIELDS, &FIELD_CALLS, 100) {
        assert_eq!(before, after, "struct fields leaked in the library");
    }
}
