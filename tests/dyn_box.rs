//! Boxed trait objects (`Box[dyn Trait]`; manuscript §7.3, #1057): objects
//! stored in locals, fields, enum payloads and sequences, returned from
//! functions, called through, lent to `dyn` parameters and dropped through
//! their vtables, in the Cranelift JIT (optimizer on and off), the evaluator
//! and AArch64 and x86-64 objects, with a typed diagnostic for each rejected
//! use.
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

const SHAPES: &str = "struct Circle { r: i64 }
struct Rect { w: i64, h: i64 }
trait Shape {
    fn area(in self) -> i64;
    fn grow(inout self, by: i64);
}
impl Shape for Circle {
    fn area(in self) -> i64 { wrapping_mul(wrapping_mul(self.r, self.r), 3) }
    fn grow(inout self, by: i64) { self.r = wrapping_add(self.r, by); }
}
impl Shape for Rect {
    fn area(in self) -> i64 { wrapping_mul(self.w, self.h) }
    fn grow(inout self, by: i64) { self.w = wrapping_add(self.w, by); }
}
impl Shape for i64 {
    fn area(in self) -> i64 { self }
    fn grow(inout self, by: i64) { self = wrapping_add(self, by); }
}
fn describe(in s: dyn Shape) -> i64 { wrapping_mul(s.area(), 10) }
fn pump(inout s: dyn Shape) { s.grow(1); }
";

#[test]
fn boxed_locals_dispatch_mutate_and_lend() {
    let src = format!(
        "{SHAPES}fn main() -> i64 {{
    let c: Box[dyn Shape] = Box::new(Circle {{ r: 2 }});
    let mut r: Box[dyn Shape] = Box::new(Rect {{ w: 3, h: 5 }});
    r.grow(2);
    pump(inout r);
    let n = Box::new[dyn Shape](7);
    wrapping_add(wrapping_add(wrapping_mul(c.area(), 10000), wrapping_mul(r.area(), 100)), wrapping_add(describe(in n), describe(in c)))
}}
"
    );
    // Circle 12, Rect (3+2+1)*5 = 30, i64 7*10, Circle 12*10.
    assert_eq!(execute(&src), (120000 + 3000 + 70 + 120).to_string());
}

const SOURCES: &str = "struct Counter { next: u64 }
struct Fixed { v: u64, calls: u64 }
trait CidSource {
    fn next_cid(inout self) -> u64;
}
impl CidSource for Counter {
    fn next_cid(inout self) -> u64 {
        self.next = wrapping_add(self.next, 1);
        self.next
    }
}
impl CidSource for Fixed {
    fn next_cid(inout self) -> u64 {
        self.calls = wrapping_add(self.calls, 1);
        wrapping_add(self.v, self.calls)
    }
}
struct Server { cids: Box[dyn CidSource], spare: Option[Box[dyn CidSource]], issued: u64 }
impl Server {
    fn issue(inout self) -> u64 {
        self.issued = wrapping_add(self.issued, 1);
        self.cids.next_cid()
    }
}
fn source(fixed: bool) -> Box[dyn CidSource] {
    if fixed { Box::new(Fixed { v: 100, calls: 0 }) } else { Box::new(Counter { next: 0 }) }
}
";

#[test]
fn boxed_fields_options_and_results() {
    let src = format!(
        "{SOURCES}fn main() -> u64 {{
    let mut s = Server {{ cids: source(false), spare: Some(source(true)), issued: 0 }};
    let a = s.issue();
    let b = s.issue();
    s.cids = Box::new(Fixed {{ v: 50, calls: 0 }});
    let c = s.issue();
    let d = match s.spare {{ Some(f) => {{ let mut g = f; g.next_cid() }}, None => 0 }};
    wrapping_add(wrapping_add(wrapping_mul(a, 1000000), wrapping_mul(b, 10000)), wrapping_add(wrapping_mul(c, 100), wrapping_add(d, s.issued)))
}}
"
    );
    // a 1, b 2, c 51 after replacing the source (the old one is dropped),
    // d 101 from the spare, three issued.
    assert_eq!(
        execute(&src),
        (1_000_000 + 20_000 + 5_100 + 101 + 3).to_string()
    );
}

