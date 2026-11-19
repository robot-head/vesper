//! Static checking of fixed-size array writes and methods (§5, #1091):
//! `a[i] op= v`, `a.set(i, v)` and `a.len()` on an array `[T; N]` (see
//! `crate::arrays`). Reads `a[i]` are the tuple's dynamic index.
use super::{Checker, Env, Place, Usage, integer, numeric};
use crate::arrays::{ARRAY_OPERATION, element};
use crate::ast::*;
use std::sync::Arc;

impl Checker<'_> {
    /// Whether `index` is provably in `0..n` here: a constant or a
    /// dominating guard's bounds.
    pub(super) fn array_index_total(&self, index: &Expr, env: &Env, n: usize) -> bool {
        self.array_index_bounds(index, env, 0)
            .is_some_and(|(lo, hi)| lo >= 0 && hi < n as i128)
    }

    /// The checker's bounds of `index`, through `+` and `-` of a constant
    /// (`a[i - 1]`, `a[i + 1]`), which raise on overflow and so cannot wrap.
    fn array_index_bounds(&self, index: &Expr, env: &Env, depth: usize) -> Option<(i128, i128)> {
        if let Some(bounds) = self.integer_bounds(index, env) {
            return Some(bounds);
        }
        let ExprKind::Binary(op, a, b) = &index.kind else {
            return None;
        };
        if depth > 8 || !matches!(op.as_str(), "+" | "-") {
            return None;
        }
        let (lo, hi) = self.array_index_bounds(a, env, depth + 1)?;
        let c = self.integer_fact(b, env)?;
        Some(if op == "+" {
            (lo + c, hi + c)
        } else {
            (lo - c, hi - c)
        })
    }

    /// The bounds of `a.len()` on an array `a`: its constant length.
    pub(super) fn array_len_bounds(&self, e: &Expr, env: &Env) -> Option<(i128, i128)> {
        let ExprKind::Call { callee, args, .. } = &e.kind else {
            return None;
        };
        let ExprKind::Field(base, method) = &callee.kind else {
            return None;
        };
        if method != "len" || !args.is_empty() {
            return None;
        }
        let n = element(&self.peek_type(base, env))?.1 as i128;
        Some((n, n))
    }

    /// `base[index] op= value` on an array place, or `None` when `base` is
    /// not an array.
    #[inline(never)]
    pub(super) fn array_assign(
        &mut self,
        e: &Expr,
        op: &str,
        target: &Expr,
        value: &Expr,
        env: &mut Env,
    ) -> Option<Type> {
        let ExprKind::Index(base, index) = &target.kind else {
            return None;
        };
        if matches!(index.kind, ExprKind::Range { .. }) {
            return None;
        }
        let array = self.peek_type(base, env);
        let (element_ty, n) = element(&array).map(|(t, n)| (t.clone(), n))?;
        let Some(p) = Place::from_expr(base) else {
            self.error(
                ARRAY_OPERATION,
                "an array element write needs an array place, such as a `let mut` binding or a field",
                base.span.clone(),
            );
            self.expr(value, env, Usage::Own);
            return Some(Type::unit());
        };
        let place_ty = self.place(&p, env, Usage::Borrow(Pass::Inout), base.span.clone());
        self.types
            .insert((base.span.start, base.span.end), place_ty.clone());
        self.types
            .insert((target.span.start, target.span.end), element_ty.clone());
        let i = self.expr(index, env, Usage::Own);
        if !integer(&i) {
            self.error("E0200", "index must be an integer", index.span.clone());
        }
        self.array_element_write(&e.span, op, &p, index, value, &element_ty, n, env);
        Some(Type::unit())
    }

    /// `a.len()` and `a.set(i, v)` on an array, or `None` when `base` is not
    /// an array.
    #[inline(never)]
    pub(super) fn array_method_call(
        &mut self,
        base: &Expr,
        method: &str,
        args: &[Arg],
        env: &mut Env,
        span: Span,
    ) -> Option<Type> {
        if !matches!(method, "len" | "set") {
            return None;
        }
        let array = self.peek_type(base, env);
        let (element_ty, n) = element(&array).map(|(t, n)| (t.clone(), n))?;
        let arity = if method == "len" { 0 } else { 2 };
        if args.len() != arity {
            self.error(
                "E0207",
                format!("'{method}' expects {arity} arguments, found {}", args.len()),
                span.clone(),
            );
            for a in args {
                self.expr(&a.expr, env, Usage::Own);
            }
            return Some(Type::Infer);
        }
        if method == "len" {
            self.expr(base, env, Usage::Borrow(Pass::In));
            return Some(Type::named("u64"));
        }
        let Some(p) = Place::from_expr(base) else {
            self.error(
                ARRAY_OPERATION,
                "'set' writes an array place, such as a `let mut` binding or a field",
                base.span.clone(),
            );
            for a in args {
                self.expr(&a.expr, env, Usage::Own);
            }
            return Some(Type::unit());
        };
        let place_ty = self.place(&p, env, Usage::Borrow(Pass::Inout), base.span.clone());
        self.types
            .insert((base.span.start, base.span.end), place_ty);
        let u64_ty = Type::named("u64");
        let t = self.expr_expected(&args[0].expr, env, Usage::Own, &u64_ty);
        self.expect_type(&u64_ty, &t, args[0].expr.span.clone());
        self.array_element_write(&span, "=", &p, &args[0].expr, &args[1].expr, &element_ty, n, env);
        Some(Type::unit())
    }

    /// The shared rules of an element write at `at`: an integer index, the
    /// bounds failure, the value's type and a compound operator's overflow.
    #[allow(clippy::too_many_arguments)]
    fn array_element_write(
        &mut self,
        at: &Span,
        op: &str,
        p: &Place,
        index: &Expr,
        value: &Expr,
        element_ty: &Type,
        n: usize,
        env: &mut Env,
    ) {
        if !self.array_index_total(index, env, n) {
            self.primitive_failure(env, at.clone());
        }
        let actual = self.expr_expected(value, env, Usage::Own, element_ty);
        self.expect_type(element_ty, &actual, value.span.clone());
        if self.mode(element_ty) != Mode::Copy {
            self.error(
                "E0311",
                "array element writes need copy elements",
                value.span.clone(),
            );
        }
        if op != "=" {
            if !numeric(element_ty) {
                self.error(
                    "E0200",
                    "compound assignment requires numeric operands",
                    at.clone(),
                );
            }
            if let Some(kind) = crate::numeric::IntegerKind::from_type(element_ty)
                && crate::numeric::may_overflow(op, kind)
            {
                self.overflow_failure(env, at.clone());
            }
            if matches!(op, "/=" | "%=") && integer(element_ty) && !self.nonzero(value, env) {
                self.primitive_failure(env, at.clone());
            }
        }
        if let Some(binding) = env.bindings.get_mut(&p.root) {
            binding.mutation = Arc::new(());
            binding.integer = None;
            binding.nonzero = false;
            binding.range = None;
        }
    }
}
