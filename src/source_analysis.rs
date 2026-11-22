use crate::{ast::*, check::World};
use std::collections::BTreeSet;

pub(crate) fn visit(e: &Expr, callback: &mut impl FnMut(&Expr)) {
    callback(e);
    match &e.kind {
        ExprKind::Tuple(es) => {
            for e in es {
                visit(e, callback)
            }
        }
        ExprKind::Struct { fields, .. } => {
            for (_, e) in fields {
                visit(e, callback)
            }
        }
        ExprKind::Block { statements, tail } => {
            for s in statements {
                visit(
                    match s {
                        Statement::Let { value, .. } => value,
                        Statement::Expr(e) => e,
                    },
                    callback,
                )
            }
            if let Some(e) = tail {
                visit(e, callback)
            }
        }
        ExprKind::Call { callee, args, .. } => {
            visit(callee, callback);
            for a in args {
                visit(&a.expr, callback)
            }
        }
        ExprKind::Unary(_, e) | ExprKind::Field(e, _) | ExprKind::Try(e) => visit(e, callback),
        ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) | ExprKind::Assign(_, a, b) => {
            visit(a, callback);
            visit(b, callback)
        }
        ExprKind::If {
            condition,
            then,
            otherwise,
        } => {
            visit(condition, callback);
            visit(then, callback);
            if let Some(e) = otherwise {
                visit(e, callback)
            }
        }
        ExprKind::Match { value, arms } => {
            visit(value, callback);
            for (_, e) in arms {
                visit(e, callback)
            }
        }
        ExprKind::Closure { body, .. } => visit(body, callback),
        ExprKind::Handle {
            state,
            body,
            ret,
            ops,
            ..
        } => {
            if let Some((_, e)) = state {
                visit(e, callback)
            }
            visit(body, callback);
            visit(&ret.body, callback);
            for c in ops {
                visit(&c.body, callback)
            }
        }
        k @ (ExprKind::Loop(_)
        | ExprKind::For { .. }
        | ExprKind::Range { .. }
        | ExprKind::Break(_)
        | ExprKind::Continue
        | ExprKind::Return(_)) => {
            for c in k.control_children().into_iter().flatten() {
                visit(c, callback)
            }
        }
        _ => {}
    }
}
pub(crate) fn contains_function(world: &World, t: &Type) -> bool {
    fn visit(world: &World, t: &Type, seen: &mut BTreeSet<String>) -> bool {
        match t {
            Type::Function { .. } => true,
            Type::Tuple(ts) => ts.iter().any(|t| visit(world, t, seen)),
            Type::Named(n, args) => {
                if !seen.insert(n.clone()) {
                    return args.iter().any(|t| matches!(t, Type::Function { .. }));
                }
                let result = world.adts.get(n).is_some_and(|a| {
                    let vars = a
                        .generics
                        .iter()
                        .zip(args)
                        .map(|(g, t)| (g.name.clone(), t.clone()))
                        .collect();
                    a.variants
                        .iter()
                        .flat_map(|v| &v.fields)
                        .any(|f| visit(world, &substitute(&f.ty, &vars), seen))
                });
                seen.remove(n);
                result
            }
            _ => false,
        }
    }
    visit(world, t, &mut BTreeSet::new())
}
/// Whether the program needs Cranelift's NaN canonicalisation: any
/// `eff [DetFloat]` function promises bit-identical floats on x86-64 and
/// AArch64, whose default NaN bits differ (§36.2).
pub fn needs_nan_canonicalization(world: &World) -> bool {
    world.functions.values().any(|f| {
        crate::capability_effects::capabilities(&f.effects)
            .contains(&crate::capability_effects::Capability::DetFloat)
            || closure_capabilities(f).contains(&crate::capability_effects::Capability::DetFloat)
    })
}
/// Capability effects declared on closure literals anywhere in `f`'s body.
/// Closures are specialized into the function that contains them, so their
/// code is part of that function's Cranelift function.
pub fn closure_capabilities(f: &Function) -> BTreeSet<crate::capability_effects::Capability> {
    let mut out = BTreeSet::new();
    if let Some(body) = &f.body {
        visit(body, &mut |e| {
            if let ExprKind::Closure { effects, .. } = &e.kind {
                out.extend(crate::capability_effects::capabilities(effects));
            }
        });
    }
    out
}
