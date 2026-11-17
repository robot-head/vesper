//! `f32` and the float contract (`vesper.float.ieee754/1`): every operator,
//! math intrinsic and conversion over a fixed corpus of special values
//! (signed zeros, subnormals, ties, infinities, quiet and signaling NaNs)
//! gives the same bits in the evaluator (optimized and not), the JIT
//! (optimized and not) and the reference semantics in
//! `vesper::numeric::float`. CI runs this on x86-64 and AArch64 hosts; on
//! every host the programs also compile to x86-64 and AArch64 objects. The
//! transcendental prelude functions match golden hashes, so their bits are
//! the same on every architecture.
use std::{
    fs,
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};
use vesper::{
    check, lean, native,
    numeric::{
        ArithFailure, IntegerKind,
        float::{self, Float, FloatKind, MathOp},
    },
    parser,
    runtime::Runtime,
};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn checked(source: &str) -> check::Checked {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{source}\n{:?}", p.diagnostics);
    check::check(&p.module)
}
fn accepted(source: &str) -> check::Checked {
    let c = checked(source);
    assert!(c.diagnostics.is_empty(), "{source}\n{:?}", c.diagnostics);
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
/// Evaluator and JIT (both optimized and not) agree on `main`, and both
/// object targets compile. Returns the value.
fn agree(source: &str) -> String {
    let c = accepted(source);
    let mut seen: Option<String> = None;
    for optimize in [true, false] {
        let mut runtime = Runtime::new(&c.world);
        runtime.optimize = optimize;
        let value = runtime.run("main", vec![]).unwrap().to_string();
        let jit = native::jit_with_options(&c.world, "main", optimize)
            .unwrap()
            .to_string();
        assert_eq!(
            jit, value,
            "JIT vs evaluator (optimize {optimize}): {source}"
        );
        if let Some(seen) = &seen {
            assert_eq!(&value, seen, "optimized vs unoptimized: {source}");
        }
        seen = Some(value);
    }
    let (x86, _) =
        native::object_for_target(&c.world, "main", Some("x86_64-unknown-linux-gnu")).unwrap();
    assert_eq!(&x86[..4], b"\x7fELF");
    let (arm, _) =
        native::object_for_target(&c.world, "main", Some("aarch64-unknown-linux-gnu")).unwrap();
    assert_eq!(u16::from_le_bytes([arm[18], arm[19]]), 183);
    seen.unwrap()
}
fn agree_with(source: &str, expected: &str) {
    assert_eq!(agree(source), expected, "{source}");
}

/// Special `f32` values: ±0, subnormals, the smallest normal, ±1, ties for
/// `round`, 1/3, values around 2^23, ±max, ±∞, a quiet and a signaling NaN
/// with a payload, π, ±2^31 and 255.5.
const F32_CORPUS: [u32; 26] = [
    0x0000_0000,
    0x8000_0000,
    0x0000_0001,
    0x807f_ffff,
    0x0080_0000,
    0x3f80_0000,
    0xbf80_0000,
    0x3fc0_0000,
    0xc020_0000,
    0x3f00_0000,
    0xbf00_0000,
    0x3eaa_aaab,
    0x4b00_0001,
    0x4aff_ffff,
    0x7f7f_ffff,
    0xff7f_ffff,
    0x7f80_0000,
    0xff80_0000,
    0x7fc0_0000,
    0x7fa0_0001,
    0x4049_0fdb,
    0x4f00_0000,
    0xcf00_0000,
    0x437f_8000,
    0x3f7f_ffff,
    0xc2fe_0000,
];
/// Special `f64` values, as for `f32`, plus values exactly halfway between
/// two `f32`s (ties for `nearest_f32`) and an `f64` subnormal.
const F64_CORPUS: [u64; 26] = [
    0x0000_0000_0000_0000,
    0x8000_0000_0000_0000,
    0x0000_0000_0000_0001,
    0x800f_ffff_ffff_ffff,
    0x0010_0000_0000_0000,
    0x3ff0_0000_0000_0000,
    0xbff0_0000_0000_0000,
    0x3ff8_0000_0000_0000,
    0xc004_0000_0000_0000,
    0x3fe0_0000_0000_0000,
    0xbfe0_0000_0000_0000,
    0x3fd5_5555_5555_5555,
    0x4330_0000_0000_0001,
    0x432f_ffff_ffff_ffff,
    0x7fef_ffff_ffff_ffff,
    0xffef_ffff_ffff_ffff,
    0x7ff0_0000_0000_0000,
    0xfff0_0000_0000_0000,
    0x7ff8_0000_0000_0000,
    0x7ff4_0000_0000_0001,
    0x4009_21fb_5444_2d18,
    0x43e0_0000_0000_0000,
    0xc3e0_0000_0000_0000,
    0x3ff0_0000_1000_0000,
    0x3ff0_0000_3000_0000,
    0x47ef_ffff_e000_0000,
];
fn corpus(kind: FloatKind) -> Vec<Float> {
    match kind {
        FloatKind::F32 => F32_CORPUS
            .iter()
            .map(|b| Float::from_bits(kind, u64::from(*b)))
            .collect(),
        FloatKind::F64 => F64_CORPUS
            .iter()
            .map(|b| Float::from_bits(kind, *b))
            .collect(),
    }
}
fn other(kind: FloatKind) -> FloatKind {
    match kind {
        FloatKind::F32 => FloatKind::F64,
        FloatKind::F64 => FloatKind::F32,
    }
}

/// What an operation returns, and how its result enters the hash.
#[derive(Clone, Copy, PartialEq)]
enum Result_ {
    /// A float of the operand kind; `canonical` says whether a NaN result
    /// is canonicalised in a DetFloat program.
    Same {
        canonical: bool,
    },
    /// A float of the other kind (a conversion, which canonicalises).
    Other,
    Bool,
    /// An integer of this kind, reinterpreted as a `u64`.
    Integer(IntegerKind),
}
struct Op {
    source: &'static str,
    arity: usize,
    result: Result_,
    reference: fn(FloatKind, &[Float]) -> Reference,
}
enum Reference {
    Float(Float),
    Bool(bool),
    Integer(i128),
}
fn f2(a: Float, b: Float, f32op: fn(f32, f32) -> f32, f64op: fn(f64, f64) -> f64) -> Float {
    match (a, b) {
        (Float::F32(a), Float::F32(b)) => Float::F32(f32op(a, b)),
        (Float::F64(a), Float::F64(b)) => Float::F64(f64op(a, b)),
        _ => unreachable!(),
    }
}
fn cmp(a: Float, b: Float, f: fn(f64, f64) -> bool) -> Reference {
    // Widening is exact, so comparing the f64 values compares the floats.
    Reference::Bool(f(a.widen(), b.widen()))
}
fn math(op: MathOp, args: &[Float]) -> Reference {
    Reference::Float(op.apply(args).unwrap())
}
fn ops() -> Vec<Op> {
    use Result_::*;
    let fl = |canonical| Same { canonical };
    vec![
        Op {
            source: "-a",
            arity: 1,
            result: fl(false),
            reference: |_, x| {
                Reference::Float(match x[0] {
                    Float::F32(a) => Float::F32(-a),
                    Float::F64(a) => Float::F64(-a),
                })
            },
        },
        Op {
            source: "abs(a)",
            arity: 1,
            result: fl(false),
            reference: |_, x| math(MathOp::Abs, x),
        },
        Op {
            source: "sqrt(a)",
            arity: 1,
            result: fl(true),
            reference: |_, x| math(MathOp::Sqrt, x),
        },
        Op {
            source: "floor(a)",
            arity: 1,
            result: fl(true),
            reference: |_, x| math(MathOp::Floor, x),
        },
        Op {
            source: "ceil(a)",
            arity: 1,
            result: fl(true),
            reference: |_, x| math(MathOp::Ceil, x),
        },
        Op {
            source: "round(a)",
            arity: 1,
            result: fl(true),
            reference: |_, x| math(MathOp::Round, x),
        },
        Op {
            source: "trunc(a)",
            arity: 1,
            result: fl(true),
            reference: |_, x| math(MathOp::Trunc, x),
        },
        Op {
            source: "CONVERT(a)",
            arity: 1,
            result: Other,
            reference: |k, x| Reference::Float(float::nearest_float(other(k), x[0])),
        },
        Op {
            source: "saturating_i32(a)",
            arity: 1,
            result: Integer(IntegerKind::I32),
            reference: |_, x| {
                Reference::Integer(float::float_to_integer_saturating(IntegerKind::I32, x[0]))
            },
        },
        Op {
            source: "saturating_u8(a)",
            arity: 1,
            result: Integer(IntegerKind::U8),
            reference: |_, x| {
                Reference::Integer(float::float_to_integer_saturating(IntegerKind::U8, x[0]))
            },
        },
        Op {
            source: "saturating_i64(a)",
            arity: 1,
            result: Integer(IntegerKind::I64),
            reference: |_, x| {
                Reference::Integer(float::float_to_integer_saturating(IntegerKind::I64, x[0]))
            },
        },
        Op {
            source: "saturating_u64(a)",
            arity: 1,
            result: Integer(IntegerKind::U64),
            reference: |_, x| {
                Reference::Integer(float::float_to_integer_saturating(IntegerKind::U64, x[0]))
            },
        },
        Op {
            source: "a + b",
            arity: 2,
            result: fl(true),
            reference: |_, x| Reference::Float(f2(x[0], x[1], |a, b| a + b, |a, b| a + b)),
        },
        Op {
            source: "a - b",
            arity: 2,
            result: fl(true),
            reference: |_, x| Reference::Float(f2(x[0], x[1], |a, b| a - b, |a, b| a - b)),
        },
        Op {
            source: "a * b",
            arity: 2,
            result: fl(true),
            reference: |_, x| Reference::Float(f2(x[0], x[1], |a, b| a * b, |a, b| a * b)),
        },
        Op {
            source: "a / b",
            arity: 2,
            result: fl(true),
            reference: |_, x| Reference::Float(f2(x[0], x[1], |a, b| a / b, |a, b| a / b)),
        },
        // `%` is a library call: its NaN results are never canonicalised.
        Op {
            source: "a % b",
            arity: 2,
            result: fl(false),
            reference: |_, x| Reference::Float(f2(x[0], x[1], |a, b| a % b, |a, b| a % b)),
        },
        Op {
            source: "min(a, b)",
            arity: 2,
            result: fl(true),
            reference: |_, x| math(MathOp::Min, x),
        },
        Op {
            source: "max(a, b)",
            arity: 2,
            result: fl(true),
            reference: |_, x| math(MathOp::Max, x),
        },
        Op {
            source: "a < b",
            arity: 2,
            result: Bool,
            reference: |_, x| cmp(x[0], x[1], |a, b| a < b),
        },
        Op {
            source: "a <= b",
            arity: 2,
            result: Bool,
            reference: |_, x| cmp(x[0], x[1], |a, b| a <= b),
        },
        Op {
            source: "a == b",
            arity: 2,
            result: Bool,
            reference: |_, x| cmp(x[0], x[1], |a, b| a == b),
        },
        Op {
            source: "a != b",
            arity: 2,
            result: Bool,
            reference: |_, x| cmp(x[0], x[1], |a, b| a != b),
        },
        Op {
            source: "fma(a, b, c)",
            arity: 3,
            result: fl(true),
            reference: |_, x| math(MathOp::Fma, x),
        },
    ]
}
/// Operands of the ternary sweep (its first values).
const TERNARY: usize = 9;
const FNV_OFFSET: u64 = 14695981039346656037;
fn mix(h: u64, x: u64) -> u64 {
    (h ^ x).wrapping_mul(1099511628211)
}
/// The hash word of a float result. In `exact` (DetFloat) mode a NaN of a
/// canonicalising operation is the canonical NaN and other NaNs keep their
/// bits, except `%`, whose NaNs are normalized like outside DetFloat, where
/// every NaN is `u64::MAX`.
fn float_word(f: Float, exact: bool, canonical: bool, op: &str) -> u64 {
    if f.is_nan() && (!exact || op.contains('%')) {
        u64::MAX
    } else if f.is_nan() && canonical {
        f.canonical().to_bits()
    } else {
        f.to_bits()
    }
}
fn reference_hashes(kind: FloatKind, exact: bool) -> Vec<u64> {
    let values = corpus(kind);
    ops()
        .iter()
        .map(|op| {
            let n = if op.arity == 3 { TERNARY } else { values.len() };
            let mut tuples: Vec<Vec<Float>> = vec![vec![]];
            for _ in 0..op.arity {
                tuples = tuples
                    .into_iter()
                    .flat_map(|t| {
                        values[..n].iter().map(move |v| {
                            let mut t = t.clone();
                            t.push(*v);
                            t
                        })
                    })
                    .collect();
            }
            tuples.iter().fold(FNV_OFFSET, |h, args| {
                let word = match ((op.reference)(kind, args), op.result) {
                    (Reference::Float(f), Result_::Same { canonical }) => {
                        float_word(f, exact, canonical, op.source)
                    }
                    (Reference::Float(f), Result_::Other) => float_word(f, exact, true, op.source),
                    (Reference::Bool(b), _) => u64::from(b),
                    (Reference::Integer(n), _) => n as i64 as u64,
                    _ => unreachable!(),
                };
                mix(h, word)
            })
        })
        .collect()
}
/// A program whose `main` returns one hash per operation of [`ops`] over
/// the corpus of `kind`, computed the way [`reference_hashes`] does.
fn corpus_program(kind: FloatKind, exact: bool) -> String {
    let k = kind.name();
    let values = corpus(kind);
    let n = values.len();
    let mut out = String::new();
    let value = |i: usize| format!("from_bits_{k}({})", values[i].to_bits());
    out.push_str(&format!("fn value(i: u64) -> {k} {{ "));
    for i in 0..n - 1 {
        out.push_str(&format!("if i == {i} {{ {} }} else ", value(i)));
    }
    out.push_str(&format!("{{ {} }} }}\n", value(n - 1)));
    let widen = |k: FloatKind, x: &str| match k {
        FloatKind::F32 => format!("widen_u64(to_bits_f32({x}))"),
        FloatKind::F64 => format!("to_bits_f64({x})"),
    };
    for (name, kind) in [("bits", kind), ("other_bits", other(kind))] {
        let kn = kind.name();
        let body = widen(kind, "x");
        if exact {
            out.push_str(&format!("fn {name}(x: {kn}) -> u64 {{ {body} }}\n"));
        } else {
            out.push_str(&format!(
                "fn {name}(x: {kn}) -> u64 {{ if x != x {{ 18446744073709551615 }} else {{ {body} }} }}\n"
            ));
        }
        out.push_str(&format!(
            "fn n{name}(x: {kn}) -> u64 {{ if x != x {{ 18446744073709551615 }} else {{ {body} }} }}\n"
        ));
    }
    out.push_str("fn mix(h: u64, x: u64) -> u64 { wrapping_mul(h ^ x, 1099511628211) }\n");
    if exact {
        // Any DetFloat function turns on NaN canonicalisation.
        out.push_str(&format!(
            "fn det(x: {k}) -> {k} eff [DetFloat] {{ x + x }}\n"
        ));
    }
    let convert = match kind {
        FloatKind::F32 => "widen_f64",
        FloatKind::F64 => "nearest_f32",
    };
    let mut calls = vec![];
    for (index, op) in ops().iter().enumerate() {
        let expr = op.source.replace("CONVERT", convert);
        let word = match op.result {
            Result_::Same { .. } if op.source.contains('%') => format!("nbits({expr})"),
            Result_::Same { .. } => format!("bits({expr})"),
            Result_::Other => format!("other_bits({expr})"),
            Result_::Bool => format!("if {expr} {{ 1 }} else {{ 0 }}"),
            Result_::Integer(_) => format!("wrapping_u64({expr})"),
        };
        let limit = if op.arity == 3 { TERNARY } else { n };
        let s = format!("s{index}");
        let next = |i: usize| format!("wrapping_add(i{i}, 1)");
        let (params, start) = match op.arity {
            1 => (
                "i0: u64, h: u64".to_string(),
                format!("{s}(0, {FNV_OFFSET})"),
            ),
            2 => (
                "i0: u64, i1: u64, h: u64".to_string(),
                format!("{s}(0, 0, {FNV_OFFSET})"),
            ),
            _ => (
                "i0: u64, i1: u64, i2: u64, h: u64".to_string(),
                format!("{s}(0, 0, 0, {FNV_OFFSET})"),
            ),
        };
        let bind = ["a", "b", "c"][..op.arity]
            .iter()
            .enumerate()
            .map(|(i, v)| format!("let {v}: {k} = value(i{i});"))
            .collect::<Vec<_>>()
            .join(" ");
        let body = match op.arity {
            1 => format!(
                "if i0 == {limit} {{ h }} else {{ {bind} {s}({}, mix(h, {word})) }}",
                next(0)
            ),
            2 => format!(
                "if i0 == {limit} {{ h }} else if i1 == {limit} {{ {s}({}, 0, h) }} else {{ {bind} {s}(i0, {}, mix(h, {word})) }}",
                next(0),
                next(1)
            ),
            _ => format!(
                "if i0 == {limit} {{ h }} else if i1 == {limit} {{ {s}({}, 0, 0, h) }} else if i2 == {limit} {{ {s}(i0, {}, 0, h) }} else {{ {bind} {s}(i0, i1, {}, mix(h, {word})) }}",
                next(0),
                next(1),
                next(2)
            ),
        };
        out.push_str(&format!("fn {s}({params}) -> u64 {{ {body} }}\n"));
        calls.push(start);
    }
    out.push_str(&format!(
        "fn main() -> ({}) {{ ({}) }}\n",
        vec!["u64"; calls.len()].join(", "),
        calls.join(", ")
    ));
    out
}
fn tuple_words(value: &str) -> Vec<u64> {
    value
        .trim_matches(|c| c == '(' || c == ')')
        .split(", ")
        .map(|w| {
            w.parse()
                .unwrap_or_else(|_| panic!("not a word: {w} in {value}"))
        })
        .collect()
}
fn check_corpus(kind: FloatKind, exact: bool) {
    let source = corpus_program(kind, exact);
    let got = tuple_words(&agree(&source));
    let expected = reference_hashes(kind, exact);
    for ((op, got), expected) in ops().iter().zip(&got).zip(&expected) {
        assert_eq!(
            got,
            expected,
            "{} on {} (DetFloat: {exact})",
            op.source,
            kind.name()
        );
    }
}

#[test]
fn f32_corpus_matches_the_reference_semantics() {
    check_corpus(FloatKind::F32, false);
}
#[test]
fn f64_corpus_matches_the_reference_semantics() {
    check_corpus(FloatKind::F64, false);
}
#[test]
fn detfloat_programs_canonicalise_nan_results_bit_exactly() {
    check_corpus(FloatKind::F32, true);
    check_corpus(FloatKind::F64, true);
}

/// Inputs of the transcendental functions: zeros, subnormals, tiny and
/// moderate values, the overflow and underflow thresholds of `exp`,
/// multiples of π/4, large arguments, infinities and a NaN.
const TRANSCENDENTAL_INPUTS: [f64; 34] = [
    0.0,
    -0.0,
    5e-324,
    2.2250738585072014e-308,
    1e-300,
    1e-10,
    1e-5,
    0.1,
    0.5,
    std::f64::consts::FRAC_PI_4,
    1.0,
    -1.0,
    std::f64::consts::FRAC_PI_2,
    2.0,
    -2.5,
    std::f64::consts::PI,
    -std::f64::consts::PI,
    std::f64::consts::TAU,
    10.0,
    88.7,
    100.0,
    -100.0,
    700.0,
    709.78,
    710.0,
    -745.0,
    -746.0,
    12345.678,
    1e6,
    1e10,
    1e300,
    f64::INFINITY,
    f64::NEG_INFINITY,
    f64::NAN,
];
fn transcendental_program(function: &str, kind: FloatKind) -> String {
    let k = kind.name();
    let items: Vec<String> = TRANSCENDENTAL_INPUTS
        .iter()
        .map(|x| {
            let bits = Float::F64(*x);
            let arg = match kind {
                FloatKind::F32 => format!("nearest_f32(from_bits_f64({}))", bits.to_bits()),
                FloatKind::F64 => format!("from_bits_f64({})", bits.to_bits()),
            };
            match kind {
                FloatKind::F32 => format!("widen_u64(to_bits_f32({function}_{k}({arg})))"),
                FloatKind::F64 => format!("to_bits_f64({function}_{k}({arg}))"),
            }
        })
        .collect();
    format!(
        "fn main() -> ({}) {{ ({}) }}",
        vec!["u64"; items.len()].join(", "),
        items.join(", ")
    )
}
/// Golden FNV hashes of the transcendental results over
/// [`TRANSCENDENTAL_INPUTS`], per function and kind. Every host and backend
/// must reproduce them bit for bit.
const GOLDEN: [(&str, FloatKind, u64); 8] = [
    ("exp", FloatKind::F64, 0xa1492c2951b9d659),
    ("ln", FloatKind::F64, 0x6e89c7c93c0cc543),
    ("sin", FloatKind::F64, 0x732848cb848173ab),
    ("cos", FloatKind::F64, 0x13a3c9a518e3a82e),
    ("exp", FloatKind::F32, 0xdbea5bbb6f717c63),
    ("ln", FloatKind::F32, 0x875ba6f933d73244),
    ("sin", FloatKind::F32, 0x787adb1904d8a2c1),
    ("cos", FloatKind::F32, 0xfd4cc7a2eebfed6d),
];
fn ulps(a: f64, b: f64) -> u64 {
    if a == b || (a.is_nan() && b.is_nan()) {
        return 0;
    }
    let key = |x: f64| {
        let b = x.to_bits() as i64;
        if b < 0 { i64::MIN - b } else { b }
    };
    key(a).abs_diff(key(b))
}
fn ulps32(a: f32, b: f32) -> u64 {
    if a == b || (a.is_nan() && b.is_nan()) {
        return 0;
    }
    let key = |x: f32| {
        let b = x.to_bits() as i32;
        i64::from(if b < 0 { i32::MIN - b } else { b })
    };
    key(a).abs_diff(key(b))
}

#[test]
fn transcendentals_are_bit_identical_and_within_one_ulp() {
    let mut report = vec![];
    for (function, kind, golden) in GOLDEN {
        let source = transcendental_program(function, kind);
        let words = tuple_words(&agree(&source));
        let hash = words.iter().fold(FNV_OFFSET, |h, w| mix(h, *w));
        report.push(format!("(\"{function}\", FloatKind::{kind:?}, {hash:#x}),"));
        for (x, w) in TRANSCENDENTAL_INPUTS.iter().zip(&words) {
            let reference = match function {
                "exp" => x.exp(),
                "ln" => x.ln(),
                "sin" => x.sin(),
                _ => x.cos(),
            };
            // Argument reduction is accurate below 2^20 * pi/2.
            if matches!(function, "sin" | "cos") && x.abs() > 1.6e6 {
                continue;
            }
            match kind {
                FloatKind::F64 => {
                    let got = f64::from_bits(*w);
                    assert!(
                        ulps(got, reference) <= 1,
                        "{function}_f64({x}) = {got}, std gives {reference}"
                    );
                }
                FloatKind::F32 => {
                    let xf = *x as f32;
                    let reference = match function {
                        "exp" => f64::from(xf).exp(),
                        "ln" => f64::from(xf).ln(),
                        "sin" => f64::from(xf).sin(),
                        _ => f64::from(xf).cos(),
                    } as f32;
                    let got = f32::from_bits(*w as u32);
                    assert!(
                        ulps32(got, reference) <= 1,
                        "{function}_f32({xf}) = {got}, std gives {reference}"
                    );
                }
            }
        }
        assert_eq!(
            hash,
            golden,
            "{function}_{}: golden hash; every host computes:\n{}",
            kind.name(),
            report.join("\n")
        );
    }
}

#[test]
fn float_literals_take_their_kind_from_the_context() {
    // 0.1 as an f32 is the f32 nearest 0.1, not 0.1's f64 bits.
    agree_with(
        "fn scale(x: f32) -> f32 { 2.0 * x + 0.5 }
fn main() -> (u32, u32, u64, u32, u32) {
    let a: f32 = 0.1;
    let pair: (f32, f64) = (0.1, 0.1);
    let (b, c) = pair;
    (to_bits_f32(a), to_bits_f32(b), to_bits_f64(c), to_bits_f32(scale(a)), to_bits_f32(-1.5))
}",
        &format!(
            "({}, {}, {}, {}, {})",
            0.1f32.to_bits(),
            0.1f32.to_bits(),
            0.1f64.to_bits(),
            (2.0f32 * 0.1 + 0.5).to_bits(),
            (-1.5f32).to_bits()
        ),
    );
    agree_with(
        "fn pick(x: f32) -> u8 { match x { 1.5 => 10, 0.0 => 20, _ => 30 } }
fn main() -> (u8, u8, u8, u8) { (pick(1.5), pick(0.0), pick(-0.0), pick(2.0)) }",
        "(10, 20, 20, 30)",
    );
}

#[test]
fn nested_tuples_take_float_kinds_from_the_context() {
    agree_with(
        "fn main() -> ((u32, u32), u32) { let t: ((f32, f32), f32) = ((0.1, 0.2), 0.3); let ((a, b), c) = t; ((to_bits_f32(a), to_bits_f32(b)), to_bits_f32(c)) }",
        &format!(
            "(({}, {}), {})",
            0.1f32.to_bits(),
            0.2f32.to_bits(),
            0.3f32.to_bits()
        ),
    );
    accepted("fn f() -> ((f32, f32), f32) { ((0.1, 0.2), 0.3) }");
}

#[test]
fn integer_and_float_conversions_agree_with_rust() {
    agree_with(
        "fn main() -> (u32, u32, u64, u64, u32, u64, u32, u64) {
    let a: i64 = 16777217;
    let b: u64 = 18446744073709551615;
    let c: i64 = -9223372036854775808;
    let d: i32 = -2147483648;
    let e: i16 = -32768;
    let f: u32 = 4294967295;
    (to_bits_f32(nearest_f32(a)), to_bits_f32(nearest_f32(b)), to_bits_f64(nearest_f64(c)),
     to_bits_f64(widen_f64(d)), to_bits_f32(widen_f32(e)), to_bits_f64(widen_f64(f)),
     to_bits_f32(nearest_f32(from_bits_f64(4607182418800017408))), to_bits_f64(widen_f64(nearest_f32(0.1))))
}",
        &format!(
            "({}, {}, {}, {}, {}, {}, {}, {})",
            (16777217i64 as f32).to_bits(),
            (u64::MAX as f32).to_bits(),
            (i64::MIN as f64).to_bits(),
            f64::from(i32::MIN).to_bits(),
            f32::from(i16::MIN).to_bits(),
            f64::from(u32::MAX).to_bits(),
            1.0f32.to_bits(),
            f64::from(0.1f32).to_bits()
        ),
    );
}

