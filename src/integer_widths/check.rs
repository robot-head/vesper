//! Static checking of the integer-width operators and intrinsics of
//! `vesper.arith.checked/1`: `&`, `|`, `^`, `~`, `<<`, `>>`, the rotations
//! and the explicit conversions (see [`crate::integer_widths`]).
use super::{Checker, Env, Usage};
use crate::ast::*;
use crate::numeric::{Conversion, IntegerKind};

impl Checker<'_> {
    /// Records the contextual integer type of an unsuffixed literal operand.
    fn settle_literal(&mut self, e: &Expr, t: &Type, to: &Type) -> Type {
        if matches!(t, Type::IntLiteral(_)) {
            self.types.insert((e.span.start, e.span.end), to.clone());
            to.clone()
        } else {
            t.clone()
        }
    }
    /// The goal type an integer operand takes from its context.
    fn integer_goal(expected: &Type) -> Type {
        if IntegerKind::from_type(expected).is_some() && !matches!(expected, Type::IntLiteral(_)) {
            expected.clone()
        } else {
            Type::Infer
        }
    }
    /// Typechecks a shift amount: any integer kind, an unsuffixed literal
    /// being a `u32`. Returns its bounds when known (a constant, or a range
    /// from a dominating guard such as `if n < 64`).
    fn shift_amount(&mut self, e: &Expr, env: &mut Env, what: &str) -> Option<(i128, i128)> {
        let t = self.expr_expected(e, env, Usage::Own, &Type::Infer);
        let fact = self.integer_bounds(e, env);
        if let Type::IntLiteral(n) = t
            && !IntegerKind::U32.fits(n)
        {
            self.error(
                "E0209",
                format!("integer literal {n} is outside the range of u32"),
                e.span.clone(),
            );
        }
        let t = self.settle_literal(e, &t, &Type::named("u32"));
        if IntegerKind::from_type(&t).is_none() && t != Type::Infer {
            self.error(
                "E0200",
                format!("{what} requires an integer amount, found {t}"),
                e.span.clone(),
            );
        }
        fact
    }
    /// `a << n` and `a >> n` (`vesper.arith.checked/1`): `a` of any integer
    /// kind, `n` of any integer kind; an amount not proven to be in
    /// `0..bits` (by a constant or a guard's range) may raise
    /// `Fail[ArithOverflow]`.
    pub(super) fn shift_expr(
        &mut self,
        op: &str,
        l: &Expr,
        r: &Expr,
        env: &mut Env,
        span: Span,
        expected: &Type,
    ) -> Type {
        let raw = self.expr_expected(l, env, Usage::Own, &Self::integer_goal(expected));
        let t = crate::numeric::default_type(&raw);
        self.settle_literal(l, &raw, &t);
        let amount = self.shift_amount(r, env, &format!("'{op}'"));
        let Some(kind) = IntegerKind::from_type(&t) else {
            if t != Type::Infer {
                self.error(
                    "E0200",
                    format!("'{op}' requires an integer operand, found {t}"),
                    l.span.clone(),
                );
            }
            return t;
        };
        if !amount.is_some_and(|(lo, hi)| lo >= 0 && hi < i128::from(kind.bits())) {
            self.overflow_failure(env, span);
        }
        t
    }
    /// `&`, `|` and `^` after operand typing: both operands are one integer
    /// kind.
    pub(super) fn check_bit_operands(&mut self, op: &str, common: &Type, span: Span) {
        if IntegerKind::from_type(common).is_none() && *common != Type::Infer {
            self.error(
                "E0200",
                format!("'{op}' requires integer operands of one kind, found {common}"),
                span,
            );
        }
    }
    /// Prefix `~`: the bitwise complement of one integer operand.
    pub(super) fn check_complement(&mut self, t: &Type, span: Span) {
        if IntegerKind::from_type(t).is_none() && *t != Type::Infer {
            self.error(
                "E0200",
                format!("prefix '~' requires an integer operand, found {t}"),
                span,
            );
        }
    }
    pub(super) fn by_value_operands(
        &mut self,
        name: &str,
        types: &[Type],
        args: &[Arg],
        n: usize,
        span: &Span,
    ) {
        if !types.is_empty() || args.len() != n {
            self.error(
                "E0207",
                format!(
                    "'{name}' takes {n} operand{} and no type arguments",
                    if n == 1 { "" } else { "s" }
                ),
                span.clone(),
            );
        }
        for a in args {
            if a.pass != Pass::In || a.explicit {
                self.error(
                    "E0308",
                    format!("'{name}' takes its operands by value"),
                    a.expr.span.clone(),
                );
            }
        }
    }
    /// `widen_k(x)`, `narrow_k(x)` and `wrapping_k(x)`.
    pub(super) fn conversion_call(
        &mut self,
        name: &str,
        (conversion, to): (Conversion, IntegerKind),
        types: &[Type],
        args: &[Arg],
        env: &mut Env,
        span: Span,
    ) -> Type {
        self.by_value_operands(name, types, args, 1, &span);
        let Some(a) = args.first() else {
            return to.ty();
        };
        let t = self.expr_expected(&a.expr, env, Usage::Own, &Type::Infer);
        let fact = self.integer_bounds(&a.expr, env);
        let t = match t {
            // An unsuffixed literal keeps its default kind, except that a
            // widening takes it at the target kind when it fits.
            Type::IntLiteral(n) if conversion == Conversion::Widen && to.fits(n) => {
                self.settle_literal(&a.expr, &Type::IntLiteral(n), &to.ty())
            }
            Type::IntLiteral(n) => {
                let d = IntegerKind::default_for(n).ty();
                self.settle_literal(&a.expr, &Type::IntLiteral(n), &d)
            }
            t => t,
        };
        // A conversion accepts any kind: it does not type an open literal
        // binding (`literal_typing`).
        self.literal_unconstrained(&a.expr);
        if let Some(from) = crate::numeric::FloatKind::from_type(&t) {
            self.float_to_integer(name, conversion, from, a, env, span);
            return to.ty();
        }
        let Some(from) = IntegerKind::from_type(&t) else {
            if t != Type::Infer {
                self.error(
                    "E0200",
                    format!("'{name}' requires an integer operand, found {t}"),
                    a.expr.span.clone(),
                );
            }
            return to.ty();
        };
        match conversion {
            Conversion::Widen if !from.widens_to(to) => self.error(
                "E0200",
                format!(
                    "'{name}' cannot represent every {} value; use narrow_{} (checked) or wrapping_{} (modular)",
                    from.name(),
                    to.name(),
                    to.name()
                ),
                a.expr.span.clone(),
            ),
            Conversion::Narrow
                if !from.widens_to(to) && !fact.is_some_and(|(lo, hi)| to.fits(lo) && to.fits(hi)) =>
            {
                self.overflow_failure(env, span)
            }
            _ => {}
        }
        to.ty()
    }
    /// `rotate_left(a, n)` and `rotate_right(a, n)`: total.
    pub(super) fn rotate_call(
        &mut self,
        name: &str,
        types: &[Type],
        args: &[Arg],
        env: &mut Env,
        span: Span,
        expected: &Type,
    ) -> Type {
        self.by_value_operands(name, types, args, 2, &span);
        let Some(a) = args.first() else {
            return Type::Infer;
        };
        let raw = self.expr_expected(&a.expr, env, Usage::Own, &Self::integer_goal(expected));
        let t = crate::numeric::default_type(&raw);
        self.settle_literal(&a.expr, &raw, &t);
        if IntegerKind::from_type(&t).is_none() && t != Type::Infer {
            self.error(
                "E0200",
                format!("'{name}' requires an integer operand, found {t}"),
                a.expr.span.clone(),
            );
        }
        if let Some(n) = args.get(1) {
            self.shift_amount(&n.expr, env, &format!("'{name}'"));
        }
        t
    }
}

