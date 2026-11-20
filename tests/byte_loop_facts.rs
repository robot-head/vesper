//! Byte loops, slice headers and range facts in loops (self-hosting SH1
//! lane 3, #1146).
//!
//! - Comparisons that dominate an element access prove it in bounds
//!   (`src/bounds_hoist/facts.rs`): `for i in a..e` after `if e > s.len()`,
//!   a loop bound through an inlined `min`, `loop { if i >= s.len() {
//!   break; } .. }`, straight-line reads after `if i + 1 >= n { return }`,
//!   and the same after `Fail::raise`.
//! - A loop bounded by a compared end the function cannot relate to the
//!   length (`end` read from a line table) is versioned behind a guard
//!   `end <= s.len()`.
//! - A sequence field of an `in` parameter is read once at entry, like a
//!   borrowed sequence parameter.
//! - A range fact from a guard around a loop reaches its body, so `x % n`
//!   there needs no `Fail` (`src/loops/check.rs`).
//!
//! Every program runs in the evaluator, both JIT configurations, x86-64 and
//! AArch64 objects and (where a C driver links) an executable, and all
//! agree, including when an access is out of bounds.
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

fn guards(program: &ir::Program, function: &str) -> usize {
    count(program, function, |op| {
        matches!(op, SeqOp::BoundsGuard { .. })
    })
}

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
            "vesper-byte-loop-facts-{}-{}",
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

/// `byte` as the span and front end v2 ports write it, and a source of
/// `n` bytes `0, 7, 14, ..` (mod 256).
const COMMON: &str = "fn byte(in t: Slice[u8], i: u64) -> u8 {
    match t.try_get(i) {
        Option::Some(x) => x,
        Option::None => 0,
    }
}
fn bytes(n: u64) -> ChunkSeq[u8] {
    let mut s: ChunkSeq[u8] = ChunkSeq::new();
    for i in 0..n {
        s.push(wrapping_u8(wrapping_mul(i, 7)));
    }
    s
}
fn min(a: u64, b: u64) -> u64 {
    if a <= b { a } else { b }
}
";

fn program(body: &str, main: &str) -> String {
    format!("{COMMON}{body}\nfn main() -> u64 {{\n{main}\n}}\n")
}

/// The source bytes `bytes(n)` builds.
fn source_bytes(n: u64) -> Vec<u64> {
    (0..n).map(|i| (i * 7) % 256).collect()
}

const COMPARED: &str = "pub fn sum_checked(in b: Slice[u8], e: u64) -> u64 {
    if e > b.len() {
        return 0;
    }
    let mut h: u64 = 0;
    for i in 0..e {
        h = wrapping_add(h, widen_u64(byte(in b, i)));
    }
    h
}
pub fn sum_min(in b: Slice[u8], e: u64) -> u64 {
    let m = min(e, b.len());
    let mut h: u64 = 0;
    for i in 0..m {
        h = wrapping_add(h, widen_u64(byte(in b, i)));
    }
    h
}
pub fn sum_while(in b: Slice[u8], s: u64) -> u64 {
    let n = b.len();
    let mut h: u64 = 0;
    let mut i = s;
    loop {
        if i >= n {
            break;
        }
        h = wrapping_add(h, widen_u64(byte(in b, i)));
        i = wrapping_add(i, 1);
    }
    h
}
pub fn pair(in b: Slice[u8], i: u64) -> u64 {
    let n = b.len();
    if wrapping_add(i, 1) >= n {
        return 0;
    }
    let d = byte(in b, wrapping_add(i, 1));
    wrapping_add(widen_u64(byte(in b, i)), widen_u64(d))
}
pub fn pair_checked(in b: Slice[u8], i: u64) -> u64 eff [Fail[ArithOverflow]] {
    if i + 1 >= b.len() {
        return 0;
    }
    wrapping_add(widen_u64(byte(in b, i)), widen_u64(byte(in b, i + 1)))
}";

