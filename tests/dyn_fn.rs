//! Function objects (manuscript §7.3, port wave P3b lane 1, #1063):
//! closures lent as `in dyn Fn(..)` and `inout dyn FnMut(..)`, and stored
//! as `Box[dyn FnMut(..)]` and `Box[dyn FnOnce(..)]`, in the Cranelift JIT
//! (optimizer on and off), the evaluator and AArch64 and x86-64 objects,
//! with a typed diagnostic for each rejected use.
//!
//! Sources are inline: the self-hosted front end does not parse `dyn` yet.
use vesper::{check, native, parser, runtime::Runtime, seq::native_runtime};

fn world(source: &str) -> check::World {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "parse: {:?}", p.diagnostics);
    assert_eq!(p.syntax().text().to_string(), source, "lossless CST");
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "check: {:#?}", c.diagnostics);
    c.world
}

/// Runs `main` in the JIT with the optimizer on and off (they must agree,
/// free every sequence themselves and free nothing twice) and emits
/// AArch64 and x86-64 objects.
fn execute(source: &str) -> String {
    let w = world(source);
    // Every box is freed once, by the JIT with the optimizer on and off.
    for optimize in [true, false] {
        vesper::fip::native_runtime::reset_counts();
        native::jit_with_options(&w, "main", optimize)
            .unwrap_or_else(|e| panic!("JIT (optimize {optimize}) failed: {e}"));
        assert_eq!(
            vesper::fip::native_runtime::alloc_count(),
            vesper::fip::native_runtime::free_count(),
            "optimize {optimize}: boxes leaked or freed twice"
        );
    }
    let mut results = vec![];
    for optimize in [true, false] {
        let stray = native_runtime::stray_frees();
        let value = native::jit_with_options(&w, "main", optimize)
            .unwrap_or_else(|e| panic!("JIT (optimize {optimize}) failed: {e}"))
            .to_string();
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

fn rejects(source: &str, code: &str) -> Vec<vesper::ast::Diagnostic> {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "parse: {:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(
        c.diagnostics.iter().any(|d| d.code == code),
        "expected {code}; got {:#?}",
        c.diagnostics
    );
    c.diagnostics
}

const APPLY: &str = "fn apply(in f: dyn Fn(i64) -> i64, x: i64) -> i64 { f(x) }
fn twice(inout f: dyn FnMut(i64) -> i64, x: i64) -> i64 { wrapping_add(f(x), f(x)) }
";

#[test]
fn closures_lent_as_fn_and_fn_mut() {
    let src = format!(
        "{APPLY}fn main() -> i64 {{
    let k: i64 = 5;
    let base = 100;
    let a = apply(in |x: i64| wrapping_add(wrapping_mul(x, k), base), 2);
    let mut count: i64 = 0;
    let b = twice(inout |x: i64| {{ count = wrapping_add(count, 1); wrapping_mul(x, count) }}, 10);
    // `count` moved into the object: the caller's copy is unchanged.
    let c = apply(in |x: i64| wrapping_add(x, 1), apply(in |y: i64| wrapping_mul(y, 3), 4));
    wrapping_add(wrapping_mul(a, 10000), wrapping_add(wrapping_mul(b, 100), c))
}}
"
    );
    // a = 2 * 5 + 100; b = 10 * 1 + 10 * 2 (the object's count persists
    // across its calls); c = 4 * 3 + 1.
    assert_eq!(execute(&src), (110 * 10000 + 30 * 100 + 13).to_string());
}

const CALLBACKS: &str = "struct Bus { handlers: ChunkSeq[Box[dyn FnMut(i64) -> i64]], fired: i64 }
impl Bus {
    fn fire(inout self, v: i64) -> i64 eff [Fail[RuntimeError]] {
        let mut total: i64 = 0;
        for i in 0..self.handlers.len() {
            total = wrapping_add(total, self.handlers[i](v));
        }
        self.fired = wrapping_add(self.fired, 1);
        total
    }
}
fn counter(step: i64) -> Box[dyn FnMut(i64) -> i64] {
    let mut n: i64 = 0;
    Box::new(|v: i64| { n = wrapping_add(n, step); wrapping_add(v, n) })
}
";

