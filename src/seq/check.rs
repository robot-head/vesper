//! Static checking of `ChunkSeq[T]` and `Slice[T]` (§13, port lane F1):
//! operation types, element-mode rules, second-class slice arguments and
//! constructive disjointness of interval loans (`[T-CallSliceProj]`).
use super::{Checker, Env, Place, Usage};
use crate::ast::*;
use crate::seq::*;

/// One slice-valued argument of a call, for the disjointness check.
struct SliceLoan<'e> {
    place: Place,
    bounds: Option<(&'e Expr, &'e Expr)>,
    pass: Pass,
    span: Span,
}

impl Checker<'_> {
    /// `ChunkSeq::new`, `ChunkSeq::with_capacity` and `ChunkSeq::finish`.
    #[inline(never)]
    pub(super) fn seq_call(
        &mut self,
        name: &str,
        types: &[Type],
        args: &[Arg],
        env: &mut Env,
        span: Span,
        expected: &Type,
    ) -> Option<Type> {
        let op = name.strip_prefix("ChunkSeq::")?;
        match op {
            "new" | "with_capacity" => {
                let n = usize::from(op == "with_capacity");
                if args.len() != n {
                    self.error(
                        "E0207",
                        format!("'{name}' expects {n} arguments, found {}", args.len()),
                        span.clone(),
                    );
                }
                for (i, a) in args.iter().enumerate() {
                    if i < n {
                        self.seq_by_value(name, a);
                        let t = self.expr_expected(&a.expr, env, Usage::Own, &Type::named("u64"));
                        self.expect_type(&Type::named("u64"), &t, a.expr.span.clone());
                    } else {
                        self.expr(&a.expr, env, Usage::Own);
                    }
                }
                let elem = match types {
                    [t] => t.clone(),
                    [] => match elem(expected).filter(|_| is_seq_type(expected)) {
                        Some(t) => t.clone(),
                        None => {
                            self.error(
                                "E0208",
                                format!("cannot infer the element type; write {name}[T](...)"),
                                span.clone(),
                            );
                            Type::Infer
                        }
                    },
                    _ => {
                        self.error(
                            "E0205",
                            format!("'{name}' expects 1 type argument, found {}", types.len()),
                            span.clone(),
                        );
                        Type::Infer
                    }
                };
                let elem = crate::numeric::default_type(&elem);
                let ty = seq_of(elem);
                self.valid_type(&ty, &self.generics.clone(), span);
                Some(ty)
            }
            "finish" => {
                if !types.is_empty() || args.len() != 1 {
                    self.error(
                        "E0207",
                        "ChunkSeq::finish takes one sequence argument",
                        span.clone(),
                    );
                }
                let Some(a) = args.first() else {
                    return Some(Type::unit());
                };
                if a.explicit && a.pass != Pass::Sink {
                    self.error(
                        "E0308",
                        "ChunkSeq::finish consumes its sequence: pass it by 'sink'",
                        a.expr.span.clone(),
                    );
                }
                let t = self.expr(&a.expr, env, Usage::Own);
                if !is_seq_type(&t) && t != Type::Infer {
                    self.error(
                        "E1525",
                        format!("ChunkSeq::finish expects a ChunkSeq, found '{t}'"),
                        a.expr.span.clone(),
                    );
                }
                for extra in args.iter().skip(1) {
                    self.expr(&extra.expr, env, Usage::Own);
                }
                self.primitive_failure(env, span);
                Some(Type::unit())
            }
            _ => {
                self.error(
                    "E1525",
                    format!("unknown ChunkSeq operation '{name}'"),
                    span,
                );
                for a in args {
                    self.expr(&a.expr, env, Usage::Own);
                }
                Some(Type::Infer)
            }
        }
    }

    fn seq_by_value(&mut self, name: &str, a: &Arg) {
        if a.explicit && a.pass != Pass::In {
            self.error(
                "E0308",
                format!("'{name}' takes this argument by value"),
                a.expr.span.clone(),
            );
        }
    }

    /// A method call on a `ChunkSeq[T]` or `Slice[T]` receiver.
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn seq_method_call(
        &mut self,
        base: &Expr,
        method: &str,
        types: &[Type],
        args: &[Arg],
        env: &mut Env,
        span: Span,
    ) -> Option<Type> {
        let base_ty = self.peek_type(base, env);
        if !is_seq_or_slice(&base_ty) {
            return None;
        }
        let elem_ty = elem(&base_ty).cloned().unwrap_or(Type::Infer);
        let seq = is_seq_type(&base_ty);
        let Some(op) = resolve(method, Some(&base_ty)) else {
            self.error(
                "E1525",
                format!(
                    "unknown method '{method}' on {}",
                    if seq { CHUNK_SEQ_TYPE } else { SLICE_TYPE }
                ),
                span,
            );
            self.expr(base, env, Usage::Borrow(Pass::In));
            for a in args {
                self.expr(&a.expr, env, Usage::Own);
            }
            return Some(Type::Infer);
        };
        if !types.is_empty() {
            self.error(
                "E0207",
                format!("'{method}' takes no type arguments"),
                span.clone(),
            );
        }
        if op == SeqOp::AsSlice {
            return Some(self.slice_view(base, None, env, span, method == "as_slice_mut"));
        }
        let arity = match op {
            SeqOp::Len | SeqOp::SliceLen | SeqOp::Pop | SeqOp::Clear => 0,
            SeqOp::Push
            | SeqOp::Get
            | SeqOp::SliceGet
            | SeqOp::TryGet
            | SeqOp::Truncate
            | SeqOp::Extend => 1,
            _ => 2,
        };
        if args.len() != arity {
            self.error(
                "E0207",
                format!("'{method}' expects {arity} arguments, found {}", args.len()),
                span.clone(),
            );
        }
        self.seq_receiver(base, env, op.receiver_pass());
        let u64_ty = Type::named("u64");
        let mode = self.mode(&elem_ty);
        let known = elem_ty != Type::Infer;
        match op {
            SeqOp::Get | SeqOp::SliceGet | SeqOp::TryGet if known && mode != Mode::Copy => self
                .error(
                    "E1522",
                    format!(
                        "cannot copy '{elem_ty}' out of a sequence because it is not copy; use pop"
                    ),
                    span.clone(),
                ),
            SeqOp::Extend if known && mode != Mode::Copy => self.error(
                "E1522",
                format!("extend copies its elements, but '{elem_ty}' is not copy"),
                span.clone(),
            ),
            SeqOp::Set | SeqOp::SliceSet | SeqOp::TrySet | SeqOp::Clear | SeqOp::Truncate
                if known && mode == Mode::Linear =>
            {
                self.error(
                    "E1523",
                    format!(
                        "'{method}' would drop linear '{elem_ty}' elements; pop and consume them instead"
                    ),
                    span.clone(),
                );
            }
            _ => {}
        }
        for (i, a) in args.iter().enumerate() {
            let index_arg = matches!(
                (op, i),
                (
                    SeqOp::Get
                        | SeqOp::SliceGet
                        | SeqOp::Set
                        | SeqOp::SliceSet
                        | SeqOp::TryGet
                        | SeqOp::TrySet
                        | SeqOp::Truncate,
                    0
                )
            );
            if index_arg {
                self.seq_by_value(method, a);
                let t = self.expr_expected(&a.expr, env, Usage::Own, &u64_ty);
                self.expect_type(&u64_ty, &t, a.expr.span.clone());
            } else if op == SeqOp::Extend {
                if a.explicit && a.pass != Pass::In {
                    self.error(
                        "E0308",
                        "extend borrows its source slice by 'in'",
                        a.expr.span.clone(),
                    );
                }
                self.slice_args
                    .insert((a.expr.span.start, a.expr.span.end), Pass::In);
                let t = self.expr(&a.expr, env, Usage::Borrow(Pass::In));
                self.expect_type(&slice_of(elem_ty.clone()), &t, a.expr.span.clone());
                if let (Some(src), Some(dst)) =
                    (self.seq_view_place(&a.expr), Place::from_expr(base))
                    && src.root == dst.root
                {
                    self.error(
                        "E0301",
                        "extend cannot read from the sequence it grows",
                        a.expr.span.clone(),
                    );
                }
            } else if i < arity {
                let t = self.expr_expected(&a.expr, env, Usage::Own, &elem_ty);
                self.expect_type(&elem_ty, &t, a.expr.span.clone());
            } else {
                self.expr(&a.expr, env, Usage::Own);
            }
        }
        if op.fallible() {
            self.primitive_failure(env, span);
        }
        Some(match op {
            SeqOp::Len | SeqOp::SliceLen => u64_ty,
            SeqOp::Pop | SeqOp::TryGet => Type::Named("Option".into(), vec![elem_ty]),
            SeqOp::TrySet => Type::named("bool"),
            SeqOp::Get | SeqOp::SliceGet => elem_ty,
            _ => Type::unit(),
        })
    }

    /// Borrows a method receiver: `in` reads may use any expression, `inout`
    /// updates need a place.
    fn seq_receiver(&mut self, base: &Expr, env: &mut Env, pass: Pass) {
        if pass == Pass::Inout {
            let Some(p) = Place::from_expr(base) else {
                self.error(
                    "E0300",
                    "this sequence operation updates its receiver, which must be a place",
                    base.span.clone(),
                );
                self.expr(base, env, Usage::Own);
                return;
            };
            let t = self.place(&p, env, Usage::Borrow(Pass::Inout), base.span.clone());
            self.types.insert((base.span.start, base.span.end), t);
            if let Some(b) = env.bindings.get_mut(&p.root) {
                b.mutation = std::sync::Arc::new(());
            }
        } else {
            self.expr(base, env, Usage::Borrow(Pass::In));
        }
    }

    /// Whether `e` (an index expression) is a sequence or slice index.
    pub(super) fn seq_index_applies(&self, e: &Expr, base: &Expr, env: &Env) -> bool {
        interval(e).is_some() || is_seq_or_slice(&self.peek_type(base, env))
    }

    /// `base[index]` on a sequence or slice: an element read, or (with an
    /// interval index) a slice argument. `None` for other bases.
    #[inline(never)]
    pub(super) fn seq_index(
        &mut self,
        e: &Expr,
        base: &Expr,
        index: &Expr,
        env: &mut Env,
        usage: Usage,
    ) -> Option<Type> {
        if let Some((_, lo, hi)) = interval(e) {
            let inout = matches!(usage, Usage::Borrow(Pass::Inout));
            return Some(self.slice_view(base, Some((lo, hi)), env, e.span.clone(), inout));
        }
        let base_ty = self.peek_type(base, env);
        if !is_seq_or_slice(&base_ty) {
            return None;
        }
        let elem_ty = elem(&base_ty).cloned().unwrap_or(Type::Infer);
        self.expr(base, env, Usage::Borrow(Pass::In));
        let u64_ty = Type::named("u64");
        let t = self.expr_expected(index, env, Usage::Own, &u64_ty);
        self.expect_type(&u64_ty, &t, index.span.clone());
        if elem_ty != Type::Infer && self.mode(&elem_ty) != Mode::Copy {
            self.error(
                "E1522",
                format!(
                    "cannot copy '{elem_ty}' out of a sequence because it is not copy; use pop"
                ),
                e.span.clone(),
            );
        }
        self.primitive_failure(env, e.span.clone());
        Some(elem_ty)
    }

    /// A slice view of `base` (whole, or `[lo..hi]`), allowed only where a
    /// call argument was registered in `slice_args`.
    fn slice_view(
        &mut self,
        base: &Expr,
        bounds: Option<(&Expr, &Expr)>,
        env: &mut Env,
        span: Span,
        mut inout: bool,
    ) -> Type {
        let base_ty = self.peek_type(base, env);
        match self.slice_args.get(&(span.start, span.end)) {
            None => self.error(
                "E1521",
                "a slice is a second-class borrow: write it directly as an 'in' or 'inout' call argument",
                span.clone(),
            ),
            // The view is borrowed as the parameter borrows it.
            Some(Pass::Inout) => inout = true,
            Some(_) => {}
        }
        if bounds.is_none() && !is_seq_type(&base_ty) && base_ty != Type::Infer {
            self.error(
                "E1525",
                format!("as_slice expects a ChunkSeq, found '{base_ty}'"),
                span.clone(),
            );
        }
        if !is_seq_or_slice(&base_ty) {
            if base_ty != Type::Infer {
                self.error(
                    "E1525",
                    format!("slice intervals need a ChunkSeq or Slice, found '{base_ty}'"),
                    base.span.clone(),
                );
            }
            self.expr(base, env, Usage::Borrow(Pass::In));
            return Type::Infer;
        }
        let elem_ty = elem(&base_ty).cloned().unwrap_or(Type::Infer);
        let u64_ty = Type::named("u64");
        if let Some((lo, hi)) = bounds {
            for b in [lo, hi] {
                let t = self.expr_expected(b, env, Usage::Own, &u64_ty);
                self.expect_type(&u64_ty, &t, b.span.clone());
            }
        }
        let usage = Usage::Borrow(if inout { Pass::Inout } else { Pass::In });
        match Place::from_expr(base) {
            Some(mut p) => {
                p.path.push(match bounds {
                    Some((lo, hi)) => interval_segment(lo, hi),
                    None => WHOLE_SEGMENT.into(),
                });
                self.place(&p, env, usage, base.span.clone());
                self.types
                    .insert((base.span.start, base.span.end), base_ty.clone());
            }
            None => {
                self.error(
                    "E1521",
                    "slice the sequence through a place (a binding or a field)",
                    base.span.clone(),
                );
                self.expr(base, env, usage);
            }
        }
        slice_of(elem_ty)
    }

    /// The loaned place of a slice argument (`s[a..b]` or `s.as_slice()`),
    /// with its synthetic interval segment.
    pub(super) fn seq_view_place(&self, e: &Expr) -> Option<Place> {
        let (base, segment) = if let Some((base, lo, hi)) = interval(e) {
            (base, interval_segment(lo, hi))
        } else {
            let (base, _) = whole_view(e)?;
            (base, WHOLE_SEGMENT.to_string())
        };
        let mut p = Place::from_expr(base)?;
        p.path.push(segment);
        Some(p)
    }

    /// Registers the slice arguments of a call and proves every pair that
    /// shares a place and is not `in`/`in` disjoint by construction.
    pub(super) fn seq_call_slices(&mut self, actuals: &[(&Expr, Pass)], env: &Env) {
        let mut loans: Vec<SliceLoan> = vec![];
        for (e, pass) in actuals {
            let bounds = if let Some((_, lo, hi)) = interval(e) {
                Some((lo, hi))
            } else if whole_view(e).is_some() {
                None
            } else {
                continue;
            };
            if *pass == Pass::Sink {
                self.error(
                    "E1521",
                    "a slice is a second-class borrow and cannot be passed by 'sink'",
                    e.span.clone(),
                );
            }
            self.slice_args.insert((e.span.start, e.span.end), *pass);
            let Some(mut place) = self.seq_view_place(e) else {
                continue;
            };
            place.path.pop();
            loans.push(SliceLoan {
                place,
                bounds,
                pass: *pass,
                span: e.span.clone(),
            });
        }
        // Bounds are evaluated with the arguments, so a bound whose root an
        // argument mutates proves nothing.
        let mut mutated: Vec<String> = vec![];
        for (e, pass) in actuals {
            if matches!(pass, Pass::Inout | Pass::Sink)
                && let Some(p) = Place::from_expr(e)
            {
                mutated.push(p.root);
            }
            crate::regions::visit_expr(e, &mut |x| {
                if let ExprKind::Call { callee, args, .. } = &x.kind {
                    // A method may mutate its receiver.
                    if let ExprKind::Field(base, method) = &callee.kind
                        && !matches!(method.as_str(), "len" | "get" | "try_get" | "as_slice")
                        && let Some(p) = Place::from_expr(base)
                    {
                        mutated.push(p.root);
                    }
                    for a in args {
                        if matches!(a.pass, Pass::Inout | Pass::Sink)
                            && let Some(p) = Place::from_expr(&a.expr)
                        {
                            mutated.push(p.root.clone());
                        }
                    }
                }
            });
        }
        let stable = |b: &Expr| {
            let mut ok = true;
            crate::regions::visit_expr(b, &mut |x| {
                if let ExprKind::Name(n) = &x.kind
                    && mutated.contains(n)
                {
                    ok = false;
                }
            });
            ok
        };
        for (i, a) in loans.iter().enumerate() {
            for b in &loans[..i] {
                if a.place != b.place || (a.pass == Pass::In && b.pass == Pass::In) {
                    continue;
                }
                let disjoint = match (a.bounds, b.bounds) {
                    (Some((alo, ahi)), Some((blo, bhi))) => {
                        if interval_segment(alo, ahi) == interval_segment(blo, bhi) {
                            // Equal segments overlap as places; the loan check reports it.
                            continue;
                        }
                        let le = |x: &Expr, y: &Expr| {
                            same_bound(x, y)
                                || matches!(
                                    (self.integer_fact(x, env), self.integer_fact(y, env)),
                                    (Some(x), Some(y)) if x <= y
                                )
                        };
                        [alo, ahi, blo, bhi].into_iter().all(stable)
                            && (le(ahi, blo) || le(bhi, alo))
                    }
                    _ => false,
                };
                if !disjoint {
                    self.error(
                        "E0301",
                        format!(
                            "slice arguments of '{}' may overlap and violate alias XOR mutability",
                            a.place.display()
                        ),
                        a.span.clone(),
                    );
                    self.related(b.span.clone(), "earlier slice argument of the same place");
                    self.help("Split a sequence at a shared bound, as in s[0..mid] and s[mid..n], to prove the intervals disjoint.");
                }
            }
        }
    }

    /// `ChunkSeq[T]` and `Slice[T]` in a type annotation or signature.
    pub(super) fn valid_seq_type(&mut self, name: &str, args: &[Type], g: &[Generic], span: Span) {
        if args.len() != 1 {
            self.error(
                "E0101",
                format!("{name} expects 1 type argument, found {}", args.len()),
                span.clone(),
            );
        }
        for a in args {
            self.valid_type(a, g, span.clone());
            if matches!(a, Type::Function { .. }) {
                self.error(
                    "E1524",
                    format!("{name} elements cannot be functions; store data instead"),
                    span.clone(),
                );
            }
            if mentions_slice(a) {
                self.error(
                    "E1520",
                    format!("Slice[T] is a second-class borrow and cannot be a {name} element"),
                    span.clone(),
                );
            }
        }
    }
}
