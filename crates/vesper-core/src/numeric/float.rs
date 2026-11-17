//! The float semantics shared by checking, execution and codegen.
//!
//! # Float contract (`vesper.float.ieee754/1`)
//!
//! Every backend (checker, evaluator, Cranelift JIT and object backend)
//! implements this one contract. Lean extraction models `f64` as Lean's
//! `Float` and `f32` as `Float32` for the basic operations and refuses the
//! intrinsics below with a typed error; the Wasm, eBPF and verified backends
//! refuse `f32` with typed errors.
//!
//! * Float kinds are `f32` (IEEE 754 binary32) and `f64` (binary64).
//!   Operands of a binary operator have the same kind; there are no implicit
//!   conversions.
//! * An unsuffixed float literal has type `f32` where its context expects an
//!   `f32` (an annotation, a parameter, the other operand of an operator) and
//!   `f64` otherwise. An `f32` literal denotes the binary32 value nearest
//!   (ties to even) to the literal's binary64 value.
//! * `+`, `-`, `*`, `/`, `sqrt` and `fma` are the IEEE 754 operations,
//!   correctly rounded to nearest, ties to even; `fma(a, b, c)` computes
//!   `a * b + c` with one rounding. They never raise. There is no fast-math:
//!   backends do not reassociate, contract `a * b + c` into a fused
//!   multiply-add, or flush subnormals to zero (except inside
//!   `eff [ConstDenorm]`, which flushes operands and results).
//! * `%` is the exact remainder truncated toward zero (C `fmod`, Rust `%`):
//!   its sign is the dividend's.
//! * Prefix `-x` flips the sign bit and `abs(x)` clears it; both keep a NaN's
//!   payload.
//! * `min(a, b)` and `max(a, b)` are IEEE 754-2019 `minimum` and `maximum`:
//!   a NaN operand gives a NaN, and `-0.0` is less than `+0.0`.
//! * `floor`, `ceil` and `trunc` round to an integral value toward -∞, +∞
//!   and zero; `round` rounds to the nearest integral value with ties away
//!   from zero (C `round`). All four are exact and keep the sign of zero.
//! * Comparisons are IEEE 754: a NaN is unordered (every comparison but `!=`
//!   is false) and `-0.0 == +0.0`.
//! * NaN results: when an operation produces a NaN its sign and payload are
//!   unspecified (x86-64 and AArch64 differ), except in a program with an
//!   `eff [DetFloat]` function, where the result of every arithmetic
//!   operation that canonicalises (see [`MathOp::canonicalizes`]; `+ - * /`,
//!   `sqrt`, `fma`, `min`, `max`, `floor`, `ceil`, `round`, `trunc` and the
//!   float-to-float conversions) is the positive quiet NaN with an empty
//!   payload (`0x7fc00000`, `0x7ff8000000000000`). `%`, `-x` and `abs` do not
//!   canonicalise. `x != x` tests for a NaN portably.
//! * Conversions are explicit calls named after the target kind:
//!   - `widen_f64(x)` takes an `f32` or an integer kind of 32 bits or fewer,
//!     and `widen_f32(x)` an integer kind of 16 bits or fewer: they are exact,
//!     and the checker rejects any other operand.
//!   - `nearest_f32(x)` and `nearest_f64(x)` take any integer or float kind
//!     and round to nearest, ties to even. They are total: an `f64` too large
//!     for `f32` becomes an infinity and a NaN stays a NaN.
//!   - `narrow_k(x)` for an integer kind `k` and a float `x` truncates toward
//!     zero and raises `Fail[ArithOverflow]` when `x` is a NaN or the
//!     truncated value is outside `k`'s range.
//!   - `saturating_k(x)` truncates toward zero, clamps to `k`'s range and
//!     maps a NaN to zero (Rust's `as`). It is total.
//!   - `to_bits_f32(x) -> u32`, `to_bits_f64(x) -> u64`, `from_bits_f32(u32)`
//!     and `from_bits_f64(u64)` reinterpret the IEEE encoding exactly.
//! * `exp`, `ln`, `sin` and `cos` (`exp_f64`, `ln_f32`, ...) are ordinary
//!   Vesper source functions built only from the operations above, so they
//!   give the same bits on every target (see [`TRANSCENDENTALS`]).
use super::{ArithFailure, IntegerKind};
use crate::ast::Type;

