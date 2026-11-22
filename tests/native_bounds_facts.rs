//! Bounds facts (P3b gaps lane 3, #1088): counted-loop guards for indices
//! affine in the loop index (`s[i + k]`, `s[i - k]`, `s[k - i]`, sums of
//! loop-invariant terms), inner and inlined loops, and checks dominated by a
//! check of the same sequence at the same or a larger constant index
//! (`src/bounds_hoist.rs`). The evaluator, both JIT configurations, x86-64
//! and AArch64 objects and (where a C driver links) an executable agree on
//! every program, including the checked fallback when a guard fails: a short
//! sequence faults at the same iteration, and a `try_get` past the end
//! yields `None` at the same iteration, with the same effects before it.
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{check, ir, ir_check, ir_opt, native, parser, runtime::Runtime, seq::SeqOp};

static NEXT: AtomicU64 = AtomicU64::new(0);

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

fn count(program: &ir::Program, function: &str, pick: impl Fn(&SeqOp) -> bool) -> usize {
    program.functions[function]
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .filter(|i| matches!(&i.op, ir::Op::Seq { op, .. } if pick(op)))
        .count()
}

/// Bounds guards in `function`.
fn guards(program: &ir::Program, function: &str) -> usize {
    count(program, function, |op| {
        matches!(op, SeqOp::BoundsGuard { .. })
    })
}

/// Unchecked accesses in `function`.
fn unchecked(program: &ir::Program, function: &str) -> usize {
    count(program, function, |op| matches!(op, SeqOp::Hoisted(_)))
}

/// Checked element accesses left in `function`.
fn checked(program: &ir::Program, function: &str) -> usize {
    count(program, function, |op| {
        matches!(
            op,
            SeqOp::Get | SeqOp::Set | SeqOp::SliceGet | SeqOp::SliceSet | SeqOp::TryGet
        )
    })
}

fn links_executables() -> bool {
    cfg!(unix)
        && Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
}

/// Runs `main` in the evaluator, both JIT configurations, x86-64 and
/// AArch64 objects and (where a C driver links) an executable.
fn differential(name: &str, source: &str) -> u64 {
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
    let expected: u64 = evaluated.parse().unwrap();
    if links_executables() {
        let dir = std::env::temp_dir().join(format!(
            "vesper-bounds-facts-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("{name}.ves"));
        fs::write(&file, source).unwrap();
        let exe: PathBuf = dir.join(name);
        let built = Command::new(env!("CARGO_BIN_EXE_vesper"))
            .arg("build")
            .arg(&file)
            .arg("-o")
            .arg(&exe)
            .output()
            .unwrap();
        assert!(
            built.status.success(),
            "{name}: build failed: {}",
            String::from_utf8_lossy(&built.stderr)
        );
        let run = Command::new(&exe).output().unwrap();
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(run.status.code(), Some((expected & 0xff) as i32), "{name}");
    }
    expected
}

const FILL: &str = "fn fill(inout s: ChunkSeq[u64], n: u64) {
    for i in 0..n {
        s.push(wrapping_mul(i, 3));
    }
}
";

fn program(body: &str) -> String {
    format!(
        "{FILL}{body}
fn main() -> u64 {{
    handle Fail[RuntimeError] in {{
        handle Fail[ArithOverflow] in {{
            run()
        }} with {{
            return x => x,
            raise(e, k) => 7778,
        }}
    }} with {{
        return x => x,
        raise(e, k) => 7777,
    }}
}}
"
    )
}

/// `s[(max - j) + rank]` for `j` in `0..=max`, the Bench port's `add_rank`
/// written as a counted loop.
const ADD_RANK: &str = "fn add_rank(inout dist: ChunkSeq[u64], max_sum: u64, rank: u64) {
    for j in 0..=max_sum {
        let s = wrapping_sub(max_sum, j);
        let v = match dist.try_get(s) {
            Option::Some(x) => x,
            Option::None => 0,
        };
        let t = wrapping_add(s, rank);
        let w = match dist.try_get(t) {
            Option::Some(x) => x,
            Option::None => 0,
        };
        dist.try_set(t, wrapping_add(w, v));
    }
}
";

