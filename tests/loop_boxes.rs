//! Boxes owned across loops and early exits (manuscript §7.3, #1064): a
//! function with `loop`, `for`, `break` or `return` may own a `Box[T]` or a
//! `Box[dyn Trait]` in a local, a `sink` parameter or an aggregate's
//! fields, and each box is dropped exactly once on every path. Each program
//! runs in the Cranelift JIT with the optimizer on and off (zero leaks and
//! no double drops under the counting allocators), agrees with the
//! evaluator, and emits AArch64 and x86-64 objects.
//!
//! Sources are inline: the self-hosted front end does not parse `dyn` or
//! loops yet.
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

/// Objects that own heap memory of their own (a sequence and a nested
/// box), so a missed or doubled drop shows in the counts.
const OBJECTS: &str = "struct Inner { v: i64 }
struct Note { words: ChunkSeq[i64], inner: Box[Inner], k: i64 }
struct Plain { k: i64 }
trait Step {
    fn step(in self, x: i64) -> i64;
    fn bump(inout self);
    fn finish(sink self) -> i64;
}
impl Step for Note {
    fn step(in self, x: i64) -> i64 { wrapping_add(wrapping_add(x, self.k), if self.words.len() == 1 { 1 } else { 0 }) }
    fn bump(inout self) { self.k = wrapping_add(self.k, 1); }
    fn finish(sink self) -> i64 { wrapping_add(self.k, 1000) }
}
impl Step for Plain {
    fn step(in self, x: i64) -> i64 { wrapping_mul(x, self.k) }
    fn bump(inout self) { self.k = wrapping_add(self.k, 1); }
    fn finish(sink self) -> i64 { wrapping_add(self.k, 2000) }
}
fn note(k: i64) -> Box[dyn Step] {
    let mut words: ChunkSeq[i64] = ChunkSeq::new[i64]();
    words.push(k);
    Box::new(Note { words: words, inner: Box::new(Inner { v: 1 }), k: k })
}
fn plain(k: i64) -> Box[dyn Step] { Box::new[dyn Step](Plain { k: k }) }
fn take(sink b: Box[dyn Step]) -> i64 { b.finish() }
";

/// A loop that owns a boxed object and calls it on every iteration, then
/// drops it after the loop.
#[test]
fn loop_owns_and_calls_a_boxed_object() {
    let src = format!(
        "{OBJECTS}fn run(n: i64) -> i64 {{
    let mut b = note(3);
    let mut t: i64 = 0;
    for i in 0..n {{
        t = b.step(t);
        b.bump();
    }}
    t
}}
fn main() -> i64 {{ run(4) }}
"
    );
    // k = 3, 4, 5, 6 with inner 1: 4 + 5 + 6 + 7.
    assert_eq!(execute(&src), (4 + 5 + 6 + 7).to_string());
}

/// Assigning a new box inside the loop drops the one it replaces.
#[test]
fn reassignment_inside_the_loop_drops_the_old_box() {
    let src = format!(
        "{OBJECTS}fn run(n: i64) -> i64 {{
    let mut b = note(1);
    let mut t: i64 = 1;
    for i in 0..n {{
        t = b.step(t);
        if i % 2 == 0 {{ b = plain(2); }} else {{ b = note(i); }}
    }}
    t
}}
fn main() -> i64 {{ run(5) }}
"
    );
    // note1: 1+1+1=3; plain2: 6; note1: 6+1+1=8; plain2: 16; note3: 16+3+1=20.
    assert_eq!(execute(&src), "20");
}

/// A box bound inside the body is dropped at the end of each iteration and
/// on the `break` that leaves with it live.
#[test]
fn body_bindings_drop_per_iteration_and_on_break() {
    let src = format!(
        "{OBJECTS}fn run() -> i64 {{
    let mut t: i64 = 0;
    let mut i: i64 = 0;
    loop {{
        let b = note(i);
        t = b.step(t);
        if t > 10 {{ break; }}
        i = wrapping_add(i, 1);
    }}
    t
}}
fn main() -> i64 {{ run() }}
"
    );
    // 0+0+1=1, 1+1+1=3, 3+2+1=6, 6+3+1=10, 10+4+1=15.
    assert_eq!(execute(&src), "15");
}

