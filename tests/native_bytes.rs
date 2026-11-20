//! Byte loops (Stage 1 performance wave, lane 3, #933): bounds-check
//! hoisting out of counted loops (`src/bounds_hoist.rs`) and the reserved
//! 64x64 -> 128-bit multiply `mul_wide`. The evaluator, both JIT
//! configurations and an AOT executable agree on every program, including
//! the fallback to the checked loop when a sequence is too short.
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::seq::SeqOp;
use vesper::{check, ir, ir_check, ir_opt, native, parser, runtime::Runtime};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-native-bytes-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn accepted(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:#?}", c.diagnostics);
    c
}

fn links_executables() -> bool {
    cfg!(unix)
        && Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
}

fn executable_status(name: &str, source: &str) -> Option<i32> {
    let temp = Temp::new();
    let file = temp.0.join(format!("{name}.ves"));
    fs::write(&file, source).unwrap();
    let exe = temp.0.join(name);
    let built = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{name}: vesper build failed\n{}{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    Command::new(&exe).output().unwrap().status.code()
}

/// The optimized native Place IR of `source`, after hoisting, and how many
/// loops the pass versioned.
fn hoisted(source: &str) -> (ir::Program, usize) {
    let c = accepted(source);
    let mut program = ir::lower_world(&c.world);
    ir_opt::optimize(&c.world, &mut program).unwrap();
    vesper::fip::elaborate_non_fip_drops(&c.world, &mut program);
    let n = vesper::bounds_hoist::run(&mut program);
    (program, n)
}

fn count(program: &ir::Program, function: &str, pick: impl Fn(&SeqOp) -> bool) -> usize {
    program.functions[function]
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .filter(|i| matches!(&i.op, ir::Op::Seq { op, .. } if pick(op)))
        .count()
}

/// Accesses a guard or a dominating comparison made unchecked.
fn unchecked(program: &ir::Program, function: &str) -> usize {
    count(program, function, |op| matches!(op, SeqOp::Hoisted(_)))
}

fn checked(program: &ir::Program, function: &str) -> usize {
    count(program, function, |op| {
        matches!(
            op,
            SeqOp::Get
                | SeqOp::Set
                | SeqOp::SliceGet
                | SeqOp::SliceSet
                | SeqOp::TryGet
                | SeqOp::TrySet
        )
    })
}

/// Runs `main` in the evaluator, both JIT configurations and (where a C
/// driver links executables) an AOT executable; returns the result.
fn differential(name: &str, source: &str) -> String {
    let c = accepted(source);
    let evaluated = Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap_or_else(|e| panic!("{name}: evaluator failed: {e}"))
        .to_string();
    for optimize in [true, false] {
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap_or_else(|e| panic!("{name}: JIT (optimize {optimize}) failed: {e}"))
            .to_string();
        assert_eq!(
            jit, evaluated,
            "{name}: JIT (optimize {optimize}) vs evaluator"
        );
    }
    if links_executables() {
        let expected: u64 = evaluated.parse().unwrap();
        assert_eq!(
            executable_status(name, source),
            Some((expected & 0xff) as i32),
            "{name}: executable"
        );
    }
    evaluated
}

const FILL: &str = "fn fill(inout s: ChunkSeq[u64], n: u64) {\n    for i in 0..n {\n        s.push(wrapping_mul(i, 3));\n    }\n}\n";

fn program(body: &str) -> String {
    format!(
        "{FILL}{body}\nfn main() -> u64 {{\n    handle Fail[RuntimeError] in {{\n        run()\n    }} with {{\n        return x => x,\n        raise(e, k) => 7777,\n    }}\n}}\n"
    )
}

/// `total` reads `s[i]` for `i` in `0..n`.
fn sum_upto(n: &str, len: u64) -> String {
    program(&format!(
        "fn total(in s: Slice[u64], n: u64) -> u64 eff [Fail[RuntimeError]] {{\n    let mut acc: u64 = 0;\n    for i in 0..n {{\n        acc = wrapping_add(acc, s[i]);\n    }}\n    acc\n}}\nfn run() -> u64 eff [Fail[RuntimeError]] {{\n    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();\n    fill(inout s, {len});\n    total(in s.as_slice(), {n})\n}}"
    ))
}

#[test]
fn hoisted_loop_reads_agree() {
    let source = sum_upto("100", 100);
    let (program, n) = hoisted(&source);
    assert!(n >= 1, "the slice loop is versioned");
    assert!(
        ir_check::evidence(&program).is_empty(),
        "{:?}",
        ir_check::evidence(&program)
    );
    let expected: u64 = (0..100).map(|i| i * 3).sum();
    assert_eq!(differential("hoist_sum", &source), expected.to_string());
}

#[test]
fn short_sequence_faults_at_the_same_element() {
    // The guard fails, the checked loop runs and faults at i = 50.
    let source = sum_upto("80", 50);
    assert!(hoisted(&source).1 >= 1);
    assert_eq!(differential("hoist_fault", &source), "7777");
    // An empty range never faults, whatever the length.
    let empty = sum_upto("0", 0);
    assert_eq!(differential("hoist_empty", &empty), "0");
}

#[test]
fn fallback_keeps_prior_effects() {
    // `try_get` past the end yields None in the checked copy; the effects of
    // the iterations before it (the sum so far) are kept exactly.
    let source = program(
        "fn total(in s: Slice[u64], n: u64) -> u64 {
    let mut acc: u64 = 0;
    for i in 0..n {
        acc = match s.try_get(i) {
            Option::Some(v) => wrapping_add(acc, v),
            Option::None => wrapping_add(wrapping_mul(acc, 2), i),
        };
    }
    acc
}
fn run() -> u64 eff [Fail[RuntimeError]] {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    fill(inout s, 20);
    wrapping_add(total(in s.as_slice(), 23), total(in s.as_slice(), 20))
}",
    );
    assert!(hoisted(&source).1 >= 1);
    let total = |n: u64| {
        let mut acc: u64 = 0;
        for i in 0..n {
            acc = if i < 20 {
                acc.wrapping_add(i * 3)
            } else {
                acc.wrapping_mul(2).wrapping_add(i)
            };
        }
        acc
    };
    let expected = total(23).wrapping_add(total(20));
    assert_eq!(differential("hoist_try_get", &source), expected.to_string());
}