/// The subset-sum distribution of ranks `1..=n` over `len` slots, as
/// `add_rank` computes it (writes past `len` are dropped).
fn rank_distribution(n: u64, len: usize) -> Vec<u64> {
    let mut d = vec![0u64; len];
    d[0] = 1;
    let mut max = 0u64;
    for k in 1..=n {
        for j in 0..=max {
            let s = max - j;
            let v = d.get(s as usize).copied().unwrap_or(0);
            let t = (s + k) as usize;
            let w = d.get(t).copied().unwrap_or(0);
            if let Some(slot) = d.get_mut(t) {
                *slot = w.wrapping_add(v);
            }
        }
        max += k;
    }
    d
}

fn add_rank_program(n: u64, len: u64) -> String {
    program(&format!(
        "{ADD_RANK}fn run() -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {{
    let mut d: ChunkSeq[u64] = ChunkSeq::new[u64]();
    let len: u64 = {len};
    for i in 0..len {{
        d.push(0);
    }}
    d.try_set(0, 1);
    let mut max: u64 = 0;
    let ranks: u64 = {n};
    for k in 1..=ranks {{
        add_rank(inout d, max, k);
        max = wrapping_add(max, k);
    }}
    let mut acc: u64 = 0;
    for i in 0..len {{
        acc = wrapping_add(wrapping_mul(acc, 31), match d.try_get(i) {{
            Option::Some(x) => x,
            Option::None => 0,
        }});
    }}
    acc
}}"
    ))
}

fn fold(d: &[u64]) -> u64 {
    d.iter()
        .fold(0u64, |acc, v| acc.wrapping_mul(31).wrapping_add(*v))
}

#[test]
fn reverse_affine_loops_are_guarded() {
    // 1 + 2 + .. + 12 = 78 < 79 slots: every guard holds.
    let source = add_rank_program(12, 79);
    let program = hoisted(&source);
    assert!(guards(&program, "add_rank") >= 2, "s and s + rank");
    // The checked copy keeps its two reads; the fast copy has none.
    assert_eq!(checked(&program, "add_rank"), 2);
    assert!(
        guards(&program, "run") >= 2,
        "the inlined loop is guarded in its caller"
    );
    assert_eq!(
        differential("bounds_add_rank", &source),
        fold(&rank_distribution(12, 79))
    );
}

/// `ADD_RANK` written with the Bench port's helpers (`cu`, `sub`, `add`).
/// Inlined twice over (helpers into `add_rank`, `add_rank` into `run`), the
/// helpers' results reach the loop through block parameters and bound
/// locals; the index forms still are `max - j` and `max - j + rank`
/// (SH6 lane 7, #1402, gap 2).
const ADD_RANK_HELPERS: &str = "fn cu(in s: ChunkSeq[u64], i: u64) -> u64 {
    match s.try_get(i) {
        Option::Some(x) => x,
        Option::None => 0,
    }
}
fn sub(a: u64, b: u64) -> u64 { wrapping_sub(a, b) }
fn add(a: u64, b: u64) -> u64 { wrapping_add(a, b) }
fn add_rank(inout dist: ChunkSeq[u64], max_sum: u64, rank: u64) {
    for j in 0..=max_sum {
        let sum = sub(max_sum, j);
        let v = cu(in dist, sum);
        let t = add(sum, rank);
        dist.try_set(t, add(cu(in dist, t), v));
    }
}
";

#[test]
fn loops_of_inlined_helpers_are_guarded() {
    for len in [79, 40] {
        let source = add_rank_program(12, len).replace(ADD_RANK, ADD_RANK_HELPERS);
        assert!(source.contains("fn cu("));
        let program = hoisted(&source);
        assert!(
            guards(&program, "run") >= 2,
            "the doubly inlined loop is guarded in its caller"
        );
        assert!(
            unchecked(&program, "run") >= 3,
            "its fast copy is unchecked"
        );
        assert_eq!(
            differential(&format!("bounds_add_rank_helpers_{len}"), &source),
            fold(&rank_distribution(12, len as usize))
        );
    }
}

#[test]
fn failing_affine_guards_fall_back_at_the_same_iteration() {
    // 40 slots for sums up to 78: later calls overrun and take the checked
    // copy, whose `try_get` yields `None` and whose `try_set` drops the
    // write at exactly the iterations the reference does.
    let source = add_rank_program(12, 40);
    assert_eq!(
        differential("bounds_add_rank_short", &source),
        fold(&rank_distribution(12, 40))
    );
}

