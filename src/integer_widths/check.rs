//! Static checking of the integer-width operators and intrinsics of
//! `vesper.arith.checked/1`: `&`, `|`, `^`, `~`, `<<`, `>>`, the rotations
//! and the explicit conversions (see [`crate::integer_widths`]).
use super::{Checker, Env, Usage};
use crate::ast::*;
use crate::numeric::{Conversion, IntegerKind};
use std::collections::BTreeSet;

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
    /// being a `u32`. Returns its bounds when known (a constant, a mask, or
    /// a range from a dominating guard such as `if n < 64`, through `+`,
    /// `-`, `*`, `/` and `>>`: [`Self::operand_bounds`]).
    fn shift_amount(&mut self, e: &Expr, env: &mut Env, what: &str) -> Option<(i128, i128)> {
        self.total_operands += 1;
        let t = self.expr_expected(e, env, Usage::Own, &Type::Infer);
        self.total_operands -= 1;
        let fact = self.operand_bounds(e, env);
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
    /// Prefix `!` on an integer operand is its bitwise complement, `~`
    /// (manuscript ch. 3, "Integer kinds"): records the span, so the module
    /// is rewritten to `~` and checked again, and returns whether `t` is an
    /// integer kind. A literal operand defaults like any literal (`!0` is
    /// an `i64`).
    pub(super) fn integer_not(&mut self, t: &Type, span: Span) -> bool {
        if IntegerKind::from_type(t).is_none() {
            return false;
        }
        self.integer_nots.insert((span.start, span.end));
        true
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
    /// Typechecks the divisor of `/` or `%`: like a shift amount, its `+`,
    /// `-` and `*` may be proven in range by range facts.
    pub(super) fn divisor(&mut self, e: &Expr, env: &mut Env, goal: &Type) -> Type {
        self.total_operands += 1;
        let t = self.expr_expected(e, env, Usage::Own, goal);
        self.total_operands -= 1;
        t
    }
    /// Whether the divisor `e` lies on one side of zero by
    /// [`Self::operand_bounds`].
    pub(super) fn nonzero_divisor(&self, e: &Expr, env: &Env) -> bool {
        self.operand_bounds(e, env)
            .is_some_and(|(lo, hi)| lo > 0 || hi < 0)
    }
    /// [`Self::operand_bounds`] of the left operand of a binary operator,
    /// read before the right operand is checked (the right operand may
    /// write a name the left one read).
    pub(super) fn total_operand_bounds(&self, e: &Expr, env: &Env) -> Option<(i128, i128)> {
        self.operand_bounds(e, env)
    }
    /// Whether the checked `left op right` cannot overflow by range facts
    /// that [`Self::integer_bounds`] alone does not use: `+`, `-` and `*`
    /// inside a shift amount or a divisor, and a signed quotient whose
    /// divisor's [`Self::operand_bounds`] exclude `-1`.
    pub(super) fn total_operation(
        &self,
        op: &str,
        kind: IntegerKind,
        left: Option<(i128, i128)>,
        right: &Expr,
        env: &Env,
    ) -> bool {
        if op == "/" {
            return Self::quotient_in_range(op, kind, left, self.operand_bounds(right, env));
        }
        self.total_operands != 0
            && Self::bounds_in_range(op, kind, left, self.operand_bounds(right, env))
    }
    /// Inclusive bounds of a shift amount or a divisor:
    /// [`Self::integer_bounds`], and for a pure `a + b`, `a - b`, `a * b`,
    /// `a / b` and `a >> b` the interval the operands' bounds give when every
    /// value of it lies in the expression's kind. `u128` operands have none.
    pub(super) fn operand_bounds(&self, e: &Expr, env: &Env) -> Option<(i128, i128)> {
        if let Some(bounds) = self.integer_bounds(e, env) {
            return Some(bounds);
        }
        let ExprKind::Binary(op, a, b) = &e.kind else {
            return None;
        };
        if !Self::pure_integer(e) || self.u128_expr(a, env) {
            return None;
        }
        let kind = [e, a.as_ref(), b.as_ref()].into_iter().find_map(|x| {
            self.types
                .get(&(x.span.start, x.span.end))
                .and_then(IntegerKind::from_type)
        })?;
        // An empty interval (contradictory guards, unreachable code) has no
        // endpoints to compute with.
        let (alo, ahi) = self.operand_bounds(a, env).filter(|(lo, hi)| lo <= hi)?;
        let (blo, bhi) = self.operand_bounds(b, env).filter(|(lo, hi)| lo <= hi)?;
        let corners = |f: fn(i128, i128) -> Option<i128>| {
            let values = [(alo, blo), (alo, bhi), (ahi, blo), (ahi, bhi)]
                .map(|(x, y)| f(x, y).filter(|v| *v >= kind.min() && *v <= kind.max()));
            let lo = values
                .iter()
                .try_fold(i128::MAX, |m, v| v.map(|v| m.min(v)))?;
            let hi = values
                .iter()
                .try_fold(i128::MIN, |m, v| v.map(|v| m.max(v)))?;
            Some((lo, hi))
        };
        match op.as_str() {
            "+" => corners(i128::checked_add),
            "-" => corners(i128::checked_sub),
            "*" => corners(i128::checked_mul),
            // Nonnegative operands only: truncation then moves toward zero
            // monotonically in both operands.
            "/" if alo >= 0 && blo > 0 => Some((alo / bhi, ahi / blo)),
            ">>" if alo >= 0 && blo >= 0 && bhi < i128::from(kind.bits()) => {
                // `blo`/`bhi` are below the width, so the shifts are exact.
                Some((alo >> bhi, ahi >> blo))
            }
            _ => None,
        }
    }
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

/// Rewrites every prefix `!` whose span is in `nots` to `~` (see
/// [`Checker::integer_not`]). Spans are kept, so the second check reports at
/// the source's `!`.
pub(super) fn complement_nots(module: &Module, nots: &BTreeSet<(usize, usize)>) -> Module {
    fn rewrite(e: &mut Expr, nots: &BTreeSet<(usize, usize)>) {
        if let ExprKind::Unary(op, _) = &mut e.kind
            && op == "!"
            && nots.contains(&(e.span.start, e.span.end))
        {
            *op = "~".into();
        }
        for child in super::try_option::children(e) {
            rewrite(child, nots);
        }
    }
    let mut module = module.clone();
    for item in &mut module.items {
        let functions: Vec<&mut Function> = match item {
            Item::Function(f) => vec![f],
            Item::Impl { methods, .. } => methods.iter_mut().collect(),
            Item::Trait(t) => t.methods.iter_mut().collect(),
            Item::Conformance(c) => c.methods.iter_mut().collect(),
            Item::Adt(_) | Item::Effect(_) => vec![],
        };
        for f in functions {
            if let Some(body) = &mut f.body {
                rewrite(body, nots);
            }
        }
    }
    module
}
