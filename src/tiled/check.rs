//! Static checking of `TiledSeq[T]` operations, hot/cold column projections
//! (`[T-CallProj]`), and SIMD tile loads (§12.5).
use super::{Checker, Env, Place, Usage};
use crate::ast::*;
use crate::tiled::*;
use crate::vector::Profile;

impl Checker<'_> {
    /// Checks a static `TiledSeq::op[...](...)` call. Returns `None` if
    /// `name` is not in the `TiledSeq::` namespace.
    #[inline(never)]
    pub(super) fn tiled_call(
        &mut self,
        name: &str,
        types: &[Type],
        args: &[Arg],
        env: &mut Env,
        span: Span,
        expected: &Type,
    ) -> Option<Type> {
        let op = name.strip_prefix("TiledSeq::")?;
        match op {
            "new" => {
                self.arity(name, args, 0, span.clone());
                let elem = self.resolve_tiled_elem(types, expected, &span);
                Some(Type::Named(TILED_SEQ_TYPE.into(), vec![elem]))
            }
            "with_capacity" => {
                self.arity(name, args, 1, span.clone());
                if let Some(a) = args.first() {
                    if a.explicit && a.pass != Pass::In {
                        self.error(
                            "E0308",
                            "TiledSeq::with_capacity takes capacity by value",
                            a.expr.span.clone(),
                        );
                    }
                    let cap_ty = self.expr_expected(&a.expr, env, Usage::Own, &Type::named("u64"));
                    self.expect_type(&Type::named("u64"), &cap_ty, a.expr.span.clone());
                }
                let elem = self.resolve_tiled_elem(types, expected, &span);
                Some(Type::Named(TILED_SEQ_TYPE.into(), vec![elem]))
            }
            "with_region" => {
                self.arity(name, args, 2, span.clone());
                if let Some(a) = args.first() {
                    if a.explicit && a.pass != Pass::In {
                        self.error(
                            "E0308",
                            "TiledSeq::with_region takes region capability by 'in'",
                            a.expr.span.clone(),
                        );
                    }
                    let cap = match &a.expr.kind {
                        ExprKind::Name(label) if !env.bindings.contains_key(label) => {
                            self.region_capability(label)
                        }
                        _ => None,
                    };
                    if let Some(effect) = cap {
                        self.require_effect(&effect, env, span.clone());
                    } else {
                        self.error(
                            "E0790",
                            "TiledSeq::with_region expects an active 'region' capability as its first argument",
                            a.expr.span.clone(),
                        );
                    }
                }
                if let Some(a) = args.get(1) {
                    let cap_ty = self.expr_expected(&a.expr, env, Usage::Own, &Type::named("u64"));
                    self.expect_type(&Type::named("u64"), &cap_ty, a.expr.span.clone());
                }
                let elem = self.resolve_tiled_elem(types, expected, &span);
                Some(Type::Named(TILED_SEQ_TYPE.into(), vec![elem]))
            }
            "len" | "tile_count" | "push" | "get" | "set" | "col_get" | "col_set"
            | "load_tile_vec" => {
                let Some(first) = args.first() else {
                    self.arity(name, args, 1, span.clone());
                    return Some(Type::Infer);
                };
                let (method_types, elem_hint) = if let [first_ty, rest @ ..] = types
                    && matches!(op, "col_get" | "col_set" | "load_tile_vec")
                {
                    (rest, Some(first_ty))
                } else {
                    (types, None)
                };
                let base_ty = self.peek_type(&first.expr, env);
                if let Some(elem) = elem_hint
                    && let Some(actual_elem) = seq_elem(&base_ty)
                {
                    self.expect_type(elem, actual_elem, first.expr.span.clone());
                }
                let pass = if matches!(op, "push" | "set" | "col_set") {
                    Pass::Inout
                } else {
                    Pass::In
                };
                if first.explicit && first.pass != pass {
                    self.error(
                        "E0308",
                        format!("'{name}' expects {:?} on the sequence parameter", pass),
                        first.expr.span.clone(),
                    );
                }
                self.check_tiled_method(&first.expr, op, method_types, &args[1..], env, span)
            }
            _ => {
                self.error(
                    "E1508",
                    format!("unknown TiledSeq operation '{name}'"),
                    span,
                );
                for a in args {
                    self.expr(&a.expr, env, Usage::Own);
                }
                Some(Type::Infer)
            }
        }
    }

    /// Checks a method call `seq.method[...](...)` when `seq` has type
    /// `TiledSeq[T]`. Returns `None` if `seq` is not a `TiledSeq`.
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn tiled_method_call(
        &mut self,
        base: &Expr,
        method: &str,
        types: &[Type],
        args: &[Arg],
        env: &mut Env,
        span: Span,
        _expected: &Type,
    ) -> Option<Type> {
        let base_ty = self.peek_type(base, env);
        if !is_seq_type(&base_ty) {
            return None;
        }
        self.check_tiled_method(base, method, types, args, env, span)
    }

    fn resolve_tiled_elem(&mut self, types: &[Type], expected: &Type, span: &Span) -> Type {
        let elem = match types {
            [t] => t.clone(),
            [] => match seq_elem(expected) {
                Some(t) => t.clone(),
                None => {
                    self.error(
                        "E0208",
                        "cannot infer element type of TiledSeq; write TiledSeq::new[T]()",
                        span.clone(),
                    );
                    return Type::Infer;
                }
            },
            _ => {
                self.error(
                    "E0205",
                    format!(
                        "TiledSeq constructor expects 1 type argument, found {}",
                        types.len()
                    ),
                    span.clone(),
                );
                return Type::Infer;
            }
        };
        let elem = crate::numeric::default_type(&elem);
        self.valid_type(&elem, &self.generics.clone(), span.clone());
        self.check_tiled_record_type(&elem, span.clone());
        elem
    }

    pub(crate) fn check_tiled_record_type(&mut self, elem: &Type, span: Span) -> Option<u32> {
        let Type::Named(name, args) = elem else {
            if *elem != Type::Infer {
                self.error(
                    "E1504",
                    format!(
                        "TiledSeq[T] requires a 'tiled(N) struct' element type, found '{elem}'"
                    ),
                    span,
                );
            }
            return None;
        };
        if self.generics.iter().any(|g| g.name == *name) && args.is_empty() {
            return None;
        }
        let adt = self.world.adts.get(name)?;
        if let Some(w) = adt.tile_width {
            Some(w)
        } else {
            self.error(
                "E1504",
                format!("TiledSeq[T] requires a 'tiled(N) struct' type argument, but '{name}' is not tiled"),
                span,
            );
            None
        }
    }

    fn borrow_tiled_receiver(
        &mut self,
        base: &Expr,
        env: &mut Env,
        usage: Usage,
        projected_col: Option<&str>,
    ) {
        if let Some(p) = Place::from_expr(base) {
            if let Some(col) = projected_col {
                let mut col_place = p;
                col_place.path.push(col.to_string());
                self.place(&col_place, env, usage, base.span.clone());
                let base_ty = self.peek_type(base, env);
                self.types
                    .entry((base.span.start, base.span.end))
                    .or_insert(base_ty);
                return;
            }
            // Read-only metadata (`len`, `tile_count`) is allowed on any
            // projected `TiledSeq` parameter without reading column payloads.
            if matches!(usage, Usage::Borrow(Pass::In))
                && let Some(b) = env.bindings.get(&p.root)
                && !b.projections.is_empty()
            {
                let projs = b.projections.clone();
                for path in projs {
                    let mut proj_place = p.clone();
                    proj_place.path.extend(path);
                    self.place(&proj_place, env, usage, base.span.clone());
                }
                let base_ty = self.peek_type(base, env);
                self.types
                    .entry((base.span.start, base.span.end))
                    .or_insert(base_ty);
                return;
            }
        }
        self.expr(base, env, usage);
    }

    fn check_tiled_method(
        &mut self,
        base: &Expr,
        method: &str,
        types: &[Type],
        args: &[Arg],
        env: &mut Env,
        span: Span,
    ) -> Option<Type> {
        let base_ty = self.peek_type(base, env);
        let elem_ty = seq_elem(&base_ty).cloned()?;
        let adt = self
            .world
            .adts
            .get(elem_ty.name())
            .filter(|a| a.tile_width.is_some())
            .cloned();

        match method {
            "len" | "tile_count" => {
                if !types.is_empty() {
                    self.error(
                        "E0207",
                        format!("TiledSeq::{method} does not take type arguments"),
                        span.clone(),
                    );
                }
                if !args.is_empty() {
                    self.error(
                        "E0207",
                        format!("TiledSeq::{method} takes 0 arguments, found {}", args.len()),
                        span,
                    );
                    for a in args {
                        self.expr(&a.expr, env, Usage::Own);
                    }
                }
                self.borrow_tiled_receiver(base, env, Usage::Borrow(Pass::In), None);
                Some(Type::named("u64"))
            }
            "push" => {
                if !types.is_empty() {
                    self.error(
                        "E0207",
                        "TiledSeq::push takes no type arguments",
                        span.clone(),
                    );
                }
                self.borrow_tiled_receiver(base, env, Usage::Borrow(Pass::Inout), None);
                if args.len() != 1 {
                    self.error(
                        "E0207",
                        format!("TiledSeq::push expects 1 argument, found {}", args.len()),
                        span,
                    );
                    for a in args {
                        self.expr(&a.expr, env, Usage::Own);
                    }
                } else {
                    let val_ty = self.expr_expected(&args[0].expr, env, Usage::Own, &elem_ty);
                    self.expect_type(&elem_ty, &val_ty, args[0].expr.span.clone());
                }
                Some(Type::unit())
            }
            "get" => {
                if !types.is_empty() {
                    self.error(
                        "E0207",
                        "TiledSeq::get takes no type arguments",
                        span.clone(),
                    );
                }
                self.borrow_tiled_receiver(base, env, Usage::Borrow(Pass::In), None);
                if self.mode(&elem_ty) != Mode::Copy && elem_ty != Type::Infer {
                    self.error(
                        "E1507",
                        format!(
                            "cannot copy '{elem_ty}' out of TiledSeq::get because it is not Copy; read copy columns with col_get"
                        ),
                        span.clone(),
                    );
                }
                if args.len() != 1 {
                    self.error(
                        "E0207",
                        format!("TiledSeq::get expects 1 argument, found {}", args.len()),
                        span,
                    );
                    for a in args {
                        self.expr(&a.expr, env, Usage::Own);
                    }
                } else {
                    let idx_ty =
                        self.expr_expected(&args[0].expr, env, Usage::Own, &Type::named("u64"));
                    self.expect_type(&Type::named("u64"), &idx_ty, args[0].expr.span.clone());
                }
                Some(elem_ty)
            }
            "set" => {
                if !types.is_empty() {
                    self.error(
                        "E0207",
                        "TiledSeq::set takes no type arguments",
                        span.clone(),
                    );
                }
                self.borrow_tiled_receiver(base, env, Usage::Borrow(Pass::Inout), None);
                if args.len() != 2 {
                    self.error(
                        "E0207",
                        format!("TiledSeq::set expects 2 arguments, found {}", args.len()),
                        span,
                    );
                    for a in args {
                        self.expr(&a.expr, env, Usage::Own);
                    }
                } else {
                    let idx_ty =
                        self.expr_expected(&args[0].expr, env, Usage::Own, &Type::named("u64"));
                    self.expect_type(&Type::named("u64"), &idx_ty, args[0].expr.span.clone());
                    let val_ty = self.expr_expected(&args[1].expr, env, Usage::Own, &elem_ty);
                    self.expect_type(&elem_ty, &val_ty, args[1].expr.span.clone());
                }
                Some(Type::unit())
            }
            "col_get" | "col_set" => {
                let col_name = match types {
                    [c] if field_arg_name(c).is_some() => field_arg_name(c).map(str::to_string),
                    _ => {
                        self.error(
                            "E1508",
                            format!("'TiledSeq::{method}' expects one column name [field]"),
                            span.clone(),
                        );
                        None
                    }
                };
                let usage = if method == "col_set" {
                    Usage::Borrow(Pass::Inout)
                } else {
                    Usage::Borrow(Pass::In)
                };
                self.borrow_tiled_receiver(base, env, usage, col_name.as_deref());
                let field_ty = match (&adt, &col_name) {
                    (Some(a), Some(c)) => a
                        .variants
                        .first()
                        .and_then(|v| v.fields.iter().find(|f| &f.name == c))
                        .map(|f| f.ty.clone())
                        .unwrap_or_else(|| {
                            self.error(
                                "E0202",
                                format!("tiled struct '{}' has no field '{c}'", a.name),
                                span.clone(),
                            );
                            Type::Infer
                        }),
                    _ => Type::Infer,
                };
                if method == "col_get" {
                    if self.mode(&field_ty) != Mode::Copy && field_ty != Type::Infer {
                        self.error(
                            "E1507",
                            format!("column '{}' has non-Copy type {field_ty} and cannot be read with col_get", col_name.unwrap_or_default()),
                            span.clone(),
                        );
                    }
                    if args.len() != 1 {
                        self.error(
                            "E0207",
                            format!("TiledSeq::col_get expects 1 argument, found {}", args.len()),
                            span,
                        );
                        for a in args {
                            self.expr(&a.expr, env, Usage::Own);
                        }
                    } else {
                        let idx_ty =
                            self.expr_expected(&args[0].expr, env, Usage::Own, &Type::named("u64"));
                        self.expect_type(&Type::named("u64"), &idx_ty, args[0].expr.span.clone());
                    }
                    Some(field_ty)
                } else {
                    if args.len() != 2 {
                        self.error(
                            "E0207",
                            format!(
                                "TiledSeq::col_set expects 2 arguments, found {}",
                                args.len()
                            ),
                            span,
                        );
                        for a in args {
                            self.expr(&a.expr, env, Usage::Own);
                        }
                    } else {
                        let idx_ty =
                            self.expr_expected(&args[0].expr, env, Usage::Own, &Type::named("u64"));
                        self.expect_type(&Type::named("u64"), &idx_ty, args[0].expr.span.clone());
                        let val_ty = self.expr_expected(&args[1].expr, env, Usage::Own, &field_ty);
                        self.expect_type(&field_ty, &val_ty, args[1].expr.span.clone());
                    }
                    Some(Type::unit())
                }
            }
            "load_tile_vec" => {
                let (explicit_profile, col_name) = match types {
                    [c] if field_arg_name(c).is_some() => {
                        (None, field_arg_name(c).map(str::to_string))
                    }
                    [Type::Named(p, pa), c] if pa.is_empty() && field_arg_name(c).is_some() => {
                        let c_name = field_arg_name(c).unwrap_or_default().to_string();
                        let prof = Profile::from_name(p);
                        if prof.is_none() {
                            self.error(
                                "E1404",
                                format!(
                                    "unknown ISA profile '{p}' in load_tile_vec[{p}, {c_name}]"
                                ),
                                span.clone(),
                            );
                        }
                        (prof, Some(c_name))
                    }
                    _ => {
                        self.error(
                            "E1508",
                            "load_tile_vec expects [field] or [Profile, field]",
                            span.clone(),
                        );
                        (None, None)
                    }
                };
                self.borrow_tiled_receiver(base, env, Usage::Borrow(Pass::In), col_name.as_deref());
                if args.len() != 1 {
                    self.error(
                        "E0207",
                        format!(
                            "load_tile_vec expects 1 tile index argument, found {}",
                            args.len()
                        ),
                        span.clone(),
                    );
                    for a in args {
                        self.expr(&a.expr, env, Usage::Own);
                    }
                } else {
                    let idx_ty =
                        self.expr_expected(&args[0].expr, env, Usage::Own, &Type::named("u64"));
                    self.expect_type(&Type::named("u64"), &idx_ty, args[0].expr.span.clone());
                }
                let Some(adt) = adt else {
                    return Some(Type::Infer);
                };
                let Some(col) = col_name else {
                    return Some(Type::Infer);
                };
                let Some(field) = adt
                    .variants
                    .first()
                    .and_then(|v| v.fields.iter().find(|f| f.name == col))
                else {
                    self.error(
                        "E0202",
                        format!("tiled struct '{}' has no field '{col}'", adt.name),
                        span,
                    );
                    return Some(Type::Infer);
                };
                if field.temperature != Some(FieldTemperature::Hot) {
                    self.error(
                        "E1505",
                        format!(
                            "cannot SIMD-load cold field '{}.{}' via load_tile_vec; cold fields live in the side slab",
                            adt.name, col
                        ),
                        span,
                    );
                    return Some(Type::Infer);
                }
                let tile_width = adt.tile_width.unwrap_or(0);
                let Some(shape) = hot_vector_shape(&field.ty, tile_width) else {
                    self.error(
                        "E1506",
                        format!(
                            "hot column '{}.{}' ({} x {}) does not map to a 128-, 256-, or 512-bit Vec[T, N]",
                            adt.name, col, tile_width, field.ty
                        ),
                        span,
                    );
                    return Some(Type::Infer);
                };
                if let Some(p) = explicit_profile
                    && shape.bits() > p.fixed_bits()
                {
                    self.error(
                        "E1403",
                        format!(
                            "ISA profile '{}' grants {}-bit vectors, but '{}.{}' tile needs {}-bit '{}'",
                            p.name(),
                            p.fixed_bits(),
                            adt.name,
                            col,
                            shape.bits(),
                            shape.vec_type()
                        ),
                        span,
                    );
                }
                Some(shape.vec_type())
            }
            _ => {
                self.error(
                    "E1508",
                    format!("unknown method '{method}' on TiledSeq"),
                    span,
                );
                self.borrow_tiled_receiver(base, env, Usage::Borrow(Pass::In), None);
                for a in args {
                    self.expr(&a.expr, env, Usage::Own);
                }
                Some(Type::Infer)
            }
        }
    }
}