#[test]
fn affine_forms_agree() {
    // i + k, i - k, k - i, (k - i) + m and constants, exclusive and
    // inclusive ranges; the last calls overrun so their guards fail.
    let source = program(
        "fn forms(in s: Slice[u64], lo: u64, hi: u64, k: u64) -> u64 {
    let mut acc: u64 = 0;
    for i in lo..hi {
        let a = match s.try_get(wrapping_add(i, k)) {
            Option::Some(x) => x,
            Option::None => 1000,
        };
        let b = match s.try_get(wrapping_sub(i, k)) {
            Option::Some(x) => x,
            Option::None => 2000,
        };
        let c = match s.try_get(wrapping_sub(k, i)) {
            Option::Some(x) => x,
            Option::None => 3000,
        };
        let d = match s.try_get(wrapping_add(wrapping_sub(k, i), 7)) {
            Option::Some(x) => x,
            Option::None => 4000,
        };
        let e = match s.try_get(wrapping_add(i, 3)) {
            Option::Some(x) => x,
            Option::None => 5000,
        };
        acc = wrapping_add(wrapping_mul(acc, 7), wrapping_add(wrapping_add(a, b), wrapping_add(c, wrapping_add(d, e))));
    }
    acc
}
fn inclusive(in s: Slice[u64], k: u64, n: u64) -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let mut acc: u64 = 0;
    for i in 0..=n {
        acc = wrapping_add(wrapping_mul(acc, 5), wrapping_add(s[k - i], s[i + k]));
    }
    acc
}
fn run() -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    fill(inout s, 64);
    let mut acc: u64 = 0;
    acc = wrapping_add(acc, forms(in s.as_slice(), 10, 20, 10));
    acc = wrapping_add(wrapping_mul(acc, 3), forms(in s.as_slice(), 5, 30, 20));
    acc = wrapping_add(wrapping_mul(acc, 3), forms(in s.as_slice(), 0, 64, 1));
    acc = wrapping_add(wrapping_mul(acc, 3), forms(in s.as_slice(), 3, 9, 18446744073709551614));
    acc = wrapping_add(wrapping_mul(acc, 3), inclusive(in s.as_slice(), 30, 30));
    acc
}",
    );
    let program = hoisted(&source);
    assert_eq!(guards(&program, "forms"), 5, "one guard per form");
    assert_eq!(guards(&program, "inclusive"), 2);
    let s: Vec<u64> = (0..64).map(|i| i * 3).collect();
    let get = |i: u64, none: u64| s.get(i as usize).copied().unwrap_or(none);
    let forms = |lo: u64, hi: u64, k: u64| {
        let mut acc: u64 = 0;
        for i in lo..hi {
            let a = get(i.wrapping_add(k), 1000);
            let b = get(i.wrapping_sub(k), 2000);
            let c = get(k.wrapping_sub(i), 3000);
            let d = get(k.wrapping_sub(i).wrapping_add(7), 4000);
            let e = get(i.wrapping_add(3), 5000);
            acc = acc.wrapping_mul(7).wrapping_add(
                a.wrapping_add(b)
                    .wrapping_add(c.wrapping_add(d.wrapping_add(e))),
            );
        }
        acc
    };
    let mut inclusive: u64 = 0;
    for i in 0..=30u64 {
        inclusive = inclusive
            .wrapping_mul(5)
            .wrapping_add(s[(30 - i) as usize] + s[(i + 30) as usize]);
    }
    let mut expected = forms(10, 20, 10);
    expected = expected.wrapping_mul(3).wrapping_add(forms(5, 30, 20));
    expected = expected.wrapping_mul(3).wrapping_add(forms(0, 64, 1));
    expected = expected
        .wrapping_mul(3)
        .wrapping_add(forms(3, 9, u64::MAX - 1));
    expected = expected.wrapping_mul(3).wrapping_add(inclusive);
    assert_eq!(differential("bounds_forms", &source), expected);
}

