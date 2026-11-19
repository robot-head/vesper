//! Second-class trait objects (`in dyn`, `inout dyn`, `sink dyn`; manuscript
//! §7.1, port wave P3a lane 1, #1036): one compiled body dispatching through
//! vtables in the Cranelift JIT (optimizer on and off), the evaluator and
//! AArch64 and x86-64 objects, drops of unconsumed `sink dyn` parameters, zero heap
//! allocation per object, and a typed diagnostic for every rejected use.
//!
//! Sources are inline: the self-hosted front end's corpus covers every
//! repository `.ves` fixture and does not parse `dyn` yet.
use std::alloc::{GlobalAlloc, Layout, System};
use vesper::{check, native, parser, runtime::Runtime, seq::native_runtime};

/// Counts the heap allocations of each thread (the JIT runs on the test's).
struct Counting;
thread_local! {
    static ALLOCATIONS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}
fn allocations() -> u64 {
    ALLOCATIONS.with(std::cell::Cell::get)
}
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}
#[global_allocator]
static GLOBAL: Counting = Counting;

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

const SHAPES: &str = "struct Circle { r: i64 }
struct Rect { w: i64, h: i64 }
trait Shape {
    fn area(in self) -> i64;
    fn sides(in self) -> i64;
}
impl Shape for Circle {
    fn area(in self) -> i64 { wrapping_mul(wrapping_mul(self.r, self.r), 3) }
    fn sides(in self) -> i64 { 0 }
}
impl Shape for Rect {
    fn area(in self) -> i64 { wrapping_mul(self.w, self.h) }
    fn sides(in self) -> i64 { 4 }
}
impl Shape for i64 {
    fn area(in self) -> i64 { self }
    fn sides(in self) -> i64 { 1 }
}
fn describe(in s: dyn Shape) -> i64 {
    wrapping_add(wrapping_mul(s.area(), 10), s.sides())
}
fn twice(in s: dyn Shape) -> i64 {
    wrapping_add(describe(in s), Shape::area(s))
}
";

#[test]
fn one_body_dispatches_to_every_impl() {
    let src = format!(
        "{SHAPES}fn main() -> i64 {{
    let c = Circle {{ r: 2 }};
    let r = Rect {{ w: 3, h: 5 }};
    let n: i64 = 7;
    wrapping_add(wrapping_add(wrapping_mul(describe(in c), 1000000), wrapping_mul(describe(in r), 1000)), twice(in n))
}}
"
    );
    // Circle 12*10+0, Rect 15*10+4, i64 7*10+1 plus 7 again.
    assert_eq!(execute(&src), "120154078");
    let w = world(&src);
    // One `describe`, no instance per implementing type.
    assert!(w.functions.contains_key("describe"));
    assert!(
        !w.functions.keys().any(|n| n.starts_with("describe$")),
        "{:?}",
        w.functions.keys()
    );
    assert_eq!(
        w.dyn_objects.vtables.len(),
        3,
        "{:?}",
        w.dyn_objects.vtables
    );
    assert!(w.dyn_objects.stubs.contains_key("vtable$Shape$area"));
}

const COUNTERS: &str = "struct Counter { n: i64, steps: i64 }
trait Bump {
    fn bump(inout self, by: i64);
    fn get(in self) -> i64;
}
impl Bump for Counter {
    fn bump(inout self, by: i64) {
        self.n = wrapping_add(self.n, by);
        self.steps = wrapping_add(self.steps, 1);
    }
    fn get(in self) -> i64 { wrapping_add(wrapping_mul(self.n, 100), self.steps) }
}
fn twice(inout b: dyn Bump) {
    b.bump(5);
    b.bump(7);
}
fn thrice(inout b: dyn Bump) {
    twice(inout b);
    b.bump(1);
}
fn main() -> i64 {
    let mut c = Counter { n: 1, steps: 0 };
    thrice(inout c);
    c.get()
}
";

#[test]
fn inout_dyn_mutates_the_callers_place() {
    // n = 1 + 5 + 7 + 1, three steps.
    assert_eq!(execute(COUNTERS), "1403");
}

