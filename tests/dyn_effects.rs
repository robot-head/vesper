//! Trait objects whose methods perform handler effects (manuscript §7.3,
//! port wave P3b lane 1, #1063): calls through `in dyn`, `inout dyn`,
//! `sink dyn` and `Box[dyn Trait]` reach the caller's handlers, including a
//! handler installed after the object was boxed, in the Cranelift JIT
//! (optimizer on and off), the evaluator and AArch64 and x86-64 objects.
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

const NET: &str = "linear effect Net { fn emit(v: i64) -> i64; }
struct Echo { k: i64 }
struct Count { n: i64 }
trait Transport {
    fn send(inout self, v: i64) -> i64 eff [Net];
    fn peek(in self) -> i64;
}
impl Transport for Echo {
    fn send(inout self, v: i64) -> i64 eff [Net] { Net::emit(wrapping_add(v, self.k)) }
    fn peek(in self) -> i64 { self.k }
}
impl Transport for Count {
    fn send(inout self, v: i64) -> i64 eff [Net] {
        self.n = wrapping_add(self.n, 1);
        wrapping_add(Net::emit(v), wrapping_mul(self.n, 1000))
    }
    fn peek(in self) -> i64 { self.n }
}
fn send_twice(inout t: dyn Transport, v: i64) -> i64 eff [Net] {
    wrapping_add(t.send(v), t.send(v))
}
fn look(in t: dyn Transport) -> i64 { t.peek() }
";

#[test]
fn effectful_slots_through_dyn_parameters() {
    let src = format!(
        "{NET}fn main() -> i64 {{
    let mut e = Echo {{ k: 1 }};
    let mut c = Count {{ n: 0 }};
    handle Net state sent = 0 in {{
        let a = send_twice(inout e, 10);
        let b = send_twice(inout c, 20);
        wrapping_add(wrapping_mul(a, 1000000), wrapping_add(b, look(in c)))
    }} with {{
        return (x, sent) => wrapping_add(wrapping_mul(x, 10), sent),
        emit(v, sent, k) => k(wrapping_mul(v, 2), wrapping_add(sent, 1)),
    }}
}}
"
    );
    // Echo: 2 * (2 * 11) = 44; Count: (40 + 1000) + (40 + 2000) = 3080,
    // peek 2; four emits.
    let a = 44i64;
    let b = 3080 + 2;
    assert_eq!(execute(&src), ((a * 1000000 + b) * 10 + 4).to_string());
}

#[test]
fn handler_installed_after_boxing() {
    let src = format!(
        "{NET}struct Session {{ link: Box[dyn Transport], sent: i64 }}
impl Session {{
    fn push(inout self, v: i64) -> i64 eff [Net] {{
        self.sent = wrapping_add(self.sent, 1);
        self.link.send(v)
    }}
}}
fn open(echo: bool) -> Session {{
    if echo {{
        Session {{ link: Box::new(Echo {{ k: 5 }}), sent: 0 }}
    }} else {{
        Session {{ link: Box::new(Count {{ n: 0 }}), sent: 0 }}
    }}
}}
fn main() -> i64 {{
    let mut s = open(true);
    let mut t = open(false);
    let first = handle Net in {{ s.push(1) }} with {{
        return x => x,
        emit(v, k) => k(wrapping_mul(v, 3)),
    }};
    let second = handle Net in {{ wrapping_add(t.push(2), t.push(2)) }} with {{
        return x => x,
        emit(v, k) => k(wrapping_add(v, 100)),
    }};
    wrapping_add(wrapping_mul(first, 100000), wrapping_add(second, wrapping_add(s.sent, t.sent)))
}}
"
    );
    // first: (1 + 5) * 3 = 18; second: (102 + 1000) + (102 + 2000) = 3204.
    assert_eq!(execute(&src), (18 * 100000 + 3204 + 1 + 2).to_string());
}

