//! The integer semantics shared by checking, execution, optimization and codegen.
//!
//! # Checked arithmetic contract (`vesper.arith.checked/1`)
//!
//! Every backend (checker facts, evaluator, both optimizers, the Cranelift
//! JIT/object backend and Lean extraction) implements this one contract:
//!
//! * Integer kinds are `u8`, `u16`, `u32`, `u64`, `usize`, `i8`, `i16`,
//!   `i32`, `i64` and `isize`. `usize` and `isize` are 64 bits wide on every
//!   current target (they are distinct types from `u64` and `i64` with the
//!   same values). Signed kinds are two's complement. Operands of a binary
//!   operator have the same kind, except the amount of a shift; there are no
//!   implicit conversions.
//! * `+`, `-`, `*`, their compound assignments and value-level negation `-x`
//!   compute the exact mathematical result. If it is outside the operand kind
//!   they raise `Fail[ArithOverflow]` with the builtin value `ArithOverflow {}`.
//!   Unsigned negation therefore succeeds only for zero. The literal spelling
//!   `-9223372036854775808` is a literal, not a negation, and is in range.
//! * Integer `/` and `%` by zero raise `Fail[RuntimeError]` with
//!   `RuntimeError::DivisionByZero`; the divisor check precedes the overflow
//!   check. For every signed kind `MIN / -1` raises `Fail[ArithOverflow]` and
//!   `MIN % -1` is `0`. Division truncates toward zero and the remainder has
//!   the sign of the dividend.
//! * Dynamic indexing keeps `Fail[RuntimeError]` (`IndexOutOfBounds`).
//! * `wrapping_add`, `wrapping_sub` and `wrapping_mul` take two operands of
//!   one integer kind and return their result modulo `2^bits`. They are pure
//!   and total and never raise.
//! * `&`, `|`, `^` and prefix `~` act on the two's-complement bit pattern of
//!   one integer kind and are total.
//! * `a << n` and `a >> n` take `a` of any integer kind and `n` of any
//!   integer kind (an unsuffixed literal amount is a `u32`). An amount outside
//!   `0..bits(a)` raises `Fail[ArithOverflow]`, as Rust's `checked_shl`
//!   reports overflow (a new `RuntimeError` variant would break every
//!   exhaustive `match` on `RuntimeError`). `<<` discards the bits shifted
//!   out; `>>` is logical on unsigned kinds and arithmetic on signed kinds.
//! * `rotate_left(a, n)` and `rotate_right(a, n)` rotate the bit pattern by
//!   `n` modulo `bits(a)` (a negative `n` rotates the other way) and are
//!   total.
//! * Conversions are explicit calls named after the target kind `k`:
//!   `widen_k(x)` is lossless and rejected by the checker unless every value
//!   of `x`'s kind is a `k`; `narrow_k(x)` raises `Fail[ArithOverflow]` when
//!   `x` is outside `k`'s range; `wrapping_k(x)` is total and keeps the low
//!   `bits(k)` bits, read in `k`'s two's complement.
//! * Byte order: `to_le_bytes_k`, `to_be_bytes_k`, `from_le_bytes_k` and
//!   `from_be_bytes_k` for every kind `k` of 16 bits or more convert between
//!   `k` and a tuple of `bits(k) / 8` `u8`s. They are total.
//! * `f64` arithmetic keeps IEEE-754 semantics and never raises.
//! * A failing compound assignment raises before its write, so the target
//!   keeps its previous value.
//! * The checker may discharge a failure only from a proof that the exact
//!   result is in range (currently: constant facts). Discharged operations
//!   compute the same value under checked and wrapping semantics.
//! * An unlabeled `Fail[E]` request with known `E` selects the innermost
//!   visible `Fail` capability whose arguments are exactly `E`, and otherwise
//!   the innermost `Fail` capability (see [`instance_key`]).
use crate::ast::Type;

