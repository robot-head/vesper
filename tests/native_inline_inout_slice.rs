//! P3b gaps lane 4 (#1089): helpers that take an `inout` slice inline.
//!
//! A slice parameter is `in` or `inout` (E1520); both pass the view by
//! address and neither can re-point it, so an inlined `inout` helper writes
//! through the caller's view (its own slice local or parameter, or a bound
//! copy of a sub-range or `as_slice_mut()` view) into the same buffer.
//!
//! Every program runs in the reference evaluator (optimized and not), the
//! JIT with `optimize` on and off, object emission for x86-64 and AArch64,
//! and (where a C driver links executables) an AOT executable built with
//! `vesper build`. IR-level checks name the calls the inliner replaced.
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{
    check,
    ir_inline::{self, Budget},
    native, parser,
    runtime::Runtime,
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-native-inline-inout-slice-{}-{}",
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

fn world(source: &str) -> check::World {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{:#?}", c.diagnostics);
    c.world
}

fn evaluate(world: &check::World, optimize: bool) -> String {
    let mut runtime = Runtime::new(world);
    runtime.optimize = optimize;
    runtime.run("main", vec![]).unwrap().to_string()
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

/// Every backend computes `expected` for `main`.
fn agree(name: &str, source: &str, expected: &str) {
    let w = world(source);
    assert_eq!(evaluate(&w, true), expected, "{name}: evaluator");
    assert_eq!(
        evaluate(&w, false),
        expected,
        "{name}: unoptimized evaluator"
    );
    for optimize in [true, false] {
        assert_eq!(
            native::jit_with_options(&w, "main", optimize)
                .unwrap_or_else(|e| panic!("{name}: JIT optimize={optimize}: {e}"))
                .to_string(),
            expected,
            "{name}: JIT optimize={optimize}"
        );
    }
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        for optimize in [true, false] {
            let (obj, _) = native::object_with_options(&w, "main", Some(target), optimize)
                .unwrap_or_else(|e| panic!("{name}: {target} optimize={optimize}: {e}"));
            assert_eq!(&obj[..4], b"\x7fELF");
        }
    }
    if links_executables() {
        let value: u64 = expected.parse().unwrap();
        assert_eq!(
            executable_status(name, source),
            Some((value & 0xff) as i32),
            "{name}: executable"
        );
    }
}

/// Lower, optimize and inline `source` with the default budget, check the
/// token evidence, and return the report and program.
fn inlined(source: &str) -> (ir_inline::Report, vesper::ir::Program) {
    let w = world(source);
    let mut program = vesper::ir::lower_world(&w);
    vesper::ir_opt::optimize(&w, &mut program).unwrap();
    let report = ir_inline::inline(&w, &mut program, &Budget::default());
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

/// `bytes(n)`: `n` bytes `i * 37 + 11`; `digest` folds a byte slice.
const PRELUDE: &str = "fn bytes(n: u64) -> ChunkSeq[u8] {
    let mut s: ChunkSeq[u8] = ChunkSeq::new[u8]();
    for i in 0..n {
        s.push(wrapping_u8(wrapping_add(wrapping_mul(i, 37), 11)));
    }
    s
}
fn digest(in s: Slice[u8]) -> u64 {
    let mut acc: u64 = 0;
    for i in 0..s.len() {
        match s.try_get(i) {
            Option::Some(b) => acc = wrapping_add(wrapping_mul(acc, 31), widen_u64(b)),
            Option::None => (),
        }
    }
    acc
}
";

fn program(body: &str) -> String {
    format!("{PRELUDE}{body}")
}

/// The Rust model of `bytes` and `digest`.
fn model_bytes(n: u64) -> Vec<u8> {
    (0..n).map(|i| (i * 37 + 11) as u8).collect()
}
fn model_digest(s: &[u8]) -> u64 {
    s.iter()
        .fold(0u64, |a, b| a.wrapping_mul(31).wrapping_add(u64::from(*b)))
}

/// The Warp repro (port-p3b-warp.md, gap 1): `put3` forwards its own
/// `inout` slice parameter to `w8` three times.
const PUT3: &str = "fn w8(inout out: Slice[u8], at: u64, v: u8) -> u64 {
    out.try_set(at, v);
    wrapping_add(at, 1)
}
fn put3(inout out: Slice[u8]) -> u64 {
    let a = w8(inout out, 0, 1);
    let b = w8(inout out, a, 2);
    w8(inout out, b, 3)
}
fn main() -> u64 {
    let mut s: ChunkSeq[u8] = ChunkSeq::new[u8]();
    let five: u64 = 5;
    for i in 0..five {
        s.push(wrapping_u8(wrapping_add(i, 9)));
    }
    let n = put3(inout s.as_slice_mut());
    let mut acc: u64 = n;
    for i in 0..s.len() {
        match s.try_get(i) {
            Option::Some(b) => acc = wrapping_add(wrapping_mul(acc, 10), widen_u64(b)),
            Option::None => (),
        }
    }
    acc
}";

#[test]
fn put3_repro_inlines() {
    // s = [1, 2, 3, 12, 13], n = 3.
    agree("put3", PUT3, "312433");
    let (report, program) = inlined(PUT3);
    assert_eq!(calls(&program, "put3", "w8"), 0, "{report:?}");
    assert!(!report.refused.contains_key("w8"), "{report:?}");
    assert_eq!(calls(&program, "main", "put3"), 0, "{report:?}");
}

/// The optimized executable object calls neither helper from `main`.
#[test]
fn put3_object_has_no_helper_calls() {
    let w = world(PUT3);
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let mut emitted = Vec::new();
        native::object_with_observer(
            &w,
            "main",
            Some(target),
            true,
            Some(&mut |f| emitted.push(f)),
        )
        .unwrap();
        for caller in ["main", "put3"] {
            let f = emitted.iter().find(|f| f.key == caller).unwrap();
            let helper = f.callees.iter().find(|(id, _)| {
                emitted
                    .iter()
                    .any(|g| g.func_id == *id && matches!(g.key.as_str(), "w8" | "put3"))
            });
            assert!(helper.is_none(), "{target}: {caller} calls {helper:?}");
        }
    }
}