const NOTES: &str = "struct Note { body: String, words: ChunkSeq[u64] }
trait Consume {
    fn size(in self) -> u64;
    fn finish(sink self) -> u64;
}
impl Consume for Note {
    fn size(in self) -> u64 { self.words.len() }
    fn finish(sink self) -> u64 {
        let n = self.words.len();
        wrapping_add(n, wrapping_add(n, 1))
    }
}
fn note(n: u64) -> Note {
    let mut words: ChunkSeq[u64] = ChunkSeq::new[u64]();
    let mut body = String::new();
    for i in 0..n {
        words.push(i);
        body.push_str(in \"word\");
    }
    Note { body: body, words: words }
}
fn consume(sink x: dyn Consume) -> u64 { x.finish() }
fn forward(sink x: dyn Consume) -> u64 { consume(sink x) }
fn peek(sink x: dyn Consume) -> u64 { x.size() }
fn maybe(sink x: dyn Consume, take: bool) -> u64 {
    if take { x.finish() } else { 100 }
}
";

#[test]
fn sink_dyn_consumes_or_drops_exactly_once() {
    // consume 3+4, forward 2+3, peek drops a 5-word note, maybe(take) 1+2,
    // maybe(skip) drops its note and returns 100.
    let src = format!(
        "{NOTES}fn main() -> u64 {{
    let a = consume(sink note(3));
    let b = forward(sink note(2));
    let c = peek(sink note(5));
    let d = maybe(sink note(1), true);
    let e = maybe(sink note(4), false);
    wrapping_add(wrapping_add(wrapping_add(a, b), wrapping_add(c, d)), e)
}}
"
    );
    // `execute` checks every sequence and string is freed once.
    assert_eq!(execute(&src), (7 + 5 + 5 + 3 + 100).to_string());
}

const FALLIBLE: &str = "struct Div { by: i64 }
struct Scale { k: i64 }
trait Step {
    fn apply(in self, x: i64) -> i64 eff [Fail[i64]];
}
impl Step for Div {
    fn apply(in self, x: i64) -> i64 eff [Fail[i64]] {
        let r: Result[i64, i64] = if self.by == 0 { Err(7) } else { Ok(wrapping_add(x, self.by)) };
        r?
    }
}
impl Step for Scale {
    fn apply(in self, x: i64) -> i64 { wrapping_mul(x, self.k) }
}
fn run(in s: dyn Step, x: i64) -> i64 eff [Fail[i64]] { s.apply(x) }
fn guarded(in s: dyn Step, x: i64) -> i64 {
    handle Fail[i64] in {
        run(in s, x)
    } with {
        return value => value,
        raise(error, resume) => wrapping_sub(0, error),
    }
}
fn main() -> i64 {
    let ok = Div { by: 3 };
    let zero = Div { by: 0 };
    let k = Scale { k: 5 };
    wrapping_add(wrapping_add(wrapping_mul(guarded(in ok, 4), 10000), wrapping_mul(guarded(in zero, 4), 100)), guarded(in k, 4))
}
";

#[test]
fn fail_effects_flow_through_dyn_calls() {
    // 7 * 10000 - 7 * 100 + 20
    assert_eq!(execute(FALLIBLE), "69320");
}

const GENERIC: &str = "struct Circle { r: i64 }
struct Square { s: i64 }
trait Shape { fn area(in self) -> i64; }
impl Shape for Circle { fn area(in self) -> i64 { wrapping_mul(self.r, 3) } }
impl Shape for Square { fn area(in self) -> i64 { wrapping_mul(self.s, self.s) } }
fn scaled[T](in k: T, in s: dyn Shape) -> i64 where T: Shape {
    wrapping_mul(k.area(), s.area())
}
fn pick[T: Shape](in a: T, in b: T) -> i64 {
    wrapping_add(scaled(in a, in b), scaled(in b, in a))
}
fn main() -> i64 {
    let c = Circle { r: 2 };
    let q = Square { s: 3 };
    wrapping_add(wrapping_mul(scaled(in c, in q), 1000), pick(in q, in q))
}
";

