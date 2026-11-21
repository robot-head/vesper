//! Result ranges across calls (SH3 lane 6, #1245): the inclusive bounds of
//! the values a function can return, used at its call sites as the bounds
//! of the call expression (for divisors, shift amounts and the ranges of
//! `let` bindings).
//!
//! A summary is modular: it depends only on the callee's own body (and on
//! the summaries of the functions that body calls), never on a call site's
//! arguments. Each value a body can produce (its tail, the arms of `if` and
//! `match`, and every `return`) must be bounded: a constant, a `const`, a
//! parameter or immutable `let` (by its kind, or by a constant comparison
//! that guards the branch), `x & m`, `x % m`, `x / m` (nonnegative), a widening or narrowing
//! conversion, or a call with a summary of its own. Anything else gives no
//! summary. Recursion gives no fact: a function whose summary is being
//! computed has none.
use super::{Checker, Place};
use crate::ast::*;
use crate::numeric::{Conversion, IntegerKind};
use std::collections::{BTreeMap, BTreeSet};

/// Bounds of what an expression produces when it completes normally.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Value {
    /// It never completes normally (`return`, `break`, `continue`).
    Never,
    Range(i128, i128),
    Unknown,
}
impl Value {
    fn join(self, other: Value) -> Value {
        match (self, other) {
            (Value::Never, v) | (v, Value::Never) => v,
            (Value::Range(a, b), Value::Range(c, d)) => Value::Range(a.min(c), b.max(d)),
            _ => Value::Unknown,
        }
    }
    fn bounds(self) -> Option<(i128, i128)> {
        match self {
            Value::Range(lo, hi) => Some((lo, hi)),
            _ => None,
        }
    }
}

/// A summary being computed: `Some(None)` is in progress (recursion).
pub(super) type ResultRanges = BTreeMap<String, Option<Option<(i128, i128)>>>;

/// The kind bounds of an integer type other than `u128` (whose values pass
/// the exact `i128` domain).
fn kind_bounds(t: &Type) -> Option<(i128, i128)> {
    if matches!(t, Type::IntLiteral(_)) {
        return None;
    }
    let k = IntegerKind::from_type(t)?;
    (k != IntegerKind::U128).then(|| (k.min(), k.max()))
}

fn clamp(v: Value, bounds: Option<(i128, i128)>) -> Value {
    match (v, bounds) {
        (Value::Range(a, b), Some((lo, hi))) => {
            let (a, b) = (a.max(lo), b.min(hi));
            if a <= b {
                Value::Range(a, b)
            } else {
                Value::Never
            }
        }
        (v, _) => v,
    }
}

struct Summary<'c, 'w> {
    checker: &'c Checker<'w>,
    /// Names some write may change: assigned anywhere in the body, or lent
    /// `inout`. A guard refines only names outside this set.
    written: BTreeSet<String>,
}

