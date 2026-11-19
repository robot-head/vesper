//! Drop glue on every exit (self-hosting SH1 lane 2, #1145): `Fail::raise`,
//! `?`, handler aborts and early `return` drop every live owned local, and
//! returning one field of a local struct drops the others. The shapes are
//! gaps 1-3 of the toolchain pilots (docs/parallel-results/
//! self-host-toolchain-pilots.md). Each program runs in the JIT with the
//! optimizer on and off, where the sequence runtime counts every string and
//! sequence buffer the program did not free itself; it must agree with the
//! evaluator, and both object targets must build.
use vesper::{check, native, parser, runtime::Runtime, seq::native_runtime};

fn world(source: &str) -> check::World {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "parse: {:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "check: {:#?}", c.diagnostics);
    c.world
}

fn execute(source: &str) -> String {
    let w = world(source);
    let mut results = vec![];
    for optimize in [true, false] {
        vesper::fip::native_runtime::reset_counts();
        let stray = native_runtime::stray_frees();
        let value = native::jit_with_options(&w, "main", optimize)
            .unwrap_or_else(|e| panic!("JIT (optimize {optimize}) failed: {e}"))
            .to_string();
        assert_eq!(
            native_runtime::teardown_frees(),
            0,
            "JIT (optimize {optimize}) leaked strings or sequences"
        );
        assert_eq!(
            native_runtime::stray_frees(),
            stray,
            "JIT (optimize {optimize}) freed a buffer that was not live"
        );
        assert_eq!(
            vesper::fip::native_runtime::alloc_count(),
            vesper::fip::native_runtime::free_count(),
            "optimize {optimize}: boxes leaked or freed twice"
        );
        results.push(value);
    }
    assert_eq!(results[0], results[1], "optimized and unoptimized JIT disagree");
    let evaluated = Runtime::new(&w).run("main", vec![]).unwrap().to_string();
    assert_eq!(evaluated, results[0], "the evaluator and the JIT disagree");
    for target in ["aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu"] {
        let (object, _) = native::object_for_target(&w, "main", Some(target))
            .unwrap_or_else(|e| panic!("{target} object: {e}"));
        assert_eq!(&object[..4], b"\x7fELF");
    }
    results.remove(0)
}

/// Gap 1: `Fail::raise` with a live local struct whose `String` field owns
/// a buffer.
#[test]
fn raise_drops_a_live_struct_string_field() {
    let src = "struct P { out: String, err: u64 }
fn f(n: u64) -> String eff [Fail[u64]] {
    let mut p = P { out: String::with_capacity(64), err: 0 };
    p.out.push_str(\"partial\");
    if n == 0 { Fail::raise(1); }
    let P { out, err } = p;
    out
}
fn main() -> u64 {
    let a = handle Fail[u64] in { f(0).len() } with { return x => x, raise(e, k) => wrapping_add(100, e), };
    let b = handle Fail[u64] in { f(1).len() } with { return x => x, raise(e, k) => wrapping_add(100, e), };
    wrapping_add(wrapping_mul(a, 1000), b)
}
";
    assert_eq!(execute(src), "101007");
}

/// Gap 2: returning one field of a local struct drops the other fields.
#[test]
fn returning_a_field_drops_the_other_fields() {
    let src = "struct R { found: ChunkSeq[u64], scope: ChunkSeq[u64] }
fn g(n: u64) -> ChunkSeq[u64] {
    let mut r = R { found: ChunkSeq::new(), scope: ChunkSeq::new() };
    r.found.push(n);
    r.scope.push(wrapping_add(n, 1));
    r.scope.push(wrapping_add(n, 2));
    r.found
}
fn main() -> u64 {
    let s = g(4);
    s.len()
}
";
    assert_eq!(execute(src), "1");
}

/// Gap 3: an early `return` inside an `if` after a scratch struct is made.
#[test]
fn early_return_drops_a_scratch_struct() {
    let src = "struct P { out: String, err: u64 }
fn h(c: bool) -> u64 {
    let mut p = P { out: String::with_capacity(16), err: 3 };
    p.out.push_str(\"scratch\");
    if c { return 0; }
    let P { out, err } = p;
    wrapping_add(out.len(), err)
}
fn main() -> u64 { wrapping_add(wrapping_mul(h(true), 100), h(false)) }
";
    assert_eq!(execute(src), "10");
}

/// `?` on a failing `Result` (which performs `Fail[E]`) with a live
/// struct that owns a string and a sequence.
#[test]
fn question_mark_drops_live_fields() {
    let src = "struct P { out: String, seen: ChunkSeq[u64], n: u64 }
fn check(n: u64) -> Result[u64, u64] { if n == 0 { Result::Err(7) } else { Result::Ok(n) } }
fn f(n: u64) -> u64 eff [Fail[u64]] {
    let mut p = P { out: String::with_capacity(8), seen: ChunkSeq::new(), n: n };
    p.out.push_str(\"abc\");
    p.seen.push(n);
    let v = check(n)?;
    let P { out, seen, n } = p;
    wrapping_add(wrapping_add(out.len(), seen.len()), v)
}
fn main() -> u64 {
    let a = handle Fail[u64] in { f(0) } with { return x => x, raise(e, k) => wrapping_add(100, e), };
    let b = handle Fail[u64] in { f(2) } with { return x => x, raise(e, k) => wrapping_add(100, e), };
    wrapping_add(wrapping_mul(a, 1000), b)
}
";
    assert_eq!(execute(src), "107006");
}

/// A handler that aborts its body (a `Fail` clause that does not resume)
/// drops the body's live struct.
#[test]
fn a_handler_abort_drops_the_body_struct() {
    let src = "struct P { out: String, err: u64 }
fn work(n: u64) -> u64 eff [Fail[u64]] {
    let mut p = P { out: String::with_capacity(8), err: n };
    p.out.push_str(\"body\");
    if p.err > 2 { Fail::raise(p.err); }
    let P { out, err } = p;
    wrapping_add(out.len(), err)
}
fn main() -> u64 {
    let mut t: u64 = 0;
    for n in 0..6 {
        t = wrapping_add(t, handle Fail[u64] in {
            let mut q = P { out: String::with_capacity(4), err: 0 };
            q.out.push_str(\"outer\");
            let r = work(n);
            let P { out, err } = q;
            wrapping_add(r, out.len())
        } with { return v => v, raise(e, k) => wrapping_mul(e, 100), });
    }
    t
}
";
    // n = 0, 1, 2 give 4 + n + 5; n = 3, 4, 5 abort with 100 n.
    assert_eq!(execute(src), "1230");
}

/// The early return sits in a loop, and only some iterations take it.
#[test]
fn an_early_return_in_a_loop_drops_the_scratch_struct() {
    let src = "struct S { a: String, b: ChunkSeq[u64] }
fn first_big(limit: u64) -> u64 {
    let mut s = S { a: String::with_capacity(4), b: ChunkSeq::new() };
    for i in 0..10 {
        s.b.push(i);
        s.a.push_str(\"x\");
        if wrapping_mul(i, i) > limit { return i; }
    }
    let S { a, b } = s;
    wrapping_add(a.len(), b.len())
}
fn main() -> u64 {
    wrapping_add(wrapping_mul(first_big(10), 100), first_big(1000))
}
";
    assert_eq!(execute(src), "420");
}

/// Returning a field of a nested struct drops every other field, nested
/// ones included.
#[test]
fn returning_a_nested_field_drops_the_rest() {
    let src = "struct In { s: String, t: String }
struct Out { inner: In, seq: ChunkSeq[u64], k: u64 }
fn g(n: u64) -> String {
    let mut o = Out { inner: In { s: String::with_capacity(4), t: String::with_capacity(4) }, seq: ChunkSeq::new(), k: n };
    o.inner.s.push_str(\"keep\");
    o.inner.t.push_str(\"drop\");
    o.seq.push(n);
    o.inner.s
}
fn main() -> u64 { g(3).len() }
";
    assert_eq!(execute(src), "4");
}

/// A field moved out on one path only: the other path still owns it.
#[test]
fn a_field_moved_on_one_path_is_dropped_on_the_other() {
    let src = "struct R { found: ChunkSeq[u64], scope: ChunkSeq[u64] }
fn eat(sink s: ChunkSeq[u64]) -> u64 { s.len() }
fn g(c: bool) -> u64 {
    let mut r = R { found: ChunkSeq::new(), scope: ChunkSeq::new() };
    r.found.push(1);
    r.scope.push(2);
    r.scope.push(3);
    let n = if c { eat(r.found) } else { 9 };
    wrapping_add(n, r.scope.len())
}
fn main() -> u64 { wrapping_add(wrapping_mul(g(true), 100), g(false)) }
";
    assert_eq!(execute(src), "311");
}

/// A by-value (`sink`) parameter whose field is moved out drops the rest.
#[test]
fn a_sink_parameter_with_a_moved_field_drops_the_rest() {
    let src = "struct R { found: String, scope: String }
fn pick(sink r: R) -> String { r.found }
fn main() -> u64 {
    let mut r = R { found: String::with_capacity(4), scope: String::with_capacity(4) };
    r.found.push_str(\"ab\");
    r.scope.push_str(\"cde\");
    pick(r).len()
}
";
    assert_eq!(execute(src), "2");
}

/// An unbound `String` call result read into a scalar is dropped.
#[test]
fn an_unbound_string_result_is_dropped() {
    let src = "fn make(n: u64) -> String {
    let mut s = String::with_capacity(8);
    for i in 0..n { s.push_str(\"z\"); }
    s
}
fn main() -> u64 { wrapping_add(make(3).len(), make(4).len()) }
";
    assert_eq!(execute(src), "7");
}

/// A `Fail` clause that raises again past the function: the body's
/// struct is freed once, when the body is unwound.
#[test]
fn a_clause_that_raises_again_frees_the_body_struct_once() {
    let src = "struct P { out: String, err: u64 }
fn held(x: u64) -> u64 eff [Fail[u64]] {
    handle Fail[u64] in {
        let mut p = P { out: String::with_capacity(8), err: x };
        p.out.push_str(\"held\");
        if x > 5 { Fail::raise(x); }
        let P { out, err } = p;
        wrapping_add(out.len(), err)
    } with { return v => v, raise(e, k) => if e > 6 { Fail::raise(wrapping_add(e, 10)) } else { e }, }
}
fn main() -> u64 {
    let a = handle Fail[u64] in { held(7) } with { return x => x, raise(e, k) => wrapping_mul(e, 100), };
    let b = handle Fail[u64] in { held(6) } with { return x => x, raise(e, k) => wrapping_mul(e, 100), };
    let c = handle Fail[u64] in { held(2) } with { return x => x, raise(e, k) => wrapping_mul(e, 100), };
    wrapping_add(wrapping_add(a, wrapping_mul(b, 10)), c)
}
";
    // a = 1700, b = 6, c = 6.
    assert_eq!(execute(src), "1766");
}
