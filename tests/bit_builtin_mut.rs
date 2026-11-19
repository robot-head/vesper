//! Bit builtins on reassigned `let mut` locals (P3b gaps lane 10, #1095,
//! part 1). Native lowering turned each builtin call into an operator but
//! left its operands as `in` reads, whose shared loans an operator never
//! releases, so a later write of the same local failed optimized IR
//! validation with E0410 (`let mut x = v; x = wrapping_shl_u32(x, 16)`).
//! Every builtin of every kind now runs on mutable locals, straight-line and
//! in loops, and the evaluator (optimized and not), the JIT (optimized and
//! not), objects, an executable and a `--lib` build agree with the Rust
//! model in `vesper::integer_widths::BitOp::eval`.
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{
    check,
    integer_widths::BitOp,
    native,
    numeric::{INTEGER_KINDS, IntegerKind},
    parser,
    runtime::Runtime,
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-bit-builtin-mut-{}-{}",
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

fn checked(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{source}\n{:?}", p.diagnostics);
    check::check(&p.module)
}

fn accepted(source: &str) -> check::Checked {
    let c = checked(source);
    let shown: Vec<_> = c
        .diagnostics
        .iter()
        .map(|d| format!("{}: {} at `{}`", d.code, d.message, &source[d.span.clone()]))
        .collect();
    assert!(c.diagnostics.is_empty(), "{source}\n{shown:#?}");
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

/// Runs `main` everywhere and checks each result against `expected`.
fn differential(name: &str, source: &str, expected: u64) {
    let c = accepted(source);
    for optimize in [true, false] {
        let mut runtime = Runtime::new(&c.world);
        runtime.optimize = optimize;
        let value = runtime
            .run("main", vec![])
            .unwrap_or_else(|e| panic!("{name}: evaluator (optimize {optimize}) failed: {e}"))
            .to_string();
        assert_eq!(
            value,
            expected.to_string(),
            "{name}: evaluator (optimize {optimize})"
        );
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap_or_else(|e| panic!("{name}: JIT (optimize {optimize}) failed: {e}"))
            .to_string();
        assert_eq!(
            jit,
            expected.to_string(),
            "{name}: JIT (optimize {optimize})"
        );
    }
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        native::object_for_target(&c.world, "main", Some(target))
            .unwrap_or_else(|e| panic!("{name}: {target} object failed: {e}"));
    }
    if links_executables() {
        assert_eq!(
            executable_status(name, source),
            Some((expected & 0xff) as i32),
            "{name}: executable"
        );
    }
}

const OFFSET: u64 = 14695981039346656037;
fn mix(acc: u64, v: u64) -> u64 {
    acc.wrapping_mul(1099511628211).wrapping_add(v)
}
fn lcg(s: u64) -> u64 {
    s.wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407)
}
/// The value of `v` (of kind `k`) as `wrapping_u64(v)`.
fn as_u64(v: i128) -> u64 {
    v as u64
}

/// Edge operands of `k`, as bit patterns: 0, 1, the top bit alone, all
/// ones, MAX, alternating bits and single bits in the middle.
fn edges(k: IntegerKind) -> Vec<u64> {
    let bits = k.bits();
    let mask = if bits == 64 {
        u64::MAX
    } else {
        (1 << bits) - 1
    };
    let mut v = vec![
        0,
        1,
        2,
        3,
        1 << (bits - 1),
        (1 << (bits - 1)) | 1,
        mask,
        mask >> 1,
        mask - 1,
        0x5555_5555_5555_5555 & mask,
        0xaaaa_aaaa_aaaa_aaaa & mask,
        1 << (bits / 2),
        (1 << (bits / 2)) - 1,
    ];
    v.dedup();
    v
}
/// Edge shift amounts: in range, the width, past it and the largest `u32`.
fn edge_amounts(k: IntegerKind) -> Vec<u32> {
    let bits = k.bits();
    vec![
        0,
        1,
        bits - 1,
        bits,
        bits + 1,
        2 * bits - 1,
        2 * bits + 3,
        100,
        u32::MAX,
    ]
}

/// The kind's value with bit pattern `p` (low `bits` bits).
fn from_pattern(k: IntegerKind, p: u64) -> i128 {
    k.wrap(i128::from(p))
}

