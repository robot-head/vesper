//! Box drops on `Fail` exits (P3b gaps lane 10, #1095, part 3): a box a
//! function owns when a call it makes fails is freed on the failing path, in
//! acyclic functions and in functions with loops, natively (JIT optimize on
//! and off, objects) as in the evaluator. Counted with the native box
//! allocator: every allocation is freed exactly once.
use vesper::{check, native, parser, runtime::Runtime, seq::native_runtime};

fn world(source: &str) -> check::World {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "parse: {:?}", p.diagnostics);
    assert_eq!(p.syntax().text().to_string(), source, "lossless CST");
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "check: {:#?}", c.diagnostics);
    c.world
}

/// Runs `main` in the JIT with the optimizer on and off: every box is
/// freed exactly once, every sequence is freed by the program itself, both
/// agree with the evaluator, and both object targets build.
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
            vesper::fip::native_runtime::alloc_count(),
            vesper::fip::native_runtime::free_count(),
            "optimize {optimize}: boxes leaked or freed twice"
        );
        assert_eq!(
            native_runtime::teardown_frees(),
            0,
            "JIT (optimize {optimize}) left sequences for teardown"
        );
        assert_eq!(
            native_runtime::stray_frees(),
            stray,
            "JIT (optimize {optimize}) freed a sequence that was not live"
        );
        results.push(value);
    }
    assert_eq!(
        results[0], results[1],
        "optimized and unoptimized JIT disagree"
    );
    let evaluated = Runtime::new(&w).run("main", vec![]).unwrap().to_string();
    assert_eq!(evaluated, results[0], "the evaluator and the JIT disagree");
    for target in ["aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu"] {
        let (object, _) = native::object_for_target(&w, "main", Some(target))
            .unwrap_or_else(|e| panic!("{target} object: {e}"));
        assert_eq!(&object[..4], b"\x7fELF");
    }
    results.remove(0)
}

const P: &str = "struct P { v: i64 }
fn risky(x: i64) -> i64 eff [Fail[RuntimeError]] { if x > 5 { 100 / wrapping_sub(x, x) } else { x } }
";

fn caught(body: &str) -> String {
    format!(
        "{P}{body}
fn main() -> i64 {{
    let a = handle Fail[RuntimeError] in {{ held(7) }} with {{ return x => x, raise(e, k) => 11, }};
    let b = handle Fail[RuntimeError] in {{ held(3) }} with {{ return x => x, raise(e, k) => 11, }};
    wrapping_add(wrapping_mul(a, 1000), b)
}}
"
    )
}

#[test]
fn an_acyclic_function_frees_its_box_when_a_call_fails() {
    let src = caught(
        "fn held(x: i64) -> i64 eff [Fail[RuntimeError]] {
    let b = Box::new(P { v: x });
    let r = risky(x);
    let Box(p) = b;
    wrapping_add(r, p.v)
}",
    );
    assert_eq!(execute(&src), "11006");
}

#[test]
fn a_loop_function_frees_its_box_when_a_call_fails() {
    let src = caught(
        "fn held(x: i64) -> i64 eff [Fail[RuntimeError]] {
    let mut b = Box::new(P { v: 0 });
    for i in 0..x {
        b = Box::new(P { v: risky(i) });
    }
    let Box(p) = b;
    p.v
}",
    );
    assert_eq!(execute(&src), "11002");
}

#[test]
fn a_box_taken_sink_by_a_failing_callee_is_its_to_drop() {
    let src = caught(
        "fn eat(sink b: Box[P], x: i64) -> i64 eff [Fail[RuntimeError]] {
    let r = risky(x);
    let Box(p) = b;
    wrapping_add(r, p.v)
}
fn held(x: i64) -> i64 eff [Fail[RuntimeError]] {
    let keep = Box::new(P { v: 1 });
    let r = eat(Box::new(P { v: x }), x);
    let Box(k) = keep;
    wrapping_add(r, k.v)
}",
    );
    assert_eq!(execute(&src), "11007");
}

#[test]
fn boxes_in_fields_and_trait_objects_are_freed_when_a_call_fails() {
    let src = caught(
        "struct Pair { a: Box[P], b: Box[P], n: i64 }
trait Get { fn get(in self) -> i64; }
impl Get for P { fn get(in self) -> i64 { self.v } }
fn held(x: i64) -> i64 eff [Fail[RuntimeError]] {
    let pair = Pair { a: Box::new(P { v: 1 }), b: Box::new(P { v: 2 }), n: x };
    let o: Box[dyn Get] = Box::new(P { v: 3 });
    let r = risky(pair.n);
    let Pair { a: a, b: b, n: n } = pair;
    let Box(pa) = a;
    let Box(pb) = b;
    wrapping_add(wrapping_add(r, o.get()), wrapping_add(pa.v, pb.v))
}",
    );
    assert_eq!(execute(&src), "11009");
}

#[test]
fn a_box_owned_in_a_handled_body_is_freed_when_its_handler_aborts() {
    let src = format!(
        "{P}fn main() -> i64 {{
    let mut t: i64 = 0;
    for x in 2..8 {{
        t = wrapping_add(t, handle Fail[RuntimeError] in {{
            let b = Box::new(P {{ v: x }});
            let r = risky(x);
            let Box(p) = b;
            wrapping_add(r, p.v)
        }} with {{ return v => v, raise(e, k) => 100, }});
    }}
    t
}}
"
    );
    // x = 2..5 give 2x; x = 6, 7 abort: 4 + 6 + 8 + 10 + 200.
    assert_eq!(execute(&src), "228");
}