#[test]
fn checked_index_arithmetic_still_raises() {
    // `s[k - i]` with a checked `-`: when `i > k` the subtraction raises,
    // in the fast copy as in the checked one (the guard fails here anyway).
    let source = program(
        "fn down(in s: Slice[u64], k: u64, n: u64) -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let mut acc: u64 = 0;
    for i in 0..n {
        acc = wrapping_add(acc, s[k - i]);
    }
    acc
}
fn run() -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    fill(inout s, 16);
    let a = down(in s.as_slice(), 15, 16);
    wrapping_add(a, down(in s.as_slice(), 3, 5))
}",
    );
    assert_eq!(guards(&hoisted(&source), "down"), 1);
    // `3 - 4` raises `ArithOverflow` before any access is out of bounds.
    assert_eq!(differential("bounds_checked_sub", &source), 7778);
}

#[test]
fn short_sequence_faults_at_the_same_iteration() {
    // `t[i + 2]` overruns at i = 8 of 0..10: the checked loop faults there.
    let source = program(
        "fn shift(in s: Slice[u64], n: u64) -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let mut acc: u64 = 0;
    for i in 0..n {
        acc = wrapping_add(acc, s[i + 2]);
    }
    acc
}
fn run() -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    fill(inout s, 10);
    let ok = shift(in s.as_slice(), 8);
    wrapping_add(ok, shift(in s.as_slice(), 10))
}",
    );
    assert_eq!(guards(&hoisted(&source), "shift"), 1);
    assert_eq!(differential("bounds_short", &source), 7777);
    let ok = program(
        "fn shift(in s: Slice[u64], n: u64) -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let mut acc: u64 = 0;
    for i in 0..n {
        acc = wrapping_add(acc, s[i + 2]);
    }
    acc
}
fn run() -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    fill(inout s, 10);
    shift(in s.as_slice(), 8)
}",
    );
    let expected: u64 = (2..10).map(|i| i * 3).sum();
    assert_eq!(differential("bounds_short_ok", &ok), expected);
}

#[test]
fn inner_loops_are_guarded() {
    // Rows of a matrix: the inner loop's `m[base + j]` is guarded once per
    // row, the outer loop is not a candidate.
    let source = program(
        "fn rows(in m: Slice[u64], r: u64, c: u64) -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let mut acc: u64 = 0;
    for i in 0..r {
        let base = i * c;
        for j in 0..c {
            acc = wrapping_add(wrapping_mul(acc, 3), m[base + j]);
        }
    }
    acc
}
fn run() -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    fill(inout s, 48);
    wrapping_add(rows(in s.as_slice(), 6, 8), rows(in s.as_slice(), 4, 13))
}",
    );
    let program = hoisted(&source);
    assert_eq!(guards(&program, "rows"), 1);
    let rows = |r: u64, c: u64| {
        let mut acc: u64 = 0;
        for i in 0..r {
            for j in 0..c {
                acc = acc.wrapping_mul(3).wrapping_add((i * c + j) * 3);
            }
        }
        acc
    };
    // The second call reads up to 51 > 47: it faults in its last row.
    let _ = rows(6, 8);
    assert_eq!(differential("bounds_rows", &source), 7777);
    let fits = source.replace(
        "rows(in s.as_slice(), 4, 13)",
        "rows(in s.as_slice(), 4, 12)",
    );
    assert_eq!(
        differential("bounds_rows_fit", &fits),
        rows(6, 8).wrapping_add(rows(4, 12))
    );
}

#[test]
fn dominated_checks_are_dropped() {
    // `g[5]` covers `g[0]`, `g[3]` and the second `g[5]`; `s[i]` covers
    // `s.set(i, ..)`.
    let source = program(
        "fn pick(in g: Slice[u64]) -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let a = g[5];
    let b = g[0];
    let c = g[3];
    let d = g[5];
    wrapping_add(wrapping_add(a, b), wrapping_add(c, d))
}
fn bump(inout s: Slice[u64], i: u64) eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    s.set(i, wrapping_add(s[i], 1));
}
fn low_first(in g: Slice[u64]) -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    wrapping_add(g[1], g[6])
}
fn run() -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let mut x: ChunkSeq[u64] = ChunkSeq::new[u64]();
    fill(inout x, 8);
    let mut t: u64 = 0;
    for r in 0..3 {
        t = wrapping_add(t, pick(in x.as_slice()));
        bump(inout x.as_slice_mut(), 2);
    }
    wrapping_add(t, low_first(in x.as_slice()))
}",
    );
    let program = hoisted(&source);
    assert_eq!(checked(&program, "pick"), 1, "only g[5] is checked");
    assert_eq!(unchecked(&program, "pick"), 3);
    assert_eq!(checked(&program, "bump"), 1);
    assert_eq!(
        checked(&program, "low_first"),
        2,
        "g[1] does not cover g[6]"
    );
    let mut x: Vec<u64> = (0..8).map(|i| i * 3).collect();
    let mut t: u64 = 0;
    for _ in 0..3 {
        t = t.wrapping_add(x[5] + x[0] + x[3] + x[5]);
        x[2] += 1;
    }
    let expected = t.wrapping_add(x[1] + x[6]);
    assert_eq!(differential("bounds_dominated", &source), expected);
    // A short slice still faults at the first, checked, access.
    let short = source.replace("fill(inout x, 8);", "fill(inout x, 5);");
    assert_eq!(differential("bounds_dominated_short", &short), 7777);
}