#[test]
fn compared_bounds_need_no_check_or_guard() {
    let source = program(
        COMPARED,
        "    let s = bytes(40);
    let a = sum_checked(in s.as_slice(), 30);
    let b = sum_checked(in s.as_slice(), 41);
    let c = sum_min(in s.as_slice(), 100);
    let d = sum_while(in s.as_slice(), 3);
    let e = pair(in s.as_slice(), 5);
    let f = pair(in s.as_slice(), 39);
    let g = handle Fail[ArithOverflow] in {
        wrapping_add(pair_checked(in s.as_slice(), 6), pair_checked(in s.as_slice(), 18446744073709551615))
    } with {
        return x => x,
        raise(e, k) => 99,
    };
    wrapping_add(wrapping_add(wrapping_add(a, b), wrapping_add(c, d)), wrapping_add(wrapping_add(e, f), g))",
    );
    let p = hoisted(&source);
    for f in ["sum_checked", "sum_min", "sum_while"] {
        assert_eq!(guards(&p, f), 0, "{f}: proved without a guard");
        assert_eq!(checked(&p, f), 0, "{f}: no checked access");
        assert_eq!(unchecked(&p, f), 1, "{f}");
    }
    // `i + 1 < n` (wrapping) proves `b[i + 1]` only: `i` may be u64::MAX.
    assert_eq!((checked(&p, "pair"), unchecked(&p, "pair")), (1, 1));
    // A checked `i + 1 < n` proves both.
    assert_eq!(
        (checked(&p, "pair_checked"), unchecked(&p, "pair_checked")),
        (0, 2)
    );
    let s = source_bytes(40);
    let sum = |r: std::ops::Range<usize>| s[r].iter().sum::<u64>();
    let expected = sum(0..30) + sum(0..40) + sum(3..40) + (s[5] + s[6]) + 99;
    assert_eq!(differential("byte_facts_compared", &source), expected);
}

const RAISED: &str = "copy enum E { Out }
pub fn after_raise(in text: Str, at: u64) -> u64 eff [Fail[E]] {
    let bytes = text.as_bytes();
    let n = text.len();
    if at > n {
        Fail::raise(E::Out);
    }
    let mut c: u64 = 0;
    for i in 0..at {
        if byte(in bytes, i) >= 100 {
            c = wrapping_add(c, 1);
        }
    }
    c
}";

#[test]
fn a_raise_proves_what_follows_it() {
    let source = program(
        RAISED,
        "    let a = handle Fail[E] in { after_raise(in \"hello, wide world\", 12) } with {
        return x => x,
        raise(e, k) => 1000,
    };
    let b = handle Fail[E] in { after_raise(in \"short\", 6) } with {
        return x => x,
        raise(e, k) => 1000,
    };
    wrapping_add(a, b)",
    );
    let p = hoisted(&source);
    assert_eq!(guards(&p, "after_raise"), 0);
    assert_eq!(checked(&p, "after_raise"), 0);
    let expected = b"hello, wide world"[..12]
        .iter()
        .filter(|&&b| b >= 100)
        .count() as u64
        + 1000;
    assert_eq!(differential("byte_facts_raise", &source), expected);
}

const UNTIL: &str = "pub fn sum_until(in b: Slice[u8], s: u64, e: u64) -> u64 {
    let mut h: u64 = 0;
    let mut i = s;
    loop {
        if i >= e {
            break;
        }
        let c = byte(in b, i);
        h = wrapping_add(h, widen_u64(c));
        i = wrapping_add(i, if c < 128 { 1 } else { 2 });
    }
    h
}
pub fn get_until(in b: Slice[u8], e: u64) -> u64 eff [Fail[RuntimeError]] {
    let mut h: u64 = 0;
    let mut i: u64 = 0;
    loop {
        if i >= e {
            break;
        }
        h = wrapping_add(h, widen_u64(b[i]));
        i = wrapping_add(i, 1);
    }
    h
}";

/// `sum_until` in Rust: bytes past the end read as 0.
fn sum_until(s: &[u64], start: u64, end: u64) -> u64 {
    let (mut h, mut i) = (0u64, start);
    while i < end {
        let c = s.get(i as usize).copied().unwrap_or(0);
        h = h.wrapping_add(c);
        i += if c < 128 { 1 } else { 2 };
    }
    h
}

#[test]
fn invariant_ends_are_guarded() {
    let source = program(
        UNTIL,
        "    let s = bytes(64);
    let a = sum_until(in s.as_slice(), 2, 50);
    let b = sum_until(in s.as_slice(), 0, 70);
    let c = sum_until(in s.as_slice(), 9, 9);
    let d = handle Fail[RuntimeError] in {
        wrapping_add(get_until(in s.as_slice(), 64), get_until(in s.as_slice(), 65))
    } with {
        return x => x,
        raise(e, k) => 5000,
    };
    wrapping_add(wrapping_add(a, b), wrapping_add(c, d))",
    );
    let p = hoisted(&source);
    for f in ["sum_until", "get_until"] {
        assert_eq!(guards(&p, f), 1, "{f}: one guard");
        assert_eq!(
            (checked(&p, f), unchecked(&p, f)),
            (1, 1),
            "{f}: checked loop and fast copy"
        );
    }
    let s = source_bytes(64);
    // `get_until(.., 65)` faults at index 64, after the first call.
    let expected = sum_until(&s, 2, 50) + sum_until(&s, 0, 70) + 5000;
    assert_eq!(differential("byte_facts_until", &source), expected);
}

