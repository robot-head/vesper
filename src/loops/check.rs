//! Typing and mode rules for `loop`, `for`, `break`, `continue`, `return`
//! and ranges
//! (see [`crate::loops`]).
//!
//! * A loop body is checked once, from the loop-entry environment with every
//!   integer fact about a binding the body may write forgotten, so the facts
//!   hold on every iteration. A body that moves a binding from outside the
//!   loop on a path that reaches the next iteration is rejected (E0312).
//! * A `break` or `return` consumes nothing: every live owned `linear`
//!   binding it would drop is rejected (E0302), as at a block end. Each
//!   `break` records its environment; the environment after the loop is the
//!   merge of them (E0309 when they disagree on a linear value).
//! * `continue` checks like a `break` whose environment flows to the back
//!   edge: loop-local linear bindings must be consumed, and nothing live at
//!   loop entry may have been moved on its path (E0312).
//! * `break` and `continue` stay inside its function, closure or handled body (E0213). A
//!   `return` may leave a handled body (abandoning its handler) but not a
//!   handler clause or a `region` block, and not a handler with linear state
//!   or an active `inout` argument loan (E0402).
use super::{Binding, Checker, Env, Usage, compatible, unresolved};
use crate::ast::*;
use std::collections::BTreeMap;
use std::sync::Arc;

/// A `for` binding: its pattern, its type, whether it is nonzero, and its
/// known integer range.
type LoopBinding<'a> = (&'a Pattern, &'a Type, bool, Option<(i128, i128)>);

/// A lexical control boundary, innermost last.
#[derive(Debug, Clone)]
pub(super) enum ControlFrame {
    /// A function or closure body: the target of `return`.
    Function { expected: Type, returned: Type },
    /// A `loop` (`value`) or `for` body: the target of `break`.
    Loop(Box<LoopFrame>),
    /// The handled body of a `handle` or `region` block.
    HandledBody { region: bool },
    /// A handler `return` or operation clause.
    Clause,
}

#[derive(Debug, Clone)]
pub(super) struct LoopFrame {
    /// Whether `break` may carry a value (`loop`, not `for`).
    value: bool,
    expected: Type,
    /// The join of the `break` value types (`Never` before any `break`).
    result: Type,
    /// The `break` value expressions, for literal defaulting.
    values: Vec<Expr>,
    /// The environment at loop entry, and after each `break`.
    entry: Env,
    exits: Vec<Env>,
    /// Depth of the block shadow stack at loop entry.
    shadows: usize,
}

const BREAK_OUTSIDE: &str = "'break' outside of a loop";
const CONTINUE_OUTSIDE: &str = "'continue' outside of a loop";

