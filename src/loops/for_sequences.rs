//! `for` over arrays, sequences and slices (self-hosting wave SH3 lane 4,
//! #1243; manuscript `ForExpr`).
//!
//! `for pat in e { body }` iterates the elements of `e` in index order when
//! `e` is a fixed array `[T; N]`, a `ChunkSeq[T]` or a `Slice[T]` (a slice
//! parameter, a bound view or a view expression such as `s.as_bytes()` or
//! `xs[a..b]`) of `copy` elements, or an array literal of static text
//! (`["a", "b"]`). The length is read once, before the first iteration, and
//! the body may read the iterated place but not write it, lend it `inout`
//! or move it (`E0301`), so the length stays the length of what is iterated.
//! An iterable that is not a place (a literal, a call result) is evaluated
//! once into a hidden binding the loop owns.
//!
//! The checker types the form and records the span of its iterable; the
//! module is then rewritten to the index loop it means and checked again
//! (as `?` on `Option` is), so every backend runs an ordinary range loop:
//!
//! ```text
//! for $for0_i in 0..xs.len() { match xs.try_get($for0_i) { Option::Some($for0_x) => { let pat = $for0_x; body; }, _ => {} } }
//! for $for0_i in 0..a.len() { let pat = a[$for0_i]; body; }
//! for $for0_i in 0..2 { let pat = match $for0_i { 0 => "a", _ => "b" }; body; }
//! ```
//!
//! An array index under the loop's range is in bounds, so the read is total;
//! a sequence element is read with the total `try_get`, whose `None` arm is
//! unreachable because the body cannot change the length. A non-`copy`
//! element of a sequence is lent to the body as an `in` element view (§13),
//! so the pattern is a name or `_`; arrays and views of non-`copy` elements
//! are refused for now (`E0213`).
use super::{Checker, Env, Place, Usage};
use crate::ast::*;
use std::collections::BTreeMap;

/// Prefix of the hidden bindings of a rewritten loop.
pub(super) const HIDDEN_PREFIX: &str = "$for";

/// How a `for` over a sequence is rewritten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Plan {
    /// An array literal of `n` static texts: the index selects the text.
    StaticText(usize),
    /// A fixed array, read by index. `place` is false for a temporary,
    /// bound with the annotation `ty` when its literals need one.
    Array { place: bool, ty: Option<Type> },
    /// A `ChunkSeq` or `Slice`, read with `try_get`. `lend` when the
    /// elements are not `copy`: the pattern (a name or `_`) binds an `in`
    /// element view (§13) instead of a copy.
    Seq { place: bool, lend: bool },
    /// A view of a place (`s.as_bytes()`, `xs.as_slice()`, `xs[a..b]`),
    /// read with `try_get` on the view; `interval` when its bounds are
    /// evaluated once into hidden bindings.
    View { interval: bool, lend: bool },
}

/// The place a view expression borrows, and its interval bounds.
fn view_base(e: &Expr) -> Option<(&Expr, Option<(&Expr, &Expr)>)> {
    if let Some((base, lo, hi)) = crate::seq::interval(e) {
        return Some((base, Some((lo, hi))));
    }
    if let Some((base, false)) = crate::seq::whole_view(e) {
        return Some((base, None));
    }
    crate::strings::whole_view(e).map(|base| (base, None))
}

