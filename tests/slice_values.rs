//! Slices as values (Rust-exit gap wave 1, lane 6, #1503; §13 "Bound
//! views"): an immutable `let` may bind a slice or string view of a place
//! the function may change (a `let mut` local, an `inout` or `sink`
//! parameter, or a field path of one). The view borrows its source `in` to
//! the end of its block, so no overlapping place may be written, lent
//! `inout` or moved meanwhile (E1531), while disjoint places stay usable.
//! A callee with a slice parameter inlines when its argument is such a
//! bound view, and byte-slice `cmp` finds the first differing byte of a
//! 16-byte step from its lane mask. The evaluator, both JIT configurations
//! and an executable agree.
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{check, ir_inline, native, parser, runtime::Runtime, seq::native_runtime};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-bound-views-{}-{}",
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

fn refused(body: &str, code: &str, needle: &str) {
    let c = checked(&format!("{body}\nfn main() -> u64 {{ 0 }}"));
    assert!(
        c.diagnostics
            .iter()
            .any(|d| d.code == code && d.message.contains(needle)),
        "{body}: expected {code} ({needle}): {:#?}",
        c.diagnostics
    );
}

const SUM: &str = "fn sum(in b: Slice[u8]) -> u64 {
    let mut t: u64 = 0;
    for x in b {
        t = wrapping_add(t, widen_u64(x));
    }
    t
}
fn wsum(in b: Slice[u64]) -> u64 {
    let mut t: u64 = 0;
    for x in b {
        t = wrapping_add(t, x);
    }
    t
}
";

