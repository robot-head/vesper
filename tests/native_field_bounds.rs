//! Bounds hoisting judges a receiver's stability by its place, not its root
//! variable: a counted loop that reads `st.toks[i]` while it pushes to
//! `st.out` is versioned, and one that pushes to `st.toks` itself is not
//! (`src/bounds_hoist.rs` `counted_plan`, `stable_place`). The evaluator and
//! both JIT configurations agree, including the checked fallback when the
//! guard fails.
use vesper::{check, ir, ir_check, ir_opt, native, parser, runtime::Runtime, seq::SeqOp};

fn accepted(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:#?}", c.diagnostics);
    c
}

/// The native optimize pipeline's Place IR of `source`, through hoisting.
fn hoisted(source: &str) -> ir::Program {
    let c = accepted(source);
    let mut program = ir::lower_world(&c.world);
    ir_opt::optimize(&c.world, &mut program).unwrap();
    vesper::ir_inline::inline(
        &c.world,
        &mut program,
        &vesper::ir_inline::Budget::default(),
    );
    vesper::fip::elaborate_non_fip_drops(&c.world, &mut program);
    vesper::bounds_hoist::run(&mut program);
    assert!(
        ir_check::evidence(&program).is_empty(),
        "{:?}",
        ir_check::evidence(&program)
    );
    program
}

fn guards(program: &ir::Program, function: &str) -> usize {
    program.functions[function]
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .filter(|i| {
            matches!(
                &i.op,
                ir::Op::Seq {
                    op: SeqOp::BoundsGuard { .. },
                    ..
                }
            )
        })
        .count()
}

fn differential(source: &str) -> String {
    let c = accepted(source);
    let evaluated = Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap()
        .to_string();
    for optimize in [true, false] {
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap()
            .to_string();
        assert_eq!(jit, evaluated, "JIT (optimize {optimize})");
    }
    evaluated
}

const SOURCE: &str = "struct St { toks: ChunkSeq[u64], out: ChunkSeq[u64], n: u64 }
fn fill(n: u64) -> ChunkSeq[u64] {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    for i in 0..n {
        s.push(wrapping_add(wrapping_mul(i, 7), 3));
    }
    s
}
fn copy_out(inout st: St, n: u64) {
    for i in 0..n {
        let t = match st.toks.try_get(i) {
            Option::Some(x) => x,
            Option::None => 99,
        };
        st.out.push(t);
        st.n = wrapping_add(st.n, t);
    }
}
fn grow(inout st: St, n: u64) {
    for i in 0..n {
        let t = match st.toks.try_get(i) {
            Option::Some(x) => x,
            Option::None => 99,
        };
        st.toks.push(t);
    }
}
fn sum(in s: ChunkSeq[u64]) -> u64 {
    let mut acc: u64 = 0;
    for i in 0..s.len() {
        acc = wrapping_add(wrapping_mul(acc, 31), match s.try_get(i) {
            Option::Some(x) => x,
            Option::None => 0,
        });
    }
    acc
}
fn main() -> u64 {
    let mut st = St { toks: fill(10), out: ChunkSeq::new[u64](), n: 0 };
    // In bounds: the fast copy runs.
    copy_out(inout st, 10);
    // Past the end: the guard fails and the checked loop yields 99s.
    copy_out(inout st, 14);
    grow(inout st, 5);
    wrapping_add(wrapping_add(sum(in st.out), sum(in st.toks)), st.n)
}
";

#[test]
fn other_fields_written_in_the_loop_keep_the_receiver_stable() {
    let program = hoisted(SOURCE);
    assert_eq!(
        guards(&program, "copy_out"),
        1,
        "st.out and st.n leave st.toks alone"
    );
    assert_eq!(
        guards(&program, "grow"),
        0,
        "st.toks.push changes the receiver"
    );
    differential(SOURCE);
}
