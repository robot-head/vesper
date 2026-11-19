//! `find_not_in` on targets without a byte shuffle (P3b gaps lane 11,
//! #1096). The x86-64 baseline has no SSSE3, so optimized code there tests
//! one byte at a time but skips runs of a repeated class byte 16 bytes per
//! SSE2 compare. On x86-64 Linux the executable leg is built for the
//! baseline triple (as the port harness builds ports), so it takes that
//! path; the JIT (host features) and the evaluator are the references. Inputs are runs of class bytes
//! broken at every position, by an outside byte or by another class byte.
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{check, native, parser, runtime::Runtime};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-native-find-not-in-{}-{}",
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
    let shown: Vec<_> = c
        .diagnostics
        .iter()
        .map(|d| {
            format!(
                "{}: {} at `{}`",
                d.code,
                d.message,
                &source[d.span.start..d.span.end]
            )
        })
        .collect();
    assert!(c.diagnostics.is_empty(), "{shown:#?}");
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
    // Without `--target` an executable uses the host's ISA features; the
    // explicit baseline triple (as the port harness passes) has no SSSE3.
    let mut build = Command::new(env!("CARGO_BIN_EXE_vesper"));
    build.arg("build").arg(&file).arg("-o").arg(&exe);
    if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        build.args(["--target", "x86_64-unknown-linux-gnu"]);
    }
    let built = build.output().unwrap();
    assert!(
        built.status.success(),
        "{name}: vesper build failed\n{}{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    Command::new(&exe).output().unwrap().status.code()
}

/// Runs `main` in the evaluator, both JIT configurations and (where a C
/// driver links executables) an AOT executable, and checks every result
/// against `expected`.
fn differential(name: &str, source: &str, expected: u64) {
    let c = accepted(source);
    let evaluated = Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap_or_else(|e| panic!("{name}: evaluator failed: {e}"))
        .to_string();
    assert_eq!(
        evaluated,
        expected.to_string(),
        "{name}: evaluator vs model"
    );
    for optimize in [true, false] {
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap_or_else(|e| panic!("{name}: JIT (optimize {optimize}) failed: {e}"))
            .to_string();
        assert_eq!(jit, evaluated, "{name}: JIT (optimize {optimize})");
    }
    if links_executables() {
        assert_eq!(
            executable_status(name, source),
            Some((expected & 0xff) as i32),
            "{name}: executable"
        );
    }
}

const PROGRAM: &str = "fn mix(acc: u64, v: u64) -> u64 {
    wrapping_add(wrapping_mul(acc, 1099511628211), v)
}
fn at(in s: Slice[u8], lo: u64, hi: u64, high: bool) -> u64 {
    match s.find_not_in(lo, hi, high) {
        Option::Some(i) => wrapping_add(i, 1),
        Option::None => 0,
    }
}
fn run() -> u64 eff [Fail[RuntimeError]] {
    let mut acc: u64 = 1;
    let lens: u64 = 70;
    for n in 0..lens {
        for brk in 0..wrapping_add(n, 1) {
            for kind in 0..wrapping_add(4, 0) {
                let mut s: ChunkSeq[u8] = ChunkSeq::new[u8]();
                s.resize(n, 0);
                // kind 0: an outside byte; 1: another class byte (32, a
                // space) then zeros again; 2: a run of 200s (high bytes);
                // 3: varied bytes from the break on.
                if brk < n {
                    let b: u8 = if kind == 0 { 7 } else if kind == 1 { 32 } else { 200 };
                    s.fill(brk, wrapping_add(brk, 1), b);
                    if kind == 2 {
                        s.fill(brk, n, 200);
                    }
                    if kind == 3 {
                        for i in brk..n {
                            s.try_set(i, wrapping_u8(wrapping_add(wrapping_mul(i, 37), brk)));
                        }
                    }
                }
                acc = mix(acc, at(in s.as_slice(), 4294967297, 0, false));
                acc = mix(acc, at(in s.as_slice(), 4294967297, 0, true));
                acc = mix(acc, at(in s[0..n], 1, 0, true));
                acc = mix(acc, at(in s.as_slice(), 287948901175001089, 576460743847706622, false));
            }
        }
    }
    acc
}
fn main() -> u64 {
    handle Fail[RuntimeError] in {
        run()
    } with {
        return x => x,
        raise(e, k) => 7777,
    }
}
";

fn mix(acc: u64, v: u64) -> u64 {
    acc.wrapping_mul(1099511628211).wrapping_add(v)
}

fn member(lo: u64, hi: u64, high: bool, x: u8) -> bool {
    match x {
        0..=63 => lo >> x & 1 == 1,
        64..=127 => hi >> (x - 64) & 1 == 1,
        _ => high,
    }
}

fn model_at(s: &[u8], lo: u64, hi: u64, high: bool) -> u64 {
    s.iter()
        .position(|&x| !member(lo, hi, high, x))
        .map_or(0, |i| i as u64 + 1)
}

#[test]
fn runs_broken_at_every_position() {
    let mut acc = 1u64;
    for n in 0..70usize {
        for brk in 0..=n {
            for kind in 0..4 {
                let mut s = vec![0u8; n];
                if brk < n {
                    match kind {
                        0 => s[brk] = 7,
                        1 => s[brk] = 32,
                        2 => s[brk..].fill(200),
                        _ => {
                            for (i, b) in s.iter_mut().enumerate().skip(brk) {
                                *b = (i * 37 + brk) as u8;
                            }
                        }
                    }
                }
                acc = mix(acc, model_at(&s, 4294967297, 0, false));
                acc = mix(acc, model_at(&s, 4294967297, 0, true));
                acc = mix(acc, model_at(&s, 1, 0, true));
                acc = mix(
                    acc,
                    model_at(&s, 287948901175001089, 576460743847706622, false),
                );
            }
        }
    }
    differential("find_not_in_runs", PROGRAM, acc);
}

/// The x86-64 baseline object runs `find_not_in` with SSE2 compares and no
/// shuffle; an unoptimized object has no vectors.
#[test]
fn baseline_object_skips_runs_with_vectors() {
    let class = "fn main() -> u64 {
    let n: u64 = 100;
    let mut s: ChunkSeq[u8] = ChunkSeq::new[u8]();
    s.resize(n, 0);
    match s.find_not_in(1, 0, false) {
        Option::Some(i) => i,
        Option::None => 0,
    }
}
";
    let c = accepted(class);
    let target = "x86_64-unknown-linux-gnu";
    let (_, fast) = native::object_with_options(&c.world, "main", Some(target), true)
        .unwrap_or_else(|e| panic!("{target}: optimized object failed: {e}"));
    assert!(
        fast.contains("i8x16") && fast.contains("vhigh_bits"),
        "{fast}"
    );
    assert!(!fast.contains("swizzle"), "{fast}");
    let (_, slow) = native::object_with_options(&c.world, "main", Some(target), false)
        .unwrap_or_else(|e| panic!("{target}: object failed: {e}"));
    assert!(!slow.contains("i8x16"), "{slow}");
}