/// Versioned identity of the float semantics. Change it whenever any rule
/// above changes.
pub const FLOAT_SEMANTICS: &str = "vesper.float.ieee754/1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FloatKind {
    F32,
    F64,
}
/// Every float kind, in source spelling order.
pub const FLOAT_KINDS: [FloatKind; 2] = [FloatKind::F32, FloatKind::F64];
/// Whether `name` spells a float kind.
pub fn is_float_name(name: &str) -> bool {
    FloatKind::from_name(name).is_some()
}
impl FloatKind {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "f32" => Some(Self::F32),
            "f64" => Some(Self::F64),
            _ => None,
        }
    }
    pub fn from_type(t: &Type) -> Option<Self> {
        match t {
            Type::Named(n, args) if args.is_empty() => Self::from_name(n),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::F32 => "f32",
            Self::F64 => "f64",
        }
    }
    pub fn ty(self) -> Type {
        Type::named(self.name())
    }
    pub fn bits(self) -> u32 {
        match self {
            Self::F32 => 32,
            Self::F64 => 64,
        }
    }
    /// Significand precision in bits, including the implicit bit.
    pub fn precision(self) -> u32 {
        match self {
            Self::F32 => 24,
            Self::F64 => 53,
        }
    }
    /// The unsigned integer kind of the same width (`to_bits` result).
    pub fn bits_kind(self) -> IntegerKind {
        match self {
            Self::F32 => IntegerKind::U32,
            Self::F64 => IntegerKind::U64,
        }
    }
    /// The canonical NaN's encoding (positive, quiet, empty payload).
    pub fn canonical_nan_bits(self) -> u64 {
        match self {
            Self::F32 => 0x7fc0_0000,
            Self::F64 => 0x7ff8_0000_0000_0000,
        }
    }
    /// Whether every value of the integer kind `k` is exactly a value of
    /// this float kind (`widen_f32`/`widen_f64` accept it).
    pub fn holds(self, k: IntegerKind) -> bool {
        let magnitude_bits = if k.signed() { k.bits() - 1 } else { k.bits() };
        magnitude_bits <= self.precision()
    }
}

/// A float value of either kind, the reference semantics' operand.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Float {
    F32(f32),
    F64(f64),
}
impl Float {
    pub fn kind(self) -> FloatKind {
        match self {
            Self::F32(_) => FloatKind::F32,
            Self::F64(_) => FloatKind::F64,
        }
    }
    /// The encoding, zero-extended to 64 bits.
    pub fn to_bits(self) -> u64 {
        match self {
            Self::F32(x) => u64::from(x.to_bits()),
            Self::F64(x) => x.to_bits(),
        }
    }
    /// The value of `kind` with the low `bits(kind)` bits of `bits`.
    pub fn from_bits(kind: FloatKind, bits: u64) -> Self {
        match kind {
            FloatKind::F32 => Self::F32(f32::from_bits(bits as u32)),
            FloatKind::F64 => Self::F64(f64::from_bits(bits)),
        }
    }
    pub fn is_nan(self) -> bool {
        match self {
            Self::F32(x) => x.is_nan(),
            Self::F64(x) => x.is_nan(),
        }
    }
    /// The value as an `f64` (exact).
    pub fn widen(self) -> f64 {
        match self {
            Self::F32(x) => f64::from(x),
            Self::F64(x) => x,
        }
    }
    /// The canonical NaN of this value's kind when it is a NaN.
    pub fn canonical(self) -> Self {
        if self.is_nan() {
            Self::from_bits(self.kind(), self.kind().canonical_nan_bits())
        } else {
            self
        }
    }
}