const PROCESSORS: &str = "struct Gain { k: i64, out: ChunkSeq[i64] }
struct Offset { by: i64, label: String }
trait Processor {
    fn process(inout self, x: i64) -> i64;
}
impl Processor for Gain {
    fn process(inout self, x: i64) -> i64 {
        let y = wrapping_mul(x, self.k);
        self.out.push(y);
        y
    }
}
impl Processor for Offset {
    fn process(inout self, x: i64) -> i64 { wrapping_add(x, self.by) }
}
fn label() -> String {
    let mut s = String::new();
    s.push_str(in \"offset\");
    s
}
fn gain(k: i64) -> Box[dyn Processor] {
    Box::new(Gain { k: k, out: ChunkSeq::new[i64]() })
}
";

#[test]
fn sequences_of_boxed_objects() {
    let src = format!(
        "{PROCESSORS}fn main() -> i64 {{
    handle Fail[RuntimeError] in {{ run() }} with {{ return x => x, raise(e, k) => -1, }}
}}
fn run() -> i64 eff [Fail[RuntimeError]] {{
    let mut chain: ChunkSeq[Box[dyn Processor]] = ChunkSeq::new[Box[dyn Processor]]();
    chain.push(gain(2));
    chain.push(Box::new[dyn Processor](Offset {{ by: 3, label: label() }}));
    chain.push(gain(10));
    let mut x: i64 = 1;
    for round in 0..3 {{
        for i in 0..chain.len() {{
            x = chain[i].process(x);
        }}
    }}
    x
}}
"
    );
    // One round: x -> (2x + 3) * 10; three rounds from 1.
    let round = |x: i64| (2 * x + 3) * 10;
    assert_eq!(execute(&src), round(round(round(1))).to_string());
}

const NOTES: &str = "struct N { v: u64 }
struct Note { words: ChunkSeq[u64], inner: Box[N], tag: u64 }
struct Plain { v: u64 }
trait Consume {
    fn size(in self) -> u64;
    fn finish(sink self) -> u64;
}
impl Consume for Note {
    fn size(in self) -> u64 { wrapping_add(self.words.len(), self.tag) }
    fn finish(sink self) -> u64 {
        let n = self.words.len();
        wrapping_add(n, 1000)
    }
}
impl Consume for Plain {
    fn size(in self) -> u64 { self.v }
    fn finish(sink self) -> u64 { wrapping_add(self.v, 2000) }
}
fn words(n: u64) -> ChunkSeq[u64] {
    let mut words: ChunkSeq[u64] = ChunkSeq::new[u64]();
    for i in 0..n {
        words.push(i);
    }
    words
}
// A function with a loop cannot own a box yet.
fn note(n: u64, tag: u64) -> Box[dyn Consume] {
    Box::new(Note { words: words(n), inner: Box::new(N { v: n }), tag: tag })
}
fn take(sink x: dyn Consume) -> u64 { x.finish() }
fn peek(sink x: dyn Consume) -> u64 { x.size() }
fn look(in x: dyn Consume) -> u64 { x.size() }
";

/// Each boxed object is freed once: dropped at the end of its scope,
/// replaced by assignment, consumed by a `sink self` method through the
/// box, lent to a `sink dyn` parameter that consumes or drops it, and held
/// by a sequence that is dropped.
#[test]
fn boxed_objects_drop_exactly_once() {
    let src = format!(
        "{NOTES}fn main() -> u64 {{
    let a = note(3, 10);
    let mut b = note(2, 20);
    let first = look(in b);
    b = Box::new[dyn Consume](Plain {{ v: 7 }});
    let c = note(4, 30);
    let consumed = c.finish();
    let d = take(sink note(5, 0));
    let e = peek(sink note(6, 40));
    let mut all: ChunkSeq[Box[dyn Consume]] = ChunkSeq::new[Box[dyn Consume]]();
    all.push(note(1, 50));
    all.push(Box::new[dyn Consume](Plain {{ v: 9 }}));
    let kept = all.len();
    wrapping_add(wrapping_add(wrapping_add(a.size(), first), wrapping_add(b.size(), consumed)), wrapping_add(wrapping_add(d, e), kept))
}}
"
    );
    // a 13, first 22, b 7, consumed 1004, d 1005, e 46, kept 2.
    assert_eq!(
        execute(&src),
        (13 + 22 + 7 + 1004 + 1005 + 46 + 2).to_string()
    );
}