fn library(name: &str, source: &str) {
    let temp = Temp::new();
    let file = temp.0.join(format!("{name}.ves"));
    fs::write(&file, source).unwrap();
    let object = temp.0.join(format!("{name}.o"));
    let built = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg("build")
        .arg("--lib")
        .arg(&file)
        .arg("-o")
        .arg(&object)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{name}: vesper build --lib failed\n{}{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
}

/// The Rust model of `step` in [`program`].
fn model_step(k: IntegerKind, acc: u64, v: i128, n: u32) -> u64 {
    let mut x = v;
    let mut a = mix(acc, as_u64(BitOp::LeadingZeros.eval(k, x, 0)));
    x = BitOp::WrappingShl.eval(k, x, n);
    a = mix(a, as_u64(BitOp::TrailingZeros.eval(k, x, 0)));
    x = BitOp::WrappingShr.eval(k, x, n >> 1);
    a = mix(a, as_u64(BitOp::CountOnes.eval(k, x, 0)));
    for _ in 0..3 {
        x = k.wrap(BitOp::WrappingShl.eval(k, x, 1) ^ v);
        x = BitOp::WrappingShr.eval(k, x, n);
    }
    let mut c = BitOp::CountOnes.eval(IntegerKind::U32, i128::from(n), 0);
    c = BitOp::WrappingShl.eval(IntegerKind::U32, c, 3);
    c = BitOp::LeadingZeros.eval(IntegerKind::U32, c, 0);
    mix(mix(a, as_u64(x)), as_u64(c))
}

const RANDOM: u64 = 200;

fn program(k: IntegerKind) -> (String, u64) {
    let kn = k.name();
    let mut calls = String::new();
    let mut acc = OFFSET;
    for p in edges(k) {
        for n in edge_amounts(k) {
            calls += &format!("    acc = step(acc, wrapping_{kn}({p}), {n});\n");
            acc = model_step(k, acc, from_pattern(k, p), n);
        }
    }
    let mut s = 1u64;
    for _ in 0..RANDOM {
        s = lcg(s);
        let x = from_pattern(k, (s >> 7) >> ((s >> 1) & 63));
        let n = (s >> 57) as u32;
        acc = model_step(k, acc, x, n);
    }
    let source = format!(
        "fn mix(acc: u64, v: u64) -> u64 {{ wrapping_add(wrapping_mul(acc, 1099511628211), v) }}
pub fn step(acc: u64, v: {kn}, n: u32) -> u64 {{
    let mut x = v;
    let mut a = mix(acc, widen_u64(leading_zeros_{kn}(x)));
    x = wrapping_shl_{kn}(x, n);
    a = mix(a, widen_u64(trailing_zeros_{kn}(x)));
    x = wrapping_shr_{kn}(x, n >> 1);
    a = mix(a, widen_u64(count_ones_{kn}(x)));
    for i in 0..3 {{
        x = wrapping_shl_{kn}(x, 1) ^ v;
        x = wrapping_shr_{kn}(x, n);
    }}
    let mut c = count_ones_u32(n);
    c = wrapping_shl_u32(c, 3);
    c = leading_zeros_u32(c);
    mix(mix(a, wrapping_u64(x)), widen_u64(c))
}}
fn main() -> u64 {{
    let mut acc: u64 = {OFFSET};
{calls}    let mut s: u64 = 1;
    for i in 0..{RANDOM} {{
        s = wrapping_add(wrapping_mul(s, 6364136223846793005), 1442695040888963407);
        let mut x = wrapping_{kn}(wrapping_shr_u64(s >> 7, wrapping_u32(s >> 1)));
        let mut n = wrapping_u32(s >> 57);
        acc = step(acc, x, n);
        x = wrapping_shl_{kn}(x, n);
        n = count_ones_u32(n);
    }}
    acc
}}
"
    );
    (source, acc)
}

#[test]
fn every_builtin_on_a_mutable_local_agrees_in_every_backend() {
    for k in INTEGER_KINDS {
        let (source, expected) = program(k);
        let name = format!("bits_mut_{}", k.name());
        differential(&name, &source, expected);
        library(&name, &source);
    }
}

/// The two repros from the Kinetic and Prism H3 port reports.
#[test]
fn the_port_repros_build_as_libraries() {
    let source = "pub fn f(v: u32) -> u32 { let mut x = v; x = wrapping_shl_u32(x, 16); x }
pub fn g(n: u64) -> u64 {
    let mut acc: u64 = 1;
    for i in 0..n { acc = wrapping_shl_u64(acc, 5); acc = acc ^ i; }
    acc
}
fn main() -> u64 { wrapping_add(widen_u64(f(3)), g(7)) }
";
    library("repros", source);
    let mut acc: u64 = 1;
    for i in 0..7 {
        acc = (acc << 5) ^ i;
    }
    differential("repros", source, (3u64 << 16).wrapping_add(acc));
}