impl Summary<'_, '_> {
    fn value(&self, e: &Expr, scope: &BTreeMap<String, Option<(i128, i128)>>) -> Value {
        match &e.kind {
            ExprKind::Literal(Literal::Int(n)) => Value::Range(*n, *n),
            ExprKind::Name(n) => match scope.get(n) {
                // An empty range is a branch no value reaches.
                Some(Some((lo, hi))) if lo > hi => Value::Never,
                Some(Some((lo, hi))) => Value::Range(*lo, *hi),
                _ => Value::Unknown,
            },
            ExprKind::Return(_) | ExprKind::Break(_) | ExprKind::Continue => Value::Never,
            ExprKind::Block { statements, tail } => {
                let mut scope = scope.clone();
                for s in statements {
                    match s {
                        Statement::Let {
                            pat,
                            ty,
                            value,
                            mutable,
                        } => {
                            if self.value(value, &scope) == Value::Never {
                                return Value::Never;
                            }
                            let declared = kind_bounds(ty);
                            let bounds = match (&pat.kind, mutable) {
                                (PatternKind::Bind(_), false) => {
                                    clamp(self.value(value, &scope), declared)
                                        .bounds()
                                        .or(declared)
                                }
                                _ => declared,
                            };
                            let mut names = vec![];
                            pattern_names_into(pat, &mut names);
                            for n in names {
                                let b = if matches!(pat.kind, PatternKind::Bind(_)) {
                                    bounds
                                } else {
                                    None
                                };
                                scope.insert(n, b);
                            }
                        }
                        Statement::Expr(e) => {
                            if self.value(e, &scope) == Value::Never {
                                return Value::Never;
                            }
                        }
                    }
                }
                match tail {
                    Some(t) => self.value(t, &scope),
                    None => Value::Unknown,
                }
            }
            ExprKind::If {
                condition,
                then,
                otherwise: Some(otherwise),
            } => {
                let (yes, no) = (
                    self.refine(condition, scope, true),
                    self.refine(condition, scope, false),
                );
                self.value(then, &yes).join(self.value(otherwise, &no))
            }
            ExprKind::Match { value, arms } if !arms.is_empty() => {
                if self.value(value, scope) == Value::Never {
                    return Value::Never;
                }
                arms.iter().fold(Value::Never, |acc, (pat, body)| {
                    let mut scope = scope.clone();
                    let mut names = vec![];
                    pattern_names_into(pat, &mut names);
                    for n in names {
                        scope.insert(n, None);
                    }
                    acc.join(self.value(body, &scope))
                })
            }
            ExprKind::Binary(op, a, b) if op == "&" => {
                let mask = |x: &Expr| self.value(x, scope).bounds().filter(|(lo, _)| *lo >= 0);
                match (mask(a), mask(b)) {
                    (Some((_, x)), Some((_, y))) => Value::Range(0, x.min(y)),
                    (Some((_, m)), None) | (None, Some((_, m))) => Value::Range(0, m),
                    _ => Value::Unknown,
                }
            }
            ExprKind::Binary(op, a, b) if op == "%" => {
                match (self.value(a, scope).bounds(), self.value(b, scope).bounds()) {
                    (Some((alo, ahi)), Some((blo, bhi))) if alo >= 0 && blo > 0 => {
                        Value::Range(0, ahi.min(bhi - 1))
                    }
                    _ => Value::Unknown,
                }
            }
            // Nonnegative operands only, so the quotient cannot overflow and
            // its bounds are the corner quotients.
            ExprKind::Binary(op, a, b) if op == "/" => {
                match (self.value(a, scope).bounds(), self.value(b, scope).bounds()) {
                    (Some((alo, ahi)), Some((blo, bhi))) if alo >= 0 && blo > 0 => {
                        Value::Range(alo / bhi, ahi / blo)
                    }
                    _ => Value::Unknown,
                }
            }
            ExprKind::Call { callee, args, .. } => {
                let ExprKind::Name(name) = &callee.kind else {
                    return Value::Unknown;
                };
                if scope.contains_key(name) {
                    return Value::Unknown;
                }
                if let Some(range) = self.checker.call_range(name) {
                    return match range {
                        Some((lo, hi)) => Value::Range(lo, hi),
                        None => Value::Unknown,
                    };
                }
                let to = match crate::numeric::conversion(name) {
                    Some((Conversion::Widen | Conversion::Narrow, to))
                        if args.len() == 1 && to != IntegerKind::U128 =>
                    {
                        (to.min(), to.max())
                    }
                    _ => return Value::Unknown,
                };
                // A widening keeps its operand. A narrowing keeps it only
                // when it cannot fail: a handler that resumes the failure
                // supplies some other value of the target kind.
                match self.value(&args[0].expr, scope) {
                    Value::Range(lo, hi) if lo >= to.0 && hi <= to.1 => Value::Range(lo, hi),
                    Value::Never => Value::Never,
                    _ => Value::Range(to.0, to.1),
                }
            }
            _ => Value::Unknown,
        }
    }
    /// `scope` where `condition` is `truth`: a comparison of an unwritten
    /// name with a constant narrows the name.
    fn refine(
        &self,
        condition: &Expr,
        scope: &BTreeMap<String, Option<(i128, i128)>>,
        truth: bool,
    ) -> BTreeMap<String, Option<(i128, i128)>> {
        let mut scope = scope.clone();
        let ExprKind::Binary(op, a, b) = &condition.kind else {
            return scope;
        };
        let constant = |x: &Expr| match self.value(x, &BTreeMap::new()) {
            Value::Range(lo, hi) if lo == hi => Some(lo),
            _ => None,
        };
        let (name, op, k) = match (&a.kind, &b.kind) {
            (ExprKind::Name(n), _) if constant(b).is_some() => {
                (n, op.as_str(), constant(b).unwrap())
            }
            (_, ExprKind::Name(n)) if constant(a).is_some() => {
                let flipped = match op.as_str() {
                    "<" => ">",
                    "<=" => ">=",
                    ">" => "<",
                    ">=" => "<=",
                    other => other,
                };
                (n, flipped, constant(a).unwrap())
            }
            _ => return scope,
        };
        if self.written.contains(name) {
            return scope;
        }
        let Some(Some((mut lo, mut hi))) = scope.get(name).copied() else {
            return scope;
        };
        let op = if truth {
            op
        } else {
            match op {
                "<" => ">=",
                "<=" => ">",
                ">" => "<=",
                ">=" => "<",
                "==" => "!=",
                "!=" => "==",
                _ => return scope,
            }
        };
        match op {
            "<" => hi = hi.min(k.saturating_sub(1)),
            "<=" => hi = hi.min(k),
            ">" => lo = lo.max(k.saturating_add(1)),
            ">=" => lo = lo.max(k),
            "==" => (lo, hi) = (lo.max(k), hi.min(k)),
            "!=" => {
                if lo == k {
                    lo = lo.saturating_add(1);
                }
                if hi == k {
                    hi = hi.saturating_sub(1);
                }
            }
            _ => return scope,
        }
        // An empty range is a dead branch: its value never reaches the join.
        scope.insert(
            name.clone(),
            Some((lo, hi)).filter(|(lo, hi)| lo <= hi).or(Some((1, 0))),
        );
        scope
    }
}

