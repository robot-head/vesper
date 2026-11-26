//! Bounds hoisting of scaled indexes and fixed-width accesses (#2022):
//! counted loops whose accesses index `c * (±i + Σp - Σn) + Σq - Σr` for a
//! constant scale `c` (`i * C + k`, `wrapping_mul(i, C)`, `(k + i) << C`),
//! and whose fixed-width reads and writes of byte sequences
//! (`try_read_u64_le`, `write_u32_be`, ...) touch several elements from
//! their index (`src/bounds_hoist.rs`). The evaluator, both JIT
//! configurations, x86-64 and AArch64 objects and (where a C driver links)
//! an executable agree on every program, including the checked fallback
//! when a guard fails: a short sequence faults at the same iteration, and a
//! total access past the end yields `None` or `false` at the same
//! iteration, with the same effects before it.
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{
    check, ir, ir_check, ir_opt, native, parser,
    runtime::Runtime,
    seq::{Access, SeqOp},
};

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

/// Scaled bounds guards in `function`.
fn scaled_guards(program: &ir::Program, function: &str) -> usize {
    count(
        program,
        function,
        |op| matches!(op, SeqOp::BoundsGuard { form, .. } if form.scaled),
    )
}

/// Bounds guards of `width` elements in `function`.
fn wide_guards(program: &ir::Program, function: &str, width: u8) -> usize {
    count(
        program,
        function,
        |op| matches!(op, SeqOp::BoundsGuard { form, .. } if form.width == width),
    )
}

/// Unchecked element accesses in `function`.
fn unchecked(program: &ir::Program, function: &str) -> usize {
    count(
        program,
        function,
        |op| matches!(op, SeqOp::Hoisted(a) if !matches!(a, Access::Wide(_))),
    )
}

/// Unchecked fixed-width accesses in `function`.
fn unchecked_wide(program: &ir::Program, function: &str) -> usize {
    count(program, function, |op| {
        matches!(op, SeqOp::Hoisted(Access::Wide(_)))
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
            "vesper-scaled-bounds-test-{}-{}",
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

/// `body` with a `main` that maps a raised `IndexOutOfBounds` to 7777 and
/// an overflow to 7778.
fn program(body: &str) -> String {
    format!(
        "fn gw(in s: Slice[u64], i: u64) -> u64 {{
    match s.try_get(i) {{
        Option::Some(x) => x,
        Option::None => 1000,
    }}
}}
fn rd(in s: Slice[u8], at: u64) -> u64 {{
    match s.try_read_u64_le(at) {{
        Option::Some(v) => v,
        Option::None => 77,
    }}
}}
fn words(n: u64) -> ChunkSeq[u64] {{
    let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
    for i in 0..n {{
        s.push(wrapping_add(wrapping_mul(i, 2654435761), 1));
    }}
    s
}}
fn bytes(n: u64) -> ChunkSeq[u8] {{
    let mut s: ChunkSeq[u8] = ChunkSeq::new[u8]();
    for i in 0..n {{
        s.push(wrapping_u8(wrapping_add(wrapping_mul(i, 37), 11)));
    }}
    s
}}
{body}
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

/// The Kinetic world substeps' body reads: `b[i * 5 + j]` through a total
/// helper, for `i` in `lo..n`.
const STRIDED: &str = "fn strided(in b: Slice[u64], lo: u64, n: u64) -> u64 {
    let mut acc: u64 = 0;
    for i in lo..n {
        let bi = wrapping_mul(i, 5);
        let w0 = gw(in b, bi);
        let w1 = gw(in b, wrapping_add(bi, 1));
        let w4 = gw(in b, wrapping_add(bi, 4));
        acc = wrapping_add(wrapping_mul(acc, 31), w0 ^ w1 ^ w4);
    }
    acc
}
";

#[test]
fn strided_reads_are_guarded_once() {
    let source = program(&format!(
        "{STRIDED}fn run() -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {{
    let s = words(60);
    let v = s.as_slice();
    // 12 rows of 5 fit; 13 do not, so that call takes the checked loop.
    let a = strided(in v, 0, 12);
    let b = strided(in v, 3, 13);
    let c = strided(in v, 7, 7);
    wrapping_add(wrapping_mul(a, 3), wrapping_add(b, c))
}}"
    ));
    let program = hoisted(&source);
    assert_eq!(
        scaled_guards(&program, "strided"),
        1,
        "one guard covers b[5i], b[5i + 1] and b[5i + 4]"
    );
    assert_eq!(unchecked(&program, "strided"), 3);
    differential("scaled_strided", &source);
}

