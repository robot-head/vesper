//! Typing of the reserved vector intrinsics (§14.3). A child module of
//! `check`, entered through one hook in `Checker::call`. The `Isa[P]`
//! requirements are checked afterwards by [`crate::vector::check_world`].
use super::{Checker, Env, Place, Usage};
use crate::ast::*;
use crate::vector::{Elem, Intrinsic, Kind, Profile, Shape, SliceIo, shape_of};

impl Checker<'_> {
    /// Typecheck a call of a reserved `Vec::`, `Mask::` or `Isa::` path.
    /// Kept out of line so its locals do not enlarge every `call` frame.
    /// `None` when `name` is not in those namespaces.
    #[inline(never)]
    pub(super) fn vector_call(
        &mut self,
        name: &str,
        types: &[Type],
        args: &[Arg],
        env: &mut Env,
        span: Span,
        expected: &Type,
    ) -> Option<Type> {
        use Intrinsic as I;
        let intrinsic = match Intrinsic::lookup(name)? {
            Ok(i) => i,
            Err(()) => {
                self.error(
                    "E1402",
                    format!("unknown vector operation '{name}'"),
                    span.clone(),
                );
                for a in args {
                    self.expr_expected(&a.expr, env, Usage::Own, &Type::Infer);
                }
                return Some(Type::Infer);
            }
        };
        // A slice view argument (`s[a..b]`, `s.as_slice()`) has no type yet:
        // it selects the slice form like any `Slice[T]` operand.
        let first = args.first().map(|a| {
            if crate::seq::interval(&a.expr).is_some() || crate::seq::whole_view(&a.expr).is_some()
            {
                crate::seq::slice_of(Type::Infer)
            } else {
                self.peek_type(&a.expr, env)
            }
        });
        if SliceIo::applies(name, first.as_ref()) {
            return Some(self.vector_slice_io(name, intrinsic, types, args, env, span, expected));
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
        if intrinsic == Intrinsic::IsaSupported {
            self.arity(name, args, 0, span.clone());
            match types {
                [Type::Named(p, a)] if a.is_empty() && Profile::from_name(p).is_some() => {}
                _ => self.error(
                    "E1404",
                    "Isa::supported takes one ISA profile type argument",
                    span.clone(),
                ),
            }
            return Some(Type::named("bool"));
        }
        // The shape: explicit `[T, N]`, else the expected type, else the
        // first operand.
        let explicit = if intrinsic.takes_shape_arguments() {
            match types {
                [] => None,
                [elem, Type::Named(count, none)] if none.is_empty() => {
                    let shape = Elem::from_type(elem)
                        .zip(count.parse::<u16>().ok())
                        .and_then(|(e, n)| Shape::new(e, n));
                    if shape.is_none() {
                        self.error(
                            "E1401",
                            format!(
                                "[{elem}, {count}] is not a 128-, 256- or 512-bit shape of u8, u32, u64, i64, f32 or f64 lanes"
                            ),
                            span.clone(),
                        );
                        return Some(self.vector_fallback(args, env));
                    }
                    shape
                }
                _ => {
                    self.error(
                        "E1401",
                        format!("'{name}' takes a [T, N] lane type and count"),
                        span.clone(),
                    );
                    return Some(self.vector_fallback(args, env));
                }
            }
        } else {
            if !types.is_empty() {
                self.error(
                    "E0207",
                    format!("'{name}' does not take type arguments"),
                    span.clone(),
                );
            }
            None
        };
        let result_kind = if intrinsic.mask_result() {
            Kind::Mask
        } else {
            Kind::Vec
        };
        let from_expected = shape_of(expected)
            .filter(|(k, _)| *k == result_kind)
            .map(|(_, s)| s);
        let (shape, rest) = if let Some(s) = explicit {
            (s, 0)
        } else if intrinsic.takes_shape_arguments() {
            match from_expected {
                Some(s) => (s, 0),
                None => {
                    self.error(
                        "E1401",
                        format!("cannot infer the shape of '{name}'; write {name}[T, N](...)"),
                        span.clone(),
                    );
                    return Some(self.vector_fallback(args, env));
                }
            }
        } else {
            // Type the first operand, guided by the expected result where
            // the operand has the result's type.
            let Some(first) = args.first() else {
                self.arity(name, args, 1, span.clone());
                return Some(Type::Infer);
            };
            let operand_kind = if intrinsic.mask_operand() {
                Kind::Mask
            } else {
                Kind::Vec
            };
            let same_shape = operand_kind == result_kind && !matches!(intrinsic, I::Convert(_));
            let goal = match (from_expected, same_shape) {
                (Some(s), true) => s.ty(operand_kind),
                _ => Type::Infer,
            };
            let t = self.expr_expected(&first.expr, env, Usage::Own, &goal);
            match shape_of(&t) {
                Some((k, s)) if k == operand_kind => (s, 1),
                _ => {
                    if t != Type::Infer {
                        self.error(
                            "E1402",
                            format!(
                                "'{name}' expects a {} operand, found {t}",
                                operand_kind.name()
                            ),
                            first.expr.span.clone(),
                        );
                    }
                    for a in &args[1..] {
                        self.expr_expected(&a.expr, env, Usage::Own, &Type::Infer);
                    }
                    return Some(Type::Infer);
                }
            }
        };
        if let Some(message) = intrinsic.lane_error(shape.elem) {
            self.error("E1402", message, span.clone());
        }
        let vec = shape.vec_type();
        let mask = shape.mask_type();
        let lane = shape.elem.ty();
        let u64t = Type::named("u64");
        let string = Type::named("String");
        // Operand types after the first (already typed when `rest == 1`).
        let (operands, result): (Vec<Type>, Type) = match intrinsic {
            I::Splat => (vec![lane.clone()], vec.clone()),
            I::From => (vec![lane.clone(); usize::from(shape.lanes)], vec.clone()),
            I::Binary(_) => (vec![vec.clone(), vec.clone()], vec.clone()),
            I::Not | I::Unary(_) => (vec![vec.clone()], vec.clone()),
            I::MulAdd => (vec![vec.clone(), vec.clone(), vec.clone()], vec.clone()),
            I::Convert(c) => match c.result(shape) {
                Some(r) => (vec![vec.clone()], r.vec_type()),
                None => {
                    self.error(
                        "E1401",
                        format!(
                            "'{name}' of Vec{shape} would be a {}-bit vector; convert 128-, 256- or 512-bit halves",
                            u32::from(shape.lanes) * c.target(shape.elem).bits()
                        ),
                        span.clone(),
                    );
                    (vec![vec.clone()], Type::Infer)
                }
            },
            I::Shift(_) => (vec![vec.clone(), u64t.clone()], vec.clone()),
            I::Compare(_) => (vec![vec.clone(), vec.clone()], mask.clone()),
            I::Select => (vec![mask.clone(), vec.clone(), vec.clone()], vec.clone()),
            I::MaskBinary(_) => (vec![mask.clone(), mask.clone()], mask.clone()),
            I::MaskNot => (vec![mask.clone()], mask.clone()),
            I::MaskCount | I::MaskBitmask => (vec![mask.clone()], u64t.clone()),
            I::MaskAny | I::MaskAll => (vec![mask.clone()], Type::named("bool")),
            I::Reduce(_) => (vec![vec.clone()], lane.clone()),
            I::Extract => (vec![vec.clone(), u64t.clone()], lane.clone()),
            I::Replace => (vec![vec.clone(), u64t.clone(), lane.clone()], vec.clone()),
            I::Shuffle => (
                std::iter::once(vec.clone())
                    .chain(std::iter::repeat_n(u64t.clone(), usize::from(shape.lanes)))
                    .collect(),
                vec.clone(),
            ),
            I::LoadMasked => (vec![string.clone(), u64t.clone()], vec.clone()),
            I::InBounds => (vec![string.clone(), u64t.clone()], mask.clone()),
            I::Active => (vec![u64t.clone(), u64t.clone()], mask.clone()),
            I::IsaSupported | I::Load | I::Store | I::StoreMasked => {
                unreachable!("handled above")
            }
        };
        if intrinsic == I::MaskBitmask && shape.lanes > 64 {
            self.error(
                "E1402",
                "Mask::bitmask needs at most 64 lanes",
                span.clone(),
            );
        }
        if !self.arity(name, args, operands.len(), span.clone()) {
            for a in &args[rest.min(args.len())..] {
                self.expr_expected(&a.expr, env, Usage::Own, &Type::Infer);
            }
            return Some(result);
        }
        let constants = intrinsic.constant_positions(args.len());
        for (i, (a, goal)) in args.iter().zip(&operands).enumerate().skip(rest) {
            let usage = if *goal == string {
                Usage::Borrow(Pass::In)
            } else {
                Usage::Own
            };
            let t = self.expr_expected(&a.expr, env, usage, goal);
            self.expect_type(goal, &t, a.expr.span.clone());
            if matches!(t, Type::IntLiteral(_)) {
                self.types
                    .insert((a.expr.span.start, a.expr.span.end), goal.clone());
            }
            if constants.contains(&i) {
                match &a.expr.kind {
                    ExprKind::Literal(Literal::Int(n))
                        if *n >= 0 && *n < i128::from(shape.lanes) => {}
                    ExprKind::Literal(Literal::Int(n)) => self.error(
                        "E1402",
                        format!("lane index {n} is out of range for {} lanes", shape.lanes),
                        a.expr.span.clone(),
                    ),
                    _ => self.error(
                        "E1402",
                        format!("'{name}' takes literal lane indices"),
                        a.expr.span.clone(),
                    ),
                }
            }
        }
        Some(result)
    }
    /// `Vec::load[T, N](in s, at)`, `Vec::store(inout s, at, v)` and their
    /// masked forms on a `Slice[T]` or `ChunkSeq[T]` (see [`SliceIo`]).
    #[allow(clippy::too_many_arguments)]
    fn vector_slice_io(
        &mut self,
        name: &str,
        intrinsic: Intrinsic,
        types: &[Type],
        args: &[Arg],
        env: &mut Env,
        span: Span,
        expected: &Type,
    ) -> Type {
        let store = matches!(intrinsic, Intrinsic::Store | Intrinsic::StoreMasked);
        let masked = matches!(intrinsic, Intrinsic::LoadMasked | Intrinsic::StoreMasked);
        let result_fallback = if store { Type::unit() } else { Type::Infer };
        if !self.arity(name, args, if store { 3 } else { 2 }, span.clone()) {
            self.vector_fallback(args, env);
            return result_fallback;
        }
        if store && !types.is_empty() {
            self.error(
                "E0207",
                format!("'{name}' does not take type arguments"),
                span.clone(),
            );
        }
        // The sequence or slice, borrowed `in` by a load and `inout` by a
        // store.
        let s = &args[0];
        let pass = if store { Pass::Inout } else { Pass::In };
        if store && !(s.explicit && s.pass == Pass::Inout) {
            self.error(
                "E0308",
                format!("'{name}' writes its slice: pass it by 'inout'"),
                s.expr.span.clone(),
            );
        } else if !store && s.explicit && s.pass != Pass::In {
            self.error(
                "E0308",
                format!("'{name}' borrows its slice by 'in'"),
                s.expr.span.clone(),
            );
        }
        let key = (s.expr.span.start, s.expr.span.end);
        self.slice_args.insert(key, pass);
        let view =
            crate::seq::interval(&s.expr).is_some() || crate::seq::whole_view(&s.expr).is_some();
        let seq_ty = match Place::from_expr(&s.expr) {
            Some(p) if store && !view => {
                let t = self.place(&p, env, Usage::Borrow(Pass::Inout), s.expr.span.clone());
                self.types.insert(key, t.clone());
                if let Some(b) = env.bindings.get_mut(&p.root) {
                    b.mutation = std::sync::Arc::new(());
                    b.integer = None;
                }
                t
            }
            _ => self.expr(&s.expr, env, Usage::Borrow(pass)),
        };
        // The sequence stays loaned while the offset and the stored vector
        // are evaluated, as for an ordinary call's arguments.
        let loan_base = self.loans.len();
        if let Some(place) = self
            .seq_view_place(&s.expr)
            .or_else(|| Place::from_expr(&s.expr))
        {
            self.loans.push((place, pass, s.expr.span.clone()));
        }
        let elem_ty = crate::seq::elem(&seq_ty).map(crate::numeric::default_type);
        if elem_ty.is_none() && seq_ty != Type::Infer {
            self.error(
                "E1402",
                format!("'{name}' takes a Slice[T] or ChunkSeq[T], found '{seq_ty}'"),
                s.expr.span.clone(),
            );
        }
        let u64t = Type::named("u64");
        let at = &args[1];
        if at.pass != Pass::In || at.explicit {
            self.error(
                "E0308",
                format!("'{name}' takes its lane offset by value"),
                at.expr.span.clone(),
            );
        }
        let t = self.expr_expected(&at.expr, env, Usage::Own, &u64t);
        self.expect_type(&u64t, &t, at.expr.span.clone());
        if matches!(t, Type::IntLiteral(_)) {
            self.types
                .insert((at.expr.span.start, at.expr.span.end), u64t.clone());
        }
        // The shape: the stored vector's, else explicit `[T, N]`, else the
        // expected result.
        let shape = if store {
            let v = &args[2];
            if v.pass != Pass::In || v.explicit {
                self.error(
                    "E0308",
                    format!("'{name}' takes its vector by value"),
                    v.expr.span.clone(),
                );
            }
            let t = self.expr_expected(&v.expr, env, Usage::Own, &Type::Infer);
            match shape_of(&t) {
                Some((Kind::Vec, s)) => Some(s),
                _ => {
                    if t != Type::Infer {
                        self.error(
                            "E1402",
                            format!("'{name}' stores a Vec operand, found {t}"),
                            v.expr.span.clone(),
                        );
                    }
                    None
                }
            }
        } else {
            match types {
                [] => match shape_of(expected) {
                    Some((Kind::Vec, s)) => Some(s),
                    _ => {
                        self.error(
                            "E1401",
                            format!("cannot infer the shape of '{name}'; write {name}[T, N](...)"),
                            span.clone(),
                        );
                        None
                    }
                },
                [elem, Type::Named(count, none)] if none.is_empty() => {
                    let shape = Elem::from_type(elem)
                        .zip(count.parse::<u16>().ok())
                        .and_then(|(e, n)| Shape::new(e, n));
                    if shape.is_none() {
                        self.error(
                            "E1401",
                            format!(
                                "[{elem}, {count}] is not a 128-, 256- or 512-bit shape of u8, u32, u64, i64, f32 or f64 lanes"
                            ),
                            span.clone(),
                        );
                    }
                    shape
                }
                _ => {
                    self.error(
                        "E1401",
                        format!("'{name}' takes a [T, N] lane type and count"),
                        span.clone(),
                    );
                    None
                }
            }
        };
        if let (Some(shape), Some(elem_ty)) = (shape, &elem_ty)
            && *elem_ty != Type::Infer
            && *elem_ty != shape.elem.ty()
        {
            self.error(
                "E1402",
                format!(
                    "'{name}' moves {} lanes, but the sequence holds '{elem_ty}'",
                    shape.elem.name()
                ),
                span.clone(),
            );
        }
        self.loans.truncate(loan_base);
        if !masked {
            self.primitive_failure(env, span);
        }
        match (store, shape) {
            (true, _) => Type::unit(),
            (false, Some(s)) => s.vec_type(),
            (false, None) => Type::Infer,
        }
    }
    pub(super) fn arity(&mut self, name: &str, args: &[Arg], n: usize, span: Span) -> bool {
        if args.len() == n {
            return true;
        }
        self.error(
            "E0207",
            format!("'{name}' expects {n} arguments, found {}", args.len()),
            span,
        );
        false
    }
    fn vector_fallback(&mut self, args: &[Arg], env: &mut Env) -> Type {
        for a in args {
            self.expr_expected(&a.expr, env, Usage::Own, &Type::Infer);
        }
        Type::Infer
    }
}