fn pattern_names_into(p: &Pattern, out: &mut Vec<String>) {
    match &p.kind {
        PatternKind::Bind(n) => out.push(n.clone()),
        PatternKind::Tuple(ps) => ps.iter().for_each(|p| pattern_names_into(p, out)),
        PatternKind::Constructor { fields, .. } => {
            fields.iter().for_each(|(_, p)| pattern_names_into(p, out))
        }
        PatternKind::Wild | PatternKind::Literal(_) => {}
    }
}

/// Names assigned anywhere in `body` (closures included) or that a call may
/// write: an argument the call lends `inout`, whether the call site says so
/// or the callee's parameter mode infers it, and a method receiver. Only a
/// user function's by-value parameter, a constructor field and the integer
/// conversions, wrapping and rotation builtins are known not to write an
/// argument; any other callee may.
fn written_names(checker: &Checker, body: &Expr) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    crate::regions::visit_expr(body, &mut |e| match &e.kind {
        ExprKind::Assign(_, place, _) => {
            if let Some(p) = Place::from_expr(place) {
                out.insert(p.root);
            }
        }
        ExprKind::Call { callee, args, .. } => {
            let by_value = |i: usize| -> bool {
                let ExprKind::Name(name) = &callee.kind else {
                    return false;
                };
                if let Some(f) = checker.world.functions.get(name) {
                    return f.params.get(i).is_some_and(|p| p.pass != Pass::Inout);
                }
                checker.world.constructor(name).is_some()
                    || crate::numeric::conversion(name).is_some()
                    || crate::numeric::wrapping_intrinsic(name).is_some()
                    || crate::numeric::rotate_intrinsic(name).is_some()
            };
            for (i, a) in args.iter().enumerate() {
                if (a.pass == Pass::Inout || !by_value(i))
                    && let Some(p) = Place::from_expr(&a.expr)
                {
                    out.insert(p.root);
                }
            }
            if let ExprKind::Field(base, _) = &callee.kind
                && let Some(p) = Place::from_expr(base)
            {
                out.insert(p.root);
            }
        }
        _ => {}
    });
    out
}