/// The math intrinsics on both float kinds. They take operands of one kind
/// and return that kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MathOp {
    Sqrt,
    Abs,
    Floor,
    Ceil,
    Round,
    Trunc,
    Min,
    Max,
    Fma,
}
impl MathOp {
    pub const ALL: [MathOp; 9] = [
        Self::Sqrt,
        Self::Abs,
        Self::Floor,
        Self::Ceil,
        Self::Round,
        Self::Trunc,
        Self::Min,
        Self::Max,
        Self::Fma,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Sqrt => "sqrt",
            Self::Abs => "abs",
            Self::Floor => "floor",
            Self::Ceil => "ceil",
            Self::Round => "round",
            Self::Trunc => "trunc",
            Self::Min => "min",
            Self::Max => "max",
            Self::Fma => "fma",
        }
    }
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|op| op.name() == name)
    }
    pub fn arity(self) -> usize {
        match self {
            Self::Min | Self::Max => 2,
            Self::Fma => 3,
            _ => 1,
        }
    }
    /// Whether a NaN result is canonicalised in a `DetFloat` program (only
    /// `abs`, a sign-bit operation, keeps its operand's NaN).
    pub fn canonicalizes(self) -> bool {
        self != Self::Abs
    }
    /// The reference semantics. `args` has [`Self::arity`] operands of one
    /// kind.
    pub fn apply(self, args: &[Float]) -> Option<Float> {
        Some(match args {
            [Float::F32(a)] => Float::F32(match self {
                Self::Sqrt => a.sqrt(),
                Self::Abs => a.abs(),
                Self::Floor => a.floor(),
                Self::Ceil => a.ceil(),
                Self::Round => a.round(),
                Self::Trunc => a.trunc(),
                _ => return None,
            }),
            [Float::F64(a)] => Float::F64(match self {
                Self::Sqrt => a.sqrt(),
                Self::Abs => a.abs(),
                Self::Floor => a.floor(),
                Self::Ceil => a.ceil(),
                Self::Round => a.round(),
                Self::Trunc => a.trunc(),
                _ => return None,
            }),
            [Float::F32(a), Float::F32(b)] => Float::F32(match self {
                Self::Min => minimum_f32(*a, *b),
                Self::Max => maximum_f32(*a, *b),
                _ => return None,
            }),
            [Float::F64(a), Float::F64(b)] => Float::F64(match self {
                Self::Min => minimum_f64(*a, *b),
                Self::Max => maximum_f64(*a, *b),
                _ => return None,
            }),
            [Float::F32(a), Float::F32(b), Float::F32(c)] if self == Self::Fma => {
                Float::F32(a.mul_add(*b, *c))
            }
            [Float::F64(a), Float::F64(b), Float::F64(c)] if self == Self::Fma => {
                Float::F64(a.mul_add(*b, *c))
            }
            _ => return None,
        })
    }
}
/// The math intrinsic spelled `name`.
pub fn math_intrinsic(name: &str) -> Option<MathOp> {
    MathOp::from_name(name)
}
macro_rules! min_max {
    ($min:ident, $max:ident, $t:ty) => {
        /// IEEE 754-2019 `minimum`: NaN-propagating, `-0.0 < +0.0`.
        pub fn $min(a: $t, b: $t) -> $t {
            if a.is_nan() || b.is_nan() {
                a + b
            } else if a == b {
                // Equal operands differ at most in the sign of zero.
                if a.is_sign_negative() { a } else { b }
            } else if a < b {
                a
            } else {
                b
            }
        }
        /// IEEE 754-2019 `maximum`: NaN-propagating, `-0.0 < +0.0`.
        pub fn $max(a: $t, b: $t) -> $t {
            if a.is_nan() || b.is_nan() {
                a + b
            } else if a == b {
                if a.is_sign_negative() { b } else { a }
            } else if a > b {
                a
            } else {
                b
            }
        }
    };
}
min_max!(minimum_f32, maximum_f32, f32);
min_max!(minimum_f64, maximum_f64, f64);