#[test]
fn checked_float_to_integer_conversions_raise_arith_overflow() {
    let cases: [(&str, f64); 10] = [
        ("i8", 127.9),
        ("i8", 128.0),
        ("i8", -128.9),
        ("i8", -129.0),
        ("u8", -0.9),
        ("u8", -1.0),
        ("u64", 18446744073709549568.0),
        ("u64", 18446744073709551616.0),
        ("i64", -9223372036854775808.0),
        ("i64", f64::NAN),
    ];
    let mut source = String::new();
    let mut calls = vec![];
    let mut expected = vec![];
    for (i, (k, x)) in cases.iter().enumerate() {
        source.push_str(&format!(
            "fn n{i}(x: f64) -> {k} eff [Fail[ArithOverflow]] {{ narrow_{k}(x) }}
fn t{i}(x: f64) -> (u8, {k}) {{ handle Fail[ArithOverflow] in {{ (0, n{i}(x)) }} with {{ return v => v, raise(e, k) => (1, 0), }} }}
"
        ));
        calls.push(format!("t{i}(from_bits_f64({}))", x.to_bits()));
        let kind = IntegerKind::from_name(k).unwrap();
        expected.push(
            match float::float_to_integer_checked(kind, Float::F64(*x)) {
                Ok(n) => format!("(0, {n})"),
                Err(ArithFailure::Overflow) => "(1, 0)".into(),
                Err(e) => panic!("{e:?}"),
            },
        );
    }
    source.push_str(&format!(
        "fn main() -> ({}) {{ ({}) }}",
        cases
            .iter()
            .map(|(k, _)| format!("(u8, {k})"))
            .collect::<Vec<_>>()
            .join(", "),
        calls.join(", ")
    ));
    agree_with(&source, &format!("({})", expected.join(", ")));
    // A checked conversion of a float always needs the effect.
    rejects("fn f(x: f32) -> i32 { narrow_i32(x) }", "E0401");
    accepted("fn f(x: f32) -> i32 { saturating_i32(x) }");
}