#[test]
fn strided_writes_fault_at_the_same_iteration() {
    // `set` (checked) of `b[3i + 2]` faults on the row past the end, after
    // the rows before it are written: the caller sees 7777.
    let source = program(
        "fn stamp(inout b: ChunkSeq[u64], n: u64) eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    for i in 0..n {
        let at = wrapping_add(i * 3, 2);
        b.set(at, wrapping_add(b.get(at), i));
        b.set(i * 3, 9);
    }
}
fn run() -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let mut s = words(30);
    stamp(inout s, 10);
    let mut acc: u64 = 0;
    for i in 0..30 {
        acc = wrapping_add(wrapping_mul(acc, 31), match s.try_get(i) {
            Option::Some(x) => x,
            Option::None => 5,
        });
    }
    stamp(inout s, 11);
    acc
}",
    );
    let program = hoisted(&source);
    assert!(scaled_guards(&program, "stamp") >= 1);
    assert_eq!(differential("scaled_set_fault", &source), 7777);
}

#[test]
fn negated_and_shifted_forms_agree() {
    // (m - i) * 4 + 1, (k + i) << 2 and (i - k) * 3 + 6, with both
    // holding and failing guards.
    let source = program(
        "fn forms(in s: Slice[u64], lo: u64, hi: u64, k: u64, m: u64) -> u64 {
    let mut acc: u64 = 0;
    for i in lo..hi {
        let a = gw(in s, wrapping_add(wrapping_mul(wrapping_sub(m, i), 4), 1));
        let b = gw(in s, wrapping_add(k, i) << 2);
        let c = gw(in s, wrapping_add(wrapping_mul(3, wrapping_sub(i, k)), 6));
        acc = wrapping_add(wrapping_mul(acc, 131), wrapping_add(a, wrapping_add(b, c)));
    }
    acc
}
fn run() -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let s = words(100);
    let v = s.as_slice();
    let mut acc: u64 = 0;
    for k in 0..6 {
        acc = wrapping_add(acc, forms(in v, k, wrapping_add(k, 20), 2, 24));
        acc = wrapping_add(acc, forms(in v, 2, 30, k, 30));
    }
    acc
}",
    );
    let program = hoisted(&source);
    assert!(scaled_guards(&program, "forms") >= 3, "one guard per form");
    differential("scaled_forms", &source);
}

/// The vesper-simd scan's inner loop: four `try_read_u64_le` at `(k + t)
/// << 5`, `+ 8`, `+ 16` and `+ 24`.
const SCAN: &str = "fn scan(in data: Slice[u8], k: u64, block: u64) -> u64 {
    let mut acc: u64 = 0;
    for t in 0..block {
        let at = wrapping_add(k, t) << 5;
        let w0 = rd(in data, at);
        let w1 = rd(in data, wrapping_add(at, 8));
        let w2 = rd(in data, wrapping_add(at, 16));
        let w3 = rd(in data, wrapping_add(at, 24));
        acc = wrapping_add(wrapping_mul(acc, 7), (w0 ^ w1) ^ (w2 ^ w3));
    }
    acc
}
";

#[test]
fn wide_reads_are_guarded_over_their_bytes() {
    let source = program(&format!(
        "{SCAN}fn run() -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {{
    let s = bytes(1000);
    let v = s.as_slice();
    let mut acc: u64 = 0;
    // 31 quads of 32 bytes fit in 1000 bytes (992); the 32nd reads bytes
    // 992..1024, so its last words are None: those calls fall back.
    for k in 0..4 {{
        acc = wrapping_add(acc, scan(in v, k, 27));
        acc = wrapping_add(acc, scan(in v, k, 29));
    }}
    // A read straddling the end (bytes 996..1004) and one starting at it.
    acc = wrapping_add(acc, rd(in v, 996));
    acc = wrapping_add(acc, rd(in v, 1000));
    acc
}}"
    ));
    let program = hoisted(&source);
    assert_eq!(
        wide_guards(&program, "scan", 32),
        1,
        "one 32-byte guard covers the four 8-byte reads"
    );
    assert_eq!(unchecked_wide(&program, "scan"), 4);
    differential("scaled_wide_scan", &source);
}

