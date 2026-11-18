//! Native drops of what a dropped value owns (§13, port gaps lane 2, #985):
//! the elements of a dropped `ChunkSeq` of sequences, strings and
//! aggregates, a sequence field replaced through `inout`, a whole value
//! replaced through `inout`, by-value (`sink`) aggregates, unbound
//! temporaries and destructured bindings.
//!
//! Every program runs in the evaluator, the Cranelift JIT (optimizer on and
//! off) and, where a C compiler links, an executable and a C-ABI library.
//! The JIT must free every allocation itself (nothing is left for its
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
            "vesper-drop-glue-test-{}-{}",
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

/// Runs `source` everywhere through `main` (which calls every function of
/// `calls`) and from C `rounds` times; returns `main`'s value.
fn no_leaks(name: &str, source: &str, calls: &[&str]) -> u64 {
    let sum = calls
        .iter()
        .fold(String::from("0"), |acc, f| format!("wrapping_add({acc}, {f}())"));
    let value = differential(name, &format!("{source}\nfn main() -> u64 {{ {sum} }}\n"));
    if let Some((before, after)) = library_live(name, source, calls, 50) {
        assert_eq!(before, after, "{name}: the library leaked allocations");
    }
    value
}

/// A dropped sequence frees its elements: nested sequences, strings,
/// aggregates holding sequences, and `Option`s of sequences.
const NESTED: &str = r#"
fn u(x: u64) -> u64 { x }
struct Item { key: u64, vals: ChunkSeq[u64], name: String }
fn word(n: u64) -> String {
    let mut t = String::new();
    for i in u(0)..n { t.push_str(in "word"); }
    t
}
fn row(n: u64) -> ChunkSeq[u64] {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    for i in u(0)..n { s.push(i); }
    s
}
pub fn seq_of_seqs() -> u64 {
    let mut outer: ChunkSeq[ChunkSeq[u64]] = ChunkSeq::new[ChunkSeq[u64]]();
    for i in u(0)..u(6) { outer.push(row(i)); }
    outer.len()
}
pub fn seq_of_strings() -> u64 {
    let mut names: ChunkSeq[String] = ChunkSeq::new[String]();
    for i in u(1)..u(5) { names.push(word(i)); }
    names.len()
}
pub fn seq_of_items() -> u64 {
    let mut items: ChunkSeq[Item] = ChunkSeq::new[Item]();
    for i in u(0)..u(4) { items.push(Item { key: i, vals: row(i), name: word(i) }); }
    items.len()
}
pub fn three_levels() -> u64 {
    let mut cube: ChunkSeq[ChunkSeq[ChunkSeq[u64]]] = ChunkSeq::new[ChunkSeq[ChunkSeq[u64]]]();
    for i in u(0)..u(3) {
        let mut plane: ChunkSeq[ChunkSeq[u64]] = ChunkSeq::new[ChunkSeq[u64]]();
        for j in u(0)..i { plane.push(row(j)); }
        cube.push(plane);
    }
    cube.len()
}
pub fn seq_of_options() -> u64 {
    let mut slots: ChunkSeq[Option[ChunkSeq[u64]]] = ChunkSeq::new[Option[ChunkSeq[u64]]]();
    slots.push(Some(row(3)));
    slots.push(None);
    slots.push(Some(row(2)));
    slots.len()
}
pub fn popped_then_dropped() -> u64 {
    let mut outer: ChunkSeq[ChunkSeq[u64]] = ChunkSeq::new[ChunkSeq[u64]]();
    outer.push(row(2));
    outer.push(row(5));
    match outer.pop() {
        Some(last) => last.len(),
        None => 0,
    }
}
pub fn set_clear_truncate() -> u64 {
    let mut outer: ChunkSeq[ChunkSeq[u64]] = ChunkSeq::new[ChunkSeq[u64]]();
    for i in u(0)..u(5) { outer.push(row(i)); }
    let set = outer.try_set(1, row(9));
    let missed = outer.try_set(40, row(9));
    outer.truncate(3);
    let n = outer.len();
    outer.clear();
    outer.push(row(1));
    let mut names: ChunkSeq[String] = ChunkSeq::new[String]();
    names.push(word(2));
    let renamed = names.try_set(0, word(3));
    let ok = names.try_set(0, word(1));
    wrapping_add(n, names.len())
}
pub fn sink_seq_of_seqs() -> u64 {
    let mut outer: ChunkSeq[ChunkSeq[u64]] = ChunkSeq::new[ChunkSeq[u64]]();
    outer.push(row(4));
    take_rows(outer)
}
fn take_rows(sink rows: ChunkSeq[ChunkSeq[u64]]) -> u64 { rows.len() }
"#;