#[test]
fn invalid_float_uses_are_rejected() {
    rejects("fn f(a: f32, b: f64) -> f32 { a + b }", "E0200");
    rejects("fn f(a: f64) -> f32 { a }", "E0200");
    rejects("fn f(a: f32) -> f32 { a + 1 }", "E0200");
    rejects("fn f(a: f64) -> f32 { widen_f32(a) }", "E0200");
    rejects("fn f(a: i64) -> f64 { widen_f64(a) }", "E0200");
    rejects("fn f(a: i32) -> f32 { widen_f32(a) }", "E0200");
    rejects("fn f(a: u64) -> f64 { sqrt(a) }", "E0200");
    rejects("fn f(a: f32, b: f64) -> f32 { min(a, b) }", "E0200");
    rejects("fn f(a: f32) -> f32 { fma(a, a) }", "E0207");
    rejects("fn f(a: i64) -> u8 { saturating_u8(a) }", "E0200");
    rejects("fn f(a: f32) -> u8 { wrapping_u8(a) }", "E0200");
    rejects("fn f(a: f32) -> u64 { widen_u64(a) }", "E0200");
    rejects("fn f(a: f64) -> u32 { to_bits_f32(a) }", "E0200");
    rejects("fn f(a: u64) -> f32 { from_bits_f32(a) }", "E0200");
    rejects("fn f(a: f32) -> f32 { ~a }", "E0200");
    // User functions take precedence over the intrinsics.
    accepted(
        "fn min(a: i64, b: i64) -> i64 { if a < b { a } else { b } } fn main() -> i64 { min(1, 2) }",
    );
}