impl Checker<'_> {
    /// How the checker resolves a call of the plain name `name` (not a
    /// local binding): `None` when it is not a user function (a
    /// constructor, an intrinsic the checker prefers, or no function), else
    /// the function's result range, if it has one.
    pub(super) fn call_range(&self, name: &str) -> Option<Option<(i128, i128)>> {
        if self.world.constructor(name).is_some()
            || crate::numeric::wrapping_intrinsic(name).is_some()
            || !self.world.functions.contains_key(name)
        {
            return None;
        }
        Some(self.callee_range(name))
    }
    /// The inclusive bounds of every value the function `name` can return,
    /// if its body bounds them (see the module docs).
    pub(super) fn callee_range(&self, name: &str) -> Option<(i128, i128)> {
        if let Some(known) = self.result_ranges.borrow().get(name) {
            return known.flatten();
        }
        let f = self.world.functions.get(name)?;
        let result = kind_bounds(&f.result)?;
        let body = f.body.as_ref()?;
        if !f.generics.is_empty() {
            return None;
        }
        self.result_ranges
            .borrow_mut()
            .insert(name.to_string(), Some(None));
        let summary = Summary {
            checker: self,
            written: written_names(self, body),
        };
        let scope = f
            .params
            .iter()
            .map(|p| (p.name.clone(), kind_bounds(&p.ty)))
            .collect();
        let mut value = summary.value(body, &scope);
        // Every `return` contributes its value; a bare `return` or one whose
        // value is unbounded gives no summary. Its value is taken with only
        // the parameters in scope, and only names no pattern of the body
        // rebinds keep their bounds there.
        let mut bound = vec![];
        let mut returns = vec![];
        crate::regions::visit_expr(body, &mut |e| match &e.kind {
            ExprKind::Return(v) => returns.push(v.as_deref().cloned()),
            ExprKind::Block { statements, .. } => {
                for s in statements {
                    if let Statement::Let { pat, .. } = s {
                        pattern_names_into(pat, &mut bound);
                    }
                }
            }
            ExprKind::Match { arms, .. } => arms
                .iter()
                .for_each(|(p, _)| pattern_names_into(p, &mut bound)),
            ExprKind::For { pat, .. } => pattern_names_into(pat, &mut bound),
            ExprKind::Closure { params, .. } => bound.extend(params.iter().map(|p| p.name.clone())),
            ExprKind::Handle {
                state, ret, ops, ..
            } => {
                if let Some((p, _)) = state {
                    pattern_names_into(p, &mut bound);
                }
                for c in std::iter::once(&**ret).chain(ops) {
                    c.pats
                        .iter()
                        .for_each(|p| pattern_names_into(p, &mut bound));
                }
            }
            _ => {}
        });
        let mut outer = scope;
        for n in &bound {
            outer.insert(n.clone(), None);
        }
        for r in returns {
            value = value.join(match &r {
                Some(v) => match summary.value(v, &outer) {
                    // `return return x`: the inner one is counted itself.
                    Value::Never => Value::Never,
                    v => v,
                },
                None => Value::Unknown,
            });
        }
        let range = clamp(value, Some(result)).bounds();
        self.result_ranges
            .borrow_mut()
            .insert(name.to_string(), Some(range));
        range
    }
}