const KEPT: &str = "pub fn moved(in b: Slice[u8], j: u64) -> u64 {
    let mut i = j;
    if i < b.len() {
        i = wrapping_add(i, 5);
        return widen_u64(byte(in b, i));
    }
    0
}
pub fn written_in_loop(in b: Slice[u8]) -> u64 {
    let n = b.len();
    let mut h: u64 = 0;
    let mut i: u64 = 0;
    loop {
        if i >= n {
            break;
        }
        i = wrapping_add(i, 3);
        h = wrapping_add(h, widen_u64(byte(in b, i)));
    }
    h
}
pub fn other_slice(in a: Slice[u8], in b: Slice[u8], i: u64) -> u64 {
    if i < a.len() {
        return widen_u64(byte(in b, i));
    }
    0
}
pub fn grown(inout s: ChunkSeq[u8], i: u64) -> u64 {
    if i >= s.len() {
        return 0;
    }
    s.truncate(0);
    match s.try_get(i) {
        Option::Some(x) => widen_u64(x),
        Option::None => 77,
    }
}";

#[test]
fn changed_indices_and_lengths_keep_their_checks() {
    let source = program(
        KEPT,
        "    let s = bytes(10);
    let t = bytes(4);
    let a = moved(in s.as_slice(), 7);
    let b = written_in_loop(in s.as_slice());
    let c = other_slice(in s.as_slice(), in t.as_slice(), 6);
    let mut u = bytes(10);
    let d = grown(inout u, 3);
    wrapping_add(wrapping_add(a, b), wrapping_add(c, d))",
    );
    let p = hoisted(&source);
    for f in ["moved", "written_in_loop", "other_slice", "grown"] {
        assert_eq!(unchecked(&p, f), 0, "{f}: nothing proved");
    }
    let s = source_bytes(10);
    let b: u64 = [3usize, 6, 9].iter().map(|&i| s[i]).sum::<u64>();
    let expected = b + 77;
    assert_eq!(differential("byte_facts_kept", &source), expected);
}

const FIELDS: &str = "struct Arena { nodes: ChunkSeq[u64], kids: ChunkSeq[u64] }
fn at(in s: ChunkSeq[u64], i: u64) -> u64 {
    match s.try_get(i) {
        Option::Some(v) => v,
        Option::None => 0,
    }
}
pub fn sum4(in a: Arena, i: u64) -> u64 {
    let x = at(in a.nodes, i);
    let y = at(in a.nodes, wrapping_add(i, 1));
    let z = at(in a.kids, i);
    let w = at(in a.kids, wrapping_add(i, 9));
    wrapping_add(wrapping_add(x, y), wrapping_add(z, w))
}";

#[test]
fn sequence_fields_of_in_parameters_are_read_once() {
    let source = format!(
        "{FIELDS}
fn main() -> u64 {{
    let mut n: ChunkSeq[u64] = ChunkSeq::new();
    let mut k: ChunkSeq[u64] = ChunkSeq::new();
    for i in 0..8 {{
        n.push(i);
        k.push(wrapping_add(i, 10));
    }}
    let a = Arena {{ nodes: n, kids: k }};
    let r = wrapping_add(sum4(in a, 2), sum4(in a, 7));
    let Arena {{ nodes, kids }} = a;
    r
}}
"
    );
    let p = hoisted(&source);
    let hoists = vesper::bounds_hoist::header_hoists(&p.functions["sum4"]);
    let mut fields: Vec<String> = hoists
        .iter()
        .filter(|h| h.entry)
        .map(|h| h.place.display())
        .collect();
    fields.sort();
    assert_eq!(fields, ["a.kids", "a.nodes"]);
    // sum4(2) = 2 + 3 + 12 + 0, sum4(7) = 7 + 0 + 17 + 0.
    assert_eq!(differential("byte_facts_fields", &source), 17 + 24);
}