/// `return` from inside a loop drops the boxes still owned, consumes the
/// one it returns through, and a `break` value may carry a box out.
#[test]
fn return_and_break_with_live_boxes() {
    let src = format!(
        "{OBJECTS}fn first_over(limit: i64) -> i64 {{
    let keep = note(7);
    let mut b = plain(1);
    let mut i: i64 = 0;
    loop {{
        if b.step(10) > limit {{ return take(b); }}
        i = wrapping_add(i, 1);
        b = plain(i);
        if i > 100 {{ return keep.step(0); }}
    }}
}}
fn pick(n: i64) -> Box[dyn Step] {{
    let mut i: i64 = 0;
    loop {{
        let b = note(i);
        if i == n {{ break b; }}
        i = wrapping_add(i, 1);
    }}
}}
fn main() -> i64 {{
    let a = first_over(35);
    let b = first_over(5000);
    let c = take(pick(3));
    wrapping_add(wrapping_add(a, b), c)
}}
"
    );
    // first_over(35): plain(4) is the first with 10k > 35, finish 2004;
    // first_over(5000) returns keep.step(0) = 8; pick(3) finishes 1003.
    assert_eq!(execute(&src), (2004 + 8 + 1003).to_string());
}

/// A box moved out on the path that leaves the loop is dropped on the
/// other exits only.
#[test]
fn moves_on_leaving_paths() {
    let src = format!(
        "{OBJECTS}fn run(n: i64, stop: i64) -> i64 {{
    let b = note(5);
    let mut t: i64 = 0;
    for i in 0..n {{
        if i == stop {{
            t = wrapping_add(t, take(b));
            break;
        }}
        t = wrapping_add(t, 1);
    }}
    t
}}
fn main() -> i64 {{ wrapping_add(run(4, 2), wrapping_mul(run(3, 9), 100000)) }}
"
    );
    // run(4, 2): 2 + 1005; run(3, 9): never taken, 3, the box dropped after.
    assert_eq!(execute(&src), (2 + 1005 + 300000).to_string());
}

/// A `sink` box parameter and plain `Box[T]` locals take the same path.
#[test]
fn sink_parameters_and_plain_boxes() {
    let src = format!(
        "{OBJECTS}struct Counter {{ n: i64 }}
fn drive(sink b: Box[dyn Step], rounds: i64) -> i64 {{
    let mut c: Box[Counter] = Box::new(Counter {{ n: 0 }});
    let mut t: i64 = 0;
    for i in 0..rounds {{
        t = b.step(t);
        c = Box::new(Counter {{ n: i }});
    }}
    let Box(last) = c;
    wrapping_add(t, last.n)
}}
fn main() -> i64 {{ wrapping_add(drive(note(2), 3), drive(plain(3), 0)) }}
"
    );
    // note(2) steps 3, 6, 9 and the last counter is 2; plain(3) runs no rounds.
    assert_eq!(execute(&src), "11");
}

/// Structs and enums owning boxes are dropped through their fields.
#[test]
fn aggregates_owning_boxes_in_loops() {
    let src = format!(
        "{OBJECTS}struct Driver {{ transport: Box[dyn Step], spare: Option[Box[dyn Step]], sent: i64 }}
fn run(n: i64) -> i64 {{
    let mut d = Driver {{ transport: note(1), spare: Some(plain(2)), sent: 0 }};
    for i in 0..n {{
        d.sent = d.transport.step(d.sent);
        if i == 1 {{ d.transport = plain(3); }}
    }}
    d.sent
}}
fn main() -> i64 {{ run(4) }}
"
    );
    // note1: 2, 4; plain3: 12, 36.
    assert_eq!(execute(&src), "36");
}