/// Conversions to a float kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatConversion {
    /// `widen_<f>(x)`: exact; the checker rejects an operand kind that has
    /// values `f` cannot represent.
    Widen,
    /// `nearest_<f>(x)`: round to nearest, ties to even; total.
    Nearest,
}
impl FloatConversion {
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Widen => "widen_",
            Self::Nearest => "nearest_",
        }
    }
    pub fn name(self, to: FloatKind) -> String {
        format!("{}{}", self.prefix(), to.name())
    }
}
/// The conversion to a float kind spelled `name`.
pub fn float_conversion(name: &str) -> Option<(FloatConversion, FloatKind)> {
    [FloatConversion::Widen, FloatConversion::Nearest]
        .into_iter()
        .find_map(|c| Some((c, FloatKind::from_name(name.strip_prefix(c.prefix())?)?)))
}
/// Whether every value of `from` is exactly a value of `to`.
pub fn float_widens_to(from: FloatKind, to: FloatKind) -> bool {
    from <= to
}
/// The total float-to-integer conversion prefix (`saturating_<k>`).
pub const SATURATING_PREFIX: &str = "saturating_";
/// The integer kind of the saturating conversion spelled `name`.
pub fn saturating_conversion(name: &str) -> Option<IntegerKind> {
    IntegerKind::from_name(name.strip_prefix(SATURATING_PREFIX)?)
}
/// `x` converted to the float kind `to`, rounding to nearest, ties to even.
pub fn nearest_float(to: FloatKind, x: Float) -> Float {
    match (to, x) {
        (FloatKind::F32, Float::F64(x)) => Float::F32(x as f32),
        (FloatKind::F64, Float::F32(x)) => Float::F64(f64::from(x)),
        (_, x) => x,
    }
}
/// The integer value `n` (in range of some integer kind) converted to the
/// float kind `to`, rounding to nearest, ties to even.
pub fn integer_to_float(to: FloatKind, n: i128) -> Float {
    // Every in-range integer is an i64 or a u64; Rust's `as` rounds both
    // correctly (one rounding).
    match to {
        FloatKind::F32 if n < 0 => Float::F32(n as i64 as f32),
        FloatKind::F32 => Float::F32(n as u64 as f32),
        FloatKind::F64 if n < 0 => Float::F64(n as i64 as f64),
        FloatKind::F64 => Float::F64(n as u64 as f64),
    }
}
/// The exclusive real bounds `(lo, hi)` such that a truncated float `t` is
/// in `k`'s range exactly when `lo <= t < hi`. Both are powers of two (or
/// zero), so they are exact in every float kind.
pub fn truncation_bounds(k: IntegerKind) -> (f64, f64) {
    if k.signed() {
        let half = 2f64.powi(k.bits() as i32 - 1);
        (-half, half)
    } else {
        (0.0, 2f64.powi(k.bits() as i32))
    }
}
/// Checked `narrow_k(x)` of a float: truncation toward zero; a NaN or an
/// out-of-range value raises `Fail[ArithOverflow]`.
pub fn float_to_integer_checked(k: IntegerKind, x: Float) -> Result<i128, ArithFailure> {
    let t = x.widen().trunc();
    let (lo, hi) = truncation_bounds(k);
    if lo <= t && t < hi {
        Ok(t as i128)
    } else {
        Err(ArithFailure::Overflow)
    }
}
/// Total `saturating_k(x)`: truncation toward zero, clamped to `k`'s range,
/// NaN to zero.
pub fn float_to_integer_saturating(k: IntegerKind, x: Float) -> i128 {
    let t = x.widen().trunc();
    if t.is_nan() {
        0
    } else if t <= k.min() as f64 {
        k.min()
    } else if t >= k.max() as f64 {
        k.max()
    } else {
        t as i128
    }
}

/// The bit-reinterpretation intrinsics: `to_bits_<f>` (`true`) and
/// `from_bits_<f>` (`false`).
pub fn bits_intrinsic(name: &str) -> Option<(bool, FloatKind)> {
    if let Some(k) = name.strip_prefix("to_bits_") {
        return Some((true, FloatKind::from_name(k)?));
    }
    Some((
        false,
        FloatKind::from_name(name.strip_prefix("from_bits_")?)?,
    ))
}

/// Whether `name` is a float intrinsic: a math operation, a conversion to a
/// float kind, a saturating conversion or a bit reinterpretation. (A
/// checked `narrow_k` of a float is an integer [`super::conversion`].)
pub fn is_float_intrinsic(name: &str) -> bool {
    math_intrinsic(name).is_some()
        || float_conversion(name).is_some()
        || saturating_conversion(name).is_some()
        || bits_intrinsic(name).is_some()
}

