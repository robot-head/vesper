//! `Trace` rules E0750–E0754 on a checked World (see the parent module).
use super::{ACTIVE_SPAN, SPAN_CONTEXT, TRACE, is_named, mentions};
use crate::ast::*;
use crate::check::{Checked, World};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn check(checked: &Checked) -> Vec<Diagnostic> {
    let world = &checked.world;
    if !super::active(world) {
        return vec![];
    }
    let mut w = Walker {
        world,
        types: &checked.expression_types,
        diagnostics: vec![],
        labels: BTreeSet::new(),
        bindings: BTreeMap::new(),
    };
    for adt in world.adts.values() {
        if adt.name == ACTIVE_SPAN {
            continue;
        }
        if let Some(field) = adt
            .variants
            .iter()
            .flat_map(|v| &v.fields)
            .find(|f| mentions(&f.ty, ACTIVE_SPAN))
        {
            w.error(
                "E0750",
                format!(
                    "field '{}' of '{}' stores an ActiveSpan; a span cannot leave the function that started it",
                    field.name, adt.name
                ),
                adt.span.clone(),
                "finish the span with Trace::emit_ok, emit_err or emit_attr before the function returns",
            );
        }
    }
    for f in world.functions.values() {
        w.function(f);
    }
    w.diagnostics
}

#[derive(Clone, Copy)]
struct Scope {
    /// `Trace` is available (in the row, or inside a `handle Trace` body).
    trace: bool,
    /// Inside an operation clause of a `handle Trace` handler.
    clause: bool,
}

struct Walker<'a> {
    world: &'a World,
    types: &'a TypeMap,
    diagnostics: Vec<Diagnostic>,
    /// Labels of `Trace` instances in scope (`eff [t: Trace]`).
    labels: BTreeSet<String>,
    /// Declared or inferred types of the current function's bindings
    /// (flow-insensitive), for places the type map does not cover.
    bindings: BTreeMap<String, Type>,
}

