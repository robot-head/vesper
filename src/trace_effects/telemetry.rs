//! `Telemetrable` coherence and redaction, E0756–E0758 (see the parent module).
//!
//! Secret types are `capability_effects::SECRET_TYPES` declared as ADTs, and
//! a type is secret-bearing when it mentions one through its arguments or
//! fields, as in the `ConstTime` checker. That machinery is reused read-only.
use super::TELEMETRABLE;
use crate::ast::*;
use crate::capability_effects::SECRET_TYPES;
use crate::check::{Checked, World};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn check(module: &Module, checked: &Checked) -> Vec<Diagnostic> {
    let world = &checked.world;
    let mut diagnostics = vec![];
    let mut seen: BTreeMap<String, Span> = BTreeMap::new();
    for item in &module.items {
        let Item::Conformance(c) = item else { continue };
        if c.trait_ref.name != TELEMETRABLE || c.trait_ref.package.is_some() {
            continue;
        }
        let key = c.self_ty.to_string();
        if let Some(first) = seen.get(&key) {
            diagnostics.push(
                Diagnostic::new(
                    "E0756",
                    format!("'{key}' already has a Telemetrable instance; a type has exactly one exported encoding"),
                    c.span.clone(),
                )
                .with_related(first.clone(), "first instance")
                .with_help("remove one instance; telemetry encodings must be coherent per type"),
            );
        } else {
            seen.insert(key.clone(), c.span.clone());
        }
        if matches!(c.kind, ConformanceKind::Canonical) {
            let generic = matches!(&c.self_ty, Type::Named(n, a) if a.is_empty() && c.generics.iter().any(|g| &g.name == n));
            let secret = matches!(&c.self_ty, Type::Named(n, _) if SECRET_TYPES.contains(&n.as_str()) && world.adts.contains_key(n));
            if generic || secret {
                diagnostics.push(
                    Diagnostic::new(
                        "E0757",
                        format!(
                            "a canonical Telemetrable impl for '{key}' would export {}",
                            if secret { "a secret" } else { "every type, secrets included" }
                        ),
                        c.span.clone(),
                    )
                    .with_help("declare an explicit, named redacting model: `model Redacted…: Telemetrable for …`"),
                );
            }
        }
    }
    let generated: BTreeSet<&String> = world
        .elaboration
        .generated
        .iter()
        .filter(|(_, g)| g.trait_name == TELEMETRABLE)
        .map(|(name, _)| name)
        .collect();
    for name in generated {
        let Some(f) = world.functions.get(name) else {
            continue;
        };
        let Some(body) = &f.body else { continue };
        let mut w = Redaction {
            world,
            types: &checked.expression_types,
            diagnostics: &mut diagnostics,
        };
        w.expr(body, false);
    }
    diagnostics
}

struct Redaction<'a> {
    world: &'a World,
    types: &'a TypeMap,
    diagnostics: &'a mut Vec<Diagnostic>,
}

impl Redaction<'_> {
    /// `projected`: `e` is the base of a field projection, which only reads
    /// the projected field.
    fn expr(&mut self, e: &Expr, projected: bool) {
        if !projected
            && let Some(t) = self.types.get(&(e.span.start, e.span.end))
            && secret_bearing(self.world, t)
        {
            self.diagnostics.push(
                Diagnostic::new(
                    "E0758",
                    format!("a Telemetrable encoding reads secret data of type '{t}'"),
                    e.span.clone(),
                )
                .with_help(
                    "export only public fields, or emit TelemetryAttr::Redacted for the secret",
                ),
            );
            return;
        }
        match &e.kind {
            ExprKind::Field(base, _) => self.expr(base, true),
            _ => children(e, &mut |c| self.expr(c, false)),
        }
    }
}

/// Calls `f` on each direct subexpression of `e`.
fn children(e: &Expr, f: &mut dyn FnMut(&Expr)) {
    match &e.kind {
        ExprKind::Literal(_) | ExprKind::Name(_) => {}
        ExprKind::Tuple(es) => es.iter().for_each(&mut *f),
        ExprKind::Struct { fields, .. } => fields.iter().for_each(|(_, x)| f(x)),
        ExprKind::Block { statements, tail } => {
            for s in statements {
                match s {
                    Statement::Let { value, .. } => f(value),
                    Statement::Expr(x) => f(x),
                }
            }
            if let Some(t) = tail {
                f(t);
            }
        }
        ExprKind::Call { callee, args, .. } => {
            f(callee);
            args.iter().for_each(|a| f(&a.expr));
        }
        ExprKind::Field(x, _) | ExprKind::Try(x) | ExprKind::Unary(_, x) => f(x),
        ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Assign(_, a, b) => {
            f(a);
            f(b);
        }
        ExprKind::If {
            condition,
            then,
            otherwise,
        } => {
            f(condition);
            f(then);
            if let Some(o) = otherwise {
                f(o);
            }
        }
        ExprKind::Match { value, arms } => {
            f(value);
            arms.iter().for_each(|(_, x)| f(x));
        }
        ExprKind::Closure { body, .. } => f(body),
        ExprKind::Handle {
            state,
            body,
            ret,
            ops,
            ..
        } => {
            if let Some((_, s)) = state {
                f(s);
            }
            f(body);
            f(&ret.body);
            ops.iter().for_each(|c| f(&c.body));
        }
    }
}

/// Whether `t` contains a declared secret type through arguments or fields.
pub(crate) fn secret_bearing(world: &World, t: &Type) -> bool {
    fn visit(world: &World, t: &Type, seen: &mut BTreeSet<String>) -> bool {
        match t {
            Type::Named(n, args) => {
                if SECRET_TYPES.contains(&n.as_str()) && world.adts.contains_key(n) {
                    return true;
                }
                if args.iter().any(|a| visit(world, a, seen)) {
                    return true;
                }
                let Some(adt) = world.adts.get(n) else {
                    return false;
                };
                if !seen.insert(n.clone()) {
                    return false;
                }
                let found = adt
                    .variants
                    .iter()
                    .flat_map(|v| &v.fields)
                    .any(|f| visit(world, &f.ty, seen));
                seen.remove(n);
                found
            }
            Type::Tuple(ts) => ts.iter().any(|t| visit(world, t, seen)),
            _ => false,
        }
    }
    visit(world, t, &mut BTreeSet::new())
}