/// The transcendental source functions (`vesper.float.ieee754/1`): one per
/// function and float kind, spelled `<function>_<kind>`.
///
/// The `f64` functions are fdlibm's algorithms (as in musl): `exp` and `ln`
/// are within one ulp; `sin` and `cos` reduce their argument with a
/// three-part Cody–Waite reduction by π/2, accurate for `|x| < 2^20·π/2`.
/// Larger arguments still give the same bits on every target, but the
/// reduction loses accuracy (Payne–Hanek reduction is not implemented). The
/// `f32` functions evaluate the `f64` function and round once.
pub const TRANSCENDENTALS: [&str; 4] = ["exp", "ln", "sin", "cos"];
/// The transcendental function `name`: its function and float kind.
pub fn transcendental(name: &str) -> Option<(&'static str, FloatKind)> {
    let (f, k) = name.rsplit_once('_')?;
    let f = TRANSCENDENTALS.into_iter().find(|t| *t == f)?;
    Some((f, FloatKind::from_name(k)?))
}
/// The helper functions each transcendental's source calls.
fn transcendental_helpers(name: &str) -> &'static [&'static str] {
    match name {
        "sin_f64" | "cos_f64" => &[
            "float_kernel_sin_f64",
            "float_kernel_cos_f64",
            "float_rem_pio2_f64",
        ],
        "exp_f32" => &["exp_f64"],
        "ln_f32" => &["ln_f64"],
        "sin_f32" => &["sin_f64"],
        "cos_f32" => &["cos_f64"],
        _ => &[],
    }
}
/// Every source function `name` needs (itself and its helpers,
/// transitively), in a stable order.
pub fn transcendental_closure(name: &str) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    fn visit(name: &str, out: &mut Vec<&'static str>) {
        let Some(known) = ALL_SOURCES.iter().find(|n| **n == name) else {
            return;
        };
        if out.contains(known) {
            return;
        }
        out.push(known);
        for h in transcendental_helpers(name) {
            visit(h, out);
        }
    }
    visit(name, &mut out);
    out
}
const ALL_SOURCES: [&str; 11] = [
    "exp_f64",
    "ln_f64",
    "sin_f64",
    "cos_f64",
    "exp_f32",
    "ln_f32",
    "sin_f32",
    "cos_f32",
    "float_kernel_sin_f64",
    "float_kernel_cos_f64",
    "float_rem_pio2_f64",
];
/// Vesper source of the transcendental function or helper `name`.
pub fn transcendental_source(name: &str) -> Option<&'static str> {
    Some(match name {
        "exp_f64" => EXP_F64,
        "ln_f64" => LN_F64,
        "sin_f64" => SIN_F64,
        "cos_f64" => COS_F64,
        "exp_f32" => "fn exp_f32(x: f32) -> f32 { nearest_f32(exp_f64(widen_f64(x))) }",
        "ln_f32" => "fn ln_f32(x: f32) -> f32 { nearest_f32(ln_f64(widen_f64(x))) }",
        "sin_f32" => "fn sin_f32(x: f32) -> f32 { nearest_f32(sin_f64(widen_f64(x))) }",
        "cos_f32" => "fn cos_f32(x: f32) -> f32 { nearest_f32(cos_f64(widen_f64(x))) }",
        "float_kernel_sin_f64" => KERNEL_SIN,
        "float_kernel_cos_f64" => KERNEL_COS,
        "float_rem_pio2_f64" => REM_PIO2,
        _ => return None,
    })
}

// fdlibm e_exp.c (musl exp.c): k = round(x / ln2), r = x - k*ln2 in two
// parts, a rational approximation of exp(r), and a scale by 2^k.
const EXP_F64: &str = r#"fn exp_f64(x: f64) -> f64 {
    let bits: u64 = to_bits_f64(x);
    let hx: u64 = (bits >> 32) & 2147483647;
    let negative: bool = (bits >> 63) == 1;
    if x != x {
        x
    } else if x > 709.782712893383973096 {
        from_bits_f64(9218868437227405312)
    } else if x < -745.13321910194110842 {
        0.0
    } else if hx < 1043333120 {
        1.0 + x
    } else {
        let k: i64 = if hx > 1071001154 {
            if hx < 1072734898 {
                if negative { -1 } else { 1 }
            } else {
                let half: f64 = if negative { -0.5 } else { 0.5 };
                saturating_i64(1.44269504088896338700 * x + half)
            }
        } else {
            0
        };
        let kf: f64 = nearest_f64(k);
        let hi: f64 = x - kf * 6.93147180369123816490e-01;
        let lo: f64 = kf * 1.90821492927058770002e-10;
        let r: f64 = if k == 0 { x } else { hi - lo };
        let t: f64 = r * r;
        let c: f64 = r - t * (1.66666666666666019037e-01 + t * (-2.77777777770155933842e-03 + t * (6.61375632143793436117e-05 + t * (-1.65339022054652515390e-06 + t * 4.13813679705723846039e-08))));
        if k == 0 {
            1.0 - ((r * c) / (c - 2.0) - r)
        } else {
            let y: f64 = 1.0 - ((lo - (r * c) / (2.0 - c)) - hi);
            if k > 1023 {
                y * 2.0 * from_bits_f64(9214364837600034816)
            } else if k >= -1021 {
                y * from_bits_f64(wrapping_u64(wrapping_add(k, 1023)) << 52)
            } else {
                y * from_bits_f64(wrapping_u64(wrapping_add(k, 2023)) << 52) * from_bits_f64(103582791429521408)
            }
        }
    }
}"#;