/// Exact facts of `u128` over its whole range (SH3 lane 6, #1245). A fact is
/// an `i128`; a `u128` fact is its value's bit pattern, so a value above
/// `i128::MAX` reads as negative. These helpers apply each kind's checked
/// rules to facts: patterns for `u128`, exact values for the other kinds.
pub(super) fn fact_checked(
    kind: IntegerKind,
    op: &str,
    a: i128,
    b: i128,
) -> Result<i128, crate::numeric::ArithFailure> {
    if kind == IntegerKind::U128 {
        crate::numeric::wide::checked(kind, op, a as u128, b as u128).map(|n| n as i128)
    } else {
        kind.checked(op, a, b)
    }
}
/// Checked negation of a fact (only `-0` for `u128`).
pub(super) fn fact_neg(kind: IntegerKind, a: i128) -> Option<i128> {
    if kind == IntegerKind::U128 {
        (a == 0).then_some(0)
    } else {
        kind.checked_neg(a)
    }
}
/// Whether `n` is a fact of `kind`: every pattern is a `u128` value.
pub(super) fn fact_fits(kind: IntegerKind, n: i128) -> bool {
    kind == IntegerKind::U128 || kind.fits(n)
}
impl Checker<'_> {
    /// Whether `e` is typed `u128` (by its recorded type, or a name's
    /// binding).
    pub(super) fn u128_expr(&self, e: &Expr, env: &Env) -> bool {
        let named = |t: &Type| IntegerKind::from_type(t) == Some(IntegerKind::U128);
        self.types
            .get(&(e.span.start, e.span.end))
            .is_some_and(named)
            || matches!(&e.kind, ExprKind::Name(n) if env.bindings.get(n).is_some_and(|b| named(&b.ty)))
    }
}