impl Checker<'_> {
    /// Types the iterable of `for pat in iter { body }` over an array or a
    /// sequence: its element type and rewrite. `iter` is not a range.
    pub(super) fn for_sequence(
        &mut self,
        pat: &Pattern,
        iter: &Expr,
        env: &mut Env,
    ) -> Option<(Type, Plan)> {
        if let ExprKind::Tuple(items) = &iter.kind
            && !items.is_empty()
            && items.iter().all(|e| self.static_text_source(e))
        {
            for item in items {
                self.expr(item, env, Usage::Own);
            }
            return Some((
                Type::named(crate::strings::STR_TYPE),
                Plan::StaticText(items.len()),
            ));
        }
        let place = Place::from_expr(iter).is_some_and(|p| env.bindings.contains_key(&p.root));
        // A view that may be bound (of an `in` parameter, an immutable
        // binding or static text) is bound to the hidden binding; a slice
        // view of a place that may change is read as a method receiver
        // each iteration, and the loop lends the place.
        let bound = !place && view_base(iter).is_some() && self.bind_view(iter, env);
        let view = view_base(iter).filter(|(base, _)| {
            !bound
                && crate::strings::whole_view(iter).is_none()
                && Place::from_expr(base).is_some_and(|p| env.bindings.contains_key(&p.root))
        });
        let ty = if place {
            self.expr(iter, env, Usage::Borrow(Pass::In))
        } else if view.is_some() {
            self.slice_args
                .insert((iter.span.start, iter.span.end), Pass::In);
            self.expr(iter, env, Usage::Borrow(Pass::In))
        } else {
            self.expr(iter, env, Usage::Own)
        };
        // A literal array's unsuffixed elements take the kind of the loop
        // variable's uses, as a range's bounds do.
        let literal = match &ty {
            Type::Tuple(ts)
                if !place
                    && !ts.is_empty()
                    && ts.iter().all(|t| matches!(t, Type::IntLiteral(_))) =>
            {
                let hinted = self.literal_element(pat);
                if hinted.is_none() {
                    self.open_literal_bindings(pat, &ts[0]);
                }
                let element = hinted.unwrap_or_else(|| crate::numeric::default_type(&ts[0]));
                Some(Type::Tuple(vec![element; ts.len()]))
            }
            _ => None,
        };
        let ty = literal.clone().unwrap_or(ty);
        let (element, plan) = if let Some((t, _)) = crate::arrays::element(&ty) {
            (t.clone(), Plan::Array { place, ty: literal })
        } else if let Some(t) = crate::seq::elem(&ty) {
            // A non-copy element is lent as an element view; only a name or
            // `_` can bind one.
            // (A view receiver cannot lend an element view yet.)
            let lend = view.is_none()
                && self.mode(t) != Mode::Copy
                && !super::unresolved(t)
                && matches!(pat.kind, PatternKind::Bind(_) | PatternKind::Wild);
            let plan = match view {
                Some((_, bounds)) => Plan::View {
                    interval: bounds.is_some(),
                    lend,
                },
                None => Plan::Seq { place, lend },
            };
            if lend {
                return Some((t.clone(), plan));
            }
            (t.clone(), plan)
        } else {
            if !super::unresolved(&ty) {
                self.error(
                    "E0213",
                    format!(
                        "'for' iterates an integer range, a fixed array, a ChunkSeq or a Slice, found {ty}"
                    ),
                    iter.span.clone(),
                );
            }
            return None;
        };
        if self.mode(&element) != Mode::Copy && !super::unresolved(&element) {
            self.error(
                "E0213",
                format!("'for' over elements of non-copy type '{element}' binds an element view: write a name or '_' and iterate a sequence place (arrays and views of non-copy elements are not iterable yet)"),
                iter.span.clone(),
            );
            return None;
        }
        Some((element, plan))
    }

    /// The place a rewritten `for` iterates, when `pat` is its hidden index
    /// and `end` its `place.len()` (or `view.len()`) bound: the body must
    /// not change it.
    pub(super) fn for_borrow(&self, pat: &Pattern, end: &Expr) -> Option<(Place, Span)> {
        let PatternKind::Bind(name) = &pat.kind else {
            return None;
        };
        if !name.starts_with(HIDDEN_PREFIX) {
            return None;
        }
        let ExprKind::Call { callee, args, .. } = &end.kind else {
            return None;
        };
        match &callee.kind {
            ExprKind::Field(base, m) if m == "len" && args.is_empty() => {
                let base = view_base(base).map_or(&**base, |(b, _)| b);
                Place::from_expr(base).map(|p| (p, base.span.clone()))
            }
            _ => None,
        }
    }

    /// Rejects a write, `inout` loan or move of a place a `for` iterates.
    pub(super) fn check_loop_borrows(&mut self, p: &Place, span: &Span) -> bool {
        let mut ok = true;
        for (borrowed, origin) in self.loop_borrows.clone() {
            if borrowed.overlaps(p) {
                ok = false;
                self.error(
                    "E0301",
                    format!(
                        "access to '{}' conflicts with the 'for' loop that iterates '{}'",
                        p.display(),
                        borrowed.display()
                    ),
                    span.clone(),
                );
                self.related(origin, "this loop reads the elements of the place");
                self.help("A 'for' loop over a sequence reads its length once; its body may read the sequence but not write, lend `inout` or move it. Iterate the indices instead.");
            }
        }
        ok
    }
}

/// The hidden index of the `k`th rewritten loop.
fn index_name(k: usize) -> String {
    format!("{HIDDEN_PREFIX}{k}_i")
}

/// The hidden binding of the `k`th rewritten loop's temporary iterable.
fn seq_name(k: usize) -> String {
    format!("{HIDDEN_PREFIX}{k}_seq")
}