const NESTED_CALLS: [&str; 8] = [
    "seq_of_seqs",
    "seq_of_strings",
    "seq_of_items",
    "three_levels",
    "seq_of_options",
    "popped_then_dropped",
    "set_clear_truncate",
    "sink_seq_of_seqs",
];

#[test]
fn dropped_sequences_free_their_elements() {
    assert_eq!(
        no_leaks("nested", NESTED, &NESTED_CALLS),
        6 + 4 + 4 + 3 + 3 + 5 + 4 + 1
    );
}

/// A field (or a whole value) replaced through `inout` frees what it held;
/// a by-value aggregate parameter frees what it owns.
const REPLACED: &str = r#"
fn u(x: u64) -> u64 { x }
struct H { s: ChunkSeq[u64], tag: u64 }
struct Outer { h: H, names: ChunkSeq[String] }
fn row(n: u64) -> ChunkSeq[u64] {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    for i in u(0)..n { s.push(i); }
    s
}
fn reset(inout h: H) {
    h.s = ChunkSeq::with_capacity[u64](8);
}
fn replace_whole(inout h: H) {
    h = H { s: row(2), tag: 1 };
}
fn reset_deep(inout o: Outer) {
    o.h.s = row(1);
    o.names = ChunkSeq::new[String]();
}
fn take(sink h: H) -> u64 { h.tag }
fn open(sink h: H) -> ChunkSeq[u64] {
    let H { s, tag } = h;
    s
}
pub fn field_replaced() -> u64 {
    let mut s = row(5);
    let mut h = H { s: row(3), tag: 0 };
    reset(inout h);
    h.s.push(1);
    reset(inout h);
    replace_whole(inout h);
    let n = h.s.len();
    let back = open(h);
    wrapping_add(n, back.len())
}
pub fn nested_field_replaced() -> u64 {
    let mut names: ChunkSeq[String] = ChunkSeq::new[String]();
    names.push(String::new());
    let mut o = Outer { h: H { s: row(4), tag: 7 }, names: names };
    reset_deep(inout o);
    let Outer { h, names } = o;
    take(h)
}
pub fn sink_handle_dropped() -> u64 {
    let h = H { s: row(6), tag: 2 };
    take(h)
}
"#;

#[test]
fn replaced_fields_and_sink_handles_are_freed() {
    assert_eq!(
        no_leaks(
            "replaced",
            REPLACED,
            &["field_replaced", "nested_field_replaced", "sink_handle_dropped"]
        ),
        4 + 7 + 2
    );
}

/// Values that are never bound: a discarded call result and call results
/// passed by `in`.
const TEMPORARIES: &str = r#"
fn u(x: u64) -> u64 { x }
fn row(n: u64) -> ChunkSeq[u64] {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    for i in u(0)..n { s.push(i); }
    s
}
fn name(n: u64) -> String {
    let mut t = String::new();
    for i in u(0)..n { t.push_str(in "abc"); }
    t
}
fn rows(n: u64) -> ChunkSeq[ChunkSeq[u64]] {
    let mut s: ChunkSeq[ChunkSeq[u64]] = ChunkSeq::new[ChunkSeq[u64]]();
    for i in u(0)..n { s.push(row(i)); }
    s
}
fn size(in s: ChunkSeq[u64]) -> u64 { s.len() }
fn count(in s: ChunkSeq[ChunkSeq[u64]]) -> u64 { s.len() }
fn chars(in s: String) -> u64 { s.len() }
pub fn discarded() -> u64 {
    row(4);
    name(2);
    rows(3);
    1
}
pub fn passed_in() -> u64 {
    let a = size(in row(3));
    let b = chars(in name(2));
    let c = count(in rows(4));
    wrapping_add(wrapping_add(a, b), c)
}
pub fn in_a_loop() -> u64 {
    let mut total: u64 = 0;
    for i in u(0)..u(10) { total = wrapping_add(total, size(in row(i))); }
    total
}
"#;

#[test]
fn unbound_temporaries_are_freed() {
    assert_eq!(
        no_leaks("temporaries", TEMPORARIES, &["discarded", "passed_in", "in_a_loop"]),
        1 + 3 + 6 + 4 + 45
    );
}

