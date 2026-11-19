//! `sink` parameters of handler-specialized callees (P3b gaps lane 10,
//! #1095, part 3). The specializing backend inlines a function that performs
//! a handler effect into its handler; a `sink` parameter it leaves
//! unconsumed is dropped when the inlined body returns, as a separately
//! compiled callee drops it. Sequences, strings and boxes are counted.
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

const LOG: &str = "linear effect Log { fn note(v: i64) -> i64; }
struct P { v: i64 }
";

#[test]
fn an_unconsumed_sink_sequence_is_dropped_by_the_specialized_callee() {
    let src = format!(
        "{LOG}fn take(sink s: ChunkSeq[i64], x: i64) -> i64 eff [Log] {{ Log::note(x) }}
fn main() -> i64 {{
    handle Log in {{
        let mut s: ChunkSeq[i64] = ChunkSeq::new[i64]();
        s.push(1);
        s.push(2);
        take(s, 3)
    }} with {{
        return x => x,
        note(v, k) => k(wrapping_add(v, 10)),
    }}
}}
"
    );
    assert_eq!(execute(&src), "13");
}

#[test]
fn sink_parameters_of_every_kind_left_unconsumed() {
    let src = format!(
        "{LOG}struct Bag {{ items: ChunkSeq[i64], n: i64 }}
fn take(sink s: ChunkSeq[i64], sink b: Box[P], sink g: Bag, x: i64) -> i64 eff [Log] {{
    wrapping_add(Log::note(x), g.n)
}}
fn main() -> i64 {{
    let mut t: i64 = 0;
    for i in 0..3 {{
        t = wrapping_add(t, handle Log state n = 0 in {{
            let mut s: ChunkSeq[i64] = ChunkSeq::new[i64]();
            s.push(i);
            let mut items: ChunkSeq[i64] = ChunkSeq::new[i64]();
            items.push(7);
            take(s, Box::new(P {{ v: i }}), Bag {{ items: items, n: 100 }}, i)
        }} with {{
            return (x, n) => wrapping_add(x, n),
            note(v, n, k) => k(wrapping_mul(v, 2), wrapping_add(n, 1)),
        }});
    }}
    t
}}
"
    );
    // (0 + 100 + 1) + (2 + 100 + 1) + (4 + 100 + 1).
    assert_eq!(execute(&src), "309");
}

#[test]
fn a_statically_called_callee_drops_its_unconsumed_sink_sequence() {
    let src = format!(
        "{LOG}fn take(sink s: ChunkSeq[i64], x: i64) -> i64 {{ wrapping_add(x, 1) }}
fn main() -> i64 {{
    let mut s: ChunkSeq[i64] = ChunkSeq::new[i64]();
    s.push(1);
    take(s, 3)
}}
"
    );
    assert_eq!(execute(&src), "4");
}