#[test]
fn bounded_generics_box_their_type_arguments() {
    let src = format!(
        "{SHAPES}fn boxed[T: mod affine + Shape](sink x: T) -> Box[dyn Shape] {{ Box::new(x) }}
enum Slot {{ Empty, Full(Box[dyn Shape]) }}
fn area(sink s: Slot) -> i64 {{
    match s {{ Slot::Empty => 0, Slot::Full(b) => b.area() }}
}}
fn main() -> i64 {{
    let full = Slot::Full(boxed(Rect {{ w: 2, h: 4 }}));
    let empty = Slot::Empty;
    let n = boxed(5);
    wrapping_add(wrapping_add(area(sink full), area(sink empty)), wrapping_mul(n.area(), 100))
}}
"
    );
    assert_eq!(execute(&src), (8 + 500).to_string());
}

const TRAITS: &str = "struct P { x: i64 }
trait Shape { fn area(in self) -> i64; }
trait Other { fn other(in self) -> i64; }
impl Shape for P { fn area(in self) -> i64 { self.x } }
impl Other for P { fn other(in self) -> i64 { self.x } }
";

#[test]
fn rejected_boxed_objects() {
    for (code, what, body) in [
        ("E0632", "bare field", "struct H { s: dyn Shape }"),
        (
            "E0632",
            "bare type argument",
            "struct H { s: Option[dyn Shape] }",
        ),
        (
            "E0630",
            "not object safe",
            "trait Dup { fn dup(in self) -> Self; }\nstruct H { s: Box[dyn Dup] }",
        ),
        (
            "E0608",
            "method of another trait",
            "fn f(in b: Box[dyn Shape]) -> i64 { b.other() }",
        ),
        (
            "E0608",
            "lent as another trait",
            "fn g(in o: dyn Other) -> i64 { o.other() }\nfn f(in b: Box[dyn Shape]) -> i64 { g(in b) }",
        ),
        (
            "E0608",
            "no conformance",
            "fn f() -> Box[dyn Shape] { Box::new(true) }",
        ),
        (
            "E0205",
            "linear value",
            "linear struct Ticket { id: i64 }\nimpl Shape for Ticket { fn area(in self) -> i64 { self.id } }\nfn f(sink t: Ticket) -> Box[dyn Shape] { Box::new(t) }",
        ),
        (
            "E0631",
            "boxing a dyn parameter",
            "fn f(sink s: dyn Shape) -> Box[dyn Shape] { Box::new(s) }",
        ),
        (
            "E0633",
            "boxing before instantiation",
            "fn f[T: mod affine](sink x: T) -> Box[dyn Shape] { Box::new(x) }\nfn h() -> Box[dyn Shape] { f(P { x: 1 }) }",
        ),
        (
            "E0631",
            "unpacking the box",
            "fn f(sink b: Box[dyn Shape]) -> i64 { match b { Box(s) => 0 } }",
        ),
    ] {
        let d = rejects(&format!("{TRAITS}{body}\nfn main() -> i64 {{ 0 }}\n"), code);
        assert!(!d.is_empty(), "{what}");
    }
}

#[test]
fn other_backends_refuse_boxed_objects() {
    let w = world(&format!(
        "{TRAITS}fn main() -> i64 {{ let b: Box[dyn Shape] = Box::new(P {{ x: 1 }}); b.area() }}\n"
    ));
    let e = vesper::dyn_trait::refuse(&w, "verified backend").unwrap_err();
    assert_eq!(e.code, "E0634");
    assert!(e.message.contains("Box[dyn Shape]"), "{}", e.message);
}