// fdlibm e_log.c: x = 2^k * (1 + f) with sqrt(2)/2 < 1 + f < sqrt(2), and
// log(1 + f) = 2s + 2s/3 s^2 + ... with s = f / (2 + f).
const LN_F64: &str = r#"fn ln_f64(x: f64) -> f64 {
    let raw: u64 = to_bits_f64(x);
    if x != x {
        x
    } else if (raw & 9223372036854775807) == 0 {
        from_bits_f64(18442240474082181120)
    } else if (raw >> 63) == 1 {
        from_bits_f64(9221120237041090560)
    } else if raw >= 9218868437227405312 {
        x
    } else {
        let subnormal: bool = raw < 4503599627370496;
        let scaled: f64 = if subnormal { x * 1.80143985094819840000e+16 } else { x };
        let bits: u64 = to_bits_f64(scaled);
        let high: u64 = bits >> 32;
        let bias: i64 = if subnormal { 1077 } else { 1023 };
        let k0: i64 = wrapping_sub(wrapping_i64(high >> 20), bias);
        let hx: u64 = high & 1048575;
        let i: u64 = wrapping_add(hx, 614244) & 1048576;
        let k: i64 = wrapping_add(k0, wrapping_i64(i >> 20));
        let m: f64 = from_bits_f64((((hx | (i ^ 1072693248))) << 32) | (bits & 4294967295));
        let f: f64 = m - 1.0;
        let dk: f64 = nearest_f64(k);
        if (wrapping_add(hx, 2) & 1048575) < 3 {
            if f == 0.0 {
                if k == 0 { 0.0 } else { dk * 6.93147180369123816490e-01 + dk * 1.90821492927058770002e-10 }
            } else {
                let r: f64 = f * f * (0.5 - 0.33333333333333333 * f);
                if k == 0 { f - r } else { dk * 6.93147180369123816490e-01 - ((r - dk * 1.90821492927058770002e-10) - f) }
            }
        } else {
            let s: f64 = f / (2.0 + f);
            let z: f64 = s * s;
            let w: f64 = z * z;
            let t1: f64 = w * (3.999999999940941908e-01 + w * (2.222219843214978396e-01 + w * 1.531383769920937332e-01));
            let t2: f64 = z * (6.666666666666735130e-01 + w * (2.857142874366239149e-01 + w * (1.818357216161805012e-01 + w * 1.479819860511658591e-01)));
            let r: f64 = t2 + t1;
            let large: bool = hx >= 398458 && hx <= 440401;
            if large {
                let hfsq: f64 = 0.5 * f * f;
                if k == 0 { f - (hfsq - s * (hfsq + r)) } else { dk * 6.93147180369123816490e-01 - ((hfsq - (s * (hfsq + r) + dk * 1.90821492927058770002e-10)) - f) }
            } else {
                if k == 0 { f - s * (f - r) } else { dk * 6.93147180369123816490e-01 - ((s * (f - r) - dk * 1.90821492927058770002e-10) - f) }
            }
        }
    }
}"#;

// musl __sin.c: sin(x + y) on [-pi/4, pi/4]; `tail` says whether y is used.
const KERNEL_SIN: &str = r#"fn float_kernel_sin_f64(x: f64, y: f64, tail: bool) -> f64 {
    let z: f64 = x * x;
    let w: f64 = z * z;
    let r: f64 = 8.33333333332248946124e-03 + z * (-1.98412698298579493134e-04 + z * 2.75573137070700676789e-06) + z * w * (-2.50507602534068634195e-08 + z * 1.58969099521155010221e-10);
    let v: f64 = z * x;
    if tail {
        x - ((z * (0.5 * y - v * r) - y) - v * -1.66666666666666324348e-01)
    } else {
        x + v * (-1.66666666666666324348e-01 + z * r)
    }
}"#;