#[test]
fn resized_or_total_accesses_establish_nothing() {
    // A `push` between the accesses or a total `try_get` keeps the later
    // check, and the program faults as the reference does.
    let source = program(
        "fn grow(inout s: ChunkSeq[u64]) -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let a = s[3];
    s.truncate(2);
    let b = s[3];
    wrapping_add(a, b)
}
fn total_first(in g: Slice[u64]) -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let a = match g.try_get(9) {
        Option::Some(x) => x,
        Option::None => 100,
    };
    wrapping_add(a, g[4])
}
fn run() -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let mut x: ChunkSeq[u64] = ChunkSeq::new[u64]();
    fill(inout x, 6);
    let t = total_first(in x.as_slice());
    wrapping_add(t, grow(inout x))
}",
    );
    let program = hoisted(&source);
    assert_eq!(checked(&program, "grow"), 2);
    assert_eq!(checked(&program, "total_first"), 2);
    assert_eq!(differential("bounds_resized", &source), 7777);
}

#[test]
fn guard_arithmetic_that_overflows_falls_back() {
    // `i + k` with k near 2^64 wraps: the exact index does not fit, so the
    // guard fails and the checked copy yields `None` as the reference does.
    let source = program(
        "fn near(in s: Slice[u64], k: u64, n: u64) -> u64 {
    let mut acc: u64 = 0;
    for i in 0..n {
        acc = match s.try_get(wrapping_add(i, k)) {
            Option::Some(v) => wrapping_add(acc, v),
            Option::None => wrapping_add(wrapping_mul(acc, 2), i),
        };
    }
    acc
}
fn run() -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    fill(inout s, 20);
    wrapping_add(near(in s.as_slice(), 18446744073709551610, 10), near(in s.as_slice(), 4, 10))
}",
    );
    assert_eq!(guards(&hoisted(&source), "near"), 1);
    let s: Vec<u64> = (0..20).map(|i| i * 3).collect();
    let near = |k: u64, n: u64| {
        let mut acc: u64 = 0;
        for i in 0..n {
            acc = match s.get(i.wrapping_add(k) as usize) {
                Some(v) => acc.wrapping_add(*v),
                None => acc.wrapping_mul(2).wrapping_add(i),
            };
        }
        acc
    };
    assert_eq!(
        differential("bounds_overflow", &source),
        near(u64::MAX - 5, 10).wrapping_add(near(4, 10))
    );
}