#[test]
fn wide_writes_and_checked_reads_agree() {
    // try_write_u32_be and write_u16_le at 6i + 1 and 6i + 4; a checked
    // read_u64_be at 6i past the end faults after the earlier rows.
    let source = program(
        "fn pack(inout b: ChunkSeq[u8], n: u64) -> u64 eff [Fail[RuntimeError]] {
    let mut stored: u64 = 0;
    for i in 0..n {
        if b.try_write_u32_be(wrapping_add(wrapping_mul(i, 6), 1), wrapping_u32(i)) {
            stored = wrapping_add(stored, 1);
        }
        b.write_u16_le(wrapping_add(wrapping_mul(i, 6), 4), wrapping_u16(wrapping_mul(i, 3)));
    }
    stored
}
fn total(in b: ChunkSeq[u8], n: u64) -> u64 eff [Fail[RuntimeError]] {
    let mut acc: u64 = 0;
    for i in 0..n {
        acc = wrapping_add(wrapping_mul(acc, 3), b.read_u64_be(wrapping_mul(i, 6)));
    }
    acc
}
fn run() -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let mut s = bytes(64);
    let a = pack(inout s, 10);
    let b = total(in s, 9);
    let c = total(in s, 11);
    wrapping_add(a, wrapping_add(b, c))
}",
    );
    let program = hoisted(&source);
    assert_eq!(wide_guards(&program, "pack", 6), 1, "bytes 6i + 1..6i + 6");
    assert!(unchecked_wide(&program, "pack") >= 2);
    assert!(unchecked_wide(&program, "total") >= 1);
    assert_eq!(differential("scaled_wide_writes", &source), 7777);
}

#[test]
fn overflowing_scaled_indexes_fail_their_guard() {
    // (i + k) * 2^31 overflows 64 bits for a large k: the guard must fail
    // (the wrapped index would pass a length check), and the checked loop
    // yields the reference's None at every iteration.
    let source = program(
        "fn big(in s: Slice[u64], k: u64, n: u64) -> u64 {
    let mut acc: u64 = 0;
    for i in 0..n {
        acc = wrapping_add(acc, gw(in s, wrapping_mul(wrapping_add(i, k), 2147483648)));
    }
    acc
}
fn run() -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let s = words(16);
    let v = s.as_slice();
    // 2^33 * 2^31 = 2^64 wraps to 0, which is in bounds.
    let a = big(in v, 8589934592, 8);
    let b = big(in v, 0, 8);
    wrapping_add(a, b)
}",
    );
    let program = hoisted(&source);
    assert_eq!(scaled_guards(&program, "big"), 1);
    differential("scaled_overflow", &source);
}

#[test]
fn nested_versioned_copies_define_fresh_values() {
    // The outer frame loop is versioned (a scaled guard) around an inner
    // loop that is versioned first, as in the eBPF verifier's
    // `state_includes`. The fast copy defines every value under a fresh id,
    // so no value has two definitions: sharing ids let per-value facts of
    // one copy of the inner loop apply to the other.
    let source = program(
        "fn includes(in ows: Slice[u64], in ors: Slice[u64], in crs: Slice[u64]) -> u64 {
    let nf = gw(in ows, 0);
    let mut oo: u64 = 0;
    let mut acc: u64 = 0;
    for f in 0..nf {
        let m = wrapping_add(8, wrapping_mul(f, 17));
        acc = wrapping_add(acc, gw(in ows, wrapping_add(m, 1)));
        for i in 0..11 {
            if gw(in ors, wrapping_add(wrapping_add(oo, 1), i)) > gw(in crs, wrapping_add(wrapping_add(oo, 1), i)) {
                return 5000;
            }
        }
        oo = wrapping_add(oo, 12);
    }
    acc
}
fn run() -> u64 eff [Fail[RuntimeError], Fail[ArithOverflow]] {
    let mut ws = words(60);
    let ok = ws.try_set(0, 3);
    let lo = words(40);
    let hi = words(40);
    let short = words(20);
    let a = includes(in ws.as_slice(), in lo.as_slice(), in hi.as_slice());
    let b = includes(in ws.as_slice(), in short.as_slice(), in hi.as_slice());
    let c = includes(in short.as_slice(), in lo.as_slice(), in lo.as_slice());
    wrapping_add(a, wrapping_add(b, c))
}",
    );
    let program = hoisted(&source);
    let f = &program.functions["includes"];
    assert_eq!(scaled_guards(&program, "includes"), 1);
    let mut defined = std::collections::BTreeSet::new();
    for block in &f.blocks {
        for v in block
            .params
            .iter()
            .chain(block.instructions.iter().map(|i| &i.dest))
        {
            assert!(defined.insert(*v), "value {v} is defined twice");
        }
    }
    differential("scaled_nested_copies", &source);
}