// musl __cos.c: cos(x + y) on [-pi/4, pi/4].
const KERNEL_COS: &str = r#"fn float_kernel_cos_f64(x: f64, y: f64) -> f64 {
    let z: f64 = x * x;
    let w: f64 = z * z;
    let r: f64 = z * (4.16666666666666019037e-02 + z * (-1.38888888888741095749e-03 + z * 2.48015872894767294178e-05)) + w * w * (-2.75573143513906633035e-07 + z * (2.08757232129817482790e-09 + z * -1.13596475577881948265e-11));
    let hz: f64 = 0.5 * z;
    let one_minus: f64 = 1.0 - hz;
    one_minus + (((1.0 - one_minus) - hz) + (z * r - x * y))
}"#;

// musl __rem_pio2.c, medium path only: x - n*pi/2 as y0 + y1 with up to
// three Cody-Waite steps, and n mod 4.
const REM_PIO2: &str = r#"fn float_rem_pio2_f64(x: f64) -> (i64, f64, f64) {
    let ex: i64 = wrapping_i64((to_bits_f64(x) >> 52) & 2047);
    let fnv: f64 = x * 6.36619772367581382433e-01 + 6755399441055744.0 - 6755399441055744.0;
    let n: i64 = saturating_i64(fnv) & 3;
    let r1: f64 = x - fnv * 1.57079632673412561417e+00;
    let w1: f64 = fnv * 6.07710050650619224932e-11;
    let y1: f64 = r1 - w1;
    let ey1: i64 = wrapping_i64((to_bits_f64(y1) >> 52) & 2047);
    if wrapping_sub(ex, ey1) > 16 {
        let t2: f64 = r1;
        let v2: f64 = fnv * 6.07710050630396597660e-11;
        let r2: f64 = t2 - v2;
        let w2: f64 = fnv * 2.02226624879595063154e-21 - ((t2 - r2) - v2);
        let y2: f64 = r2 - w2;
        let ey2: i64 = wrapping_i64((to_bits_f64(y2) >> 52) & 2047);
        if wrapping_sub(ex, ey2) > 49 {
            let t3: f64 = r2;
            let v3: f64 = fnv * 2.02226624871116645580e-21;
            let r3: f64 = t3 - v3;
            let w3: f64 = fnv * 8.47842766036889956997e-32 - ((t3 - r3) - v3);
            let y3: f64 = r3 - w3;
            (n, y3, (r3 - y3) - w3)
        } else {
            (n, y2, (r2 - y2) - w2)
        }
    } else {
        (n, y1, (r1 - y1) - w1)
    }
}"#;

// musl sin.c.
const SIN_F64: &str = r#"fn sin_f64(x: f64) -> f64 {
    let ix: u64 = (to_bits_f64(x) >> 32) & 2147483647;
    if ix <= 1072243195 {
        if ix < 1045430272 {
            x
        } else {
            float_kernel_sin_f64(x, 0.0, false)
        }
    } else if x != x {
        x
    } else if ix >= 2146435072 {
        from_bits_f64(9221120237041090560)
    } else {
        let (n, y0, y1) = float_rem_pio2_f64(x);
        if n == 0 {
            float_kernel_sin_f64(y0, y1, true)
        } else if n == 1 {
            float_kernel_cos_f64(y0, y1)
        } else if n == 2 {
            -float_kernel_sin_f64(y0, y1, true)
        } else {
            -float_kernel_cos_f64(y0, y1)
        }
    }
}"#;