#[test]
fn bounded_generics_take_dyn_parameters() {
    // scaled(c, q) = 6 * 9; pick(q, q) = 81 + 81.
    assert_eq!(execute(GENERIC), "54162");
    let w = world(GENERIC);
    // `scaled` is instantiated per `T` only: the `dyn` parameter stays one
    // vtable call in each instance.
    let instances: Vec<_> = w
        .functions
        .keys()
        .filter(|n| n.starts_with("scaled"))
        .collect();
    assert_eq!(instances.len(), 2, "{instances:?}");
}

/// A `sink dyn` argument moves the caller's value; the object itself
/// allocates nothing: the program's allocation count does not grow with
/// the number of objects it builds.
#[test]
fn objects_allocate_nothing() {
    let program = |n: u64| {
        format!(
            "{SHAPES}fn main() -> i64 {{
    let mut total: i64 = 0;
    for i in 0..{n} {{
        let c = Circle {{ r: 1 }};
        let r = Rect {{ w: 1, h: 2 }};
        total = wrapping_add(total, wrapping_add(describe(in c), twice(in r)));
    }}
    total
}}
"
        )
    };
    let count = |n: u64| {
        let w = world(&program(n));
        let before = allocations();
        let value = native::jit_with_options(&w, "main", true).unwrap();
        (allocations() - before, value.to_string())
    };
    let (small, v1) = count(10);
    let (large, v2) = count(10000);
    assert_eq!(v1, (10 * (30 + 24 + 2)).to_string());
    assert_eq!(v2, (10000 * (30 + 24 + 2)).to_string());
    // Compiling the two programs allocates about the same; 9990 more
    // iterations would add at least 9990 allocations if objects allocated.
    assert!(
        large.abs_diff(small) < 500,
        "allocations: {small} for 10 iterations, {large} for 10000"
    );
}

const TRAITS: &str = "struct P { x: i64 }
trait Shape { fn area(in self) -> i64; }
impl Shape for P { fn area(in self) -> i64 { self.x } }
";

#[test]
fn dyn_outside_parameter_position_is_rejected() {
    for (what, body) in [
        ("result", "fn f(in s: dyn Shape) -> dyn Shape { s }"),
        ("field", "struct Holder { s: dyn Shape }"),
        ("let", "fn f(in p: P) -> i64 { let s: dyn Shape = p; 0 }"),
        (
            "type argument",
            "fn f(in s: Option[dyn Shape]) -> i64 { 0 }",
        ),
        (
            "trait method",
            "trait Visit { fn visit(in self, in s: dyn Shape) -> i64; }",
        ),
        ("closure", "fn f() -> i64 { let g = | s: dyn Shape | 0; 0 }"),
    ] {
        let d = rejects(
            &format!("{TRAITS}{body}\nfn main() -> i64 {{ 0 }}\n"),
            "E0632",
        );
        assert!(!d.is_empty(), "{what}");
    }
}

#[test]
fn escaping_dyn_values_are_rejected() {
    for (what, body) in [
        ("bound", "fn f(in s: dyn Shape) -> i64 { let t = s; 0 }"),
        (
            "tuple",
            "fn f(in s: dyn Shape) -> i64 { let t = (s, 1); 0 }",
        ),
        (
            "captured",
            "fn f(in s: dyn Shape) -> i64 { let g = || s.area(); 0 }",
        ),
        ("returned", "fn f(in s: dyn Shape) -> i64 { return s; }"),
        (
            "generic argument",
            "fn id[T](in x: T) -> i64 { 0 }\nfn f(in s: dyn Shape) -> i64 { id(in s) }",
        ),
    ] {
        let d = rejects(
            &format!("{TRAITS}{body}\nfn main() -> i64 {{ 0 }}\n"),
            "E0631",
        );
        assert!(!d.is_empty(), "{what}");
    }
    // A concrete parameter does not accept the object: a type error.
    rejects(
        &format!(
            "{TRAITS}fn g(in p: P) -> i64 {{ p.x }}\nfn f(in s: dyn Shape) -> i64 {{ g(in s) }}\nfn main() -> i64 {{ 0 }}\n"
        ),
        "E0200",
    );
    // A shadowing binding is an ordinary local.
    let src = format!(
        "{TRAITS}fn f(in s: dyn Shape) -> i64 {{ let s = 4; wrapping_add(s, 1) }}\nfn main() -> i64 {{ let p = P {{ x: 1 }}; f(in p) }}\n"
    );
    assert_eq!(execute(&src), "5");
}