/// Rewrites every `for` whose iterable span is in `plans` in `module`.
/// `after` is past every span the module's checked world uses.
pub(super) fn desugar(
    module: &Module,
    plans: &BTreeMap<(usize, usize), Plan>,
    after: usize,
) -> Module {
    let mut module = module.clone();
    let mut r = Rewriter {
        plans,
        next: after,
        // Hidden names stay distinct from any earlier rewrite's.
        count: after,
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
                r.expr(body);
            }
        }
    }
    module
}

struct Rewriter<'a> {
    plans: &'a BTreeMap<(usize, usize), Plan>,
    next: usize,
    count: usize,
}

impl Rewriter<'_> {
    fn fresh(&mut self) -> Span {
        let span = self.next..self.next + 1;
        self.next += 2;
        span
    }
    fn node(&mut self, kind: ExprKind) -> Expr {
        Expr {
            kind,
            span: self.fresh(),
        }
    }
    fn name(&mut self, n: &str) -> Expr {
        self.node(ExprKind::Name(n.into()))
    }
    fn method(&mut self, receiver: Expr, m: &str, args: Vec<Expr>) -> Expr {
        let callee = self.node(ExprKind::Field(Box::new(receiver), m.into()));
        self.node(ExprKind::Call {
            callee: Box::new(callee),
            types: vec![],
            args: args
                .into_iter()
                .map(|expr| Arg {
                    pass: Pass::In,
                    explicit: false,
                    expr,
                })
                .collect(),
        })
    }
    /// A copy of a place or view expression with fresh spans.
    fn respan(&mut self, e: &Expr) -> Expr {
        let mut copy = e.clone();
        self.respan_in(&mut copy);
        copy
    }
    fn respan_in(&mut self, e: &mut Expr) {
        e.span = self.fresh();
        for child in super::try_option::children(e) {
            self.respan_in(child);
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        for child in super::try_option::children(e) {
            self.expr(child);
        }
        let ExprKind::For { iter, .. } = &e.kind else {
            return;
        };
        let Some(plan) = self.plans.get(&(iter.span.start, iter.span.end)).cloned() else {
            return;
        };
        let ExprKind::For { pat, iter, body } =
            std::mem::replace(&mut e.kind, ExprKind::Tuple(vec![]))
        else {
            unreachable!()
        };
        let k = self.count;
        self.count += 1;
        let index = index_name(k);
        let read_index = |r: &mut Self| r.name(&index);
        // The iterated place (the iterable itself, a view with hidden
        // bounds, or the hidden binding) and the hidden bindings before the
        // loop.
        let mut hidden = vec![];
        let source = match &plan {
            Plan::StaticText(_)
            | Plan::Array { place: true, .. }
            | Plan::Seq { place: true, .. }
            | Plan::View {
                interval: false, ..
            } => *iter,
            Plan::View { interval: true, .. } => {
                let ExprKind::Index(base, interval) = iter.kind else {
                    unreachable!()
                };
                let ExprKind::Binary(op, lo, hi) = interval.kind else {
                    unreachable!()
                };
                let mut bounds = vec![];
                for (suffix, value) in [("lo", *lo), ("hi", *hi)] {
                    let name = format!("{HIDDEN_PREFIX}{k}_{suffix}");
                    let pat = Pattern {
                        kind: PatternKind::Bind(name.clone()),
                        span: self.fresh(),
                    };
                    hidden.push(Statement::Let {
                        pat,
                        ty: Type::named("u64"),
                        value,
                        mutable: false,
                    });
                    bounds.push(self.name(&name));
                }
                let hi = bounds.pop().unwrap();
                let lo = bounds.pop().unwrap();
                let interval = Expr {
                    kind: ExprKind::Binary(op, Box::new(lo), Box::new(hi)),
                    span: interval.span,
                };
                Expr {
                    kind: ExprKind::Index(base, Box::new(interval)),
                    span: iter.span,
                }
            }
            Plan::Array { place: false, ty } => {
                let name = seq_name(k);
                let pat = Pattern {
                    kind: PatternKind::Bind(name.clone()),
                    span: self.fresh(),
                };
                hidden.push(Statement::Let {
                    pat,
                    ty: ty.clone().unwrap_or(Type::Infer),
                    value: *iter,
                    mutable: false,
                });
                self.name(&name)
            }
            Plan::Seq { place: false, .. } => {
                let name = seq_name(k);
                let pat = Pattern {
                    kind: PatternKind::Bind(name.clone()),
                    span: self.fresh(),
                };
                hidden.push(Statement::Let {
                    pat,
                    ty: Type::Infer,
                    value: *iter,
                    mutable: false,
                });
                self.name(&name)
            }
        };
        let body_stmt = Statement::Expr(*body);
        let (bound, inner) = match plan {
            Plan::StaticText(n) => {
                let ExprKind::Tuple(items) = source.kind else {
                    unreachable!()
                };
                let mut arms = vec![];
                for (i, item) in items.into_iter().enumerate() {
                    let p = if i + 1 == n {
                        PatternKind::Wild
                    } else {
                        PatternKind::Literal(Literal::Int(i as i128))
                    };
                    let span = self.fresh();
                    arms.push((Pattern { kind: p, span }, item));
                }
                let scrutinee = read_index(self);
                let select = self.node(ExprKind::Match {
                    value: Box::new(scrutinee),
                    arms,
                });
                let bound = self.node(ExprKind::Literal(Literal::Int(n as i128)));
                let inner = ExprKind::Block {
                    statements: vec![
                        Statement::Let {
                            pat,
                            ty: Type::Infer,
                            value: select,
                            mutable: false,
                        },
                        body_stmt,
                    ],
                    tail: None,
                };
                (bound, inner)
            }
            Plan::Array { .. } => {
                // The bound keeps the iterable's spans, so a conflict with
                // the loop's loan points at it.
                let receiver = self.respan(&source);
                let bound = self.method(source, "len", vec![]);
                let i = read_index(self);
                let element = self.node(ExprKind::Index(Box::new(receiver), Box::new(i)));
                let inner = ExprKind::Block {
                    statements: vec![
                        Statement::Let {
                            pat,
                            ty: Type::Infer,
                            value: element,
                            mutable: false,
                        },
                        body_stmt,
                    ],
                    tail: None,
                };
                (bound, inner)
            }
            Plan::Seq { lend, .. } | Plan::View { lend, .. } => {
                let receiver = self.respan(&source);
                let bound = self.method(source, "len", vec![]);
                let i = read_index(self);
                let get = self.method(receiver, "try_get", vec![i]);
                // The element binds through a `let`, so the pattern must be
                // irrefutable (`E0210`) as for an array.
                let element = format!("{HIDDEN_PREFIX}{k}_x");
                let (payload, pat) = if lend {
                    // `Some(x)` binds an `in` view of the element.
                    (pat, None)
                } else {
                    let payload = Pattern {
                        kind: PatternKind::Bind(element.clone()),
                        span: self.fresh(),
                    };
                    (payload, Some(pat))
                };
                let some = Pattern {
                    kind: PatternKind::Constructor {
                        name: "Option::Some".into(),
                        fields: vec![("0".into(), payload)],
                        named: false,
                    },
                    span: self.fresh(),
                };
                let value = self.name(&element);
                let none = Pattern {
                    kind: PatternKind::Wild,
                    span: self.fresh(),
                };
                let mut statements = vec![];
                if let Some(pat) = pat {
                    statements.push(Statement::Let {
                        pat,
                        ty: Type::Infer,
                        value,
                        mutable: false,
                    });
                }
                statements.push(body_stmt);
                let arm = self.node(ExprKind::Block {
                    statements,
                    tail: None,
                });
                let skip = self.node(ExprKind::Block {
                    statements: vec![],
                    tail: None,
                });
                let inner = ExprKind::Block {
                    statements: vec![],
                    tail: Some(Box::new(Expr {
                        kind: ExprKind::Match {
                            value: Box::new(get),
                            arms: vec![(some, arm), (none, skip)],
                        },
                        span: self.fresh(),
                    })),
                };
                (bound, inner)
            }
        };
        let zero = self.node(ExprKind::Literal(Literal::Int(0)));
        let range = self.node(ExprKind::Range {
            start: Box::new(zero),
            end: Box::new(bound),
            inclusive: false,
        });
        let body = self.node(inner);
        let index_pat = Pattern {
            kind: PatternKind::Bind(index),
            span: self.fresh(),
        };
        let lowered = ExprKind::For {
            pat: index_pat,
            iter: Box::new(range),
            body: Box::new(body),
        };
        if hidden.is_empty() {
            e.kind = lowered;
        } else {
            let inner_loop = Expr {
                kind: lowered,
                span: e.span.clone(),
            };
            hidden.push(Statement::Expr(inner_loop));
            e.kind = ExprKind::Block {
                statements: hidden,
                tail: None,
            };
        }
    }
}