impl Walker<'_> {
    fn error(&mut self, code: &str, message: String, span: Span, help: &str) {
        self.diagnostics
            .push(Diagnostic::new(code, message, span).with_help(help));
    }
    fn ty(&self, e: &Expr) -> Option<&Type> {
        self.types.get(&(e.span.start, e.span.end))
    }
    fn is_context(&self, e: &Expr) -> bool {
        self.ty(e).is_some_and(|t| is_named(t, SPAN_CONTEXT))
    }

    fn function(&mut self, f: &Function) {
        if mentions(&f.result, ACTIVE_SPAN) {
            self.error(
                "E0750",
                format!(
                    "'{}' returns an ActiveSpan; a span cannot leave the function that started it",
                    f.name
                ),
                f.span.clone(),
                "finish the span in this function, or take it as a `sink` parameter and finish it there",
            );
        }
        for p in &f.params {
            if mentions(&p.ty, ACTIVE_SPAN) && !is_named(&p.ty, ACTIVE_SPAN) {
                self.error(
                    "E0750",
                    format!("parameter '{}' of '{}' wraps an ActiveSpan", p.name, f.name),
                    p.span.clone(),
                    "pass the span itself as a `sink` parameter",
                );
            }
        }
        let Some(body) = &f.body else { return };
        self.labels = trace_labels(&f.effects);
        self.bindings = f
            .params
            .iter()
            .map(|p| (p.name.clone(), p.ty.clone()))
            .collect();
        let mut lets = vec![];
        crate::regions::visit_expr(body, &mut |x| {
            if let ExprKind::Block { statements, .. } = &x.kind {
                for st in statements {
                    if let Statement::Let {
                        pat:
                            Pattern {
                                kind: PatternKind::Bind(n),
                                ..
                            },
                        ty,
                        value,
                    } = st
                    {
                        let t = match ty {
                            Type::Infer => self.ty(value).cloned(),
                            t => Some(t.clone()),
                        };
                        if let Some(t) = t {
                            lets.push((n.clone(), t));
                        }
                    }
                }
            }
        });
        self.bindings.extend(lets);
        for p in &f.params {
            if is_named(&p.ty, SPAN_CONTEXT) && !uses_name(body, &p.name) {
                self.dropped_context(&p.name, p.span.clone());
            }
        }
        let scope = Scope {
            trace: f.effects.instances.iter().any(|e| e.name == TRACE),
            clause: false,
        };
        self.expr(body, scope);
    }

    fn dropped_context(&mut self, name: &str, span: Span) {
        self.error(
            "E0753",
            format!("trace context '{name}' is dropped: it is never propagated"),
            span,
            "pass the context to Trace::start_child or a callee, or do not request it",
        );
    }

    fn expr(&mut self, e: &Expr, scope: Scope) {
        if let Some(t) = self.ty(e)
            && mentions(t, ACTIVE_SPAN)
            && !is_named(t, ACTIVE_SPAN)
            && !matches!(e.kind, ExprKind::Closure { .. })
        {
            self.error(
                "E0750",
                format!("this '{t}' value wraps an ActiveSpan; spans cannot be stored or wrapped"),
                e.span.clone(),
                "keep the span in its own binding and finish it in this function",
            );
        }
        match &e.kind {
            ExprKind::Literal(_) | ExprKind::Name(_) => {}
            ExprKind::Tuple(es) => es.iter().for_each(|x| self.expr(x, scope)),
            ExprKind::Struct { name, fields } => {
                if name == ACTIVE_SPAN && !scope.clause {
                    self.error(
                        "E0751",
                        "an ActiveSpan literal forges a span; only Trace::start_span and Trace::start_child create one".into(),
                        e.span.clone(),
                        "start the span through the Trace effect",
                    );
                }
                fields.iter().for_each(|(_, x)| self.expr(x, scope));
            }
            ExprKind::Block { statements, tail } => self.block(statements, tail.as_deref(), scope),
            ExprKind::Call { callee, args, .. } => {
                if self.ty(e).is_some_and(|t| is_named(t, ACTIVE_SPAN))
                    && !scope.clause
                    && !matches!(self.trace_op(callee), Some("start_span" | "start_child"))
                {
                    self.error(
                        "E0750",
                        "this call returns an ActiveSpan started elsewhere; a span cannot leave the function that started it".into(),
                        e.span.clone(),
                        "start the span with Trace::start_span or Trace::start_child in the function that finishes it",
                    );
                }
                self.expr(callee, scope);
                args.iter().for_each(|a| self.expr(&a.expr, scope));
            }
            ExprKind::Field(base, _) | ExprKind::Try(base) | ExprKind::Unary(_, base) => {
                self.expr(base, scope)
            }
            ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) => {
                self.expr(a, scope);
                self.expr(b, scope);
            }
            ExprKind::Assign(_, place, value) => {
                if !scope.clause && self.place_in_span(place) {
                    self.error(
                        "E0751",
                        "assigning into an ActiveSpan forges its context".into(),
                        place.span.clone(),
                        "spans are written only by the Trace handler",
                    );
                }
                self.expr(place, scope);
                self.expr(value, scope);
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                self.expr(condition, scope);
                self.expr(then, scope);
                if let Some(o) = otherwise {
                    self.expr(o, scope);
                }
            }
            ExprKind::Match { value, arms } => {
                self.expr(value, scope);
                for (p, arm) in arms {
                    self.pattern(p, scope);
                    self.expr(arm, scope);
                }
            }
            ExprKind::Closure {
                params,
                result,
                effects,
                body,
                ..
            } => {
                if mentions(result, ACTIVE_SPAN) {
                    self.error(
                        "E0750",
                        "this closure returns an ActiveSpan; a span cannot leave the function that started it".into(),
                        e.span.clone(),
                        "finish the span inside the closure",
                    );
                }
                for p in params {
                    if is_named(&p.ty, SPAN_CONTEXT) && !uses_name(body, &p.name) {
                        self.dropped_context(&p.name, p.span.clone());
                    }
                }
                let local = local_bindings(&[], body, params);
                let mut captured = None;
                crate::regions::visit_expr(body, &mut |x| {
                    if let ExprKind::Name(n) = &x.kind
                        && captured.is_none()
                        && !local.contains(n)
                        && self
                            .ty(x)
                            .or_else(|| self.bindings.get(n))
                            .is_some_and(|t| mentions(t, ACTIVE_SPAN))
                    {
                        captured = Some((n.clone(), x.span.clone()));
                    }
                });
                if let Some((n, span)) = captured {
                    self.error(
                        "E0750",
                        format!("this closure captures the ActiveSpan '{n}'; a span cannot leave the function that started it"),
                        span,
                        "finish the span before creating the closure, or start it inside the closure",
                    );
                }
                let inner = Scope {
                    trace: effects.instances.iter().any(|x| x.name == TRACE),
                    clause: false,
                };
                let outer = std::mem::replace(&mut self.labels, trace_labels(effects));
                self.expr(body, inner);
                self.labels = outer;
            }
            ExprKind::Handle {
                effect,
                state,
                body,
                ret,
                ops,
            } => {
                if let Some((p, s)) = state {
                    self.pattern(p, scope);
                    self.expr(s, scope);
                }
                ret.pats.iter().for_each(|p| self.pattern(p, scope));
                if effect.name != TRACE {
                    self.expr(body, scope);
                    self.expr(&ret.body, scope);
                    for c in ops {
                        c.pats.iter().for_each(|p| self.pattern(p, scope));
                        self.expr(&c.body, scope);
                    }
                    return;
                }
                self.trace_handler(e, scope.trace, ops);
                self.expr(
                    body,
                    Scope {
                        trace: true,
                        clause: false,
                    },
                );
                self.expr(&ret.body, scope);
                let clause = Scope {
                    trace: scope.trace,
                    clause: true,
                };
                for c in ops {
                    self.expr(&c.body, clause);
                }
            }
        }
    }

    fn block(&mut self, statements: &[Statement], tail: Option<&Expr>, scope: Scope) {
        for (i, s) in statements.iter().enumerate() {
            match s {
                Statement::Expr(x) => {
                    if self.is_context(x) {
                        self.error(
                            "E0753",
                            "this trace context is discarded".into(),
                            x.span.clone(),
                            "propagate the context (for example to Trace::start_child) or do not request it",
                        );
                    }
                    self.expr(x, scope);
                }
                Statement::Let { pat, ty, value } => {
                    let context = is_named(ty, SPAN_CONTEXT) || self.is_context(value);
                    if context {
                        match &pat.kind {
                            PatternKind::Wild => self.error(
                                "E0753",
                                "this trace context is bound to `_` and dropped".into(),
                                pat.span.clone(),
                                "propagate the context or do not request it",
                            ),
                            PatternKind::Bind(n) => {
                                let rest = &statements[i + 1..];
                                let used = rest.iter().any(|s| match s {
                                    Statement::Let { value, .. } => uses_name(value, n),
                                    Statement::Expr(x) => uses_name(x, n),
                                }) || tail.is_some_and(|t| uses_name(t, n));
                                if !used {
                                    self.dropped_context(n, pat.span.clone());
                                }
                            }
                            _ => {}
                        }
                    }
                    self.pattern(pat, scope);
                    self.expr(value, scope);
                }
            }
        }
        if let Some(t) = tail {
            self.expr(t, scope);
        }
    }

    fn pattern(&mut self, p: &Pattern, scope: Scope) {
        match &p.kind {
            PatternKind::Tuple(ps) => ps.iter().for_each(|x| self.pattern(x, scope)),
            PatternKind::Constructor { name, fields, .. } => {
                if name == ACTIVE_SPAN && !scope.clause {
                    self.error(
                        "E0752",
                        "destructuring an ActiveSpan discards it without finishing it".into(),
                        p.span.clone(),
                        "finish the span with Trace::emit_ok, emit_err or emit_attr",
                    );
                }
                fields.iter().for_each(|(_, x)| self.pattern(x, scope));
            }
            _ => {}
        }
    }

    /// Whether an assignment place writes inside an `ActiveSpan`. The type
    /// map does not record the bases of an assignment place, so the root
    /// binding's type is followed through the projected fields.
    fn place_in_span(&self, place: &Expr) -> bool {
        let mut path = vec![];
        let mut root = place;
        loop {
            match &root.kind {
                ExprKind::Field(base, field) => {
                    if self.ty(base).is_some_and(|t| is_named(t, ACTIVE_SPAN)) {
                        return true;
                    }
                    path.push(field.as_str());
                    root = base;
                }
                ExprKind::Index(base, _) => root = base,
                _ => break,
            }
        }
        let ExprKind::Name(name) = &root.kind else {
            return false;
        };
        let Some(mut current) = self.ty(root).or_else(|| self.bindings.get(name)).cloned() else {
            return false;
        };
        for field in path.iter().rev() {
            if is_named(&current, ACTIVE_SPAN) {
                return true;
            }
            let Type::Named(n, _) = &current else {
                return false;
            };
            let Some(next) = self.world.adts.get(n).and_then(|a| {
                a.variants
                    .iter()
                    .flat_map(|v| &v.fields)
                    .find(|f| f.name == *field)
                    .map(|f| f.ty.clone())
            }) else {
                return false;
            };
            current = next;
        }
        false
    }

    /// The `Trace` operation a callee names: `Trace::op`, or `t.op` /
    /// `t::op` for a `Trace` label `t` in scope.
    fn trace_op<'e>(&self, callee: &'e Expr) -> Option<&'e str> {
        let op = match &callee.kind {
            ExprKind::Name(n) => {
                let (head, op) = n.split_once("::")?;
                (head == TRACE || self.labels.contains(head)).then_some(op)?
            }
            ExprKind::Field(base, op) => match &base.kind {
                ExprKind::Name(l) if self.labels.contains(l) => op.as_str(),
                _ => return None,
            },
            _ => return None,
        };
        super::OPERATIONS.contains(&op).then_some(op)
    }

    /// E0754 and clause-parameter E0753 for a `handle Trace` handler.
    fn trace_handler(&mut self, e: &Expr, nested: bool, ops: &[Clause]) {
        let Some(decl) = self.world.effects.get(TRACE) else {
            return;
        };
        for c in ops {
            let Some(op) = decl.ops.iter().find(|o| o.name == c.name) else {
                continue;
            };
            for (param, pat) in op.params.iter().zip(&c.pats) {
                if !is_named(&param.ty, SPAN_CONTEXT) {
                    continue;
                }
                match &pat.kind {
                    PatternKind::Bind(n) if !uses_name(&c.body, n) => {
                        self.dropped_context(n, pat.span.clone())
                    }
                    PatternKind::Wild => self.dropped_context("_", pat.span.clone()),
                    _ => {}
                }
            }
            if nested
                && matches!(c.name.as_str(), "current_ctx" | "start_span")
                && !self.propagates(c)
            {
                self.error(
                    "E0754",
                    format!(
                        "this nested Trace handler's '{}' clause drops the enclosing trace context",
                        c.name
                    ),
                    e.span.clone(),
                    "read a context from the enclosing scope (a parent SpanContext) or forward to the outer Trace::start_child",
                );
            }
        }
    }

    /// Whether a clause performs an outer `Trace` operation or reads a
    /// `SpanContext` bound outside the handler.
    fn propagates(&self, c: &Clause) -> bool {
        let local = local_bindings(&c.pats, &c.body, &[]);
        let mut found = false;
        crate::regions::visit_expr(&c.body, &mut |x| match &x.kind {
            ExprKind::Call { callee, .. } if self.trace_op(callee).is_some() => found = true,
            ExprKind::Name(n) if !local.contains(n) && self.is_context(x) => found = true,
            _ => {}
        });
        found
    }
}