#[test]
fn non_object_safe_traits_are_rejected() {
    for (what, decl) in [
        (
            "generic method",
            "trait T1 { fn m[U](in self, in u: U) -> i64; }",
        ),
        ("no receiver", "trait T1 { fn make() -> i64; }"),
        (
            "Self argument",
            "trait T1 { fn same(in self, in other: Self) -> bool; }",
        ),
        ("Self result", "trait T1 { fn dup(in self) -> Self; }"),
        (
            "associated type",
            "trait T1 { type Out: mod copy; fn m(in self) -> i64; }",
        ),
        // Handler effects are allowed since #1063 (tests/dyn_effects.rs);
        // an open row is not.
        (
            "open effect row",
            "trait T1 { fn m[r: eff](in self) -> i64 eff [r]; }",
        ),
    ] {
        let src = format!("{decl}\nfn f(in x: dyn T1) -> i64 {{ 0 }}\nfn main() -> i64 {{ 0 }}\n");
        let d = rejects(&src, "E0630");
        assert!(!d.is_empty(), "{what}");
    }
}

#[test]
fn missing_conformances_and_linear_sinks_are_rejected() {
    // No `Shape` for `bool`.
    rejects(
        &format!(
            "{TRAITS}fn f(in s: dyn Shape) -> i64 {{ s.area() }}\nfn main() -> i64 {{ f(in true) }}\n"
        ),
        "E0608",
    );
    // A generic type cannot be coerced before instantiation.
    rejects(
        &format!(
            "{TRAITS}fn f(in s: dyn Shape) -> i64 {{ s.area() }}\nfn g[T](in x: T) -> i64 {{ f(in x) }}\nfn main() -> i64 {{ g(in P {{ x: 1 }}) }}\n"
        ),
        "E0633",
    );
}

#[test]
fn other_backends_refuse_dyn() {
    let w = world(&format!(
        "{SHAPES}fn main() -> i64 {{ let c = Circle {{ r: 1 }}; describe(in c) }}\n"
    ));
    let e = vesper::dyn_trait::refuse(&w, "verified backend").unwrap_err();
    assert_eq!(e.code, "E0634");
}

/// Micro-benchmark (run with `--ignored --nocapture`): one call through a
/// vtable slot against the monomorphized call of the same method, in the
/// optimized JIT. Reported in docs/parallel-results/port-p3-dyn-trait.md.
#[test]
#[ignore = "benchmark"]
fn bench_indirect_against_monomorphized_call() {
    const N: u64 = 50_000_000;
    let source = |call: &str| {
        format!(
            "struct Acc {{ n: u64 }}
trait Step {{ fn step(in self, x: u64) -> u64; }}
impl Step for Acc {{ fn step(in self, x: u64) -> u64 {{ wrapping_add(wrapping_mul(x, 3), self.n) }} }}
fn by_dyn(in s: dyn Step, x: u64) -> u64 {{ s.step(x) }}
fn by_mono[T: Step](in s: T, x: u64) -> u64 {{ s.step(x) }}
fn main() -> u64 {{
    let a = Acc {{ n: 1 }};
    let mut x: u64 = 0;
    for i in 0..{N} {{ x = {call}; }}
    x
}}
"
        )
    };
    let mut timings = vec![];
    for (name, call) in [
        ("dyn", "by_dyn(in a, x)"),
        ("monomorphized", "by_mono(in a, x)"),
    ] {
        let w = world(&source(call));
        let start = std::time::Instant::now();
        let value = native::jit_with_options(&w, "main", true).unwrap();
        let elapsed = start.elapsed();
        timings.push((name, elapsed, value.to_string()));
    }
    assert_eq!(timings[0].2, timings[1].2);
    for (name, elapsed, _) in &timings {
        println!(
            "{name}: {:.3} ns per call ({elapsed:?} for {N} calls, compile included)",
            elapsed.as_nanos() as f64 / N as f64
        );
    }
}