/// Bindings inside a destructuring pattern own what they bind.
const DESTRUCTURED: &str = r#"
fn u(x: u64) -> u64 { x }
struct P { a: ChunkSeq[u64], b: String }
fn row(n: u64) -> ChunkSeq[u64] {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    for i in u(0)..n { s.push(i); }
    s
}
fn pair() -> (ChunkSeq[u64], ChunkSeq[u64]) { (row(1), row(2)) }
fn make_p() -> P {
    let mut t = String::new();
    t.push_str(in "longer than any literal");
    P { a: row(3), b: t }
}
pub fn tuple() -> u64 {
    let (x, y) = pair();
    wrapping_add(x.len(), y.len())
}
pub fn record() -> u64 {
    let P { a, b } = make_p();
    wrapping_add(a.len(), b.len())
}
pub fn in_a_loop() -> u64 {
    let mut total: u64 = 0;
    for i in u(0)..u(5) {
        let (x, y) = pair();
        total = wrapping_add(total, wrapping_add(x.len(), y.len()));
    }
    total
}
"#;

#[test]
fn destructured_bindings_are_freed() {
    assert_eq!(
        no_leaks("destructured", DESTRUCTURED, &["tuple", "record", "in_a_loop"]),
        3 + 26 + 15
    );
}

/// A whole sequence or string replaced through `inout` frees the old one.
const INOUT_WHOLE: &str = r#"
fn u(x: u64) -> u64 { x }
fn row(n: u64) -> ChunkSeq[u64] {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    for i in u(0)..n { s.push(i); }
    s
}
fn renew(inout s: ChunkSeq[u64]) { s = row(2); }
fn rename(inout t: String) {
    let mut fresh = String::new();
    fresh.push_str(in "a string longer than its literal");
    t = fresh;
}
pub fn seq_replaced() -> u64 {
    let mut s = row(5);
    renew(inout s);
    renew(inout s);
    s.len()
}
pub fn string_replaced() -> u64 {
    let mut t = String::new();
    t.push_str(in "first contents, also long");
    rename(inout t);
    rename(inout t);
    t.len()
}
"#;

#[test]
fn whole_values_replaced_through_inout_are_freed() {
    assert_eq!(
        no_leaks("inout_whole", INOUT_WHOLE, &["seq_replaced", "string_replaced"]),
        2 + 32
    );
}

/// The boxes in a dropped sequence are freed with it.
#[test]
fn dropped_sequences_free_their_boxes() {
    let source = r#"
struct N { v: u64 }
fn mk(v: u64) -> Box[N] { Box::new(N { v: v }) }
fn boxes() -> u64 {
    let mut s: ChunkSeq[Box[N]] = ChunkSeq::new[Box[N]]();
    s.push(mk(1));
    s.push(mk(2));
    s.push(mk(3));
    s.len()
}
fn main() -> u64 { boxes() }
"#;
    let c = accepted(source);
    for optimize in [true, false] {
        vesper::fip::native_runtime::reset_counts();
        let value = native::jit_with_options(&c.world, "main", optimize)
            .unwrap()
            .to_string();
        assert_eq!(value, "3");
        assert_eq!(vesper::fip::native_runtime::alloc_count(), 3);
        assert_eq!(
            vesper::fip::native_runtime::free_count(),
            3,
            "optimize {optimize}: boxes not freed"
        );
        assert_eq!(native_runtime::teardown_frees(), 0);
    }
}

/// A `let`-bound struct frees everything its fields own (#972 freed its
/// sequences): strings, and the elements of the sequences in it.
const LOCAL_STRUCTS: &str = r#"
fn u(x: u64) -> u64 { x }
struct Doc { title: String, lines: ChunkSeq[String], rows: ChunkSeq[ChunkSeq[u64]], n: u64 }
fn word(n: u64) -> String {
    let mut t = String::new();
    for i in u(0)..n { t.push_str(in "word"); }
    t
}
fn row(n: u64) -> ChunkSeq[u64] {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    for i in u(0)..n { s.push(i); }
    s
}
fn doc(n: u64) -> Doc {
    let mut lines: ChunkSeq[String] = ChunkSeq::new[String]();
    let mut rows: ChunkSeq[ChunkSeq[u64]] = ChunkSeq::new[ChunkSeq[u64]]();
    for i in u(0)..n { lines.push(word(i)); rows.push(row(i)); }
    Doc { title: word(n), lines: lines, rows: rows, n: n }
}
pub fn dropped() -> u64 {
    let d = doc(3);
    wrapping_add(d.lines.len(), d.n)
}
pub fn rebound() -> u64 {
    let mut total: u64 = 0;
    for i in u(0)..u(4) {
        let d = doc(i);
        total = wrapping_add(total, d.rows.len());
    }
    total
}
pub fn field_replaced() -> u64 {
    let mut d = doc(2);
    d.title = word(5);
    d.rows = ChunkSeq::new[ChunkSeq[u64]]();
    d.title.len()
}
"#;

#[test]
fn local_structs_free_what_their_fields_own() {
    assert_eq!(
        no_leaks("local_structs", LOCAL_STRUCTS, &["dropped", "rebound", "field_replaced"]),
        6 + 6 + 20
    );
}