impl Checker<'_> {
    /// Checks a loop-family expression. Returns `None` for any other kind.
    #[inline(never)]
    pub(super) fn control_expr(
        &mut self,
        e: &Expr,
        env: &mut Env,
        expected: &Type,
    ) -> Option<Type> {
        Some(match &e.kind {
            ExprKind::Loop(body) => self.loop_body(body, env, expected, true, None, e.span.clone()),
            ExprKind::For { pat, iter, body } => {
                self.for_expr(pat, iter, body, env, e.span.clone())
            }
            ExprKind::Range { start, end, .. } => {
                self.error(
                    "E0213",
                    "a range is only supported as the iterable of a 'for' loop",
                    e.span.clone(),
                );
                self.expr(start, env, Usage::Own);
                self.expr(end, env, Usage::Own);
                Type::Infer
            }
            ExprKind::Break(value) => self.break_expr(value.as_deref(), env, e.span.clone()),
            ExprKind::Continue => self.continue_expr(env, e.span.clone()),
            ExprKind::Return(value) => self.return_expr(value.as_deref(), env, e.span.clone()),
            _ => return None,
        })
    }

    /// Enters a function or closure body whose result is `expected`.
    pub(super) fn enter_function(&mut self, expected: &Type) -> Vec<ControlFrame> {
        std::mem::replace(
            &mut self.control,
            vec![ControlFrame::Function {
                expected: expected.clone(),
                returned: Type::named("Never"),
            }],
        )
    }
    /// Leaves a function or closure body: restores the enclosing frames and
    /// joins the body type with every `return` value type.
    pub(super) fn leave_function(
        &mut self,
        outer: Vec<ControlFrame>,
        body: Type,
        span: Span,
    ) -> Type {
        let frames = std::mem::replace(&mut self.control, outer);
        match frames.into_iter().next() {
            Some(ControlFrame::Function { returned, .. }) if returned.name() != "Never" => {
                self.join_types(&body, &returned, span)
            }
            _ => body,
        }
    }
    pub(super) fn enter_control(&mut self, frame: ControlFrame) {
        self.control.push(frame);
    }
    pub(super) fn leave_control(&mut self) {
        self.control.pop();
    }

    /// Forgets integer facts about every binding the body may write, so the
    /// loop-entry environment over-approximates each iteration's start.
    fn havoc(&self, body: &Expr, env: &mut Env) {
        let mut written = std::collections::BTreeSet::new();
        let mut calls = false;
        crate::regions::visit_expr(body, &mut |x| match &x.kind {
            ExprKind::Assign(_, place, _) => {
                if let Some(p) = super::Place::from_expr(place) {
                    written.insert(p.root);
                }
            }
            ExprKind::Call { callee, args, .. } => {
                calls = true;
                if let ExprKind::Field(receiver, _) = &callee.kind
                    && let Some(p) = super::Place::from_expr(receiver)
                {
                    written.insert(p.root);
                }
                for a in args {
                    if let Some(p) = super::Place::from_expr(&a.expr) {
                        written.insert(p.root);
                    }
                }
            }
            _ => {}
        });
        for (name, b) in &mut env.bindings {
            if written.contains(name) || (calls && b.pass != Pass::Sink) {
                b.mutation = Arc::new(());
                b.integer = None;
                b.nonzero = false;
            }
        }
    }

    /// Checks a loop body as a [`LoopFrame`]. `bind` introduces a `for`
    /// pattern for the body. Returns the loop's type.
    #[allow(clippy::too_many_arguments)]
    fn loop_body(
        &mut self,
        body: &Expr,
        env: &mut Env,
        expected: &Type,
        value: bool,
        bind: Option<LoopBinding<'_>>,
        span: Span,
    ) -> Type {
        self.havoc(body, env);
        let entry = env.clone();
        self.control.push(ControlFrame::Loop(Box::new(LoopFrame {
            value,
            expected: expected.clone(),
            result: Type::named("Never"),
            values: vec![],
            entry: entry.clone(),
            exits: vec![],
            shadows: self.shadowed.len(),
        })));
        let mut shadowed = BTreeMap::new();
        let mut names = vec![];
        if let Some((pat, ty, nonzero, range)) = bind {
            for n in super::pattern_names(pat) {
                if let Some(old) = env.bindings.get(&n) {
                    shadowed.insert(n.clone(), old.clone());
                }
                names.push(n);
            }
            self.bind(pat, ty, env);
            if let PatternKind::Bind(n) = &pat.kind
                && let Some(b) = env.bindings.get_mut(n)
            {
                b.nonzero = nonzero;
                // The index stays in its range (which proves array indices
                // in bounds, §5): the bound is evaluated once and the index
                // is not assignable.
                b.range = range.map(|(lo, hi)| super::IntRange {
                    lo,
                    hi,
                    at: std::sync::Arc::clone(&b.mutation),
                });
            }
        }
        let t = self.expr(body, env, Usage::Own);
        if self.mode(&t) == Mode::Linear {
            self.error(
                "E0302",
                "unused linear loop body value must be consumed",
                body.span.clone(),
            );
        }
        for n in names {
            env.bindings.remove(&n);
        }
        env.bindings.extend(shadowed.clone());
        if t.name() != "Never" {
            self.back_edge(&entry, env, &span);
        }
        let Some(ControlFrame::Loop(frame)) = self.control.pop() else {
            unreachable!("loop frame");
        };
        let LoopFrame {
            result,
            values,
            exits,
            ..
        } = *frame;
        if value {
            for v in &values {
                self.constrain_literals(v, &result);
            }
        }
        // A `break` leaves the `for` pattern's scope: the outer bindings it
        // shadowed come back.
        let mut exits = exits
            .into_iter()
            .map(|mut exit| {
                for (name, old) in &shadowed {
                    exit.bindings
                        .entry(name.clone())
                        .or_insert_with(|| old.clone());
                }
                exit
            })
            .collect::<Vec<_>>();
        if !value {
            // A `for` loop also ends when its range is exhausted, at the
            // header of any iteration.
            exits.insert(0, entry);
        }
        let mut exits = exits.into_iter();
        if let Some(first) = exits.next() {
            *env = first;
            for exit in exits {
                let previous = env.clone();
                self.merge(env, &previous, &exit, span.clone());
            }
            if value { result } else { Type::unit() }
        } else {
            Type::named("Never")
        }
    }

    /// The environment reaching the next iteration must not have moved a
    /// binding that was live at loop entry.
    fn back_edge(&mut self, entry: &Env, env: &Env, span: &Span) {
        for (name, before) in &entry.bindings {
            let Some(after) = env.bindings.get(name) else {
                continue;
            };
            if after.span != before.span {
                continue;
            }
            if let Some(path) = after.moved.iter().find(|p| !before.moved.contains(p)) {
                let place = std::iter::once(name.as_str())
                    .chain(path.iter().map(String::as_str))
                    .collect::<Vec<_>>()
                    .join(".");
                self.error(
                    "E0312",
                    format!("'{place}' is moved in a previous iteration of the loop"),
                    span.clone(),
                );
                self.related(
                    before.span.clone(),
                    "binding introduced here, outside the loop",
                );
                for cause in &after.move_causes {
                    if !before.move_causes.iter().any(|c| c.span == cause.span) {
                        self.related(cause.span.clone(), "moved here on a path that repeats");
                    }
                }
                self.help("Move the value only on a path that leaves the loop with 'break' or 'return', or clone a copy value.");
            }
        }
    }

    fn for_expr(
        &mut self,
        pat: &Pattern,
        iter: &Expr,
        body: &Expr,
        env: &mut Env,
        span: Span,
    ) -> Type {
        let ExprKind::Range {
            start,
            end,
            inclusive,
        } = &iter.kind
        else {
            self.error(
                "E0213",
                "'for' iterates an integer range 'a..b' or 'a..=b'",
                iter.span.clone(),
            );
            self.expr(iter, env, Usage::Own);
            return Type::unit();
        };
        // A range of unsuffixed literals takes the kind of the loop
        // variable's uses (`crate::check::literal_typing`).
        let hinted = self.literal_element(pat);
        let goal = hinted.clone().unwrap_or(Type::Infer);
        let a = self.expr_expected(start, env, Usage::Own, &goal);
        // An unsuffixed start leaves the end's kind open too.
        let end_goal = match (&hinted, &a) {
            (Some(_), _) | (None, Type::IntLiteral(_)) => goal.clone(),
            (None, _) => a.clone(),
        };
        let b = self.expr_expected(end, env, Usage::Own, &end_goal);
        let joined = self.join_types(&a, &b, iter.span.clone());
        if hinted.is_none() {
            self.open_literal_bindings(pat, &joined);
        }
        let element = hinted.unwrap_or_else(|| crate::numeric::default_type(&joined));
        if crate::numeric::IntegerKind::from_type(&element).is_none() {
            if !unresolved(&element) {
                self.error(
                    "E0200",
                    format!("'for' range bounds must be integers, found {element}"),
                    iter.span.clone(),
                );
            }
        } else {
            self.expect_type(&element, &a, start.span.clone());
            self.expect_type(&element, &b, end.span.clone());
        }
        self.constrain_literals(start, &element);
        self.constrain_literals(end, &element);
        self.types.insert(
            (iter.span.start, iter.span.end),
            Type::Named("Range".into(), vec![element.clone()]),
        );
        // The lowered index test and increment (`loops/ir.rs`) sit at these
        // zero-width spans.
        let at = iter.span.start;
        self.types.insert((at, at), element.clone());
        let at = iter.span.end;
        self.types.insert((at, at), Type::named("bool"));
        if !self.irrefutable(pat, &element) {
            self.error(
                "E0210",
                "'for' pattern must be irrefutable",
                pat.span.clone(),
            );
        }
        let nonzero = self.integer_fact(start, env).is_some_and(|n| n > 0);
        let range = self
            .integer_bounds(start, env)
            .zip(
                self.integer_bounds(end, env)
                    .or_else(|| self.array_len_bounds(end, env)),
            )
            .map(|((lo, _), (_, hi))| (lo, if *inclusive { hi } else { hi - 1 }))
            .filter(|(lo, hi)| lo <= hi);
        self.loop_body(
            body,
            env,
            &Type::unit(),
            false,
            Some((pat, &element, nonzero, range)),
            span,
        )
    }

    fn break_expr(&mut self, value: Option<&Expr>, env: &mut Env, span: Span) -> Type {
        let target = self
            .control
            .iter()
            .rposition(|f| !matches!(f, ControlFrame::Loop(_)));
        let index = match self.control.last() {
            Some(ControlFrame::Loop(_)) => self.control.len() - 1,
            _ => {
                let message = match target.map(|i| &self.control[i]) {
                    Some(ControlFrame::HandledBody { .. }) if self.inside_loop() => {
                        "'break' cannot leave a handled body; return a value from the body instead"
                    }
                    Some(ControlFrame::Clause) if self.inside_loop() => {
                        "'break' cannot leave a handler clause"
                    }
                    Some(ControlFrame::Function { .. }) if self.inside_loop() => {
                        "'break' cannot leave a closure"
                    }
                    _ => BREAK_OUTSIDE,
                };
                self.error("E0213", message, span.clone());
                if let Some(v) = value {
                    self.expr(v, env, Usage::Own);
                }
                return Type::named("Never");
            }
        };
        let (allows_value, expected) = match &self.control[index] {
            ControlFrame::Loop(f) => (f.value, f.expected.clone()),
            _ => unreachable!(),
        };
        let actual = match value {
            Some(v) => {
                if !allows_value {
                    self.error(
                        "E0213",
                        "'break' in a 'for' loop takes no value",
                        v.span.clone(),
                    );
                }
                self.expr_expected(v, env, Usage::Own, &expected)
            }
            None => Type::unit(),
        };
        if expected != Type::Infer {
            self.expect_type(&expected, &actual, span.clone());
        }
        self.jump_loans("break", &span);
        let exit = self.loop_exit(env, index, &span);
        let ControlFrame::Loop(frame) = &mut self.control[index] else {
            unreachable!()
        };
        let joined_from = frame.result.clone();
        if let Some(v) = value {
            frame.values.push(v.clone());
        }
        frame.exits.push(exit);
        let joined = if allows_value {
            self.join_types(&joined_from, &actual, span.clone())
        } else {
            Type::unit()
        };
        if let ControlFrame::Loop(frame) = &mut self.control[index] {
            frame.result = joined;
        }
        Type::named("Never")
    }

    /// `continue`: jumps to the next iteration of the innermost loop. It
    /// consumes nothing, like `break`; loop-local linear bindings must be
    /// consumed, and the environment reaching the next iteration must not
    /// have moved a binding that was live at loop entry (E0312).
    fn continue_expr(&mut self, env: &mut Env, span: Span) -> Type {
        let index = match self.control.last() {
            Some(ControlFrame::Loop(_)) => self.control.len() - 1,
            _ => {
                let target = self
                    .control
                    .iter()
                    .rposition(|f| !matches!(f, ControlFrame::Loop(_)));
                let message = match target.map(|i| &self.control[i]) {
                    Some(ControlFrame::HandledBody { .. }) if self.inside_loop() => {
                        "'continue' cannot leave a handled body"
                    }
                    Some(ControlFrame::Clause) if self.inside_loop() => {
                        "'continue' cannot leave a handler clause"
                    }
                    Some(ControlFrame::Function { .. }) if self.inside_loop() => {
                        "'continue' cannot leave a closure"
                    }
                    _ => CONTINUE_OUTSIDE,
                };
                self.error("E0213", message, span);
                return Type::named("Never");
            }
        };
        self.jump_loans("continue", &span);
        let next = self.loop_exit(env, index, &span);
        let ControlFrame::Loop(frame) = &self.control[index] else {
            unreachable!()
        };
        let entry = frame.entry.clone();
        self.back_edge(&entry, &next, &span);
        Type::named("Never")
    }

    fn inside_loop(&self) -> bool {
        self.control
            .iter()
            .any(|f| matches!(f, ControlFrame::Loop(_)))
    }

    /// The environment after leaving loop `index` from here: loop-local
    /// linear bindings must already be consumed; loop-local bindings are
    /// dropped and the bindings they shadow come back.
    fn loop_exit(&mut self, env: &Env, index: usize, span: &Span) -> Env {
        let ControlFrame::Loop(frame) = &self.control[index] else {
            unreachable!()
        };
        let entry = frame.entry.clone();
        let shadows = frame.shadows;
        let local = |entry: &Env, n: &String, b: &Binding| {
            entry.bindings.get(n).is_none_or(|old| old.span != b.span)
        };
        let locals = env
            .bindings
            .iter()
            .filter(|(n, b)| local(&entry, n, b))
            .map(|(n, _)| n.clone())
            .collect::<Vec<_>>();
        self.exhaust(env, locals, span.clone());
        let mut exit = env.clone();
        for frame in self.shadowed[shadows.min(self.shadowed.len())..]
            .iter()
            .rev()
        {
            for (n, b) in frame {
                exit.bindings.insert(n.clone(), b.clone());
            }
        }
        exit.bindings.retain(|n, b| !local(&entry, n, b));
        exit
    }

    fn return_expr(&mut self, value: Option<&Expr>, env: &mut Env, span: Span) -> Type {
        let mut expected = None;
        for frame in self.control.iter().rev() {
            match frame {
                ControlFrame::Loop(_) => {}
                ControlFrame::HandledBody { region: false } => {}
                ControlFrame::HandledBody { region: true } => {
                    expected = Some(Err("'return' cannot leave a region block"));
                    break;
                }
                ControlFrame::Clause => {
                    expected = Some(Err("'return' cannot leave a handler clause"));
                    break;
                }
                ControlFrame::Function { expected: t, .. } => {
                    expected = Some(Ok(t.clone()));
                    break;
                }
            }
        }
        let expected = match expected {
            Some(Ok(t)) => t,
            other => {
                let message = match other {
                    Some(Err(m)) => m,
                    _ => "'return' outside of a function",
                };
                self.error("E0213", message, span.clone());
                if let Some(v) = value {
                    self.expr(v, env, Usage::Own);
                }
                return Type::named("Never");
            }
        };
        let actual = match value {
            Some(v) => self.expr_expected(v, env, Usage::Own, &expected),
            None => Type::unit(),
        };
        if expected != Type::Infer {
            self.expect_type(&expected, &actual, span.clone());
            if let Some(v) = value {
                self.constrain_literals(v, &expected);
            }
        }
        self.jump_loans("return", &span);
        // Linear handler state of a handled body this `return` leaves.
        for (_, depth, state, origin) in self.state_guards.clone() {
            if depth == self.effect_inference.len() && self.crosses_handled_body() {
                self.error(
                    "E0402",
                    format!("'return' would discard linear handler state {state}"),
                    span.clone(),
                );
                self.related(origin, "linear handler state initializer");
            }
        }
        let names = env.bindings.keys().cloned().collect();
        self.exhaust(env, names, span.clone());
        if let Some(ControlFrame::Function { returned, .. }) = self
            .control
            .iter_mut()
            .rev()
            .find(|f| matches!(f, ControlFrame::Function { .. }))
        {
            let previous = returned.clone();
            // An annotated result already checked `actual`; an inferred one
            // joins every early return (E0200 on a mismatch).
            let joined = if previous.name() == "Never" {
                actual
            } else if expected != Type::Infer && compatible(&previous, &actual) {
                previous
            } else {
                self.join_types(&previous, &actual, span.clone())
            };
            if let Some(ControlFrame::Function { returned, .. }) = self
                .control
                .iter_mut()
                .rev()
                .find(|f| matches!(f, ControlFrame::Function { .. }))
            {
                *returned = joined;
            }
        }
        Type::named("Never")
    }

    fn crosses_handled_body(&self) -> bool {
        self.control
            .iter()
            .rev()
            .take_while(|f| !matches!(f, ControlFrame::Function { .. }))
            .any(|f| matches!(f, ControlFrame::HandledBody { .. }))
    }

    /// A jump out of an argument list would leave an exclusive loan open.
    fn jump_loans(&mut self, what: &str, span: &Span) {
        if self.loans.iter().any(|(_, mode, _)| *mode == Pass::Inout) {
            self.error(
                "E0402",
                format!("'{what}' crosses an active inout argument loan"),
                span.clone(),
            );
        }
    }
}
