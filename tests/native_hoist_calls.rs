//! Sequence headers hoisted out of loops that call (port speed lane 3,
//! #1023).
//!
//! A loop that calls keeps its hoisted header words live across the call,
//! so they take callee-saved registers or spill. The lane measured every
//! rule for hoisting more than one receiver in such a loop (by read count,
//! by reads against calls, only in small loops) and each one ran more
//! instructions in some port, so a calling loop hoists exactly one receiver
//! or none, and a loop that calls nothing hoists them all. These plan tests
//! pin that rule; each program also runs the evaluator, both JIT
//! configurations, x86-64 and AArch64 objects, which must agree.
use vesper::{check, ir, ir_opt, native, parser, runtime::Runtime};

fn accepted(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:#?}", c.diagnostics);
    c
}

/// The receiver places hoisted in `function` of `source`, after the native
/// optimize pipeline's passes.
fn hoisted_places(source: &str, function: &str) -> Vec<String> {
    let c = accepted(source);
    let mut program = ir::lower_world(&c.world);
    ir_opt::optimize(&c.world, &mut program).unwrap();
    vesper::fip::elaborate_non_fip_drops(&c.world, &mut program);
    vesper::bounds_hoist::run(&mut program);
    let mut places: Vec<String> = vesper::bounds_hoist::header_hoists(&program.functions[function])
        .into_iter()
        .map(|h| h.place.display())
        .collect();
    places.sort();
    places
}

fn agree(name: &str, source: &str) -> String {
    let c = accepted(source);
    let evaluated = Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap_or_else(|e| panic!("{name}: evaluator failed: {e}"))
        .to_string();
    for optimize in [true, false] {
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap_or_else(|e| panic!("{name}: JIT (optimize {optimize}) failed: {e}"))
            .to_string();
        assert_eq!(jit, evaluated, "{name}: JIT (optimize {optimize})");
    }
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let (obj, _) = native::object_with_options(&c.world, "main", Some(target), true)
            .unwrap_or_else(|e| panic!("{name}: {target}: {e}"));
        assert_eq!(&obj[..4], b"\x7fELF");
    }
    evaluated
}

/// `note` is recursive, so the loops below keep a real call.
const PRELUDE: &str = r#"
fn note(x: u64, depth: u64) -> u64 {
    if depth == 0 {
        return x;
    }
    wrapping_add(note(x, wrapping_sub(depth, 1)), 1)
}

fn fill(n: u64, k: u64) -> ChunkSeq[u64] {
    let mut s = ChunkSeq::new[u64]();
    for i in 0..n {
        s.push(wrapping_mul(i, k));
    }
    s
}
"#;

fn read(name: &str, receiver: &str) -> String {
    format!(
        "let {name} = match {receiver}.try_get(wrapping_add(i, 1)) {{
            Option::Some(v) => v,
            Option::None => 0,
        }};"
    )
}

#[test]
fn a_calling_loop_hoists_its_only_receiver() {
    let source = format!(
        "{PRELUDE}
fn walk(in a: ChunkSeq[u64], n: u64) -> u64 {{
    let mut total: u64 = 0;
    for i in 0..n {{
        {}
        total = wrapping_add(total, note(x, 2));
    }}
    total
}}

fn main() -> u64 {{
    let a = fill(9, 3);
    walk(a, 9)
}}
",
        read("x", "a")
    );
    assert_eq!(hoisted_places(&source, "walk"), vec!["a"]);
    assert_eq!(agree("one", &source), (3 * 36 + 2 * 9).to_string());
}

#[test]
fn a_calling_loop_with_two_receivers_hoists_neither() {
    let source = format!(
        "{PRELUDE}
fn walk(in a: ChunkSeq[u64], in b: ChunkSeq[u64], n: u64) -> u64 {{
    let mut total: u64 = 0;
    for i in 0..n {{
        {}
        {}
        total = wrapping_add(total, note(wrapping_add(x, y), 2));
    }}
    total
}}

fn main() -> u64 {{
    let a = fill(9, 3);
    let b = fill(9, 5);
    walk(a, b, 9)
}}
",
        read("x", "a"),
        read("y", "b")
    );
    // Neither is hoisted before the loop; both are borrowed parameters the
    // function never changes, read once at entry instead, whose header
    // words Cranelift loads as read-only memory (#1088).
    let c = accepted(&source);
    let mut program = ir::lower_world(&c.world);
    ir_opt::optimize(&c.world, &mut program).unwrap();
    vesper::fip::elaborate_non_fip_drops(&c.world, &mut program);
    vesper::bounds_hoist::run(&mut program);
    let hoists = vesper::bounds_hoist::header_hoists(&program.functions["walk"]);
    assert!(hoists.iter().all(|h| h.entry), "no loop hoist");
    assert_eq!(hoisted_places(&source, "walk"), vec!["a", "b"]);
    assert_eq!(agree("two", &source), (8 * 36 + 2 * 9).to_string());
}

#[test]
fn a_loop_that_calls_nothing_hoists_every_receiver() {
    let source = format!(
        "{PRELUDE}
fn walk(in a: ChunkSeq[u64], in b: ChunkSeq[u64], n: u64) -> u64 {{
    let mut total: u64 = 0;
    for i in 0..n {{
        {}
        {}
        total = wrapping_add(total, wrapping_add(x, y));
    }}
    total
}}

fn main() -> u64 {{
    let a = fill(9, 3);
    let b = fill(9, 5);
    walk(a, b, 9)
}}
",
        read("x", "a"),
        read("y", "b")
    );
    assert_eq!(hoisted_places(&source, "walk"), vec!["a", "b"]);
    assert_eq!(agree("leaf", &source), (8 * 36).to_string());
}