#[test]
fn capability_effects_cover_f32() {
    // DetFloat code may use the correctly rounded intrinsics and the
    // transcendental prelude; remainder stays rejected.
    accepted("fn f(x: f32) -> f32 eff [DetFloat] { exp_f32(sqrt(x)) + fma(x, x, 1.0) }");
    accepted("fn f(x: f64) -> f64 eff [Realtime] { sin_f64(x) }");
    rejects(
        "fn f(a: f32, b: f32) -> f32 eff [DetFloat] { a % b }",
        "E0730",
    );
    // Only the generated prelude carries DetFloat: a user definition of the
    // same name keeps its declared effects.
    rejects(
        "fn exp_f64(x: f64) -> f64 { x % 2.0 }
fn f(x: f64) -> f64 eff [DetFloat] { exp_f64(x) }",
        "E0700",
    );
    // Bit reinterpretation is exact, also under ConstDenorm.
    agree_with(
        "fn bits32() -> u32 eff [ConstDenorm] { to_bits_f32(from_bits_f32(1)) }
fn bits64() -> u64 eff [ConstDenorm] { to_bits_f64(from_bits_f64(1)) }
fn main() -> (u32, u64) { (bits32(), bits64()) }",
        "(1, 1)",
    );
    // ConstDenorm flushes f32 subnormal operands and results.
    agree_with(
        "fn flush(x: f32) -> f32 eff [ConstDenorm] { x * 1.0 }
fn keep(x: f32) -> f32 { x * 1.0 }
fn main() -> (u32, u32) { let tiny = from_bits_f32(1); (to_bits_f32(flush(tiny)), to_bits_f32(keep(tiny))) }",
        "(0, 1)",
    );
}