// musl cos.c.
const COS_F64: &str = r#"fn cos_f64(x: f64) -> f64 {
    let ix: u64 = (to_bits_f64(x) >> 32) & 2147483647;
    if ix <= 1072243195 {
        if ix < 1044816030 {
            1.0
        } else {
            float_kernel_cos_f64(x, 0.0)
        }
    } else if x != x {
        x
    } else if ix >= 2146435072 {
        from_bits_f64(9221120237041090560)
    } else {
        let (n, y0, y1) = float_rem_pio2_f64(x);
        if n == 0 {
            float_kernel_cos_f64(y0, y1)
        } else if n == 1 {
            -float_kernel_sin_f64(y0, y1, true)
        } else if n == 2 {
            -float_kernel_cos_f64(y0, y1)
        } else {
            float_kernel_sin_f64(y0, y1, true)
        }
    }
}"#;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names() {
        assert_eq!(FloatKind::from_name("f32"), Some(FloatKind::F32));
        assert_eq!(math_intrinsic("fma").map(MathOp::arity), Some(3));
        assert_eq!(
            float_conversion("nearest_f32"),
            Some((FloatConversion::Nearest, FloatKind::F32))
        );
        assert_eq!(float_conversion("widen_u64"), None);
        assert_eq!(
            saturating_conversion("saturating_i8"),
            Some(IntegerKind::I8)
        );
        assert_eq!(bits_intrinsic("to_bits_f64"), Some((true, FloatKind::F64)));
        assert_eq!(
            bits_intrinsic("from_bits_f32"),
            Some((false, FloatKind::F32))
        );
        assert_eq!(transcendental("exp_f32"), Some(("exp", FloatKind::F32)));
        assert_eq!(transcendental("tan_f32"), None);
        assert_eq!(
            transcendental_closure("sin_f32"),
            vec![
                "sin_f32",
                "sin_f64",
                "float_kernel_sin_f64",
                "float_kernel_cos_f64",
                "float_rem_pio2_f64"
            ]
        );
        assert!(FloatKind::F64.holds(IntegerKind::U32));
        assert!(FloatKind::F64.holds(IntegerKind::I32));
        assert!(!FloatKind::F64.holds(IntegerKind::I64));
        assert!(FloatKind::F32.holds(IntegerKind::I16));
        assert!(!FloatKind::F32.holds(IntegerKind::U32));
    }
    #[test]
    fn minimum_maximum() {
        assert!(minimum_f64(-0.0, 0.0).is_sign_negative());
        assert!(minimum_f64(0.0, -0.0).is_sign_negative());
        assert!(maximum_f64(-0.0, 0.0).is_sign_positive());
        assert!(maximum_f32(0.0, -0.0).is_sign_positive());
        assert!(minimum_f64(f64::NAN, 1.0).is_nan());
        assert!(maximum_f32(1.0, f32::NAN).is_nan());
        assert_eq!(minimum_f64(1.0, 2.0), 1.0);
        assert_eq!(maximum_f64(1.0, 2.0), 2.0);
        assert_eq!(
            MathOp::Round.apply(&[Float::F64(-2.5)]),
            Some(Float::F64(-3.0))
        );
        assert_eq!(
            MathOp::Fma.apply(&[Float::F32(1.0), Float::F32(2.0), Float::F32(3.0)]),
            Some(Float::F32(5.0))
        );
    }
    #[test]
    fn float_integer_conversions() {
        use IntegerKind as K;
        assert_eq!(float_to_integer_checked(K::I8, Float::F64(127.9)), Ok(127));
        assert_eq!(
            float_to_integer_checked(K::I8, Float::F64(-128.9)),
            Ok(-128)
        );
        assert_eq!(
            float_to_integer_checked(K::I8, Float::F64(128.0)),
            Err(ArithFailure::Overflow)
        );
        assert_eq!(float_to_integer_checked(K::U8, Float::F64(-0.9)), Ok(0));
        assert_eq!(
            float_to_integer_checked(K::U64, Float::F64(f64::NAN)),
            Err(ArithFailure::Overflow)
        );
        assert_eq!(
            float_to_integer_checked(K::I64, Float::F64(-9223372036854775808.0)),
            Ok(i64::MIN as i128)
        );
        assert_eq!(
            float_to_integer_checked(K::I64, Float::F64(9223372036854775808.0)),
            Err(ArithFailure::Overflow)
        );
        assert_eq!(float_to_integer_saturating(K::U8, Float::F32(300.0)), 255);
        assert_eq!(float_to_integer_saturating(K::I16, Float::F32(f32::NAN)), 0);
        assert_eq!(
            float_to_integer_saturating(K::I64, Float::F64(f64::INFINITY)),
            i64::MAX as i128
        );
        assert_eq!(
            float_to_integer_saturating(K::U64, Float::F64(1e300)),
            u64::MAX as i128
        );
        assert_eq!(
            integer_to_float(FloatKind::F32, 16777217),
            Float::F32(16777216.0)
        );
        assert_eq!(
            integer_to_float(FloatKind::F64, u64::MAX as i128),
            Float::F64(18446744073709551616.0)
        );
        assert_eq!(
            nearest_float(FloatKind::F32, Float::F64(1e300)),
            Float::F32(f32::INFINITY)
        );
    }
}