fn trace_labels(effects: &Effects) -> BTreeSet<String> {
    effects
        .instances
        .iter()
        .filter(|e| e.name == TRACE)
        .filter_map(|e| e.label.clone())
        .collect()
}

/// Names bound by `pats`, `params` or anywhere inside `body`.
fn local_bindings(pats: &[Pattern], body: &Expr, params: &[Param]) -> BTreeSet<String> {
    let mut local: BTreeSet<String> = params.iter().map(|p| p.name.clone()).collect();
    pats.iter().for_each(|p| bindings(p, &mut local));
    crate::regions::visit_expr(body, &mut |x| match &x.kind {
        ExprKind::Block { statements, .. } => {
            for s in statements {
                if let Statement::Let { pat, .. } = s {
                    bindings(pat, &mut local);
                }
            }
        }
        ExprKind::Match { arms, .. } => arms.iter().for_each(|(p, _)| bindings(p, &mut local)),
        ExprKind::Closure { params, .. } => local.extend(params.iter().map(|p| p.name.clone())),
        ExprKind::Handle {
            ret, ops, state, ..
        } => {
            ret.pats
                .iter()
                .chain(ops.iter().flat_map(|c| &c.pats))
                .chain(state.iter().map(|(p, _)| p))
                .for_each(|p| bindings(p, &mut local));
        }
        _ => {}
    });
    local
}

fn bindings(p: &Pattern, out: &mut BTreeSet<String>) {
    match &p.kind {
        PatternKind::Bind(n) => {
            out.insert(n.clone());
        }
        PatternKind::Tuple(ps) => ps.iter().for_each(|x| bindings(x, out)),
        PatternKind::Constructor { fields, .. } => {
            fields.iter().for_each(|(_, x)| bindings(x, out))
        }
        _ => {}
    }
}

/// Whether `name` is read anywhere in `e`.
pub(super) fn uses_name(e: &Expr, name: &str) -> bool {
    let mut found = false;
    crate::regions::visit_expr(e, &mut |x| {
        if matches!(&x.kind, ExprKind::Name(n) if n == name) {
            found = true;
        }
    });
    found
}