#[test]
fn lean_extraction_models_f32_arithmetic() {
    let source = include_str!("fixtures/floats/basic.ves");
    let c = accepted(source);
    let expected = agree(source);
    let mut output = lean::extract(&c.world).unwrap();
    assert!(output.contains("Float32"), "{output}");
    output.push_str("\n#eval Vesper.«main»\n");
    let compiler = std::env::var("VESPER_LEAN").unwrap_or_else(|_| "lean".into());
    if Command::new(&compiler).arg("--version").output().is_err() {
        eprintln!("Lean validation skipped: set VESPER_LEAN or install the pinned toolchain");
        return;
    }
    let path = std::env::temp_dir().join(format!(
        "vesper-floats-{}-{}.lean",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&path, &output).unwrap();
    let result = Command::new(&compiler).arg(&path).output().unwrap();
    fs::remove_file(&path).unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stderr),
        String::from_utf8_lossy(&result.stdout)
    );
    let words = |s: &str| -> Vec<String> {
        s.split(|c: char| !c.is_ascii_alphabetic())
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect()
    };
    assert_eq!(
        words(&String::from_utf8_lossy(&result.stdout)),
        words(&expected)
    );
}

#[test]
fn lean_and_wasm_refuse_with_typed_errors() {
    for source in [
        "fn main() -> f32 { let x: f32 = 2.0; sqrt(x) }",
        "fn main() -> f64 { fma(1.0, 2.0, 3.0) }",
        "fn main() -> u64 { to_bits_f64(1.0) }",
        "fn main() -> f64 { exp_f64(1.0) }",
    ] {
        let c = accepted(source);
        let error = lean::extract(&c.world).unwrap_err();
        assert!(error.contains("float intrinsic"), "{source}: {error}");
    }
    let c = accepted("fn main() -> f32 { 1.5 }");
    let error = vesper::wasm_backend::compile(&c.world, "main", &Default::default()).unwrap_err();
    assert!(
        matches!(
            error,
            vesper::wasm_backend::WasmError::Unsupported {
                construct: vesper::wasm_backend::Unsupported::Float32,
                ..
            }
        ),
        "{error:?}"
    );
}