#[test]
fn views_bind_to_places_that_may_change() {
    let source = format!(
        "{SUM}struct L {{ words: ChunkSeq[u64], text: String, n: u64 }}
fn fields(inout l: L) -> u64 {{
    let r = {{
        let w = l.words.as_slice();
        let t = l.text.as_bytes();
        let head = w[0..1];
        // Disjoint from both views.
        l.n = wrapping_add(wsum(in w), sum(in t));
        wrapping_add(l.n, wrapping_mul(wsum(in head), 1000))
    }};
    l.words.push(7);
    wrapping_add(r, wrapping_mul(l.words.len(), 0))
}}
fn seq_param(inout xs: ChunkSeq[u8]) -> u64 {{
    let r = {{
        let s = xs[1..3];
        wrapping_add(sum(in s), wrapping_mul(s.len(), 100))
    }};
    // The view's block has ended.
    xs.push(9);
    wrapping_add(r, wrapping_mul(xs.len(), 10000))
}}
fn sink_param(sink xs: ChunkSeq[u64]) -> u64 {{
    let r = {{
        let s = xs.as_slice();
        wsum(in s)
    }};
    // The view's block has ended, so the sequence may move.
    let ys = xs;
    wrapping_add(r, wrapping_mul(ys.len(), 0))
}}
fn slice_param(inout o: Slice[u8]) -> u64 {{
    let s = o[0..2];
    sum(in s)
}}
fn main() -> u64 {{
    let mut l = L {{ words: ChunkSeq::new(), text: String::new(), n: 0 }};
    l.words.push(3);
    l.words.push(4);
    l.text.push_str(in \"ab\");
    let a = fields(inout l);
    let mut xs: ChunkSeq[u8] = ChunkSeq::new();
    xs.push(1);
    xs.push(2);
    xs.push(5);
    let b = {{
        let v = xs.as_slice();
        wrapping_add(sum(in v), sum(in v[1..2]))
    }};
    let c = seq_param(inout xs);
    let mut ws: ChunkSeq[u64] = ChunkSeq::new();
    ws.push(11);
    let d = sink_param(ws);
    let e = slice_param(inout xs.as_slice_mut());
    let mut text = String::new();
    text.push_str(in \"xyz\");
    let f = {{
        let t = text.as_str();
        t.len()
    }};
    text.push_str(in \"w\");
    wrapping_add(
        wrapping_add(wrapping_add(a, wrapping_mul(b, 10)), wrapping_mul(c, 1000000)),
        wrapping_add(wrapping_add(d, wrapping_mul(e, 7)), wrapping_mul(wrapping_add(f, text.len()), 100000000000)),
    )
}}
"
    );
    // a = (3 + 4) + (97 + 98) + 3 * 1000 = 3202; b = 8 + 2 = 10;
    // c = (2 + 5) + 200 + 4 * 10000 = 40207; d = 11; e = 1 + 2 = 3;
    // f + text.len() = 3 + 4.
    assert_eq!(
        differential("changing", &source),
        3202 + 100 + 40207 * 1_000_000 + 11 + 21 + 7 * 100_000_000_000
    );
}

#[test]
fn a_bound_view_freezes_overlapping_places() {
    let ws = "struct L { words: ChunkSeq[u64], n: u64 }\nfn take(inout l: L) { }\nfn touch(inout xs: ChunkSeq[u64]) { }";
    for (body, code, needle) in [
        (
            "fn f() -> u64 { let mut xs: ChunkSeq[u64] = ChunkSeq::new(); let s = xs.as_slice(); xs.push(1); s.len() }",
            "E1531",
            "cannot write or lend 'inout' 'xs' while the view 's' of it is in scope",
        ),
        (
            "fn f() -> u64 { let mut xs: ChunkSeq[u64] = ChunkSeq::new(); let s = xs.as_slice(); xs = ChunkSeq::new(); s.len() }",
            "E1531",
            "cannot write or lend 'inout' 'xs'",
        ),
        (
            "fn f() -> u64 { let mut xs: ChunkSeq[u64] = ChunkSeq::new(); xs.push(1); let s = xs.as_slice(); xs.try_set(0, 2); s.len() }",
            "E1531",
            "cannot write or lend 'inout' 'xs'",
        ),
        (
            "fn f() -> u64 { let mut xs: ChunkSeq[u64] = ChunkSeq::new(); let s = xs[0..0]; touch(inout xs); s.len() }",
            "E1531",
            "cannot write or lend 'inout' 'xs'",
        ),
        (
            "fn f() -> u64 { let mut xs: ChunkSeq[u64] = ChunkSeq::new(); let s = xs.as_slice(); let ys = xs; s.len() }",
            "E1531",
            "cannot move 'xs'",
        ),
        (
            "fn f(inout l: L) -> u64 { let s = l.words.as_slice(); take(inout l); s.len() }",
            "E1531",
            "cannot write or lend 'inout' 'l'",
        ),
        (
            "fn f(inout l: L) -> u64 { let s = l.words.as_slice(); l.words.push(1); s.len() }",
            "E1531",
            "cannot write or lend 'inout' 'l.words'",
        ),
        (
            "fn f(inout l: L) -> u64 { let s = l.words.as_slice(); l = L { words: ChunkSeq::new(), n: 0 }; s.len() }",
            "E1531",
            "cannot write or lend 'inout' 'l'",
        ),
        (
            "fn f() -> u64 { let mut xs: ChunkSeq[u64] = ChunkSeq::new(); xs.push(1); let s = xs.as_slice(); match xs.try_get_mut(0) { Option::Some(x) => { x = 2; } Option::None => {} } s.len() }",
            "E1531",
            "cannot write or lend 'inout' 'xs'",
        ),
        (
            "fn f() -> u64 { let mut t = String::new(); let b = t.as_str(); t.push_str(in \"x\"); b.len() }",
            "E1531",
            "cannot write or lend 'inout' 't'",
        ),
        // A loop body that writes the source of a view bound before it.
        (
            "fn f() -> u64 { let mut xs: ChunkSeq[u64] = ChunkSeq::new(); let s = xs.as_slice(); for i in 0..2 { xs.push(i); } s.len() }",
            "E1531",
            "cannot write or lend 'inout' 'xs'",
        ),
        // A view stays read-only and second-class.
        (
            "fn f() -> u64 { let mut xs: ChunkSeq[u64] = ChunkSeq::new(); let mut s = xs.as_slice(); s.len() }",
            "E1521",
            "second-class",
        ),
        (
            "fn f() { let mut xs: ChunkSeq[u64] = ChunkSeq::new(); let s = xs.as_slice(); s.fill(1); }",
            "E0313",
            "cannot mutate 's'",
        ),
        (
            "fn f() -> Slice[u64] { let mut xs: ChunkSeq[u64] = ChunkSeq::new(); let s = xs.as_slice(); s }",
            "E1520",
            "",
        ),
    ] {
        refused(&format!("{ws}\n{body}"), code, needle);
    }
}

#[test]
fn a_loop_over_a_view_of_a_changing_place_keeps_its_loan() {
    refused(
        "fn f() { let mut xs: ChunkSeq[u64] = ChunkSeq::new(); xs.push(1); for x in xs.as_slice() { xs.push(x); } }",
        "E0301",
        "conflicts with the 'for' loop",
    );
}

#[test]
fn slice_callees_inline_through_bound_views() {
    let source = format!(
        "{SUM}fn first(in b: Slice[u8]) -> u64 {{
    match b.try_get(0) {{
        Option::Some(x) => widen_u64(x),
        Option::None => 0,
    }}
}}
fn run(inout xs: ChunkSeq[u8]) -> u64 {{
    let s = xs.as_slice();
    let t = s[1..s.len()];
    let r = wrapping_add(first(in s), wrapping_mul(first(in t), 10));
    r
}}
fn main() -> u64 {{
    let mut xs: ChunkSeq[u8] = ChunkSeq::new();
    xs.push(4);
    xs.push(6);
    let mut ys: ChunkSeq[u8] = ChunkSeq::new();
    ys.push(1);
    let v = ys.as_slice();
    wrapping_add(run(inout xs), wrapping_mul(first(in v), 100))
}}
"
    );
    assert_eq!(differential("inline", &source), 4 + 60 + 100);
    let w = accepted(&source).world;
    let mut program = vesper::ir::lower_world(&w);
    vesper::ir_opt::optimize(&w, &mut program).unwrap();
    let report = ir_inline::inline(&w, &mut program, &ir_inline::Budget::default());
    assert!(
        !report.refused.contains_key("first"),
        "{:?}",
        report.refused
    );
    let calls = |caller: &str| {
        program.functions[caller]
            .blocks
            .iter()
            .flat_map(|b| &b.instructions)
            .filter(|i| matches!(&i.op, vesper::ir::Op::Call { callee, .. } if callee == "first"))
            .count()
    };
    assert_eq!(calls("run"), 0);
    assert_eq!(calls("main"), 0);
}

/// Every pair of byte strings of lengths 0..=40 that differ in at most one
/// byte (or in length): `cmp` and `eq` agree in every backend.
#[test]
fn byte_slice_cmp_finds_the_first_difference() {
    let source = "fn rank(in a: Slice[u8], in b: Slice[u8]) -> u64 {
    let c = a.cmp(in b);
    let e = a.eq(in b);
    let r: u64 = if c < 0 { 1 } else if c > 0 { 2 } else { 3 };
    if e == (c == 0) { r } else { 100 }
}
fn main() -> u64 {
    let mut acc: u64 = 0;
    let n: u64 = 40;
    for la in 0..=n {
        for lb in [0, 1, 15, 16, 17, 31, 32, 33, 40] {
            let lb: u64 = lb;
            for at in 0..=la {
                let mut a: ChunkSeq[u8] = ChunkSeq::new();
                let mut b: ChunkSeq[u8] = ChunkSeq::new();
                for i in 0..la {
                    a.push(wrapping_u8(wrapping_add(65, i % 23)));
                }
                for i in 0..lb {
                    b.push(wrapping_u8(wrapping_add(65, i % 23)));
                }
                // One byte of `b` raised or lowered at `at`.
                if at < lb {
                    let x = match b.try_get(at) { Option::Some(x) => x, Option::None => 0 };
                    let y = if at % 2 == 0 { wrapping_u8(wrapping_add(widen_u64(x), 1)) } else { wrapping_u8(wrapping_sub(widen_u64(x), 1)) };
                    if at != la {
                        b.try_set(at, y);
                    }
                }
                let s = a.as_slice();
                let t = b.as_slice();
                acc = wrapping_add(wrapping_mul(acc, 3), rank(in s, in t));
                acc = wrapping_add(acc, rank(in a[0..la], in b[0..lb]));
            }
        }
    }
    acc
}
";
    differential("cmp", source);
}