/// An unconsumed `sink dyn` object frees the boxes it owns, alone or in an
/// aggregate, exactly once.
#[test]
fn sink_dyn_drops_owned_boxes() {
    let src = "struct N { v: u64 }
struct Pair { a: Box[N], b: Box[N], n: u64 }
trait Size { fn size(in self) -> u64; }
impl Size for Pair { fn size(in self) -> u64 { self.n } }
struct One { b: Box[N] }
impl Size for One { fn size(in self) -> u64 { 1 } }
fn peek(sink x: dyn Size) -> u64 { x.size() }
fn main() -> u64 {
    let p = Pair { a: Box::new(N { v: 1 }), b: Box::new(N { v: 2 }), n: 5 };
    let one = One { b: Box::new(N { v: 3 }) };
    wrapping_add(peek(sink p), peek(sink one))
}
";
    let w = world(src);
    for optimize in [true, false] {
        vesper::fip::native_runtime::reset_counts();
        let value = native::jit_with_options(&w, "main", optimize)
            .unwrap()
            .to_string();
        assert_eq!(value, "6");
        assert_eq!(vesper::fip::native_runtime::alloc_count(), 3);
        assert_eq!(
            vesper::fip::native_runtime::free_count(),
            3,
            "optimize {optimize}: boxes freed wrongly"
        );
    }
    assert_eq!(execute(src), "6");
}

/// A bounded generic instance records its `dyn` parameters at its type
/// arguments, so a trait argument naming a type parameter finds its vtable.
#[test]
fn dyn_trait_arguments_follow_instantiation() {
    let src = "struct S { k: i64 }
trait Tag { fn tag(in self) -> i64; }
impl Tag for i64 { fn tag(in self) -> i64 { self } }
trait Read[T] { fn get(in self, in v: T) -> i64; }
impl Read[i64] for S { fn get(in self, in v: i64) -> i64 { wrapping_mul(self.k, v) } }
fn take[T: Tag](in m: T, in x: dyn Read[T]) -> i64 { wrapping_add(m.tag(), x.get(in m)) }
fn main() -> i64 {
    let s = S { k: 10 };
    take(in 4, in s)
}
";
    assert_eq!(execute(src), "44");
}

/// A forwarded object keeps its vtable, so it may only be forwarded as the
/// same trait.
#[test]
fn forwarding_needs_the_same_trait() {
    let src = "trait A { fn a(in self) -> i64; }
trait B { fn b(in self) -> i64; }
impl A for i64 { fn a(in self) -> i64 { 1 } }
impl B for i64 { fn b(in self) -> i64 { 2 } }
fn f(in x: dyn B) -> i64 { x.b() }
fn g(in x: dyn A) -> i64 { f(in x) }
fn main() -> i64 { g(in 3) }
";
    rejects(src, "E0608");
}

/// One program has one vtable per (object, type): a second coercion under
/// another model is refused rather than dispatched to the first.
#[test]
fn coercions_under_other_models_are_refused() {
    let src = "struct K { v: i64 }
trait R { fn r(in self) -> i64; }
impl R for K { fn r(in self) -> i64 { 1 } }
model M2: R for K { fn r(in self) -> i64 { 2 } }
fn f(in x: dyn R) -> i64 { x.r() }
fn main() -> i64 {
    let k = K { v: 0 };
    wrapping_add(f(in k), { use model M2; f(in k) })
}
";
    rejects(src, "E0633");
    // The same model twice shares one vtable.
    assert_eq!(
        execute(&src.replace("{ use model M2; f(in k) }", "f(in k)")),
        "2"
    );
}

/// A slot cannot carry a function value (native code specializes such
/// functions into their callers), in `self` or another parameter.
#[test]
fn function_values_in_slots_are_refused() {
    for src in [
        "trait Apply { fn apply(in self, sink f: fn(i64) -> i64) -> i64; }
impl Apply for i64 { fn apply(in self, sink f: fn(i64) -> i64) -> i64 { f(self) } }
fn run(in a: dyn Apply) -> i64 { a.apply(|x| x) }
fn main() -> i64 { run(in 3) }
",
        "struct F { f: fn(i64) -> i64 }
trait Go { fn go(in self) -> i64; }
impl Go for F { fn go(in self) -> i64 { 0 } }
fn run(in a: dyn Go) -> i64 { a.go() }
fn main() -> i64 { run(in F { f: |x| x }) }
",
    ] {
        rejects(src, "E0630");
    }
}