#[test]
fn hoisted_writes_inclusive_and_nested_loops_agree() {
    let source = program(
        "fn scale(inout x: Slice[u64], k: u64) {
    let n = x.len();
    if n > 0 {
        for i in 0..=wrapping_sub(n, 1) {
            let v = match x.try_get(i) {
                Option::Some(v) => v,
                Option::None => 0,
            };
            x.try_set(i, wrapping_mul(v, k));
        }
    } else {
        ()
    }
}
fn pairs(in s: Slice[u64]) -> u64 eff [Fail[RuntimeError]] {
    let mut acc: u64 = 0;
    for i in 0..s.len() {
        for j in i..s.len() {
            acc = wrapping_add(acc, wrapping_mul(s[i], s[j]));
        }
    }
    acc
}
fn run() -> u64 eff [Fail[RuntimeError]] {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    fill(inout s, 30);
    scale(inout s.as_slice_mut(), 5);
    for i in 0..s.len() {
        let v = s[i];
        s.set(i, wrapping_add(v, 1));
    }
    pairs(in s.as_slice())
}",
    );
    // `scale` and the set loop are versioned; `pairs` reads below
    // `s.len()`, which its loop comparisons prove with no guard.
    let (program, _) = hoisted(&source);
    for f in ["scale", "pairs", "run"] {
        assert!(unchecked(&program, f) >= 1, "{f}: unchecked accesses");
    }
    assert!(ir_check::evidence(&program).is_empty());
    let s: Vec<u64> = (0..30).map(|i| i * 3 * 5 + 1).collect();
    let mut expected: u64 = 0;
    for i in 0..s.len() {
        for j in i..s.len() {
            expected = expected.wrapping_add(s[i].wrapping_mul(s[j]));
        }
    }
    assert_eq!(differential("hoist_nested", &source), expected.to_string());
}

#[test]
fn offset_copies_agree_and_fall_back() {
    // `dst[at + k] = src[from + k]`, as warp-tcp copies payloads; the
    // second and third calls overrun `src` and `dst`, so they take the
    // checked loop.
    let source = program(
        "fn copy_into(in src: Slice[u64], from: u64, inout dst: Slice[u64], at: u64, n: u64) {
    for k in 0..n {
        match src.try_get(wrapping_add(from, k)) {
            Option::Some(b) => {
                dst.try_set(wrapping_add(at, k), b);
            },
            Option::None => (),
        }
    }
}
fn run() -> u64 eff [Fail[RuntimeError]] {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    fill(inout s, 40);
    let mut d: ChunkSeq[u64] = ChunkSeq::new[u64]();
    for i in 0..30 {
        d.push(1);
    }
    copy_into(in s.as_slice(), 5, inout d.as_slice_mut(), 2, 10);
    copy_into(in s.as_slice(), 35, inout d.as_slice_mut(), 12, 10);
    copy_into(in s.as_slice(), 0, inout d.as_slice_mut(), 25, 10);
    copy_into(in s.as_slice(), 18446744073709551615, inout d.as_slice_mut(), 0, 3);
    let mut acc: u64 = 0;
    for i in 0..d.len() {
        acc = wrapping_add(wrapping_mul(acc, 3), d[i]);
    }
    acc
}",
    );
    // `copy_into` is versioned; the final sum's `i < d.len()` proves its
    // reads with no guard.
    let (program, _) = hoisted(&source);
    for f in ["copy_into", "run"] {
        assert!(unchecked(&program, f) >= 1, "{f}: unchecked accesses");
    }
    assert!(ir_check::evidence(&program).is_empty());
    let s: Vec<u64> = (0..40).map(|i| i * 3).collect();
    let mut d = vec![1u64; 30];
    for (from, at, n) in [
        (5u64, 2u64, 10u64),
        (35, 12, 10),
        (0, 25, 10),
        (u64::MAX, 0, 3),
    ] {
        for k in 0..n {
            if let Some(&b) = s.get(from.wrapping_add(k) as usize)
                && let Some(slot) = d.get_mut(at.wrapping_add(k) as usize)
            {
                *slot = b;
            }
        }
    }
    let expected = d
        .iter()
        .fold(0u64, |acc, v| acc.wrapping_mul(3).wrapping_add(*v));
    assert_eq!(differential("hoist_offsets", &source), expected.to_string());
}

