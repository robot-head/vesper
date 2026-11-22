//! `?` on `Option[T]` (P3b gaps lane 7, #1092; manuscript `TryExpr`).
//!
//! In a function or closure whose result is `Option[U]`, `e?` with
//! `e: Option[T]` is `T`: it evaluates `e`, continues with the payload of
//! `Option::Some`, and returns `Option::None` from the enclosing function on
//! `Option::None`. (`?` on `Result[T, E]` keeps its meaning: it performs
//! `Fail[E]`.)
//!
//! The checker types the form and records its span; the module is then
//! rewritten to the early exit it means and checked again, so every
//! backend runs it as an ordinary `match` with a `return`:
//!
//! ```text
//! match e { Option::Some($try) => $try, Option::None => return Option::None }
//! ```
//!
//! The `match` keeps the span of `e?`, and the `return` takes the span of
//! the `?` token, so the second check reports a `return` it rejects (a live
//! linear value, an inout loan, a handler clause) at the `?`. The two
//! expressions no source spells (the payload name and `Option::None`) get
//! spans after every span of the first check's world.
use super::Checker;
use super::loop_rules::ControlFrame;
use crate::ast::*;
use std::collections::BTreeSet;

impl Checker<'_> {
    /// Types `e?` with `e: Option[payload]`: the enclosing function or
    /// closure must return `Option` (E0200).
    pub(super) fn try_option(&mut self, e: &Expr, payload: &Type) -> Type {
        let result = self.control.iter().rev().find_map(|f| match f {
            ControlFrame::Function { expected, .. } => Some(expected.clone()),
            _ => None,
        });
        match result {
            Some(Type::Named(n, args)) if n == "Option" && args.len() == 1 => {}
            Some(Type::Infer) => {}
            _ => {
                self.error(
                    "E0200",
                    "'?' on Option[T] requires a function returning Option",
                    e.span.clone(),
                );
                return Type::Infer;
            }
        }
        self.option_tries.insert((e.span.start, e.span.end));
        payload.clone()
    }
}

/// The name bound to the payload of the `k`th rewritten `?`.
fn payload(k: usize) -> String {
    format!("$try{k}")
}

/// Rewrites every `?` whose span is in `tries` in `module`. `after` is past
/// every span the module's checked world uses.
pub(super) fn desugar(module: &Module, tries: &BTreeSet<(usize, usize)>, after: usize) -> Module {
    let mut module = module.clone();
    let mut next = after;
    let mut count = 0;
    let mut rewrite = |e: &mut Expr| {
        rewrite_expr(e, tries, &mut next, &mut count);
    };
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
                rewrite(body);
            }
        }
    }
    module
}

fn fresh(next: &mut usize) -> Span {
    let span = *next..*next + 1;
    *next += 2;
    span
}

fn rewrite_expr(
    e: &mut Expr,
    tries: &BTreeSet<(usize, usize)>,
    next: &mut usize,
    count: &mut usize,
) {
    for child in children(e) {
        rewrite_expr(child, tries, next, count);
    }
    if !tries.contains(&(e.span.start, e.span.end)) {
        return;
    }
    let ExprKind::Try(value) = &mut e.kind else {
        return;
    };
    let value = std::mem::replace(
        &mut **value,
        Expr {
            kind: ExprKind::Tuple(vec![]),
            span: e.span.clone(),
        },
    );
    let name = payload(*count);
    *count += 1;
    // The `?` token.
    let question = e.span.end.saturating_sub(1)..e.span.end;
    let some = Pattern {
        kind: PatternKind::Constructor {
            name: "Option::Some".into(),
            fields: vec![(
                "0".into(),
                Pattern {
                    kind: PatternKind::Bind(name.clone()),
                    span: question.clone(),
                },
            )],
            named: false,
        },
        span: e.span.clone(),
    };
    let none = Pattern {
        kind: PatternKind::Constructor {
            name: "Option::None".into(),
            fields: vec![],
            named: false,
        },
        span: question.clone(),
    };
    let exit = Expr {
        kind: ExprKind::Return(Some(Box::new(Expr {
            kind: ExprKind::Name("Option::None".into()),
            span: fresh(next),
        }))),
        span: question,
    };
    let use_payload = Expr {
        kind: ExprKind::Name(name),
        span: fresh(next),
    };
    e.kind = ExprKind::Match {
        value: Box::new(value),
        arms: vec![(some, use_payload), (none, exit)],
    };
}

/// The direct subexpressions of `e`.
pub(super) fn children(e: &mut Expr) -> Vec<&mut Expr> {
    if e.kind.control_children().is_some() {
        return e.kind.control_children_mut().unwrap_or_default();
    }
    match &mut e.kind {
        ExprKind::Literal(_) | ExprKind::Name(_) => vec![],
        ExprKind::Tuple(es) => es.iter_mut().collect(),
        ExprKind::Struct { fields, .. } => fields.iter_mut().map(|(_, e)| e).collect(),
        ExprKind::Block { statements, tail } => {
            let mut out: Vec<&mut Expr> = statements
                .iter_mut()
                .map(|s| match s {
                    Statement::Let { value, .. } => value,
                    Statement::Expr(e) => e,
                })
                .collect();
            if let Some(t) = tail {
                out.push(t);
            }
            out
        }
        ExprKind::Call { callee, args, .. } => {
            let mut out = vec![&mut **callee];
            out.extend(args.iter_mut().map(|a| &mut a.expr));
            out
        }
        ExprKind::Field(e, _) | ExprKind::Try(e) | ExprKind::Unary(_, e) => vec![&mut **e],
        ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Assign(_, a, b) => {
            vec![&mut **a, &mut **b]
        }
        ExprKind::If {
            condition,
            then,
            otherwise,
        } => {
            let mut out = vec![&mut **condition, &mut **then];
            if let Some(o) = otherwise {
                out.push(o);
            }
            out
        }
        ExprKind::Match { value, arms } => {
            let mut out = vec![&mut **value];
            out.extend(arms.iter_mut().map(|(_, e)| e));
            out
        }
        ExprKind::Closure { body, .. } => vec![&mut **body],
        ExprKind::Handle {
            state,
            body,
            ret,
            ops,
            ..
        } => {
            let mut out = vec![];
            if let Some((_, e)) = state {
                out.push(&mut **e);
            }
            out.push(&mut **body);
            out.push(&mut ret.body);
            out.extend(ops.iter_mut().map(|c| &mut c.body));
            out
        }
        ExprKind::Loop(_)
        | ExprKind::For { .. }
        | ExprKind::Range { .. }
        | ExprKind::Break(_)
        | ExprKind::Continue
        | ExprKind::Return(_) => vec![],
    }
}