#[test]
fn loops_that_own_boxes_are_guarded() {
    // Each iteration replaces the box `c` (dropping the old one), so the
    // loop carries box drops; the fast copy reclaims them at the same
    // points, and a short slice still faults in the checked copy.
    let source = program(
        "struct Counter { n: u64 }
fn owned(in s: Slice[u64], n: u64) -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let mut c: Box[Counter] = Box::new(Counter { n: 0 });
    let mut acc: u64 = 0;
    for i in 0..n {
        acc = wrapping_add(acc, s[i]);
        c = Box::new(Counter { n: wrapping_add(acc, i) });
    }
    let Box(last) = c;
    wrapping_add(acc, last.n)
}
fn run() -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    fill(inout s, 12);
    owned(in s.as_slice(), 12)
}",
    );
    let program_ir = hoisted(&source);
    let f = &program_ir.functions["owned"];
    assert!(
        !f.box_drops_before_inst.is_empty()
            || !f.box_drops_after_inst.is_empty()
            || !f.box_drops_before_term.is_empty(),
        "the loop owns boxes"
    );
    assert_eq!(guards(&program_ir, "owned"), 1);
    assert_eq!(unchecked(&program_ir, "owned"), 1);
    // Box drops balance in both JIT configurations.
    let c = accepted(&source);
    for optimize in [true, false] {
        vesper::fip::native_runtime::reset_counts();
        native::jit_with_options(&c.world, "main", optimize).unwrap();
        assert_eq!(
            vesper::fip::native_runtime::alloc_count(),
            vesper::fip::native_runtime::free_count(),
            "optimize {optimize}: boxes leaked or freed twice"
        );
    }
    let acc: u64 = (0..12).map(|i| i * 3).sum();
    assert_eq!(differential("bounds_boxes", &source), acc + acc + 11);
    let short = source.replace("fill(inout s, 12);", "fill(inout s, 7);");
    assert_eq!(differential("bounds_boxes_short", &short), 7777);
}

#[test]
fn short_constant_loops_are_not_versioned() {
    // Three iterations of four accesses through four index forms save less
    // than four guards cost (the Kinetic sweep-and-prune overlap test), so
    // the loop keeps its checks; a long constant loop is still guarded.
    let source = program(
        "fn three() -> u64 { 3 }
fn overlaps(in lo: Slice[u64], in hi: Slice[u64], s: u64, t: u64) -> bool eff [Fail[RuntimeError]] {
    let bs = wrapping_mul(s, 3);
    let bt = wrapping_mul(t, 3);
    for a in 0..three() {
        if !(lo[wrapping_add(bs, a)] <= hi[wrapping_add(bt, a)] && lo[wrapping_add(bt, a)] <= hi[wrapping_add(bs, a)]) {
            return false;
        }
    }
    true
}
fn long(in s: Slice[u64]) -> u64 eff [Fail[RuntimeError]] {
    let mut acc: u64 = 0;
    for i in 0..16 {
        acc = wrapping_add(acc, s[i]);
    }
    acc
}
fn run() -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let mut lo: ChunkSeq[u64] = ChunkSeq::new[u64]();
    let mut hi: ChunkSeq[u64] = ChunkSeq::new[u64]();
    fill(inout lo, 30);
    fill(inout hi, 30);
    let mut n: u64 = 0;
    for s in 0..9 {
        for t in 0..9 {
            if overlaps(in lo.as_slice(), in hi.as_slice(), s, t) {
                n = wrapping_add(n, 1);
            }
        }
    }
    wrapping_add(wrapping_mul(n, 1000), long(in lo.as_slice()))
}",
    );
    let ir = hoisted(&source);
    assert_eq!(guards(&ir, "overlaps"), 0);
    assert_eq!(guards(&ir, "long"), 1);
    // lo == hi, element k holding 3k: boxes s and t overlap iff s == t.
    assert_eq!(
        differential("bounds_short_loops", &source),
        9 * 1000 + (0..16).map(|i| i * 3).sum::<u64>()
    );
}

#[test]
fn short_inner_loops_keep_their_checks() {
    // A triangle loop (`j` in `i + 1..n`) changes its range with the outer
    // loop and has one access for its one guard (the Ignis use-pair check):
    // only the outer loop is guarded. A row loop of fixed length is guarded
    // ([`inner_loops_are_guarded`]).
    let source = program(
        "fn pairs(in s: Slice[u64], n: u64) -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let mut hits: u64 = 0;
    for i in 0..n {
        let a = s[i];
        for j in wrapping_add(i, 1)..n {
            if s[j] == a {
                hits = wrapping_add(hits, 1);
            }
        }
    }
    hits
}
fn run() -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    for i in 0..12 {
        s.push(i % 4);
    }
    pairs(in s.as_slice(), 12)
}",
    );
    // (With `n = s.len()` both loops are proved in bounds without a guard,
    // `tests/byte_loop_facts.rs`; a bound passed in needs the guards.)
    assert_eq!(guards(&hoisted(&source), "pairs"), 1);
    // Each residue appears 3 times: 3 pairs each.
    assert_eq!(differential("bounds_pairs", &source), 12);
}