/// Sub-range and `as_slice_mut()` arguments bind a copy of the view, a
/// whole caller slice local is used in place, and writes land in the
/// caller's buffer in every case. `put_u16` is inlined into `fill_hdr`
/// first, and `fill_hdr` into `run`.
#[test]
fn writes_reach_the_callers_buffer() {
    let source = program(
        "fn put_u16(inout out: Slice[u8], at: u64, v: u64) -> u64 {
    out.try_set(at, wrapping_u8(v >> 8));
    out.try_set(wrapping_add(at, 1), wrapping_u8(v));
    wrapping_add(at, 2)
}
fn fill_hdr(inout out: Slice[u8], a: u64, b: u64) -> u64 {
    let at = put_u16(inout out, 0, a);
    put_u16(inout out, at, b)
}
fn run() -> u64 {
    let mut s = bytes(16);
    let n1 = fill_hdr(inout s[3..9], 0x1234, 0xabcd);
    let n2 = fill_hdr(inout s.as_slice_mut(), 0x0102, 0x0304);
    let n3 = put_u16(inout s[14..16], 1, 0xffff);
    let n4 = fill_hdr(inout s[15..16], 0x5555, 0x6666);
    wrapping_add(wrapping_mul(digest(in s.as_slice()), 1000), wrapping_add(wrapping_add(n1, n2), wrapping_add(n3, n4)))
}
fn main() -> u64 { run() }",
    );
    let mut s = model_bytes(16);
    s[3..7].copy_from_slice(&[0x12, 0x34, 0xab, 0xcd]);
    s[0..4].copy_from_slice(&[0x01, 0x02, 0x03, 0x04]);
    // put_u16 at 1 of s[14..16]: only out[1] = 0xff fits.
    s[15] = 0xff;
    // fill_hdr on s[15..16]: out[0] = 0x55, the rest is out of range.
    s[15] = 0x55;
    let expected = model_digest(&s)
        .wrapping_mul(1000)
        .wrapping_add(4 + 4 + 3 + 4);
    agree("writes", &source, &expected.to_string());
    let (report, program) = inlined(&source);
    assert_eq!(calls(&program, "fill_hdr", "put_u16"), 0, "{report:?}");
    assert_eq!(calls(&program, "run", "fill_hdr"), 0, "{report:?}");
    assert_eq!(calls(&program, "run", "put_u16"), 0, "{report:?}");
}

/// A helper with a loop and a `Fail` row (`set` faults out of range), and
/// a word slice: the inlined raise performs the caller's `Fail`.
#[test]
fn looping_and_failing_writers() {
    let source = "fn ramp(inout out: Slice[u64], k: u64) eff [Fail[RuntimeError]] {
    for i in 0..out.len() {
        out.set(i, wrapping_mul(wrapping_add(i, 1), k));
    }
}
fn poke(inout out: Slice[u64], at: u64) eff [Fail[RuntimeError]] {
    out.set(at, 7);
}
fn fill(inout w: ChunkSeq[u64], at: u64) -> u64 eff [Fail[RuntimeError]] {
    ramp(inout w.as_slice_mut(), 3);
    ramp(inout w[1..3], 100);
    poke(inout w[2..4], at);
    let mut acc: u64 = 0;
    for i in 0..w.len() {
        acc = wrapping_add(wrapping_mul(acc, 1000), w[i]);
    }
    acc
}
fn run(at: u64) -> u64 {
    let mut w: ChunkSeq[u64] = ChunkSeq::new[u64]();
    let four: u64 = 4;
    for i in 0..four {
        w.push(i);
    }
    handle Fail[RuntimeError] in { fill(inout w, at) } with { return v => v, raise(e, k) => 5, }
}
fn main() -> u64 { wrapping_add(wrapping_mul(run(1), 10), run(2)) }";
    // run(1): [3, 100, 200, 12] then w[3] = 7 -> 3_100_200_007; run(2): 5.
    let expected = 3_100_200_007u64.wrapping_mul(10).wrapping_add(5);
    agree("ramp", source, &expected.to_string());
    let (report, program) = inlined(source);
    assert_eq!(calls(&program, "fill", "ramp"), 0, "{report:?}");
    assert_eq!(calls(&program, "fill", "poke"), 0, "{report:?}");
}
