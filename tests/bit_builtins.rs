//! Bit builtins (port gaps lane 5, #988): `leading_zeros_k`,
//! `trailing_zeros_k`, `count_ones_k`, `wrapping_shl_k` and
//! `wrapping_shr_k` for every integer kind `k`. Every program runs in the
//! evaluator (optimized and not), the JIT (optimized and not) and, where a C
//! driver links executables, an AOT executable, over edge values and a
//! pseudo-random stream of operands and amounts, and matches the Rust model
//! in `vesper::integer_widths::BitOp::eval`. Optimized native code lowers
//! each builtin to one Cranelift instruction instead of a call.
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper::{
    check,
    integer_widths::{BIT_BUILTINS, BitOp, bit_builtin},
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
            "vesper-bit-builtins-{}-{}",
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

fn rejects(source: &str, code: &str) {
    let c = checked(source);
    assert!(
        c.diagnostics.iter().any(|d| d.code == code),
        "expected {code} for {source}: {:?}",
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

fn model_step(k: IntegerKind, acc: u64, x: i128, n: u32) -> u64 {
    let mut acc = acc;
    for op in [
        BitOp::LeadingZeros,
        BitOp::TrailingZeros,
        BitOp::CountOnes,
        BitOp::WrappingShl,
        BitOp::WrappingShr,
    ] {
        acc = mix(acc, as_u64(op.eval(k, x, n)));
    }
    acc
}

const RANDOM: u64 = 300;

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
        // Sparse operands too, so leading and trailing counts vary.
        let x = from_pattern(k, (s >> 7) >> ((s >> 1) & 63));
        let n = (s >> 57) as u32;
        acc = model_step(k, acc, x, n);
    }
    let source = format!(
        "fn mix(acc: u64, v: u64) -> u64 {{ wrapping_add(wrapping_mul(acc, 1099511628211), v) }}
fn step(acc: u64, x: {kn}, n: u32) -> u64 {{
    let a1 = mix(acc, widen_u64(leading_zeros_{kn}(x)));
    let a2 = mix(a1, widen_u64(trailing_zeros_{kn}(x)));
    let a3 = mix(a2, widen_u64(count_ones_{kn}(x)));
    let a4 = mix(a3, wrapping_u64(wrapping_shl_{kn}(x, n)));
    mix(a4, wrapping_u64(wrapping_shr_{kn}(x, n)))
}}
fn main() -> u64 {{
    let mut acc: u64 = {OFFSET};
{calls}    let mut s: u64 = 1;
    for i in 0..{RANDOM} {{
        s = wrapping_add(wrapping_mul(s, 6364136223846793005), 1442695040888963407);
        let x = wrapping_{kn}(wrapping_shr_u64(s >> 7, wrapping_u32(s >> 1)));
        let n = wrapping_u32(s >> 57);
        acc = step(acc, x, n);
    }}
    acc
}}
"
    );
    (source, acc)
}

#[test]
fn the_model_matches_rust() {
    assert_eq!(BitOp::LeadingZeros.eval(IntegerKind::U8, 1, 0), 7);
    assert_eq!(BitOp::LeadingZeros.eval(IntegerKind::I16, -1, 0), 0);
    assert_eq!(BitOp::LeadingZeros.eval(IntegerKind::U32, 0, 0), 32);
    assert_eq!(BitOp::TrailingZeros.eval(IntegerKind::I8, 0, 0), 8);
    assert_eq!(
        BitOp::TrailingZeros.eval(IntegerKind::I64, i128::from(i64::MIN), 0),
        63
    );
    assert_eq!(BitOp::CountOnes.eval(IntegerKind::I32, -1, 0), 32);
    assert_eq!(BitOp::WrappingShl.eval(IntegerKind::U8, 0x81, 9), 0x02);
    assert_eq!(BitOp::WrappingShl.eval(IntegerKind::I8, 1, 7), -128);
    assert_eq!(BitOp::WrappingShr.eval(IntegerKind::I8, -128, 7), -1);
    assert_eq!(BitOp::WrappingShr.eval(IntegerKind::U16, 0x8000, 31), 1);
    for k in INTEGER_KINDS {
        for p in edges(k) {
            let x = from_pattern(k, p);
            for n in edge_amounts(k) {
                let r = match k {
                    IntegerKind::U8 => {
                        let v = x as u8;
                        [
                            v.leading_zeros().into(),
                            v.trailing_zeros().into(),
                            v.count_ones().into(),
                            v.wrapping_shl(n).into(),
                            v.wrapping_shr(n).into(),
                        ]
                    }
                    IntegerKind::I8 => {
                        let v = x as i8;
                        [
                            v.leading_zeros().into(),
                            v.trailing_zeros().into(),
                            v.count_ones().into(),
                            v.wrapping_shl(n).into(),
                            v.wrapping_shr(n).into(),
                        ]
                    }
                    IntegerKind::U16 => {
                        let v = x as u16;
                        [
                            v.leading_zeros().into(),
                            v.trailing_zeros().into(),
                            v.count_ones().into(),
                            v.wrapping_shl(n).into(),
                            v.wrapping_shr(n).into(),
                        ]
                    }
                    IntegerKind::I16 => {
                        let v = x as i16;
                        [
                            v.leading_zeros().into(),
                            v.trailing_zeros().into(),
                            v.count_ones().into(),
                            v.wrapping_shl(n).into(),
                            v.wrapping_shr(n).into(),
                        ]
                    }
                    IntegerKind::U32 => {
                        let v = x as u32;
                        [
                            v.leading_zeros().into(),
                            v.trailing_zeros().into(),
                            v.count_ones().into(),
                            v.wrapping_shl(n).into(),
                            v.wrapping_shr(n).into(),
                        ]
                    }
                    IntegerKind::I32 => {
                        let v = x as i32;
                        [
                            v.leading_zeros().into(),
                            v.trailing_zeros().into(),
                            v.count_ones().into(),
                            v.wrapping_shl(n).into(),
                            v.wrapping_shr(n).into(),
                        ]
                    }
                    IntegerKind::U64 | IntegerKind::Usize => {
                        let v = x as u64;
                        [
                            v.leading_zeros().into(),
                            v.trailing_zeros().into(),
                            v.count_ones().into(),
                            v.wrapping_shl(n).into(),
                            v.wrapping_shr(n).into(),
                        ]
                    }
                    IntegerKind::I64 | IntegerKind::Isize => {
                        let v = x as i64;
                        [
                            v.leading_zeros().into(),
                            v.trailing_zeros().into(),
                            v.count_ones().into(),
                            v.wrapping_shl(n).into(),
                            v.wrapping_shr(n).into(),
                        ]
                    }
                };
                let ops = [
                    BitOp::LeadingZeros,
                    BitOp::TrailingZeros,
                    BitOp::CountOnes,
                    BitOp::WrappingShl,
                    BitOp::WrappingShr,
                ];
                for (op, want) in ops.iter().zip(r) {
                    let want: i128 = want;
                    assert_eq!(op.eval(k, x, n), want, "{op:?} {k:?} {x} {n}");
                }
            }
        }
    }
}

#[test]
fn every_kind_agrees_in_every_backend() {
    for k in INTEGER_KINDS {
        let (source, expected) = program(k);
        differential(&format!("bits_{}", k.name()), &source, expected);
    }
}

#[test]
fn every_name_is_a_builtin_of_its_kind() {
    for k in INTEGER_KINDS {
        for (prefix, op) in BIT_BUILTINS {
            assert_eq!(bit_builtin(&format!("{prefix}{}", k.name())), Some((op, k)));
        }
    }
    assert_eq!(bit_builtin("leading_zeros"), None);
    assert_eq!(bit_builtin("count_ones_f64"), None);
    assert_eq!(bit_builtin("wrapping_u32"), None);
}

#[test]
fn the_names_are_reserved_and_typed() {
    rejects(
        "fn count_ones_u32(x: u32) -> u32 { x } fn main() -> u32 { count_ones_u32(3) }",
        "E0100",
    );
    rejects(
        "fn main() -> u32 { let x: u64 = 3; leading_zeros_u32(x) }",
        "E0200",
    );
    rejects("fn main() -> u64 { leading_zeros_u64(3) }", "E0200");
    // The builtins are total: no handler and no effect row.
    accepted(
        "fn f(x: i64, n: u32) -> i64 { wrapping_shl_i64(x, n) | wrapping_shr_i64(x, n) }
fn main() -> u32 { count_ones_u8(255) }",
    );
}

/// Optimized native code holds one instruction per builtin and calls none.
#[test]
fn native_code_uses_single_instructions() {
    let source = "fn f(x: u64, y: i32, n: u32) -> u64 {
    let a = widen_u64(leading_zeros_u64(x));
    let b = widen_u64(trailing_zeros_i32(y));
    let c = widen_u64(count_ones_u64(x));
    let d = wrapping_shl_u64(x, n);
    let e = wrapping_u64(wrapping_shr_i32(y, n));
    a ^ b ^ c ^ d ^ e
}
fn main() -> u64 { f(12345, -77, 70) }";
    let c = accepted(source);
    for optimize in [true, false] {
        let mut clif = String::new();
        let mut observe = |f: native::EmittedFunction| {
            if f.source == "f" {
                clif = f.function.display().to_string();
            }
        };
        native::object_with_observer(
            &c.world,
            "main",
            Some("x86_64-unknown-linux-gnu"),
            optimize,
            Some(&mut observe),
        )
        .unwrap();
        for ins in [" clz ", " ctz ", " popcnt ", " ishl ", " sshr "] {
            assert!(clif.contains(ins), "optimize {optimize}: no{ins}in\n{clif}");
        }
        assert!(
            !clif.contains("call "),
            "optimize {optimize}: a call remains\n{clif}"
        );
    }
}