#[test]
fn sequences_of_boxed_fn_mut_callbacks() {
    let src = format!(
        "{CALLBACKS}fn main() -> i64 {{
    handle Fail[RuntimeError] in {{ run() }} with {{ return x => x, raise(e, k) => -1, }}
}}
fn run() -> i64 eff [Fail[RuntimeError]] {{
    let mut bus = Bus {{ handlers: ChunkSeq::new[Box[dyn FnMut(i64) -> i64]](), fired: 0 }};
    bus.handlers.push(counter(1));
    bus.handlers.push(counter(10));
    let scale: i64 = 3;
    bus.handlers.push(Box::new[dyn FnMut(i64) -> i64](|v: i64| wrapping_mul(v, scale)));
    let first = bus.fire(1);
    let second = bus.fire(2);
    wrapping_add(wrapping_mul(first, 1000), wrapping_add(second, bus.fired))
}}
"
    );
    // first: (1 + 1) + (1 + 10) + 3 = 16; second: (2 + 2) + (2 + 20) + 6 = 32.
    assert_eq!(execute(&src), (16 * 1000 + 32 + 2).to_string());
}

const ONCE: &str = "struct Token { id: i64, words: ChunkSeq[i64] }
fn make_token(id: i64) -> Token {
    let mut words: ChunkSeq[i64] = ChunkSeq::new[i64]();
    words.push(id);
    Token { id: id, words: words }
}
fn finisher(sink t: Token) -> Box[dyn FnOnce(i64) -> i64] {
    Box::new(|x: i64| { let Token { id, words } = t; wrapping_add(wrapping_mul(id, 10), x) })
}
fn run_once(sink f: Box[dyn FnOnce(i64) -> i64], x: i64) -> i64 { f(x) }
";

#[test]
fn fn_once_boxes_are_consumed_by_the_call() {
    // Called once, dropped uncalled, and passed on and called: every
    // captured sequence is freed exactly once.
    let src = format!(
        "{ONCE}fn main() -> i64 {{
    let a = finisher(make_token(4));
    let b = finisher(make_token(5));
    let unused = finisher(make_token(6));
    let x = a(2);
    let y = run_once(sink b, 3);
    wrapping_add(wrapping_mul(x, 1000), y)
}}
"
    );
    assert_eq!(execute(&src), (42 * 1000 + 53).to_string());
}

#[test]
fn objects_lent_allocate_nothing() {
    // `in dyn Fn` and `inout dyn FnMut` objects live in the caller's frame.
    let src = format!(
        "{APPLY}fn main() -> i64 {{
    let k: i64 = 7;
    let mut s: i64 = 0;
    wrapping_add(apply(in |x: i64| wrapping_mul(x, k), 6), twice(inout |x: i64| {{ s = wrapping_add(s, x); s }}, 1))
}}
"
    );
    let w = world(&src);
    for optimize in [true, false] {
        vesper::fip::native_runtime::reset_counts();
        let value = native::jit_with_options(&w, "main", optimize).unwrap();
        assert_eq!(value.to_string(), (42 + 3).to_string());
        assert_eq!(vesper::fip::native_runtime::alloc_count(), 0);
    }
}

#[test]
fn effectful_function_objects() {
    let src = "linear effect Log { fn note(v: i64) -> i64; }
fn each(inout f: dyn FnMut(i64) -> i64 eff [Log], n: i64) -> i64 eff [Log] {
    wrapping_add(f(n), f(wrapping_add(n, 1)))
}
fn hook() -> Box[dyn FnMut(i64) -> i64 eff [Log]] {
    let mut seen: i64 = 0;
    Box::new(|v: i64| { seen = wrapping_add(seen, 1); Log::note(wrapping_add(v, seen)) })
}
fn main() -> i64 {
    let mut h = hook();
    let k: i64 = 100;
    handle Log state notes = 0 in {
        let a = each(inout |v: i64| Log::note(wrapping_add(v, k)), 1);
        let b = wrapping_add(h(10), h(10));
        wrapping_add(wrapping_mul(a, 1000), b)
    } with {
        return (x, notes) => wrapping_add(wrapping_mul(x, 10), notes),
        note(v, notes, k) => k(wrapping_mul(v, 2), wrapping_add(notes, 1)),
    }
}
";
    // a = 2 * 101 + 2 * 102 = 406; b = 2 * 11 + 2 * 12 = 46; four notes.
    assert_eq!(execute(src), ((406 * 1000 + 46) * 10 + 4).to_string());
}