#[test]
fn guards_around_loops_reach_their_bodies() {
    let source = "fn mod_loop(x: u64, n: u64, k: u64) -> u64 {
    let mut acc: u64 = 0;
    if n > 0 {
        for i in 0..k {
            acc = wrapping_add(acc, wrapping_add(x, i) % n);
        }
    }
    acc
}
fn div_while(x: u64, n: u64) -> u64 {
    let mut acc: u64 = 0;
    let mut i: u64 = 0;
    if n != 0 {
        loop {
            if i >= 10 {
                break;
            }
            acc = wrapping_add(acc, x / n);
            i = wrapping_add(i, 1);
        }
    }
    acc
}
fn main() -> u64 {
    wrapping_add(wrapping_add(mod_loop(7, 3, 10), mod_loop(7, 0, 10)), div_while(9, 2))
}
";
    let expected: u64 = (0..10).map(|i| (7 + i) % 3).sum::<u64>() + 10 * (9 / 2);
    assert_eq!(differential("byte_facts_mod", source), expected);
    // The fact is gone once the body writes the divisor.
    let written = "fn f(x: u64, n: u64) -> u64 {
    let mut m = n;
    let mut acc: u64 = 0;
    if m > 0 {
        for i in 0..4 {
            acc = wrapping_add(acc, x % m);
            m = wrapping_sub(m, 1);
        }
    }
    acc
}
fn main() -> u64 { f(9, 2) }
";
    let c = check::check(&parser::parse(written).module);
    assert!(
        c.diagnostics
            .iter()
            .any(|d| format!("{d:?}").contains("E0401")),
        "{:#?}",
        c.diagnostics
    );
}

const LOCALS: &str = "pub fn filled(n: u64) -> u64 {
    let mut v: ChunkSeq[u64] = ChunkSeq::new();
    for i in 0..n {
        v.push(wrapping_mul(i, 3));
    }
    let m = v.len();
    let mut h: u64 = 0;
    let mut k: u64 = 0;
    loop {
        if k >= m {
            break;
        }
        h = wrapping_add(h, match v.try_get(k) { Option::Some(x) => x, Option::None => 0 });
        k = wrapping_add(k, 1);
    }
    h
}
pub fn shrunk(n: u64) -> u64 {
    let mut v: ChunkSeq[u64] = ChunkSeq::new();
    for i in 0..n {
        v.push(i);
    }
    let m = v.len();
    let mut h: u64 = 0;
    let mut k: u64 = 0;
    loop {
        if k >= m {
            break;
        }
        v.truncate(wrapping_sub(m, wrapping_add(k, 1)));
        h = wrapping_add(h, match v.try_get(k) { Option::Some(x) => x, Option::None => 100 });
        k = wrapping_add(k, 1);
    }
    h
}";

#[test]
fn local_lengths_count_between_the_compare_and_the_access() {
    let source = program(LOCALS, "    wrapping_add(filled(20), shrunk(6))");
    let p = hoisted(&source);
    // `v` grows before `v.len()` is read, not after.
    assert_eq!((checked(&p, "filled"), unchecked(&p, "filled")), (0, 1));
    // `truncate` between the compare and the access keeps the check.
    assert_eq!(unchecked(&p, "shrunk"), 0);
    let filled: u64 = (0..20).map(|i| i * 3).sum();
    // shrunk(6): k = 0..6 with len 5, 4, 3, 2, 1, 0 after truncate.
    let shrunk: u64 = (0..6u64)
        .map(|k| if k < 5 - k.min(5) { k } else { 100 })
        .sum();
    assert_eq!(differential("byte_facts_locals", &source), filled + shrunk);
}

const SNAPSHOTS: &str = "pub fn snapshot_end(in b: Slice[u8], inout limit: u64) -> u64 {
    let end = limit;
    limit = 0;
    let mut h: u64 = 0;
    let mut i: u64 = 0;
    loop {
        if i >= end {
            break;
        }
        h = wrapping_add(h, widen_u64(byte(in b, i)));
        i = wrapping_add(i, 1);
    }
    h
}
pub fn snapshot_index(in b: Slice[u8]) -> u64 {
    let mut h: u64 = 0;
    let mut i: u64 = 0;
    loop {
        let j = i;
        if j >= b.len() {
            break;
        }
        h = wrapping_add(h, widen_u64(byte(in b, j)));
        i = wrapping_add(j, 1);
    }
    h
}";

/// A bound read from a local before the local changes is not the local's
/// value in the preheader; an index bound once to a read of a changing
/// local compares and accesses the same read.
#[test]
fn snapshots_of_changing_locals() {
    let source = program(
        SNAPSHOTS,
        "    let s = bytes(64);
    let mut far: u64 = 70;
    let mut near: u64 = 40;
    let a = snapshot_end(in s.as_slice(), inout far);
    let b = snapshot_end(in s.as_slice(), inout near);
    wrapping_add(wrapping_add(a, b), wrapping_add(snapshot_index(in s.as_slice()), wrapping_add(far, near)))",
    );
    let p = hoisted(&source);
    assert_eq!(unchecked(&p, "snapshot_index"), 1, "snapshot_index");
    assert_eq!(guards(&p, "snapshot_end"), 0, "snapshot_end");
    let s = source_bytes(64);
    let all: u64 = s.iter().sum();
    // Bytes past the end read as 0.
    let expected = all + s[..40].iter().sum::<u64>() + all;
    assert_eq!(differential("byte_facts_snapshots", &source), expected);
}