#[test]
fn loop_variables_lent_inout_are_not_induction_values() {
    // `change` moves the index out of range: the access must stay checked.
    let source = program(
        "fn change(inout x: u64) {
    x = 1000;
}
fn run() -> u64 eff [Fail[RuntimeError]] {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    fill(inout s, 2);
    let mut acc: u64 = 0;
    for i in 0..s.len() {
        change(inout i);
        acc = wrapping_add(acc, s[i]);
    }
    acc
}",
    );
    assert_eq!(hoisted(&source).1, 0);
    assert_eq!(differential("hoist_inout_index", &source), "7777");
}

#[test]
fn resizing_loops_are_not_versioned() {
    // The loop pushes onto the sequence it indexes.
    let source = program(
        "fn run() -> u64 eff [Fail[RuntimeError]] {
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    fill(inout s, 10);
    for i in 0..s.len() {
        let v = s[i];
        s.push(v);
    }
    let mut acc: u64 = 0;
    for i in 0..s.len() {
        acc = wrapping_add(acc, s[i]);
    }
    acc
}",
    );
    let (program, _) = hoisted(&source);
    let run = &program.functions["run"];
    let guards = run
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .filter(|i| {
            matches!(
                i.op,
                ir::Op::Seq {
                    op: vesper::seq::SeqOp::BoundsGuard { .. },
                    ..
                }
            )
        })
        .count();
    // The read-only loop's `i < s.len()` proves its read without a guard,
    // so a guard would mean the pushing loop was versioned.
    assert_eq!(guards, 0, "the pushing loop is not versioned");
    assert!(
        checked(&program, "run") >= 1,
        "the pushing loop stays checked"
    );
    assert!(
        unchecked(&program, "run") >= 1,
        "the read-only loop is unchecked"
    );
    let expected: u64 = 2 * (0..10).map(|i| i * 3).sum::<u64>();
    assert_eq!(differential("hoist_resize", &source), expected.to_string());
}

#[test]
fn mul_wide_agrees_with_u128() {
    let cases: [(u64, u64); 6] = [
        (0, 0),
        (1, u64::MAX),
        (u64::MAX, u64::MAX),
        (u64::MAX, u64::MAX - 2),
        (123_456_789_012_345, 987_654_321_098_765),
        (1 << 63, 3),
    ];
    let mut body =
        String::from("fn run() -> u64 eff [Fail[RuntimeError]] {\n    let mut acc: u64 = 0;\n");
    let mut expected: u64 = 0;
    for (k, (a, b)) in cases.iter().enumerate() {
        body.push_str(&format!(
            "    let (lo{k}, hi{k}) = mul_wide({a}, {b});\n    acc = wrapping_add(wrapping_mul(acc, 31), wrapping_add(lo{k}, wrapping_mul(hi{k}, 7)));\n"
        ));
        let p = u128::from(*a) * u128::from(*b);
        expected = expected
            .wrapping_mul(31)
            .wrapping_add((p as u64).wrapping_add(((p >> 64) as u64).wrapping_mul(7)));
    }
    body.push_str("    acc\n}");
    assert_eq!(
        differential("mul_wide", &program(&body)),
        expected.to_string()
    );
}

#[test]
fn mul_wide_is_reserved() {
    let p = parser::parse(
        "fn mul_wide(a: u64, b: u64) -> u64 { a }\nfn main() -> u64 { mul_wide(1, 2) }",
    );
    let c = check::check(&p.module);
    assert!(
        c.diagnostics
            .iter()
            .any(|d| d.code == "E0100" && d.message.contains("mul_wide")),
        "{:?}",
        c.diagnostics
    );
}
