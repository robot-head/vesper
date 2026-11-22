//! Rust-exit gap wave 1, lane 2 (#1499): cold inlined callees off the hot
//! path. A callee that owns a sequence, spliced into a branch only rare
//! calls take, gave the whole caller its frame, callee-saved saves, slot
//! zeroing and drop-flag branches (gap 8 of the Kinetic/Resonance report).
//! Such a call stays a call; the same callee is still inlined where every
//! call, or every loop iteration, can reach it.
//!
//! The program runs through the reference evaluator, the JIT with
//! `optimize` on and off, and object emission for x86-64 and AArch64.
use vesper::{
    check,
    ir_inline::{self, Budget, Refusal},
    native, parser,
    runtime::Runtime,
};

/// Kept out of `tests/fixtures` so the corpus that the sampled self-host
/// differentials walk stays as it was.
const SOURCE: &str = r#"
// Gap 8 (#1499): a callee that owns a sequence stays a call on a cold
// branch, and is inlined where every call or every loop iteration runs it.
fn slow(x: u64) -> u64 {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    s.push(x);
    s.push(wrapping_add(x, 1));
    s.len()
}

fn total(in s: ChunkSeq[u64]) -> u64 {
    wrapping_mul(s.len(), 3)
}

pub fn hot(x: u64) -> u64 {
    if x > 1000000 {
        return slow(x);
    }
    wrapping_add(x, 1)
}

pub fn every(x: u64) -> u64 {
    wrapping_add(slow(x), x)
}

pub fn looped(n: u64) -> u64 {
    let mut t: u64 = 0;
    for i in 0..n {
        if i == 3 {
            t = wrapping_add(t, slow(i));
        }
    }
    t
}

pub fn borrowed(x: u64) -> u64 {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    s.push(x);
    if x > 7 {
        return total(in s);
    }
    x
}

fn main() -> u64 {
    wrapping_add(
        wrapping_add(hot(5), hot(2000000)),
        wrapping_add(wrapping_add(every(4), looped(6)), wrapping_add(borrowed(3), borrowed(9)))
    )
}
"#;
const COLD: Refusal = Refusal::Site("sequence owner on a cold path");

fn world(source: &str) -> check::World {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    c.world
}

fn inlined(w: &check::World) -> (ir_inline::Report, vesper::ir::Program) {
    let mut program = vesper::ir::lower_world(w);
    vesper::ir_opt::optimize(w, &mut program).unwrap();
    let report = ir_inline::inline(w, &mut program, &Budget::default());
    let evidence = vesper::ir_check::evidence(&program);
    assert!(evidence.is_empty(), "{evidence:?}");
    (report, program)
}

fn calls(program: &vesper::ir::Program, caller: &str, callee: &str) -> usize {
    program.functions[caller]
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .filter(|i| matches!(&i.op, vesper::ir::Op::Call { callee: c, .. } if c == callee))
        .count()
}

#[test]
fn every_backend_agrees() {
    let w = world(SOURCE);
    for optimize in [true, false] {
        let mut runtime = Runtime::new(&w);
        runtime.optimize = optimize;
        assert_eq!(runtime.run("main", vec![]).unwrap().to_string(), "22");
        assert_eq!(
            native::jit_with_options(&w, "main", optimize)
                .unwrap_or_else(|e| panic!("JIT optimize={optimize}: {e}"))
                .to_string(),
            "22",
            "JIT optimize={optimize}"
        );
        for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
            let (obj, _) = native::object_with_options(&w, "main", Some(target), optimize)
                .unwrap_or_else(|e| panic!("{target} optimize={optimize}: {e}"));
            assert_eq!(&obj[..4], b"\x7fELF");
        }
    }
}

#[test]
fn cold_sequence_owner_stays_a_call() {
    let w = world(SOURCE);
    let (report, program) = inlined(&w);
    // `hot` reaches `slow` only on its rare branch.
    assert_eq!(calls(&program, "hot", "slow"), 1, "{report:?}");
    assert!(
        report.refused["slow"].get(&COLD).is_some_and(|n| *n >= 1),
        "{report:?}"
    );
    // Every call of `every` runs `slow`, and `looped` may run it on any
    // iteration: both keep the inlined copy.
    assert_eq!(calls(&program, "every", "slow"), 0, "{report:?}");
    assert_eq!(calls(&program, "looped", "slow"), 0, "{report:?}");
    // `total` only borrows its sequence, so a cold site still inlines it.
    assert_eq!(calls(&program, "borrowed", "total"), 0, "{report:?}");
}
