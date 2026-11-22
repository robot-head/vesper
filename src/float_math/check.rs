//! Static checking of `vesper.float.ieee754/1`: float literals in `f32`
//! context, the math intrinsics, float conversions and bit reinterpretation
//! (see [`crate::numeric::float`]).
use super::{Checker, Env, Usage};
use crate::ast::*;
use crate::numeric::IntegerKind;
use crate::numeric::float::{FloatConversion, FloatKind, MathOp};

/// Whether `e` is built only from float literals, negation and arithmetic,
/// so that it can be retyped at another float kind without side effects.
pub(super) fn float_literal_tree(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Literal(Literal::Float(_)) => true,
        ExprKind::Unary(op, v) if op == "-" => float_literal_tree(v),
        ExprKind::Binary(op, a, b) if matches!(op.as_str(), "+" | "-" | "*" | "/" | "%") => {
            float_literal_tree(a) && float_literal_tree(b)
        }
        _ => false,
    }
}

impl Checker<'_> {
    /// The goal type a float operand takes from its context.
    fn float_goal(expected: &Type) -> Type {
        if FloatKind::from_type(expected).is_some() {
            expected.clone()
        } else {
            Type::Infer
        }
    }
    /// Types `e` at `goal`, retyping a float literal tree that defaulted to
    /// `f64` when `goal` is `f32`.
    pub(super) fn retype_float_literal(
        &mut self,
        e: &Expr,
        env: &mut Env,
        found: &Type,
        goal: &Type,
    ) -> Type {
        if FloatKind::from_type(goal) == Some(FloatKind::F32)
            && FloatKind::from_type(found) == Some(FloatKind::F64)
            && float_literal_tree(e)
        {
            self.expr_expected(e, env, Usage::Own, goal)
        } else {
            found.clone()
        }
    }
    /// `sqrt`, `abs`, `floor`, `ceil`, `round`, `trunc`, `min`, `max` and
    /// `fma`: operands of one float kind, the result of that kind. Total.
    pub(super) fn math_call(
        &mut self,
        op: MathOp,
        types: &[Type],
        args: &[Arg],
        env: &mut Env,
        span: Span,
        expected: &Type,
    ) -> Type {
        let name = op.name();
        self.by_value_operands(name, types, args, op.arity(), &span);
        let mut common = Self::float_goal(expected);
        let mut actual = vec![];
        for a in args {
            let t = self.expr_expected(&a.expr, env, Usage::Own, &common);
            if common == Type::Infer {
                common = crate::numeric::default_type(&t);
            }
            actual.push(t);
        }
        for (a, t) in args.iter().zip(actual) {
            let t = self.retype_float_literal(&a.expr, env, &t, &common);
            self.expect_type(&common, &t, a.expr.span.clone());
        }
        if FloatKind::from_type(&common).is_none() && common != Type::Infer {
            self.error(
                "E0200",
                format!(
                    "'{name}' requires float operands of one kind (f32 or f64), found {common}"
                ),
                span,
            );
        }
        common
    }
    /// Types the single operand of a conversion; an unsuffixed integer
    /// literal takes `literal_kind`.
    fn conversion_operand(
        &mut self,
        a: &Arg,
        env: &mut Env,
        goal: &Type,
        literal_kind: IntegerKind,
    ) -> Type {
        let t = self.expr_expected(&a.expr, env, Usage::Own, goal);
        if let Type::IntLiteral(n) = t {
            let k = if literal_kind.fits(n) {
                literal_kind
            } else {
                IntegerKind::default_for(n)
            };
            self.types
                .insert((a.expr.span.start, a.expr.span.end), k.ty());
            return k.ty();
        }
        t
    }
    /// `widen_f32(x)`, `widen_f64(x)` (exact) and `nearest_f32(x)`,
    /// `nearest_f64(x)` (rounding, total).
    pub(super) fn float_conversion_call(
        &mut self,
        name: &str,
        (conversion, to): (FloatConversion, FloatKind),
        types: &[Type],
        args: &[Arg],
        env: &mut Env,
        span: Span,
    ) -> Type {
        self.by_value_operands(name, types, args, 1, &span);
        let Some(a) = args.first() else {
            return to.ty();
        };
        let literal = match to {
            FloatKind::F32 => IntegerKind::I16,
            FloatKind::F64 => IntegerKind::I32,
        };
        let literal = if conversion == FloatConversion::Widen {
            literal
        } else {
            IntegerKind::I64
        };
        let t = self.conversion_operand(a, env, &Type::Infer, literal);
        let exact = if let Some(from) = FloatKind::from_type(&t) {
            crate::numeric::float::float_widens_to(from, to)
        } else if let Some(k) = IntegerKind::from_type(&t) {
            if k.wide() {
                self.wide_float_refusal(name, a.expr.span.clone());
                return to.ty();
            }
            to.holds(k)
        } else {
            if t != Type::Infer {
                self.error(
                    "E0200",
                    format!("'{name}' requires an integer or float operand, found {t}"),
                    a.expr.span.clone(),
                );
            }
            return to.ty();
        };
        if conversion == FloatConversion::Widen && !exact {
            self.error(
                "E0200",
                format!(
                    "'{name}' cannot represent every {t} value exactly; use nearest_{} (rounds to nearest, ties to even)",
                    to.name()
                ),
                a.expr.span.clone(),
            );
        }
        to.ty()
    }
    /// `saturating_k(x)`: a float to the integer kind `k`, total.
    pub(super) fn saturating_call(
        &mut self,
        name: &str,
        to: IntegerKind,
        types: &[Type],
        args: &[Arg],
        env: &mut Env,
        span: Span,
    ) -> Type {
        self.by_value_operands(name, types, args, 1, &span);
        if to.wide() {
            self.wide_float_refusal(name, span.clone());
        }
        if let Some(a) = args.first() {
            let t = self.expr_expected(&a.expr, env, Usage::Own, &Type::Infer);
            if FloatKind::from_type(&t).is_none() && t != Type::Infer {
                self.error(
                    "E0200",
                    format!("'{name}' requires a float operand, found {t}"),
                    a.expr.span.clone(),
                );
            }
        }
        to.ty()
    }
    /// Float conversions do not take or produce `i128`/`u128` yet (SH2
    /// lane 2, #1174).
    pub(super) fn wide_float_refusal(&mut self, name: &str, span: Span) {
        self.error(
            "E0200",
            format!("'{name}' does not convert 128-bit integers yet; convert a 64-bit half"),
            span,
        );
    }
    /// `to_bits_f(x)` and `from_bits_f(n)`: exact reinterpretation.
    pub(super) fn bits_call(
        &mut self,
        name: &str,
        (to_bits, kind): (bool, FloatKind),
        types: &[Type],
        args: &[Arg],
        env: &mut Env,
        span: Span,
    ) -> Type {
        self.by_value_operands(name, types, args, 1, &span);
        let (operand, result) = if to_bits {
            (kind.ty(), kind.bits_kind().ty())
        } else {
            (kind.bits_kind().ty(), kind.ty())
        };
        if let Some(a) = args.first() {
            let t = self.conversion_operand(a, env, &operand, kind.bits_kind());
            let t = self.retype_float_literal(&a.expr, env, &t, &operand);
            self.expect_type(&operand, &t, a.expr.span.clone());
        }
        result
    }
    /// `narrow_k(x)` of a float `x`: truncation, raising
    /// `Fail[ArithOverflow]` on a NaN or an out-of-range value. `widen_k` and
    /// `wrapping_k` do not take floats.
    pub(super) fn float_to_integer(
        &mut self,
        name: &str,
        conversion: crate::numeric::Conversion,
        from: FloatKind,
        a: &Arg,
        env: &mut Env,
        span: Span,
    ) {
        if crate::numeric::is_wide_name(&name[conversion.prefix().len()..]) {
            self.wide_float_refusal(name, span);
        } else if conversion == crate::numeric::Conversion::Narrow {
            self.overflow_failure(env, span);
        } else {
            let to = &name[conversion.prefix().len()..];
            self.error(
                "E0200",
                format!(
                    "'{name}' does not convert {}; use narrow_{to} (checked) or saturating_{to} (clamping)",
                    from.name()
                ),
                a.expr.span.clone(),
            );
        }
    }
}