/// An AOT executable computes the same corpus and transcendental hashes as
/// the evaluator (its exit status is 0 when they match).
#[test]
fn native_executables_compute_the_same_bits() {
    if !cfg!(unix) {
        return;
    }
    let mut source = corpus_program(FloatKind::F32, false);
    let expected = tuple_words(
        &Runtime::new(&accepted(&source).world)
            .run("main", vec![])
            .unwrap()
            .to_string(),
    );
    let transcendental =
        transcendental_program("sin", FloatKind::F32).replace("fn main()", "fn sines()");
    let sines = tuple_words(
        &Runtime::new(&accepted(&transcendental.replace("fn sines()", "fn main()")).world)
            .run("main", vec![])
            .unwrap()
            .to_string(),
    );
    source = source.replace("fn main()", "fn corpus()");
    source.push_str(&transcendental);
    source.push('\n');
    let fold = |name: &str, n: usize| {
        let names: Vec<String> = (0..n).map(|i| format!("w{i}")).collect();
        let mixes: String = names
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let previous = if i == 0 {
                    FNV_OFFSET.to_string()
                } else {
                    format!("h{}", i - 1)
                };
                format!("let h{i}: u64 = mix({previous}, {w}); ")
            })
            .collect();
        format!("let ({}) = {name}(); {mixes}h{}", names.join(", "), n - 1)
    };
    let hash = |words: &[u64]| words.iter().fold(FNV_OFFSET, |h, w| mix(h, *w));
    source.push_str(&format!(
        "fn corpus_hash() -> u64 {{ {} }}
fn sines_hash() -> u64 {{ {} }}
fn main() -> u8 {{ if corpus_hash() == {} && sines_hash() == {} {{ 0 }} else {{ 1 }} }}",
        fold("corpus", expected.len()),
        fold("sines", sines.len()),
        hash(&expected),
        hash(&sines)
    ));
    accepted(&source);
    let dir = std::env::temp_dir().join(format!(
        "vesper-floats-exe-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("floats.ves");
    fs::write(&file, &source).unwrap();
    let exe = dir.join("floats");
    let built = Command::new(env!("CARGO_BIN_EXE_vesper"))
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "vesper build failed\n{}{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    let run = Command::new(&exe).output().unwrap();
    fs::remove_dir_all(&dir).ok();
    assert_eq!(
        run.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