#[test]
fn sequences_of_effectful_boxes() {
    let src = format!(
        "{NET}fn link(i: i64) -> Box[dyn Transport] {{
    if i == 1 {{ Box::new(Count {{ n: 0 }}) }} else {{ Box::new(Echo {{ k: i }}) }}
}}
fn main() -> i64 {{
    handle Fail[RuntimeError] in {{
        handle Net state seen = 0 in {{ run() }} with {{
            return (x, seen) => wrapping_add(wrapping_mul(x, 100), seen),
            emit(v, seen, k) => k(v, wrapping_add(seen, 1)),
        }}
    }} with {{ return x => x, raise(e, k) => -1, }}
}}
fn run() -> i64 eff [Net, Fail[RuntimeError]] {{
    let mut links: ChunkSeq[Box[dyn Transport]] = ChunkSeq::new[Box[dyn Transport]]();
    links.push(link(0));
    links.push(link(1));
    links.push(link(2));
    // Resuming handlers inside loops hang natively on main (a separate
    // gap), so the rounds are written out.
    let a = wrapping_add(links[0].send(1), wrapping_add(links[1].send(1), links[2].send(1)));
    let b = wrapping_add(links[0].send(1), wrapping_add(links[1].send(1), links[2].send(1)));
    wrapping_add(a, b)
}}
"
    );
    // Per round: Echo 0 -> 1, Count -> 1 + 1000 * n, Echo 2 -> 3.
    let total = (1 + 1001 + 3) + (1 + 2001 + 3);
    assert_eq!(execute(&src), (total * 100 + 6).to_string());
}

const CONSUME: &str = "linear effect Log { fn note(v: i64) -> i64; }
// The specializing backend leaks a sequence owned by a `sink` parameter of
// a handler-specialized function, called statically too (a separate gap),
// so the consumed value owns none.
struct Ticket { id: i64, words: (i64, i64) }
trait Job {
    fn finish(sink self) -> i64 eff [Log, Fail[RuntimeError]];
}
impl Job for Ticket {
    fn finish(sink self) -> i64 eff [Log, Fail[RuntimeError]] {
        let (n, m) = self.words;
        Log::note(wrapping_add(self.id, wrapping_add(n, m)))
    }
}
impl Job for i64 {
    fn finish(sink self) -> i64 eff [Log, Fail[RuntimeError]] { Log::note(self) }
}
fn words(n: i64) -> (i64, i64) { (n, n) }
fn run_job(sink j: dyn Job) -> i64 eff [Log, Fail[RuntimeError]] { j.finish() }
";

#[test]
fn effectful_sink_slots_consume_once() {
    let src = format!(
        "{CONSUME}fn main() -> i64 {{
    let boxed: Box[dyn Job] = Box::new(Ticket {{ id: 10, words: words(1) }});
    let plain: Box[dyn Job] = Box::new[dyn Job](7);
    handle Fail[RuntimeError] in {{ handle Log state calls = 0 in {{
        let a = boxed.finish();
        let b = run_job(sink Ticket {{ id: 20, words: words(2) }});
        let c = plain.finish();
        wrapping_add(wrapping_mul(a, 10000), wrapping_add(wrapping_mul(b, 100), c))
    }} with {{
        return (x, calls) => wrapping_add(wrapping_mul(x, 10), calls),
        note(v, calls, k) => k(wrapping_add(v, 1), wrapping_add(calls, 1)),
    }} }} with {{ return x => x, raise(e, k) => -1, }}
}}
"
    );
    // a = 10 + 2 * 1 + 1, b = 20 + 2 * 2 + 1, c = 7 + 1; three notes.
    assert_eq!(
        execute(&src),
        ((13 * 10000 + 25 * 100 + 8) * 10 + 3).to_string()
    );
}

