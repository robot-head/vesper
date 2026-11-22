//! Static checking of `String` and `Str` (§13, port lane F2): primitive
//! method types and second-class views.
use super::{Checker, Env, Place, Usage};
use crate::ast::*;
use crate::strings::*;

impl Checker<'_> {
    /// Whether `e` is a `String` or `Str` operand, by its static type.
    pub(super) fn text_operand(&mut self, e: &Expr, env: &Env) -> bool {
        is_text(&self.text_receiver_type(e, env))
    }

    /// `l op r` for a comparison of strings: both operands are borrowed and
    /// compare in byte order; a `String` and a `Str` compare as views.
    pub(super) fn string_comparison(&mut self, l: &Expr, r: &Expr, env: &mut Env) -> Type {
        for e in [l, r] {
            let t = self.expr(e, env, Usage::Borrow(Pass::In));
            if t != Type::Infer && !is_text(&t) {
                self.error(
                    "E0200",
                    format!("a string compares only with a String or Str, found '{t}'"),
                    e.span.clone(),
                );
            }
        }
        Type::named("bool")
    }

    /// The static type of a string-method receiver, or `Infer`.
    fn text_receiver_type(&mut self, base: &Expr, env: &Env) -> Type {
        if let ExprKind::Literal(Literal::String(_)) = &base.kind {
            return Type::named(STRING_TYPE);
        }
        let t = self.peek_type(base, env);
        if t != Type::Infer || Place::from_expr(base).is_some() {
            return t;
        }
        // A call result: typed by its function or method, unchecked yet.
        self.receiver_type(base, env)
    }

    /// A primitive method call on a `String` or `Str` receiver; `None` when
    /// the receiver is not text or the method is not a primitive (prelude
    /// methods resolve as ordinary `String::m` / `Str::m` functions).
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn string_method_call(
        &mut self,
        base: &Expr,
        method: &str,
        types: &[Type],
        args: &[Arg],
        env: &mut Env,
        span: Span,
    ) -> Option<Type> {
        let base_ty = self.text_receiver_type(base, env);
        if !is_text(&base_ty) {
            return None;
        }
        let Some(op) = resolve(method, &base_ty) else {
            if is_str_type(&base_ty)
                && matches!(method, "push_str" | "push_char" | "clear" | "reserve")
            {
                self.error(
                    "E1532",
                    format!("'{method}' updates a String; a Str view is read-only"),
                    span,
                );
                self.expr(base, env, Usage::Borrow(Pass::In));
                for a in args {
                    self.expr(&a.expr, env, Usage::Own);
                }
                return Some(Type::Infer);
            }
            return None;
        };
        if !types.is_empty() {
            self.error(
                "E0207",
                format!("'{method}' takes no type arguments"),
                span.clone(),
            );
        }
        if args.len() != op.arity() {
            self.error(
                "E0207",
                format!(
                    "'{method}' expects {} arguments, found {}",
                    op.arity(),
                    args.len()
                ),
                span.clone(),
            );
        }
        if op.is_view() {
            // Views take no arguments; still check any extras for their
            // effects, since lowering evaluates every supplied argument.
            for a in args {
                self.expr(&a.expr, env, Usage::Own);
            }
            return Some(self.text_view(base, None, op, env, span));
        }
        if op.mutates() {
            let Some(p) = Place::from_expr(base) else {
                self.error(
                    "E0300",
                    "this string operation updates its receiver, which must be a place",
                    base.span.clone(),
                );
                self.expr(base, env, Usage::Own);
                return Some(Type::unit());
            };
            let t = self.place(&p, env, Usage::Borrow(Pass::Inout), base.span.clone());
            self.types.insert((base.span.start, base.span.end), t);
            if let Some(b) = env.bindings.get_mut(&p.root) {
                b.mutation = std::sync::Arc::new(());
            }
        } else {
            let t = self.expr(base, env, Usage::Borrow(Pass::In));
            self.types.insert((base.span.start, base.span.end), t);
        }
        let u64_ty = Type::named("u64");
        let u8_ty = Type::named("u8");
        for (i, a) in args.iter().enumerate() {
            if i >= op.arity() {
                self.expr(&a.expr, env, Usage::Own);
                continue;
            }
            if op == StrOp::PushStr {
                if a.explicit && a.pass != Pass::In {
                    self.error(
                        "E0308",
                        "push_str borrows its source by 'in'",
                        a.expr.span.clone(),
                    );
                }
                self.slice_args
                    .insert((a.expr.span.start, a.expr.span.end), Pass::In);
                let t = self.expr(&a.expr, env, Usage::Borrow(Pass::In));
                if t != Type::Infer && !is_text(&t) {
                    self.expect_type(&Type::named(STR_TYPE), &t, a.expr.span.clone());
                }
                let source = Place::from_expr(&a.expr).or_else(|| self.seq_view_place(&a.expr));
                // Distinct field paths are disjoint, as for call arguments.
                if let (Some(src), Some(dst)) = (source, Place::from_expr(base))
                    && src.overlaps(&dst)
                {
                    self.error(
                        "E0301",
                        "push_str cannot read from the string it grows",
                        a.expr.span.clone(),
                    );
                }
                continue;
            }
            if a.explicit && a.pass != Pass::In {
                self.error(
                    "E0308",
                    format!("'{method}' takes this argument by value"),
                    a.expr.span.clone(),
                );
            }
            let u32_ty = Type::named("u32");
            let goal = if op == StrOp::PushChar {
                &u32_ty
            } else {
                &u64_ty
            };
            let t = self.expr_expected(&a.expr, env, Usage::Own, goal);
            self.expect_type(goal, &t, a.expr.span.clone());
            self.constrain_literals(&a.expr, goal);
        }
        if op.fallible() {
            self.primitive_failure(env, span);
        }
        Some(match op {
            StrOp::Len => u64_ty,
            StrOp::ByteAt => u8_ty,
            StrOp::TryByteAt => Type::Named("Option".into(), vec![u8_ty]),
            StrOp::PushChar => Type::named("bool"),
            _ => Type::unit(),
        })
    }

    /// `base[lo..hi]` on a `String` or `Str`; `None` for other bases.
    pub(super) fn string_interval(&mut self, e: &Expr, env: &mut Env) -> Option<Type> {
        let (base, lo, hi) = crate::seq::interval(e)?;
        if !is_text(&self.text_receiver_type(base, env)) {
            return None;
        }
        Some(self.text_view(base, Some((lo, hi)), StrOp::SubStr, env, e.span.clone()))
    }

    /// Registers `value`, the initializer of an immutable `let`, as a bound
    /// view when it is a string or slice view (`s.as_bytes()`, `s.as_str()`,
    /// `s[a..b]`, `s.as_slice()`) of a place that stays unchanged while the
    /// view is in scope: an `in` parameter, an immutable binding that is
    /// itself a view, or an immutable owned binding (a `let` without `mut`
    /// holding a `String`, a `ChunkSeq` or an aggregate of them), which
    /// cannot be written and, while the view is in scope, cannot be moved
    /// ([`Checker::check_view_sources`], SH3 lane 4). With `changeable`, also
    /// a place the function may change (a `let mut` local or an `inout` or
    /// `sink` parameter), which then cannot be written, lent `inout` or
    /// moved while the view is in scope (gap wave 1 lane 6). The bound view is a
    /// read-only `Str` or `Slice` local that cannot outlive its source: the
    /// binding is shared (`in`), so it cannot be moved, rebound mutably or
    /// written, and views are second-class (never returned or stored).
    pub(in crate::check) fn bind_view(
        &mut self,
        value: &Expr,
        env: &Env,
        changeable: bool,
    ) -> bool {
        let base = view_source(value);
        // A view of a string literal, or of the static text a `-> Str`
        // function returns, is a view of static bytes and never dangles.
        if base.is_some_and(|b| self.static_text_source(b)) {
            self.slice_args
                .insert((value.span.start, value.span.end), Pass::In);
            return true;
        }
        let Some(root) = base.and_then(Place::from_expr) else {
            return false;
        };
        let Some(b) = env.bindings.get(&root.root) else {
            return false;
        };
        let view = crate::seq::is_slice_type(&b.ty) || is_str_type(&b.ty);
        // A view of an owned or `inout` place the function may change (a
        // `let mut` local, an `inout` or `sink` parameter, or a field of
        // one) freezes the overlapping places while the view is in scope
        // ([`Checker::check_view_sources`]). Only a `let` (`changeable`)
        // records that loan: a `for` over such a view lends the place
        // through its own loop loan instead.
        let owned = b.frozen && b.pass == Pass::Sink;
        let changeable = changeable && matches!(b.pass, Pass::Sink | Pass::Inout);
        let bound = b.pass == Pass::In || (b.frozen && view) || owned || changeable;
        if bound {
            self.slice_args
                .insert((value.span.start, value.span.end), Pass::In);
        }
        bound
    }

    /// A string literal or a call of a `-> Str` function: static text.
    pub(in crate::check) fn static_text_source(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Literal(Literal::String(_)) => true,
            ExprKind::Call { callee, .. } => matches!(&callee.kind, ExprKind::Name(n)
                if self.world.functions.get(n).is_some_and(|f| is_str_type(&f.result))),
            _ => false,
        }
    }

    /// Records the bound view `name` (declared at `span`) of an immutable
    /// owned binding, whose source must not move while the view is in scope.
    pub(in crate::check) fn record_view_source(
        &mut self,
        value: &Expr,
        name: &str,
        span: Span,
        env: &Env,
    ) {
        let Some(source) = view_source(value).and_then(Place::from_expr) else {
            return;
        };
        if let Some(b) = env.bindings.get(&source.root)
            && matches!(b.pass, Pass::Sink | Pass::Inout)
        {
            let at = b.span.clone();
            self.view_sources.push((source, at, name.to_string(), span));
        }
    }

    /// Rejects a move, a write or an `inout` loan of `p` (`write`) while a
    /// bound view of an overlapping place is in scope (`E1531`).
    pub(in crate::check) fn check_view_sources(
        &mut self,
        p: &Place,
        env: &Env,
        span: &Span,
        write: bool,
    ) -> bool {
        let mut ok = true;
        // The accessed binding: a nested binding that shadows the source's
        // name is another place.
        let accessed = env.bindings.get(&p.root).map(|b| b.span.clone());
        for (source, binding, view, at) in self.view_sources.clone() {
            // The view is live while its binding is in scope, also while a
            // nested block, loop or match arm shadows it (it comes back
            // when the shadow ends): conservatively, a later binding of
            // the same name keeps the loan.
            let live = env
                .bindings
                .get(&view)
                .is_some_and(|b| b.span == at || b.span.start > at.start)
                || self
                    .shadowed
                    .iter()
                    .any(|frame| frame.get(&view).is_some_and(|b| b.span == at));
            if live && accessed.as_ref() == Some(&binding) && source.overlaps(p) {
                ok = false;
                self.error(
                    "E1531",
                    format!(
                        "cannot {} '{}' while the view '{view}' of it is in scope",
                        if write {
                            "write or lend 'inout'"
                        } else {
                            "move"
                        },
                        p.display()
                    ),
                    span.clone(),
                );
                self.related(at, "the view is bound here");
                self.help("A bound view lives until the end of its block; change or move the source after that block, or copy what the view reads.");
            }
        }
        ok
    }

    /// A `Str` (or, for `as_bytes`, `Slice[u8]`) view of `base`, allowed only
    /// as an `in` call argument registered in `slice_args`, or as a bound
    /// view ([`Checker::bind_view`]).
    fn text_view(
        &mut self,
        base: &Expr,
        bounds: Option<(&Expr, &Expr)>,
        op: StrOp,
        env: &mut Env,
        span: Span,
    ) -> Type {
        let base_ty = self.text_receiver_type(base, env);
        match self.slice_args.get(&(span.start, span.end)) {
            None => self.error(
                "E1531",
                "a string view is a second-class borrow: write it directly as an 'in' call argument",
                span.clone(),
            ),
            Some(Pass::In) => {}
            Some(_) => self.error(
                "E1531",
                "a string view is read-only and passes only to an 'in' parameter",
                span.clone(),
            ),
        }
        let u64_ty = Type::named("u64");
        if let Some((lo, hi)) = bounds {
            for b in [lo, hi] {
                let t = self.expr_expected(b, env, Usage::Own, &u64_ty);
                self.expect_type(&u64_ty, &t, b.span.clone());
                self.constrain_literals(b, &u64_ty);
            }
        }
        match Place::from_expr(base) {
            Some(mut p) => {
                p.path.push(match bounds {
                    Some((lo, hi)) => crate::seq::interval_segment(lo, hi),
                    None => crate::seq::WHOLE_SEGMENT.into(),
                });
                self.place(&p, env, Usage::Borrow(Pass::In), base.span.clone());
            }
            // A view of a temporary (a literal or a call result) lives as
            // long as the call it is passed to.
            None => {
                self.expr(base, env, Usage::Borrow(Pass::In));
            }
        }
        self.types.insert((base.span.start, base.span.end), base_ty);
        if op == StrOp::AsBytes {
            crate::seq::slice_of(Type::named("u8"))
        } else {
            Type::named(STR_TYPE)
        }
    }
}

/// The base of a string or slice view expression (`s.as_bytes()`,
/// `s.as_str()`, `s[a..b]`, `s.as_slice()`).
fn view_source(value: &Expr) -> Option<&Expr> {
    whole_view(value)
        .or_else(|| crate::seq::interval(value).map(|(b, _, _)| b))
        .or_else(|| match crate::seq::whole_view(value) {
            Some((b, false)) => Some(b),
            _ => None,
        })
}
