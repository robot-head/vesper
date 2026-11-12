//! The integer semantics shared by checking, execution, optimization and codegen.
//!
//! # Checked arithmetic contract (`vesper.arith.checked/1`)
//!
//! Every backend (checker facts, evaluator, both optimizers, the Cranelift
//! JIT/object backend and Lean extraction) implements this one contract:
//!
//! * Integer kinds are `u8`, `u32`, `u64` and `i64`. Operands of a binary
//!   operator have the same kind; there are no implicit conversions.
//! * `+`, `-`, `*`, their compound assignments and value-level negation `-x`
//!   compute the exact mathematical result. If it is outside the operand kind
//!   they raise `Fail[ArithOverflow]` with the builtin value `ArithOverflow {}`.
//!   Unsigned negation therefore succeeds only for zero. The literal spelling
//!   `-9223372036854775808` is a literal, not a negation, and is in range.
//! * Integer `/` and `%` by zero raise `Fail[RuntimeError]` with
//!   `RuntimeError::DivisionByZero`; the divisor check precedes the overflow
//!   check. `i64::MIN / -1` raises `Fail[ArithOverflow]`; `i64::MIN % -1` is
//!   `0`. Division truncates toward zero and the remainder has the sign of
//!   the dividend.
//! * Dynamic indexing keeps `Fail[RuntimeError]` (`IndexOutOfBounds`).
//! * `wrapping_add`, `wrapping_sub` and `wrapping_mul` take two operands of
//!   one integer kind and return their result modulo `2^bits`. They are pure
//!   and total and never raise.
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
        || (kind == IntegerKind::I64 && matches!(op, "/" | "/="))
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegerKind {
    U8,
    U32,
    U64,
    I64,
}
impl IntegerKind {
    pub fn from_type(t: &Type) -> Option<Self> {
        Some(match t.name() {
            "u8" => Self::U8,
            "u32" => Self::U32,
            "u64" => Self::U64,
            "i64" => Self::I64,
            _ => {
                if let Type::IntLiteral(n) = t {
                    return Some(Self::default_for(*n));
                } else {
                    return None;
                }
            }
        })
    }
    pub fn default_for(n: i128) -> Self {
        if n > i64::MAX as i128 {
            Self::U64
        } else {
            Self::I64
        }
    }
    pub fn ty(self) -> Type {
        Type::named(match self {
            Self::U8 => "u8",
            Self::U32 => "u32",
            Self::U64 => "u64",
            Self::I64 => "i64",
        })
    }
    pub fn bits(self) -> u32 {
        match self {
            Self::U8 => 8,
            Self::U32 => 32,
            _ => 64,
        }
    }
    pub fn unsigned(self) -> bool {
        self != Self::I64
    }
    pub fn fits(self, n: i128) -> bool {
        match self {
            Self::I64 => (i64::MIN as i128..=i64::MAX as i128).contains(&n),
            _ => (0..=(1i128 << self.bits()) - 1).contains(&n),
        }
    }
    pub fn min(self) -> i128 {
        if self == Self::I64 {
            i64::MIN as i128
        } else {
            0
        }
    }
    pub fn max(self) -> i128 {
        if self == Self::I64 {
            i64::MAX as i128
        } else {
            self.mask() as i128
        }
    }
    /// Reduce modulo `2^bits` into this kind's value range.
    pub fn wrap(self, n: i128) -> i128 {
        if self == Self::I64 {
            n as i64 as i128
        } else {
            (n as u64 & self.mask()) as i128
        }
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
    const KINDS: [IntegerKind; 4] = [
        IntegerKind::U8,
        IntegerKind::U32,
        IntegerKind::U64,
        IntegerKind::I64,
    ];
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
    }
}