#[test]
fn slot_effects_reach_the_callers_row() {
    // The trait method's handler effect flows to the caller as for a static
    // call: a caller without `Net` in its row is refused.
    rejects(
        &format!("{NET}fn quiet(inout t: dyn Transport) -> i64 {{ t.send(1) }}\n"),
        "E0401",
    );
    rejects(
        &format!("{NET}fn quiet(inout t: Box[dyn Transport]) -> i64 {{ t.send(1) }}\n"),
        "E0401",
    );
    // An open row stays outside object safety.
    rejects(
        "trait Open { fn go[r: eff](in self) -> i64 eff [r]; }
struct S { }
impl Open for S { fn go[r: eff](in self) -> i64 eff [r] { 1 } }
fn call(in o: dyn Open) -> i64 { o.go() }
",
        "E0630",
    );
}

#[test]
fn foreign_effects_through_slots() {
    // `Ffi[Lib]` operations are direct native calls, so their slot is an
    // ordinary compiled function.
    let vsys = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/ffi_lowering/bridge.vsys"),
    )
    .unwrap()
    .replace("@DIGEST@", &"0".repeat(64));
    let mut imports = vesper::ffi_lowering::Imports::new();
    imports
        .add(&vsys, vesper::ffi_lowering::Handler::Direct)
        .unwrap();
    let checked = vesper::ffi_lowering::check_source(
        "struct Adder { k: u32 }
trait Sum { fn sum(in self, x: u32) -> u32 eff [Ffi[Bridge]]; }
impl Sum for Adder { fn sum(in self, x: u32) -> u32 eff [Ffi[Bridge]] { Bridge::bridge_sum(x, self.k) } }
fn through(in s: dyn Sum) -> u32 eff [Ffi[Bridge]] { s.sum(2) }
fn entry() -> u32 eff [Ffi[Bridge]] {
    let b: Box[dyn Sum] = Box::new(Adder { k: 5 });
    wrapping_add(through(in Adder { k: 1 }), b.sum(3))
}",
        &imports,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let object = checked
        .object("entry", Some("x86_64-unknown-linux-gnu"))
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(object.link.len(), 1);
    assert_eq!(object.link[0].library, "Bridge");
}

/// `cargo test --test dyn_effects -- --ignored --nocapture bench`: a call
/// through an effectful slot (`in dyn`, `in Box[dyn]`) against the same
/// method called through a bound (monomorphized) and directly, each under a
/// handler installed per iteration (a resuming handler around a loop is a
/// separate native gap), in the optimized JIT.
#[test]
#[ignore = "benchmark"]
fn bench_effectful_slots_against_specialized_calls() {
    const N: u64 = 20_000_000;
    let source = |call: &str| {
        format!(
            "linear effect Tick {{ fn tick(x: u64) -> u64; }}
struct Acc {{ n: u64 }}
trait Step {{ fn step(in self, x: u64) -> u64 eff [Tick]; }}
impl Step for Acc {{ fn step(in self, x: u64) -> u64 eff [Tick] {{ Tick::tick(wrapping_add(wrapping_mul(x, 3), self.n)) }} }}
fn by_dyn(in s: dyn Step, x: u64) -> u64 eff [Tick] {{ s.step(x) }}
fn by_box(in s: Box[dyn Step], x: u64) -> u64 eff [Tick] {{ s.step(x) }}
fn by_mono[T: Step](in s: T, x: u64) -> u64 eff [Tick] {{ s.step(x) }}
fn direct(in s: Acc, x: u64) -> u64 eff [Tick] {{ Tick::tick(wrapping_add(wrapping_mul(x, 3), s.n)) }}
fn run(in a: Acc, in b: Box[dyn Step]) -> u64 {{
    let mut x: u64 = 0;
    for i in 0..{N} {{
        x = handle Tick in {{ {call} }} with {{ return y => y, tick(v, k) => k(v), }};
    }}
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
        ("effectful slot, in dyn", "by_dyn(in a, x)"),
        ("effectful slot, in Box[dyn]", "by_box(in b, x)"),
        ("monomorphized", "by_mono(in a, x)"),
        ("direct", "direct(in a, x)"),
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
            "{name}: {:.3} ns per iteration ({elapsed:?} for {N}, compile included)",
            elapsed.as_nanos() as f64 / N as f64
        );
    }
}