/// Path calls and trait method signatures take boxed objects too.
#[test]
fn path_calls_and_trait_signatures() {
    let src = format!(
        "{SHAPES}trait Factory {{ fn make(in self, k: i64) -> Box[dyn Shape]; }}
struct Squares {{ base: i64 }}
impl Factory for Squares {{
    fn make(in self, k: i64) -> Box[dyn Shape] {{ Box::new(Rect {{ w: wrapping_add(self.base, k), h: wrapping_add(self.base, k) }}) }}
}}
fn main() -> i64 {{
    let f = Squares {{ base: 2 }};
    let b = f.make(1);
    let mut c = f.make(2);
    Shape::grow(c, 1);
    wrapping_add(Shape::area(b), wrapping_mul(c.area(), 100))
}}
"
    );
    // 3*3 and (4+1)*4.
    assert_eq!(execute(&src), (9 + 2000).to_string());
}

/// Micro-benchmark (run with `--ignored --nocapture`): a call through a
/// boxed object's vtable against a `dyn` parameter's and a monomorphized
/// call of the same method, in the optimized JIT.
#[test]
#[ignore = "benchmark"]
fn bench_boxed_against_dyn_and_monomorphized_calls() {
    const N: u64 = 50_000_000;
    let source = |call: &str| {
        format!(
            "struct Acc {{ n: u64 }}
trait Step {{ fn step(in self, x: u64) -> u64; }}
impl Step for Acc {{ fn step(in self, x: u64) -> u64 {{ wrapping_add(wrapping_mul(x, 3), self.n) }} }}
fn by_dyn(in s: dyn Step, x: u64) -> u64 {{ s.step(x) }}
fn by_mono[T: Step](in s: T, x: u64) -> u64 {{ s.step(x) }}
fn by_box(in s: Box[dyn Step], x: u64) -> u64 {{ s.step(x) }}
// A function with a loop may borrow a boxed object, not own one.
fn run(in a: Acc, in b: Box[dyn Step]) -> u64 {{
    let mut x: u64 = 0;
    for i in 0..{N} {{ x = {call}; }}
    x
}}
fn main() -> u64 {{
    let a = Acc {{ n: 1 }};
    let b: Box[dyn Step] = Box::new(Acc {{ n: 1 }});
    run(in a, in b)
}}
"
        )
    };
    let mut timings = vec![];
    for (name, call) in [
        ("boxed", "by_box(in b, x)"),
        ("dyn", "by_dyn(in a, x)"),
        ("monomorphized", "by_mono(in a, x)"),
    ] {
        let w = world(&source(call));
        let start = std::time::Instant::now();
        let value = native::jit_with_options(&w, "main", true).unwrap();
        let elapsed = start.elapsed();
        timings.push((name, elapsed, value.to_string()));
    }
    assert!(timings.iter().all(|t| t.2 == timings[0].2));
    for (name, elapsed, _) in &timings {
        println!(
            "{name}: {:.3} ns per call ({elapsed:?} for {N} calls, compile included)",
            elapsed.as_nanos() as f64 / N as f64
        );
    }
}

/// Loops may call through boxed objects that a sequence or an `inout`
/// parameter holds (the shape of a processor chain or a connection's ID
/// source).
#[test]
fn loops_call_through_held_boxed_objects() {
    let src = format!(
        "{PROCESSORS}{SOURCES}struct Chain {{ procs: ChunkSeq[Box[dyn Processor]] }}
impl Chain {{
    fn run(inout self, x: i64, rounds: u64) -> i64 eff [Fail[RuntimeError]] {{
        let mut y = x;
        for r in 0..rounds {{
            for i in 0..self.procs.len() {{
                y = self.procs[i].process(y);
            }}
        }}
        y
    }}
}}
fn issue_many(inout s: Server, n: u64) -> u64 {{
    let mut last: u64 = 0;
    for i in 0..n {{ last = s.issue(); }}
    last
}}
fn main() -> i64 {{
    let mut procs: ChunkSeq[Box[dyn Processor]] = ChunkSeq::new[Box[dyn Processor]]();
    procs.push(gain(2));
    procs.push(Box::new[dyn Processor](Offset {{ by: 3, label: label() }}));
    let mut chain = Chain {{ procs: procs }};
    let y = handle Fail[RuntimeError] in {{ chain.run(1, 2) }} with {{ return x => x, raise(e, k) => -1, }};
    let mut s = Server {{ cids: source(false), spare: None, issued: 0 }};
    let last = issue_many(inout s, 5);
    wrapping_add(wrapping_mul(y, 100), if last == 5 {{ 5 }} else {{ 0 }})
}}
"
    );
    // (2*1+3) = 5, (2*5+3) = 13; the counter's fifth ID is 5.
    assert_eq!(execute(&src), (1300 + 5).to_string());
}