#[test]
fn review_fixes_for_function_objects() {
    // An argument before a hoisted `inout` closure runs first, so the
    // closure captures what it left.
    let order = "fn run(x: i64, inout f: dyn FnMut() -> i64) -> i64 { wrapping_add(x, f()) }
fn main() -> i64 { let mut n: i64 = 0; run({ n = 1; 10 }, inout || n) }
";
    assert_eq!(execute(order), "11");
    // A `FnOnce` body may mutate the captures it owns.
    let once = "fn mk() -> Box[dyn FnOnce() -> i64] {
    let mut n: i64 = 4;
    Box::new(|| { n = wrapping_add(n, 1); n })
}
fn main() -> i64 { let f = mk(); f() }
";
    assert_eq!(execute(once), "5");
    // The receiver's type comes from the function that makes it.
    let recv = "struct A { k: i64 }
struct B { k: bool }
impl A { fn apply(in self, in f: dyn Fn(i64) -> i64) -> i64 { f(self.k) } }
impl B { fn apply(in self, in f: dyn Fn(bool) -> bool) -> bool { f(self.k) } }
fn make_b() -> B { B { k: false } }
fn main() -> i64 {
    let a = A { k: 3 };
    let x = a.apply(in |v| wrapping_mul(v, 2));
    if make_b().apply(in |v| !v) { x } else { 0 }
}
";
    assert_eq!(execute(recv), "6");
    // A field named `call` holding a function object is called.
    let field = "struct Handler { call: Box[dyn Fn(i64) -> i64] }
fn main() -> i64 {
    let h = Handler { call: Box::new(|x: i64| wrapping_add(x, 1)) };
    h.call(41)
}
";
    assert_eq!(execute(field), "42");
    // A written annotation is checked against the signature, not replaced.
    rejects(
        &format!("{APPLY}fn main() -> i64 {{ apply(in |x: bool| x, 1) }}\n"),
        "E0200",
    );
    rejects(
        &format!("{APPLY}fn main() -> i64 {{ apply(in |x| -> bool x, 1) }}\n"),
        "E0200",
    );
    // A written effect row restricts the body.
    rejects(
        "linear effect Log { fn note(v: i64) -> i64; }
fn each(in f: dyn Fn(i64) -> i64 eff [Log]) -> i64 eff [Log] { f(1) }
fn main() -> i64 eff [Log] { each(in |x: i64| -> i64 eff [] Log::note(x)) }
",
        "E0401",
    );
}