/// Versioned identity of the integer semantics. Change it whenever any rule
/// above changes, since every backend and proof artifact depends on it.
pub const ARITHMETIC_SEMANTICS: &str = "vesper.arith.checked/1";
/// The reserved builtin copy type carried by `Fail[ArithOverflow]`.
pub const OVERFLOW_TYPE: &str = "ArithOverflow";
/// Builtin declaration source for the overflow error. Its single constructor
/// is the empty struct literal `ArithOverflow {}`.
pub const OVERFLOW_DECLARATION: &str = "copy struct ArithOverflow {}";
/// Explicit wrapping intrinsics and the binary operator each wraps.
pub const WRAPPING_INTRINSICS: [(&str, &str); 3] = [
    ("wrapping_add", "+"),
    ("wrapping_sub", "-"),
    ("wrapping_mul", "*"),
];
/// The operator implemented by a wrapping intrinsic name.
pub fn wrapping_intrinsic(name: &str) -> Option<&'static str> {
    WRAPPING_INTRINSICS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, op)| *op)
}
/// The intrinsic name for a wrapping operator, as used in IR (`Op::Binary`).
pub fn wrapping_name(op: &str) -> Option<&'static str> {
    WRAPPING_INTRINSICS
        .iter()
        .find(|(_, o)| *o == op)
        .map(|(n, _)| *n)
}
/// Whether an operator may raise `Fail[ArithOverflow]` on integer operands.
pub fn may_overflow(op: &str, kind: IntegerKind) -> bool {
    matches!(op, "+" | "-" | "*" | "+=" | "-=" | "*=")
        || (kind.signed() && matches!(op, "/" | "/="))
}
/// A primitive arithmetic failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArithFailure {
    /// `Fail[ArithOverflow]`.
    Overflow,
    /// `Fail[RuntimeError]` with `RuntimeError::DivisionByZero`.
    DivisionByZero,
}
/// Runtime key of an unlabeled effect instance with concrete arguments, such
/// as `$effect:Fail[ArithOverflow]`. Backends install it beside the
/// by-name `$effect:Fail` key and prefer it for typed requests.
pub fn instance_key(name: &str, args: &[Type]) -> Option<String> {
    if args.is_empty() || args.iter().any(open) {
        return None;
    }
    Some(format!(
        "$effect:{name}[{}]",
        args.iter()
            .map(|t| default_type(t).to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}
fn open(t: &Type) -> bool {
    match t {
        Type::Infer | Type::Row(_) => true,
        Type::Named(_, ts) | Type::Tuple(ts) => ts.iter().any(open),
        Type::Function { .. } => true,
        Type::IntLiteral(_) => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IntegerKind {
    U8,
    U32,
    U64,
    I64,
    U16,
    Usize,
    I8,
    I16,
    I32,
    Isize,
}
/// Every integer kind, in source spelling order.
pub const INTEGER_KINDS: [IntegerKind; 10] = [
    IntegerKind::U8,
    IntegerKind::U16,
    IntegerKind::U32,
    IntegerKind::U64,
    IntegerKind::Usize,
    IntegerKind::I8,
    IntegerKind::I16,
    IntegerKind::I32,
    IntegerKind::I64,
    IntegerKind::Isize,
];
/// Whether `name` spells an integer kind.
pub fn is_integer_name(name: &str) -> bool {
    IntegerKind::from_name(name).is_some()
}
/// Whether `name` spells a builtin scalar type: an integer kind, `f64`,
/// `bool`, `Unit` or `Never`. Every scalar is one 64-bit word in every
/// backend's value layout.
pub fn is_scalar_name(name: &str) -> bool {
    is_integer_name(name) || matches!(name, "f64" | "bool" | "Unit" | "Never")
}
impl IntegerKind {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "u8" => Self::U8,
            "u16" => Self::U16,
            "u32" => Self::U32,
            "u64" => Self::U64,
            "usize" => Self::Usize,
            "i8" => Self::I8,
            "i16" => Self::I16,
            "i32" => Self::I32,
            "i64" => Self::I64,
            "isize" => Self::Isize,
            _ => return None,
        })
    }
    pub fn from_type(t: &Type) -> Option<Self> {
        if let Type::IntLiteral(n) = t {
            return Some(Self::default_for(*n));
        }
        Self::from_name(t.name())
    }
    pub fn default_for(n: i128) -> Self {
        if n > i64::MAX as i128 {
            Self::U64
        } else {
            Self::I64
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::U8 => "u8",
            Self::U16 => "u16",
            Self::U32 => "u32",
            Self::U64 => "u64",
            Self::Usize => "usize",
            Self::I8 => "i8",
            Self::I16 => "i16",
            Self::I32 => "i32",
            Self::I64 => "i64",
            Self::Isize => "isize",
        }
    }
    pub fn ty(self) -> Type {
        Type::named(self.name())
    }
    /// Width in bits. `usize` and `isize` are 64 bits on every target.
    pub fn bits(self) -> u32 {
        match self {
            Self::U8 | Self::I8 => 8,
            Self::U16 | Self::I16 => 16,
            Self::U32 | Self::I32 => 32,
            Self::U64 | Self::Usize | Self::I64 | Self::Isize => 64,
        }
    }
    pub fn unsigned(self) -> bool {
        matches!(
            self,
            Self::U8 | Self::U16 | Self::U32 | Self::U64 | Self::Usize
        )
    }
    pub fn signed(self) -> bool {
        !self.unsigned()
    }
    /// The kind with the same width and the other signedness.
    pub fn flip_sign(self) -> Self {
        match self {
            Self::U8 => Self::I8,
            Self::U16 => Self::I16,
            Self::U32 => Self::I32,
            Self::U64 => Self::I64,
            Self::Usize => Self::Isize,
            Self::I8 => Self::U8,
            Self::I16 => Self::U16,
            Self::I32 => Self::U32,
            Self::I64 => Self::U64,
            Self::Isize => Self::Usize,
        }
    }
    pub fn fits(self, n: i128) -> bool {
        (self.min()..=self.max()).contains(&n)
    }
    pub fn min(self) -> i128 {
        if self.signed() {
            -(1i128 << (self.bits() - 1))
        } else {
            0
        }
    }
    pub fn max(self) -> i128 {
        if self.signed() {
            (1i128 << (self.bits() - 1)) - 1
        } else {
            self.mask() as i128
        }
    }
    /// Reduce modulo `2^bits` into this kind's value range (two's complement
    /// for signed kinds).
    pub fn wrap(self, n: i128) -> i128 {
        let pattern = (n as u64 & self.mask()) as i128;
        if self.signed() && pattern > self.max() {
            pattern - (1i128 << self.bits())
        } else {
            pattern
        }
    }
    /// The two's-complement bit pattern of an in-range value, zero-extended.
    pub fn pattern(self, n: i128) -> u64 {
        n as u64 & self.mask()
    }
    /// Checked `+`, `-`, `*`, `/` and `%` on in-range operands.
    pub fn checked(self, op: &str, a: i128, b: i128) -> Result<i128, ArithFailure> {
        let exact = match op {
            "+" => a + b,
            "-" => a - b,
            "*" => a.checked_mul(b).ok_or(ArithFailure::Overflow)?,
            "/" | "%" if b == 0 => return Err(ArithFailure::DivisionByZero),
            // i128 holds every quotient of 64-bit operands, so MIN / -1 is
            // exact here and then rejected by the range check.
            "/" => a / b,
            "%" => a % b,
            _ => return Err(ArithFailure::Overflow),
        };
        if self.fits(exact) {
            Ok(exact)
        } else {
            Err(ArithFailure::Overflow)
        }
    }
    /// Checked value-level negation.
    pub fn checked_neg(self, a: i128) -> Option<i128> {
        Some(-a).filter(|n| self.fits(*n))
    }
    /// Total `wrapping_add`, `wrapping_sub` and `wrapping_mul`.
    pub fn wrapping(self, op: &str, a: i128, b: i128) -> i128 {
        let (a, b) = (self.wrap(a), self.wrap(b));
        self.wrap(match op {
            "+" => a.wrapping_add(b),
            "-" => a.wrapping_sub(b),
            // Products of reduced 64-bit operands fit in i128.
            _ => a.wrapping_mul(b),
        })
    }
    pub fn mask(self) -> u64 {
        if self.bits() == 64 {
            u64::MAX
        } else {
            (1u64 << self.bits()) - 1
        }
    }
    /// The total bit operators `&`, `|` and `^` on in-range operands.
    pub fn bit(self, op: &str, a: i128, b: i128) -> Option<i128> {
        let (a, b) = (self.pattern(a), self.pattern(b));
        Some(self.wrap(match op {
            "&" => a & b,
            "|" => a | b,
            "^" => a ^ b,
            _ => return None,
        } as i128))
    }
    /// Bitwise complement `~a`.
    pub fn not(self, a: i128) -> i128 {
        self.wrap(!self.pattern(a) as i128)
    }
    /// Checked `<<` and `>>`: the amount (of any integer kind) must be in
    /// `0..bits`, else `Fail[ArithOverflow]`. `<<` discards the bits shifted out; `>>` is logical on
    /// unsigned kinds and arithmetic on signed kinds.
    pub fn shift(self, op: &str, a: i128, amount: i128) -> Result<i128, ArithFailure> {
        if !(0..i128::from(self.bits())).contains(&amount) {
            return Err(ArithFailure::Overflow);
        }
        let amount = amount as u32;
        Ok(match op {
            "<<" => self.wrap((self.pattern(a) << amount) as i128),
            ">>" if self.unsigned() => (self.pattern(a) >> amount) as i128,
            ">>" => a >> amount,
            _ => return Err(ArithFailure::Overflow),
        })
    }
    /// Total `rotate_left` and `rotate_right`: the amount (of any integer
    /// kind) is taken modulo the width, so a negative amount rotates the
    /// other way.
    pub fn rotate(self, left: bool, a: i128, amount: i128) -> i128 {
        let bits = self.bits();
        let n = amount.rem_euclid(i128::from(bits)) as u32;
        let n = if left { n } else { (bits - n) % bits };
        let p = self.pattern(a);
        let r = if n == 0 {
            p
        } else {
            ((p << n) | (p >> (bits - n))) & self.mask()
        };
        self.wrap(r as i128)
    }
    /// Whether every value of `self` is a value of `to`.
    pub fn widens_to(self, to: Self) -> bool {
        to.min() <= self.min() && self.max() <= to.max()
    }
}
/// The three explicit integer conversion families (`widen_<k>`, `narrow_<k>`
/// and `wrapping_<k>` for every integer kind `k`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conversion {
    /// `widen_<k>(x)`: lossless; the checker rejects a source kind whose
    /// range is not contained in `k`'s.
    Widen,
    /// `narrow_<k>(x)`: checked; raises `Fail[ArithOverflow]` when the value
    /// is outside `k`'s range.
    Narrow,
    /// `wrapping_<k>(x)`: total; the value modulo `2^bits(k)`, read in `k`'s
    /// two's complement.
    Wrapping,
}
impl Conversion {
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Widen => "widen_",
            Self::Narrow => "narrow_",
            Self::Wrapping => "wrapping_",
        }
    }
    /// The intrinsic name converting to `to`.
    pub fn name(self, to: IntegerKind) -> String {
        format!("{}{}", self.prefix(), to.name())
    }
    /// Converts an in-range value to `to`.
    pub fn apply(self, to: IntegerKind, n: i128) -> Result<i128, ArithFailure> {
        match self {
            Self::Wrapping => Ok(to.wrap(n)),
            _ if to.fits(n) => Ok(n),
            _ => Err(ArithFailure::Overflow),
        }
    }
}
/// The conversion intrinsic spelled `name`, with its target kind.
pub fn conversion(name: &str) -> Option<(Conversion, IntegerKind)> {
    [Conversion::Widen, Conversion::Narrow, Conversion::Wrapping]
        .into_iter()
        .find_map(|c| {
            let kind = IntegerKind::from_name(name.strip_prefix(c.prefix())?)?;
            Some((c, kind))
        })
}
/// The total rotation intrinsics; `true` rotates left.
pub const ROTATE_INTRINSICS: [(&str, bool); 2] = [("rotate_left", true), ("rotate_right", false)];
/// Whether `name` is a rotation intrinsic, and its direction.
pub fn rotate_intrinsic(name: &str) -> Option<bool> {
    ROTATE_INTRINSICS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, left)| *left)
}
/// The integer operators beyond `vesper.arith.checked/1`'s arithmetic:
/// `&`, `|`, `^` (total), `<<` and `>>` (checked shift amount).
pub fn is_bit_operator(op: &str) -> bool {
    matches!(op, "&" | "|" | "^" | "<<" | ">>")
}
/// Whether `op` is a shift operator, whose amount may fail.
pub fn is_shift(op: &str) -> bool {
    matches!(op, "<<" | ">>")
}
/// The byte-order intrinsics, each defined for every integer kind `k` of 16
/// bits or more: `to_le_bytes_<k>(x)` and `to_be_bytes_<k>(x)` return a
/// tuple of `bits / 8` `u8`s; `from_le_bytes_<k>(b)` and
/// `from_be_bytes_<k>(b)` read one back. They are pure and total and are
/// ordinary source functions (see `byte_order_source`), so every backend
/// runs them through its shifts and conversions.
pub const BYTE_ORDER_PREFIXES: [&str; 4] = [
    "to_le_bytes_",
    "to_be_bytes_",
    "from_le_bytes_",
    "from_be_bytes_",
];
/// The byte-order intrinsic `name`: its prefix and kind.
pub fn byte_order(name: &str) -> Option<(&'static str, IntegerKind)> {
    BYTE_ORDER_PREFIXES.iter().find_map(|p| {
        let kind = IntegerKind::from_name(name.strip_prefix(p)?)?;
        (kind.bits() >= 16).then_some((*p, kind))
    })
}
/// Vesper source of the byte-order intrinsic `name`, or `None`.
pub fn byte_order_source(name: &str) -> Option<String> {
    let (prefix, kind) = byte_order(name)?;
    let k = kind.name();
    let u = if kind.signed() {
        kind.flip_sign()
    } else {
        kind
    };
    let n = (kind.bits() / 8) as usize;
    let tuple = format!("({})", vec!["u8"; n].join(", "));
    let little = prefix.contains("_le_");
    // Byte i of the little-endian encoding is bits [8i, 8i+8).
    let order: Vec<usize> = if little {
        (0..n).collect()
    } else {
        (0..n).rev().collect()
    };
    Some(if prefix.starts_with("to_") {
        let bytes = order
            .iter()
            .map(|i| format!("wrapping_u8(p >> {})", 8 * i))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "fn {name}(x: {k}) -> {tuple} {{ let p: {} = wrapping_{}(x); ({bytes}) }}",
            u.name(),
            u.name()
        )
    } else {
        let names = (0..n).map(|i| format!("b{i}")).collect::<Vec<_>>();
        let terms = order
            .iter()
            .enumerate()
            .map(|(position, i)| format!("(widen_{}(b{position}) << {})", u.name(), 8 * i))
            .collect::<Vec<_>>()
            .join(" | ");
        format!(
            "fn {name}(bytes: {tuple}) -> {k} {{ let ({}) = bytes; let p: {} = {terms}; wrapping_{k}(p) }}",
            names.join(", "),
            u.name()
        )
    })
}
pub fn default_type(t: &Type) -> Type {
    match t {
        Type::IntLiteral(n) => IntegerKind::default_for(*n).ty(),
        Type::Named(n, ts) => Type::Named(n.clone(), ts.iter().map(default_type).collect()),
        Type::Tuple(ts) => Type::Tuple(ts.iter().map(default_type).collect()),
        Type::Function {
            params,
            result,
            effects,
            mode,
        } => Type::Function {
            params: params.iter().map(|(p, t)| (*p, default_type(t))).collect(),
            result: Box::new(default_type(result)),
            effects: crate::ast::Effects {
                instances: effects
                    .instances
                    .iter()
                    .map(|e| crate::ast::Effect {
                        label: e.label.clone(),
                        name: e.name.clone(),
                        args: e.args.iter().map(default_type).collect(),
                    })
                    .collect(),
                tail: effects.tail.clone(),
            },
            mode: *mode,
        },
        _ => t.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const KINDS: [IntegerKind; 10] = INTEGER_KINDS;
    #[test]
    fn endpoints_succeed_and_neighbours_overflow() {
        for k in KINDS {
            assert_eq!(k.checked("+", k.max() - 1, 1), Ok(k.max()));
            assert_eq!(k.checked("+", k.max(), 1), Err(ArithFailure::Overflow));
            assert_eq!(k.checked("-", k.min() + 1, 1), Ok(k.min()));
            assert_eq!(k.checked("-", k.min(), 1), Err(ArithFailure::Overflow));
            assert_eq!(k.checked("*", k.max(), 1), Ok(k.max()));
            assert_eq!(k.checked("*", k.max(), 2), Err(ArithFailure::Overflow));
            assert_eq!(k.wrapping("+", k.max(), 1), k.min());
            assert_eq!(k.wrapping("-", k.min(), 1), k.max());
            assert_eq!(k.checked("/", 1, 0), Err(ArithFailure::DivisionByZero));
            assert_eq!(k.checked("%", 1, 0), Err(ArithFailure::DivisionByZero));
        }
    }
    #[test]
    fn signed_corners() {
        let k = IntegerKind::I64;
        let min = i64::MIN as i128;
        assert_eq!(k.checked("/", min, -1), Err(ArithFailure::Overflow));
        assert_eq!(k.checked("%", min, -1), Ok(0));
        assert_eq!(k.checked("/", min, 0), Err(ArithFailure::DivisionByZero));
        assert_eq!(k.checked("/", -7, 2), Ok(-3));
        assert_eq!(k.checked("%", -7, 2), Ok(-1));
        assert_eq!(k.checked_neg(min), None);
        assert_eq!(k.checked_neg(-(i64::MAX as i128)), Some(i64::MAX as i128));
        assert_eq!(k.checked("*", min, -1), Err(ArithFailure::Overflow));
        assert_eq!(k.wrapping("*", min, -1), min);
        assert_eq!(IntegerKind::U64.checked_neg(0), Some(0));
        assert_eq!(IntegerKind::U8.checked_neg(1), None);
        assert_eq!(
            IntegerKind::U64.wrapping("*", k.max(), 3),
            9223372036854775805
        );
    }
    #[test]
    fn typed_instance_keys_require_concrete_arguments() {
        assert_eq!(
            instance_key("Fail", &[Type::named(OVERFLOW_TYPE)]).as_deref(),
            Some("$effect:Fail[ArithOverflow]")
        );
        assert_eq!(instance_key("Fail", &[Type::Infer]), None);
        assert_eq!(instance_key("Log", &[]), None);
        assert_eq!(wrapping_intrinsic("wrapping_mul"), Some("*"));
        assert_eq!(wrapping_name("-"), Some("wrapping_sub"));
        assert!(may_overflow("/", IntegerKind::I64));
        assert!(!may_overflow("/", IntegerKind::U64));
        assert!(may_overflow("/", IntegerKind::I8));
    }
    #[test]
    fn kinds_have_expected_ranges() {
        use IntegerKind as K;
        assert_eq!((K::I8.min(), K::I8.max()), (-128, 127));
        assert_eq!((K::I16.min(), K::I16.max()), (-32768, 32767));
        assert_eq!(K::I32.min(), i32::MIN as i128);
        assert_eq!(K::U16.max(), 65535);
        assert_eq!(K::Usize.max(), u64::MAX as i128);
        assert_eq!(K::Isize.min(), i64::MIN as i128);
        for k in KINDS {
            assert_eq!(IntegerKind::from_name(k.name()), Some(k));
            assert_eq!(k.wrap(k.max() + 1), k.min());
            assert_eq!(k.wrap(k.min() - 1), k.max());
            assert_eq!(k.flip_sign().flip_sign(), k);
            assert_eq!(k.flip_sign().bits(), k.bits());
        }
        assert_eq!(K::I8.checked("/", -128, -1), Err(ArithFailure::Overflow));
        assert_eq!(K::I8.checked("%", -128, -1), Ok(0));
        assert_eq!(K::I16.checked_neg(-32768), None);
    }
    #[test]
    fn bit_operators_shifts_and_rotates() {
        use IntegerKind as K;
        assert_eq!(K::I8.bit("&", -1, 0x0f), Some(0x0f));
        assert_eq!(K::I8.bit("|", -128, 1), Some(-127));
        assert_eq!(K::U16.bit("^", 0xffff, 0x00ff), Some(0xff00));
        assert_eq!(K::I32.not(0), -1);
        assert_eq!(K::U8.not(0), 255);
        assert_eq!(K::I8.shift("<<", 1, 7), Ok(-128));
        assert_eq!(K::I8.shift(">>", -128, 7), Ok(-1));
        assert_eq!(K::U8.shift(">>", 128, 7), Ok(1));
        assert_eq!(K::U8.shift("<<", 1, 8), Err(ArithFailure::Overflow));
        assert_eq!(K::U64.shift("<<", 1, -1), Err(ArithFailure::Overflow));
        assert_eq!(K::I64.shift("<<", 1, 63), Ok(i64::MIN as i128));
        assert_eq!(K::U8.rotate(true, 0x81, 1), 0x03);
        assert_eq!(K::U8.rotate(false, 0x81, 1), 0xc0);
        assert_eq!(K::U8.rotate(true, 0x81, -1), 0xc0);
        assert_eq!(K::I16.rotate(true, -32768, 1), 1);
        assert_eq!(K::U32.rotate(true, 5, 32), 5);
        for k in KINDS {
            for a in [k.min(), -1, 0, 1, k.max()]
                .into_iter()
                .filter(|a| k.fits(*a))
            {
                for n in [0, 1, k.bits() as i128 - 1, k.bits() as i128, 77, -3] {
                    assert_eq!(k.rotate(false, k.rotate(true, a, n), n), a, "{k:?} {a} {n}");
                }
            }
        }
    }
    #[test]
    fn conversions() {
        use IntegerKind as K;
        assert_eq!(conversion("widen_u64"), Some((Conversion::Widen, K::U64)));
        assert_eq!(conversion("narrow_i8"), Some((Conversion::Narrow, K::I8)));
        assert_eq!(
            conversion("wrapping_isize"),
            Some((Conversion::Wrapping, K::Isize))
        );
        assert_eq!(conversion("wrapping_add"), None);
        assert!(K::U8.widens_to(K::I16));
        assert!(!K::I8.widens_to(K::U64));
        assert!(K::U64.widens_to(K::Usize));
        assert!(!K::U64.widens_to(K::I64));
        assert_eq!(
            Conversion::Narrow.apply(K::U8, 256),
            Err(ArithFailure::Overflow)
        );
        assert_eq!(
            Conversion::Narrow.apply(K::U8, -1),
            Err(ArithFailure::Overflow)
        );
        assert_eq!(Conversion::Wrapping.apply(K::U8, -1), Ok(255));
        assert_eq!(Conversion::Wrapping.apply(K::I8, 200), Ok(-56));
        assert_eq!(Conversion::Wrapping.apply(K::I64, u64::MAX as i128), Ok(-1));
    }
    #[test]
    fn byte_order_sources() {
        assert_eq!(byte_order("to_le_bytes_u8"), None);
        assert_eq!(
            byte_order_source("to_be_bytes_u16").as_deref(),
            Some(
                "fn to_be_bytes_u16(x: u16) -> (u8, u8) { let p: u16 = wrapping_u16(x); (wrapping_u8(p >> 8), wrapping_u8(p >> 0)) }"
            )
        );
        assert_eq!(
            byte_order_source("from_le_bytes_i16").as_deref(),
            Some(
                "fn from_le_bytes_i16(bytes: (u8, u8)) -> i16 { let (b0, b1) = bytes; let p: u16 = (widen_u16(b0) << 0) | (widen_u16(b1) << 8); wrapping_i16(p) }"
            )
        );
    }
}