/// A call through `Box[dyn Factory[i64]]` is typed at the box's trait
/// arguments, which the stored vtable was made for.
#[test]
fn boxed_calls_use_the_boxed_trait_arguments() {
    let base = "struct Seven { k: i64 }
trait Factory[T] { fn make(in self) -> T; }
impl Factory[i64] for Seven { fn make(in self) -> i64 { wrapping_add(self.k, 7) } }
";
    let ok = format!(
        "{base}fn main() -> i64 {{
    let b: Box[dyn Factory[i64]] = Box::new(Seven {{ k: 1 }});
    b.make()
}}
"
    );
    assert_eq!(execute(&ok), "8");
    rejects(
        &format!(
            "{base}fn main() -> i64 {{
    let b: Box[dyn Factory[i64]] = Box::new(Seven {{ k: 1 }});
    let p: (i64, i64) = b.make();
    0
}}
"
        ),
        "E0200",
    );
}

/// A loop function may not own a box through an aggregate's fields: the
/// drop elaboration that would reclaim it needs an acyclic graph.
#[test]
fn loops_refuse_aggregates_owning_boxes() {
    let src = format!(
        "{TRAITS}struct Holder {{ object: Box[dyn Shape] }}
fn make() -> Box[dyn Shape] {{ Box::new(P {{ x: 3 }}) }}
fn run(n: u64) -> i64 {{
    let holder = Holder {{ object: make() }};
    let mut t: i64 = 0;
    for i in 0..n {{ t = wrapping_add(t, holder.object.area()); }}
    t
}}
fn main() -> i64 {{ run(4) }}
"
    );
    let w = world(&src);
    let e = native::jit_with_options(&w, "main", true).unwrap_err();
    assert!(e.contains("not reclaimed natively"), "{e}");
}

/// A `sink self` call that fails frees the consumed box's block before
/// the failure leaves the caller.
#[test]
fn failing_sink_calls_free_the_box() {
    let src = "struct P { x: i64 }
trait Eat { fn eat(sink self) -> i64 eff [Fail[RuntimeError]]; }
impl Eat for P { fn eat(sink self) -> i64 eff [Fail[RuntimeError]] { wrapping_add(84 / wrapping_sub(self.x, 1), 1) } }
fn go(sink b: Box[dyn Eat]) -> i64 eff [Fail[RuntimeError]] { b.eat() }
fn main() -> i64 {
    let b: Box[dyn Eat] = Box::new(P { x: 1 });
    handle Fail[RuntimeError] in { go(b) } with { return x => x, raise(e, k) => -1, }
}
";
    assert_eq!(execute(src), "-1");
}

/// A sequence element is not a place, so it may be lent `inout` only to
/// reach its object, never to a callee that could replace the box.
#[test]
fn elements_are_not_lent_to_replace_their_box() {
    let src = format!("{TRAITS}fn reset(inout b: Box[dyn Shape]) {{ b = Box::new[dyn Shape](P {{ x: 9 }}); }}
fn run() -> i64 eff [Fail[RuntimeError]] {{
    let mut s: ChunkSeq[Box[dyn Shape]] = ChunkSeq::new[Box[dyn Shape]]();
    s.push(Box::new[dyn Shape](P {{ x: 1 }}));
    reset(inout s[0]);
    s[0].area()
}}
fn main() -> i64 {{ handle Fail[RuntimeError] in {{ run() }} with {{ return x => x, raise(e, k) => -1, }} }}
");
    rejects(&src, "E0300");
}
