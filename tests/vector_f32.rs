//! `f32` vector lanes (P3b gaps lane 1, #1086): `Vec[f32, N]` and
//! `Mask[f32, N]`, the float lane operations `sqrt`, `abs`, `neg` and
//! `mul_add`, and the lane conversions `to_bits`, `from_bits`, `widen_f64`
//! and `nearest_f32`. Every program runs in the evaluator and the JIT
//! (optimize on and off), builds x86_64 and AArch64 objects and, where a C
//! driver links executables, runs as an executable; the result is also
//! checked against a Rust model of IEEE binary32 lanes.
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
            "vesper-vector-f32-{}-{}",
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
fn rejected(source: &str, code: &str, text: &str) {
    let c = checked(source);
    assert!(
        c.diagnostics
            .iter()
            .any(|d| d.code == code && d.message.contains(text)),
        "expected {code} containing {text:?}, got {:?}",
        c.diagnostics
    );
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
const CANONICAL_NAN: u32 = 0x7fc0_0000;
fn canon(x: f32) -> u32 {
    if x.is_nan() {
        CANONICAL_NAN
    } else {
        x.to_bits()
    }
}
/// Cranelift's `fmin`/`fmax`: NaN when either lane is NaN, -0.0 < +0.0.
fn fmin(a: f32, b: f32, min: bool) -> f32 {
    if a.is_nan() || b.is_nan() {
        return f32::NAN;
    }
    if a == 0.0 && b == 0.0 {
        let negative = if min {
            a.is_sign_negative() || b.is_sign_negative()
        } else {
            a.is_sign_negative() && b.is_sign_negative()
        };
        return if negative { -0.0 } else { 0.0 };
    }
    if (a < b) == min { a } else { b }
}

// ---------------------------------------------------------------- checker

#[test]
fn f32_lanes_are_vector_shapes() {
    let isa = "eff [Isa[Baseline128]]";
    accepted(&format!(
        "fn f(a: Vec[f32, 4], b: Vec[f32, 4]) -> f32 {isa} {{ Vec::reduce_add(Vec::div(a, b)) }}"
    ));
    accepted("fn f(a: Vec[f32, 8]) -> f32 eff [Isa[Avx2]] { Vec::reduce_max(Vec::sqrt(a)) }");
    accepted("fn f(a: Mask[f32, 16]) -> u64 eff [Isa[Avx512]] { Mask::bitmask(a) }");
    rejected(
        "fn f(a: Vec[f32, 16]) -> f32 eff [Isa[Avx2]] { Vec::reduce_add(a) }",
        "E1403",
        "requires 'eff [Isa[Avx512]]'",
    );
    rejected(
        "fn f(a: Vec[f32, 2]) -> u64 { 0 }",
        "E1401",
        "not a 128-, 256- or 512-bit",
    );
    rejected(
        &format!("fn f(a: Vec[f32, 4]) -> Vec[f32, 4] {isa} {{ Vec::xor(a, a) }}"),
        "E1402",
        "not defined for float lanes",
    );
    rejected(
        &format!("fn f(a: Vec[u32, 4]) -> Vec[u32, 4] {isa} {{ Vec::sqrt(a) }}"),
        "E1402",
        "f32 and f64 lanes only",
    );
    rejected(
        &format!("fn f(a: Vec[u32, 4]) -> Vec[u32, 4] {isa} {{ Vec::mul_add(a, a, a) }}"),
        "E1402",
        "f32 and f64 lanes only",
    );
    rejected(
        &format!("fn f(a: Vec[u32, 4]) -> Vec[u32, 4] {isa} {{ Vec::to_bits(a) }}"),
        "E1402",
        "takes f32 or f64 lanes",
    );
    rejected(
        &format!("fn f(a: Vec[i64, 2]) -> Vec[f64, 2] {isa} {{ Vec::from_bits(a) }}"),
        "E1402",
        "takes u32 or u64 lanes",
    );
    // A conversion's result keeps the lane count: 16 f64 lanes are 1024 bits.
    rejected(
        "fn f(a: Vec[f32, 16]) -> u64 eff [Isa[Avx512]] { let w = Vec::widen_f64(a); 0 }",
        "E1401",
        "would be a 1024-bit vector",
    );
    // Widening 4 lanes makes a 256-bit vector, which Baseline128 lacks.
    rejected(
        &format!("fn f(a: Vec[f32, 4]) -> Vec[f64, 4] {isa} {{ Vec::widen_f64(a) }}"),
        "E1403",
        "requires 'eff [Isa[Avx2]]'",
    );
    accepted("fn f(a: Vec[f32, 4]) -> Vec[f64, 4] eff [Isa[Avx2]] { Vec::widen_f64(a) }");
    accepted(&format!(
        "fn f(a: Vec[f32, 4]) -> Vec[u32, 4] {isa} {{ Vec::to_bits(Vec::neg(a)) }}"
    ));
    rejected(
        &format!("fn f(a: Vec[f32, 4]) -> Vec[f64, 2] {isa} {{ Vec::to_bits(a) }}"),
        "E0200",
        "expected Vec[f64, 2], found Vec[u32, 4]",
    );
}

// --------------------------------------------------------------- programs

/// Edge lane bit patterns: zeros, ones, the smallest subnormal and normal,
/// the largest finite, infinities, NaN, and values whose product is exact
/// only when fused.
const EDGES: [u32; 16] = [
    0x0000_0000,
    0x8000_0000,
    0x3f80_0000,
    0xbf80_0000,
    0x0000_0001,
    0x0080_0000,
    0x7f7f_ffff,
    0x7f80_0000,
    0xff80_0000,
    0x7fc0_0000,
    0x3f80_0800, // 1 + 2^-12
    0xbf80_1000, // -(1 + 2^-11)
    0x4049_0fdb, // pi
    0x3eaa_aaab, // 1/3
    0x5f00_0000, // 2^63
    0x2000_0000,
];

const PROGRAM: &str = r#"
fn u(x: u64) -> u64 { x }
fn mix(acc: u64, v: u64) -> u64 { wrapping_add(wrapping_mul(acc, 1099511628211), v) }
fn canon(v: Vec[f32, 4]) -> Vec[f32, 4] eff [Isa[Baseline128]] {
    Vec::select(Vec::ne(v, v), Vec::splat[f32, 4](from_bits_f32(2143289344)), v)
}
fn scalar(acc: u64, x: f32) -> u64 {
    if x != x { mix(acc, 2143289344) } else { mix(acc, widen_u64(to_bits_f32(x))) }
}
fn digest(acc: u64, v: Vec[f32, 4]) -> u64 eff [Isa[Baseline128]] {
    let b = Vec::to_bits(canon(v));
    let a1 = mix(acc, widen_u64(Vec::extract(b, 0)));
    let a2 = mix(a1, widen_u64(Vec::extract(b, 1)));
    let a3 = mix(a2, widen_u64(Vec::extract(b, 2)));
    mix(a3, widen_u64(Vec::extract(b, 3)))
}
fn vf(a: u32, b: u32, c: u32, d: u32) -> Vec[f32, 4] eff [Isa[Baseline128]] {
    Vec::from_bits(Vec::from[u32, 4](a, b, c, d))
}
fn ops(acc: u64, x: Vec[f32, 4], y: Vec[f32, 4], z: Vec[f32, 4]) -> u64 eff [Isa[Baseline128]] {
    let mut a = acc;
    a = digest(a, Vec::add(x, y));
    a = digest(a, Vec::sub(x, y));
    a = digest(a, Vec::mul(x, y));
    a = digest(a, Vec::div(x, y));
    a = digest(a, Vec::min(x, y));
    a = digest(a, Vec::max(x, y));
    a = digest(a, Vec::sqrt(x));
    a = digest(a, Vec::abs(x));
    a = digest(a, Vec::neg(x));
    a = digest(a, Vec::mul_add(x, y, z));
    a = mix(a, Mask::bitmask(Vec::eq(x, y)));
    a = mix(a, Mask::bitmask(Vec::ne(x, y)));
    a = mix(a, Mask::bitmask(Vec::lt(x, y)));
    a = mix(a, Mask::bitmask(Vec::le(x, y)));
    a = mix(a, Mask::bitmask(Vec::gt(x, y)));
    a = mix(a, Mask::bitmask(Vec::ge(x, y)));
    a = digest(a, Vec::select(Vec::lt(x, y), x, z));
    a = digest(a, Vec::shuffle(x, 3, 1, 2, 0));
    a = digest(a, Vec::replace(x, 2, Vec::extract(y, 1)));
    a = digest(a, Vec::splat[f32, 4](Vec::extract(z, 3)));
    a = scalar(a, Vec::reduce_add(x));
    a = scalar(a, Vec::reduce_min(y));
    scalar(a, Vec::reduce_max(z))
}
fn run() -> u64 eff [Isa[Baseline128]] {
    let mut acc: u64 = 14695981039346656037;
EDGE_CALLS
    let mut s: u64 = 7;
    for i in u(0)..u(64) {
        s = wrapping_add(wrapping_mul(s, 6364136223846793005), 1442695040888963407);
        let p = wrapping_u32(s >> 32);
        let q = wrapping_u32(s);
        s = wrapping_add(wrapping_mul(s, 6364136223846793005), 1442695040888963407);
        let r = wrapping_u32(s >> 32);
        let t = wrapping_u32(s);
        acc = ops(acc, vf(p, q, r, t), vf(t, p, q, r), vf(r, t, p, q));
    }
    acc
}

fn main() -> u64 { if Isa::supported[Baseline128]() { run() } else { 0 } }
"#;

/// The Rust model of `digest`.
fn digest(acc: u64, v: [f32; 4]) -> u64 {
    v.into_iter().fold(acc, |a, x| mix(a, u64::from(canon(x))))
}
fn scalar(acc: u64, x: f32) -> u64 {
    mix(acc, u64::from(canon(x)))
}
fn lanes(f: impl Fn(usize) -> f32) -> [f32; 4] {
    [f(0), f(1), f(2), f(3)]
}
fn bitmask(f: impl Fn(usize) -> bool) -> u64 {
    (0..4).fold(0, |m, i| m | (u64::from(f(i)) << i))
}
/// The Rust model of `ops`.
fn ops(acc: u64, x: [f32; 4], y: [f32; 4], z: [f32; 4]) -> u64 {
    let mut a = acc;
    a = digest(a, lanes(|i| x[i] + y[i]));
    a = digest(a, lanes(|i| x[i] - y[i]));
    a = digest(a, lanes(|i| x[i] * y[i]));
    a = digest(a, lanes(|i| x[i] / y[i]));
    a = digest(a, lanes(|i| fmin(x[i], y[i], true)));
    a = digest(a, lanes(|i| fmin(x[i], y[i], false)));
    a = digest(a, lanes(|i| x[i].sqrt()));
    a = digest(a, lanes(|i| x[i].abs()));
    a = digest(a, lanes(|i| -x[i]));
    a = digest(a, lanes(|i| x[i].mul_add(y[i], z[i])));
    a = mix(a, bitmask(|i| x[i] == y[i]));
    a = mix(a, bitmask(|i| x[i] != y[i]));
    a = mix(a, bitmask(|i| x[i] < y[i]));
    a = mix(a, bitmask(|i| x[i] <= y[i]));
    a = mix(a, bitmask(|i| x[i] > y[i]));
    a = mix(a, bitmask(|i| x[i] >= y[i]));
    a = digest(a, lanes(|i| if x[i] < y[i] { x[i] } else { z[i] }));
    a = digest(a, [x[3], x[1], x[2], x[0]]);
    a = digest(a, [x[0], x[1], y[1], x[3]]);
    a = digest(a, [z[3]; 4]);
    a = scalar(a, ((x[0] + x[1]) + x[2]) + x[3]);
    a = scalar(
        a,
        fmin(fmin(fmin(y[0], y[1], true), y[2], true), y[3], true),
    );
    scalar(
        a,
        fmin(fmin(fmin(z[0], z[1], false), z[2], false), z[3], false),
    )
}
fn vf(bits: [u32; 4]) -> [f32; 4] {
    bits.map(f32::from_bits)
}

#[test]
fn f32_lane_operations_agree_with_ieee_binary32() {
    let mut calls = String::new();
    let mut acc = OFFSET;
    // Every edge meets every other edge in some lane.
    for (k, chunk) in EDGES.chunks(4).enumerate() {
        for (j, other) in EDGES.chunks(4).enumerate() {
            let x = [chunk[0], chunk[1], chunk[2], chunk[3]];
            let y = [other[0], other[1], other[2], other[3]];
            let z = EDGES[(k * 4 + j) % 13..][..3]
                .iter()
                .copied()
                .chain([EDGES[j]])
                .collect::<Vec<_>>();
            let z = [z[0], z[1], z[2], z[3]];
            let fmt = |v: [u32; 4]| format!("vf({}, {}, {}, {})", v[0], v[1], v[2], v[3]);
            calls += &format!("    acc = ops(acc, {}, {}, {});\n", fmt(x), fmt(y), fmt(z));
            acc = ops(acc, vf(x), vf(y), vf(z));
        }
    }
    let mut s: u64 = 7;
    for _ in 0..64 {
        s = lcg(s);
        let (p, q) = ((s >> 32) as u32, s as u32);
        s = lcg(s);
        let (r, t) = ((s >> 32) as u32, s as u32);
        acc = ops(acc, vf([p, q, r, t]), vf([t, p, q, r]), vf([r, t, p, q]));
    }
    let source = PROGRAM.replace("EDGE_CALLS", &calls);
    differential("f32_lanes", &source, acc);
}

#[test]
fn mul_add_rounds_once() {
    // (1 + 2^-12)^2 - (1 + 2^-11) is 2^-24, but the product alone rounds
    // to 1 + 2^-11, so an unfused multiply-add gives 0.
    let source = r#"
fn run() -> u64 eff [Isa[Baseline128]] {
    let a = Vec::splat[f32, 4](from_bits_f32(1065355264));
    let c = Vec::splat[f32, 4](from_bits_f32(3212840960));
    let fused = Vec::mul_add(a, a, c);
    let split = Vec::add(Vec::mul(a, a), c);
    wrapping_add(wrapping_mul(widen_u64(to_bits_f32(Vec::extract(fused, 2))), 1000), widen_u64(to_bits_f32(Vec::extract(split, 1))))
}

fn main() -> u64 { if Isa::supported[Baseline128]() { run() } else { 0 } }
"#;
    let a = f32::from_bits(1065355264);
    let c = f32::from_bits(3212840960);
    assert_eq!(a, 1.0 + 2f32.powi(-12));
    assert_eq!(c, -(1.0 + 2f32.powi(-11)));
    assert_eq!(a.mul_add(a, c), 2f32.powi(-24));
    assert_eq!(a * a + c, 0.0);
    differential(
        "mul_add",
        source,
        u64::from(2f32.powi(-24).to_bits()) * 1000,
    );
    // f64 lanes fuse too.
    let source = r#"
fn run() -> u64 eff [Isa[Baseline128]] {
    let a = Vec::splat[f64, 2](from_bits_f64(4607182419068521472));
    let c = Vec::splat[f64, 2](from_bits_f64(13830554455654793216));
    to_bits_f64(Vec::extract(Vec::mul_add(a, a, c), 1))
}

fn main() -> u64 { if Isa::supported[Baseline128]() { run() } else { 0 } }
"#;
    let a = f64::from_bits(4607182419068521472);
    let c = f64::from_bits(13830554455654793216);
    differential("mul_add_f64", source, a.mul_add(a, c).to_bits());
}

#[test]
fn wide_f32_vectors_and_conversions() {
    // 256-bit shapes run when the host targets Avx2; the else branch keeps
    // the program total elsewhere, and both backends pick the same branch.
    let source = r#"
fn mix(acc: u64, v: u64) -> u64 { wrapping_add(wrapping_mul(acc, 1099511628211), v) }
fn wide(x: Vec[f32, 8], y: Vec[f32, 8]) -> u64 eff [Isa[Avx2]] {
    let m = Vec::mul_add(x, y, Vec::splat[f32, 8](0.5));
    let w = Vec::widen_f64(Vec::from[f32, 4](Vec::extract(m, 0), Vec::extract(m, 3), Vec::extract(m, 5), Vec::extract(m, 7)));
    let d = Vec::mul(w, Vec::splat[f64, 4](3.0));
    let n = Vec::nearest_f32(d);
    let b = Vec::to_bits(n);
    let mut a: u64 = mix(0, widen_u64(Vec::extract(b, 0)));
    a = mix(a, widen_u64(Vec::extract(b, 1)));
    a = mix(a, widen_u64(Vec::extract(b, 2)));
    a = mix(a, widen_u64(Vec::extract(b, 3)));
    a = mix(a, to_bits_f64(Vec::reduce_add(w)));
    a = mix(a, Mask::bitmask(Vec::lt(x, y)));
    mix(a, widen_u64(to_bits_f32(Vec::reduce_max(Vec::abs(Vec::sub(x, y))))))
}
fn main() -> u64 {
    if Isa::supported[Avx2]() {
        wide(
            Vec::from[f32, 8](1.5, -2.25, 3.0, 0.1, 1.0e30, -0.0, 7.0, 0.3),
            Vec::from[f32, 8](2.0, 2.25, -3.5, 10.0, 1.0e10, 0.0, 0.125, 0.7),
        )
    } else {
        1
    }
}
"#;
    // A float literal is the nearest f32 of its f64 value.
    let x = [1.5f64, -2.25, 3.0, 0.1, 1.0e30, -0.0, 7.0, 0.3].map(|v| v as f32);
    let y = [2.0f64, 2.25, -3.5, 10.0, 1.0e10, 0.0, 0.125, 0.7].map(|v| v as f32);
    let m: Vec<f32> = (0..8).map(|i| x[i].mul_add(y[i], 0.5)).collect();
    let w: Vec<f64> = [m[0], m[3], m[5], m[7]]
        .iter()
        .map(|v| f64::from(*v))
        .collect();
    let n: Vec<f32> = w.iter().map(|v| (v * 3.0) as f32).collect();
    let mut a = mix(0, u64::from(n[0].to_bits()));
    for v in &n[1..] {
        a = mix(a, u64::from(v.to_bits()));
    }
    a = mix(a, (((w[0] + w[1]) + w[2]) + w[3]).to_bits());
    a = mix(a, (0..8).fold(0, |b, i| b | (u64::from(x[i] < y[i]) << i)));
    let biggest = (0..8)
        .map(|i| (x[i] - y[i]).abs())
        .fold(f32::NEG_INFINITY, |p, q| fmin(p, q, false));
    a = mix(a, u64::from(biggest.to_bits()));
    let c = accepted(source);
    let interpreted = Runtime::new(&c.world).run("main", vec![]).unwrap();
    let expected = if interpreted.to_string() == "1" { 1 } else { a };
    differential("wide_f32", source, expected);
}

#[test]
fn bits_round_trip_through_u32_lanes() {
    // The Resonance kernel keeps f32 samples as u32 bits in a ChunkSeq.
    let source = r#"
fn run() -> u64 eff [Isa[Baseline128]] {
    let bits = Vec::from[u32, 4](1065353216, 3221225472, 2139095040, 1);
    let x = Vec::from_bits(bits);
    let y = Vec::mul(x, Vec::splat[f32, 4](2.0));
    let back = Vec::to_bits(y);
    let a = wrapping_add(widen_u64(Vec::extract(back, 0)), widen_u64(Vec::extract(back, 1)));
    wrapping_add(a, wrapping_add(widen_u64(Vec::extract(back, 2)), widen_u64(Vec::extract(back, 3))))
}

fn main() -> u64 { if Isa::supported[Baseline128]() { run() } else { 0 } }
"#;
    let x = [1065353216u32, 3221225472, 2139095040, 1].map(f32::from_bits);
    let sum = x
        .iter()
        .map(|v| u64::from((v * 2.0).to_bits()))
        .fold(0u64, u64::wrapping_add);
    differential("bits", source, sum);
}