/// A boxed object lent `sink` to a function whose handler effects are
/// specialized at the call (inlined natively): the box's block is freed when
/// the call returns, when the handler resumes it, and when the handler
/// aborts it, and an object the callee leaves unconsumed is dropped
/// through its vtable first. A call that consumes its object does not drop
/// it again; an operation clause of a handler the callee installs itself
/// does not free its caller's object.
#[test]
fn sink_lends_to_handler_specialized_callees() {
    let src = format!(
        "{OBJECTS}effect Ask {{ fn ask() -> i64; }}
effect Stop {{ fn stop() -> Never; }}
fn ask_finish(sink x: dyn Step) -> i64 eff [Ask] {{ wrapping_add(x.finish(), Ask::ask()) }}
fn ask_step(sink x: dyn Step) -> i64 eff [Ask] {{ x.step(Ask::ask()) }}
fn stop_early(sink x: dyn Step, stop: bool) -> i64 eff [Stop] {{
    if stop {{ Stop::stop(); }}
    x.step(1)
}}
fn halt() -> i64 eff [Stop] {{
    Stop::stop();
    1
}}
// Specialized for its function-valued parameter, with no handler outside
// it: the handler it installs aborts, then it keeps using its object.
fn own_handler(sink x: dyn Step, sink f: fn(i64) -> i64) -> i64 {{
    let s = handle Stop in {{ halt() }} with {{ return v => v, stop(k) => 7, }};
    f(x.step(s))
}}
fn main() -> i64 {{
    let a = handle Ask in {{ ask_finish(sink note(1)) }} with {{ return x => x, ask(k) => k(10), }};
    let b = handle Ask in {{ ask_step(sink note(3)) }} with {{ return x => x, ask(k) => k(5), }};
    let c = own_handler(sink note(4), |v| v);
    let mut t: i64 = 0;
    for i in 0..3 {{
        let s = handle Stop in {{ stop_early(sink note(i), i == 1) }} with {{ return x => x, stop(k) => 100, }};
        t = wrapping_add(t, s);
    }}
    wrapping_add(wrapping_add(wrapping_add(a, b), c), t)
}}
"
    );
    // a = 1001 + 10; b = 5+3+1 = 9; c = 7+4+1 = 12; stops: 1+0+1, 100, 1+2+1.
    assert_eq!(execute(&src), (1011 + 9 + 12 + 2 + 100 + 4).to_string());
}

/// An `@fip` function drops nothing, so with a loop it still may not own a
/// box it does not hand back (`E0806`); the native backends refuse any
/// other graph the elaboration cannot plan with [`vesper::loops::BOX_REFUSAL`].
#[test]
fn fip_loops_may_not_drop_owned_boxes() {
    let p = parser::parse(
        "copy struct P { x: i64 }
@fip fn spin(sink b: Box[P], n: u64) -> Box[P] {
    let mut t: u64 = 0;
    for i in 0..n { t = wrapping_add(t, i); }
    b
}
fn main() -> i64 {
    let Box(p) = spin(Box::new(P { x: 1 }), 3);
    p.x
}
",
    );
    assert!(p.diagnostics.is_empty(), "parse: {:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(
        c.diagnostics.iter().any(|d| d.code == "E0806"),
        "{:#?}",
        c.diagnostics
    );
}

/// Micro-benchmark (run with `--ignored --nocapture`): a loop that owns a
/// boxed object against the borrow-only workaround of #1060 (the loop moved
/// into a function that takes the box `in`), and a loop that boxes and
/// drops an object on every iteration, in the optimized JIT.
#[test]
#[ignore = "benchmark"]
fn bench_owned_against_borrowed_loops() {
    const N: u64 = 50_000_000;
    let source = |body: &str| {
        format!(
            "struct Acc {{ n: u64 }}
trait Step {{ fn step(in self, x: u64) -> u64; }}
impl Step for Acc {{ fn step(in self, x: u64) -> u64 {{ wrapping_add(wrapping_mul(x, 3), self.n) }} }}
fn borrowed(in b: Box[dyn Step]) -> u64 {{
    let mut x: u64 = 0;
    for i in 0..{N} {{ x = b.step(x); }}
    x
}}
{body}
"
        )
    };
    let cases = [
        (
            "owned",
            format!(
                "fn main() -> u64 {{
    let b: Box[dyn Step] = Box::new(Acc {{ n: 1 }});
    let mut x: u64 = 0;
    for i in 0..{N} {{ x = b.step(x); }}
    x
}}"
            ),
        ),
        (
            "borrowed",
            "fn main() -> u64 {
    let b: Box[dyn Step] = Box::new(Acc { n: 1 });
    borrowed(in b)
}"
            .to_string(),
        ),
        (
            "boxed per iteration",
            format!(
                "fn main() -> u64 {{
    let mut x: u64 = 0;
    for i in 0..{N} {{
        let b: Box[dyn Step] = Box::new(Acc {{ n: 1 }});
        x = b.step(x);
    }}
    x
}}"
            ),
        ),
    ];
    let mut timings = vec![];
    for (name, body) in &cases {
        let w = world(&source(body));
        let start = std::time::Instant::now();
        let value = native::jit_with_options(&w, "main", true).unwrap();
        timings.push((name, start.elapsed(), value.to_string()));
    }
    assert!(timings.iter().all(|t| t.2 == timings[0].2));
    for (name, elapsed, _) in &timings {
        println!(
            "{name}: {:.3} ns per iteration ({elapsed:?} for {N}, compile included)",
            elapsed.as_nanos() as f64 / N as f64
        );
    }
}