#[test]
fn rejected_function_objects() {
    // A `Fn` object cannot mutate its captures.
    rejects(
        &format!(
            "{APPLY}fn main() -> i64 {{ let mut n: i64 = 0; apply(in |x: i64| {{ n = x; n }}, 1) }}\n"
        ),
        "E0307",
    );
    // A closure capturing a local of unknown type must be annotated.
    rejects(
        &format!(
            "{APPLY}fn mk() -> i64 {{ 1 }}\nfn main() -> i64 {{ let n = wrapping_add(mk(), 1); apply(in |x: i64| wrapping_add(x, n), 1) }}\n"
        ),
        "E0102",
    );
    // A function object is a second-class `dyn` parameter.
    rejects(
        &format!("{APPLY}fn keep(in f: dyn Fn(i64) -> i64) -> i64 {{ let g = f; 0 }}\n"),
        "E0631",
    );
    // An object's effects reach its caller's row.
    rejects(
        "linear effect Log { fn note(v: i64) -> i64; }
fn quiet(in f: dyn Fn(i64) -> i64 eff [Log]) -> i64 { f(1) }
",
        "E0401",
    );
    // A closure capturing a linear value is linear: it can only be called
    // once, and an object may be dropped uncalled, so it is refused.
    rejects(
        "linear struct Ticket { id: i64 }
fn hold(sink t: Ticket) -> Box[dyn FnOnce() -> i64] {
    Box::new(|| { let Ticket { id } = t; id })
}
",
        "E0205",
    );
    rejects(
        "linear struct Ticket { id: i64 }
fn apply(in f: dyn Fn(i64) -> i64, x: i64) -> i64 { f(x) }
fn main() -> i64 { let t = Ticket { id: 1 }; apply(in |x: i64| { let Ticket { id } = t; x }, 1) }
",
        "E0310",
    );
    // Diagnostics inside a lowered closure point into the source.
    let d = rejects(
        &format!(
            "{APPLY}fn main() -> i64 {{ let mut n: i64 = 0; apply(in |x: i64| {{ n = x; n }}, 1) }}\n"
        ),
        "E0307",
    );
    let len = APPLY.len() + 200;
    assert!(d.iter().all(|d| d.span.end <= len), "{d:#?}");
    // Generic captures are not lowered yet.
    rejects(
        "fn wrap[T](sink x: T) -> Box[dyn FnOnce() -> T] { Box::new(|| x) }\n",
        "E0633",
    );
    // The arity is the signature's.
    rejects(
        &format!("{APPLY}fn main() -> i64 {{ apply(in |x: i64, y: i64| x, 1) }}\n"),
        "E0207",
    );
}

/// `cargo test --test dyn_fn -- --ignored --nocapture bench`: a call through
/// `Box[dyn Fn]`, `in dyn Fn` and a monomorphized closure parameter
/// (`fn` values are specialized at each call), optimized JIT.
#[test]
#[ignore = "benchmark"]
fn bench_function_objects_against_specialized_closures() {
    const N: u64 = 50_000_000;
    let source = |call: &str| {
        format!(
            "fn by_box(in f: Box[dyn Fn(u64) -> u64], x: u64) -> u64 {{ f(x) }}
fn by_dyn(in f: dyn Fn(u64) -> u64, x: u64) -> u64 {{ f(x) }}
fn by_fn(sink f: fn(u64) -> u64, x: u64) -> u64 {{ f(x) }}
fn step(x: u64) -> u64 {{ wrapping_add(wrapping_mul(x, 3), 1) }}
// A function with a loop may borrow a boxed object, not own one.
fn run(in b: Box[dyn Fn(u64) -> u64]) -> u64 {{
    let mut x: u64 = 0;
    for i in 0..{N} {{ x = {call}; }}
    x
}}
fn main() -> u64 {{
    let b: Box[dyn Fn(u64) -> u64] = Box::new(|x: u64| wrapping_add(wrapping_mul(x, 3), 1));
    run(in b)
}}
"
        )
    };
    let mut timings = vec![];
    for (name, call) in [
        ("in Box[dyn Fn]", "by_box(in b, x)"),
        (
            "in dyn Fn",
            "by_dyn(in |y: u64| wrapping_add(wrapping_mul(y, 3), 1), x)",
        ),
        (
            "specialized fn value",
            "by_fn(|y: u64| wrapping_add(wrapping_mul(y, 3), 1), x)",
        ),
        ("direct", "step(x)"),
    ] {
        let w = world(&source(call));
        let start = std::time::Instant::now();
        let value = native::jit_with_options(&w, "main", true).unwrap();
        let elapsed = start.elapsed();
        timings.push((name, elapsed, value.to_string()));
    }
    assert!(timings.iter().all(|t| t.2 == timings[0].2), "{timings:?}");
    for (name, elapsed, _) in &timings {
        println!(
            "{name}: {:.3} ns per call ({elapsed:?} for {N}, compile included)",
            elapsed.as_nanos() as f64 / N as f64
        );
    }
}
