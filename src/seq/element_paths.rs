//! Field paths through sequence elements and disjoint element borrows
//! (manuscript §13, self-hosting SH3 lane 2, #1241).
//!
//! An element view (§13, #1173) names one element of a `ChunkSeq[T]` or
//! `Slice[T]` in place. This module gives the forms that reach *into* an
//! element, and the forms that borrow two elements of one sequence at once,
//! their meaning as element views, so that every backend runs them as an
//! ordinary `match` on `try_get`/`try_get_mut` (one bounds check, then the
//! element's address):
//!
//! ```text
//! xs[i].f.g op= v      =>  match xs.try_get_mut(i) { Some($e) => $e.f.g op= v, None => raise }
//! f(inout xs[i].f, a)  =>  match xs.try_get_mut(i) { Some($e) => f(inout $e.f, a), None => raise }
//! xs[i].f.m()          =>  match xs.try_get[_mut](i) { Some($e) => $e.f.m(), None => raise }
//! xs[i].f              =>  match xs.try_get(i) { Some($e) => $e.f, None => raise }   (non-copy T)
//! ```
//!
//! `raise` is `Fail::raise(RuntimeError::IndexOutOfBounds)`, which is what
//! `xs[i]` raises past the end. An index that is not a literal or a name,
//! and any value operand that mentions the sequence, is bound first, so it
//! is evaluated before the view lends the sequence.
//!
//! Two element borrows of one sequence in one call (`swap(inout xs[0],
//! inout xs[1])`, `f(inout xs[i], in xs[j])`) are disjoint when their
//! indices are distinct integer literals ([`crate::check::Place::element`]).
//! Otherwise the indices are bound to names of one *distinct group*
//! (`$ixK_0`, `$ixK_1`, ..) and checked pairwise unequal first; equal
//! indices raise `IndexOutOfBounds`. The checker treats two indices of one
//! group as distinct elements. No source can spell those names.
//! `xs.swap(i, j)` is `swap(inout xs[i], inout xs[j])` for distinct indices
//! and a bounds check for equal ones.
//!
//! The checker types the module first; [`desugar`] reads the first check's
//! types to find these forms, and the rewritten module is checked again
//! (like `?` on `Option`, `crate::check::try_option`). Every node the source
//! spells keeps its span; the nodes the rewrite adds get spans after every
//! span of the first check.
use crate::ast::*;
use crate::check::{Place, World};
use crate::seq::{elem, interval, is_seq_or_slice};
use std::collections::BTreeMap;

/// The prefix of the names of a distinct group of element indices.
const DISTINCT_PREFIX: &str = "$ix";
/// The prefix of the name the `Some` arm of a rewritten view binds.
const VIEW_PREFIX: &str = "$elem";
/// The prefix of an operand bound before a view lends its sequence.
const OPERAND_PREFIX: &str = "$operand";
/// The type of a sequence index.
const INDEX_TYPE: &str = "u64";

/// Whether `name` is an index of a distinct group.
pub fn is_distinct_index(name: &str) -> bool {
    name.starts_with(DISTINCT_PREFIX)
}

fn group_of(name: &str) -> Option<(&str, &str)> {
    name.strip_prefix(DISTINCT_PREFIX)?.split_once('_')
}

/// Whether `a` and `b` are two different indices of one distinct group,
/// which the rewrite checked unequal before the call.
pub fn distinct_indices(a: &str, b: &str) -> bool {
    matches!((group_of(a), group_of(b)), (Some((g, i)), Some((h, j))) if g == h && i != j)
}

/// Rewrites every element path and every call that borrows two elements of
/// one sequence in `module`, using the types `world` and `types` of its
/// first check. `after` is past every span that check used. `None` when
/// nothing needs rewriting.
pub(crate) fn desugar(
    module: &Module,
    world: &World,
    types: &TypeMap,
    after: usize,
) -> Option<Module> {
    let mut module = module.clone();
    let methods = receiver_passes(&module);
    let mut r = Rewriter {
        world,
        types,
        methods,
        bounds: BTreeMap::new(),
        next: after,
        count: 0,
        changed: false,
    };
    for item in &mut module.items {
        let (outer, functions): (Vec<Generic>, Vec<&mut Function>) = match item {
            Item::Function(f) => (vec![], vec![&mut **f]),
            Item::Impl {
                generics, methods, ..
            } => (generics.clone(), methods.iter_mut().collect()),
            Item::Trait(t) => (vec![], t.methods.iter_mut().collect()),
            Item::Conformance(c) => (vec![], c.methods.iter_mut().collect()),
            Item::Adt(_) | Item::Effect(_) => (vec![], vec![]),
        };
        for f in functions {
            r.bounds = outer
                .iter()
                .chain(&f.generics)
                .filter(|g| !g.row)
                .map(|g| (g.name.clone(), g.bound))
                .collect();
            if let Some(body) = &mut f.body {
                r.expr(body, false);
            }
        }
    }
    r.changed.then_some(module)
}

/// Rewrites one function at a time for the checker's isolated check of a
/// test function.
pub(crate) fn desugar_function(
    f: &Function,
    world: &World,
    types: &TypeMap,
    after: usize,
) -> Option<Function> {
    let single = Module {
        items: vec![Item::Function(Box::new(f.clone()))],
        ..Module::default()
    };
    match desugar(&single, world, types, after)?.items.pop() {
        Some(Item::Function(f)) => Some(*f),
        _ => None,
    }
}

/// For each method name, whether some declaration of it takes its receiver
/// `inout`.
fn receiver_passes(module: &Module) -> BTreeMap<String, Pass> {
    let mut out: BTreeMap<String, Pass> = BTreeMap::new();
    let mut note = |f: &Function| {
        if let Some(p) = f.params.first().filter(|p| p.name == "self") {
            let entry = out.entry(f.name.clone()).or_insert(Pass::In);
            if p.pass == Pass::Inout {
                *entry = Pass::Inout;
            }
        }
    };
    for item in &module.items {
        match item {
            Item::Impl { methods, .. } => methods.iter().for_each(&mut note),
            Item::Trait(t) => t.methods.iter().for_each(&mut note),
            Item::Conformance(c) => c.methods.iter().for_each(&mut note),
            _ => {}
        }
    }
    out
}

/// One element borrowed by a rewritten view: the sequence place, its
/// index, how it is borrowed, and the name its `Some` arm binds.
struct Site {
    base: Expr,
    index: Expr,
    pass: Pass,
    name: String,
    /// The span of the source's `xs[i]`.
    span: Span,
}

struct Rewriter<'a> {
    world: &'a World,
    types: &'a TypeMap,
    methods: BTreeMap<String, Pass>,
    bounds: BTreeMap<String, Mode>,
    next: usize,
    count: usize,
    changed: bool,
}

impl Rewriter<'_> {
    fn ty(&self, e: &Expr) -> Option<&Type> {
        self.types.get(&(e.span.start, e.span.end))
    }
    fn fresh_span(&mut self) -> Span {
        let span = self.next..self.next + 1;
        self.next += 2;
        span
    }
    fn fresh(&mut self, kind: ExprKind) -> Expr {
        Expr {
            kind,
            span: self.fresh_span(),
        }
    }
    fn fresh_name(&mut self, prefix: &str) -> String {
        self.count += 1;
        format!("{prefix}{}", self.count)
    }

    /// Whether `base` is a sequence or slice place.
    fn seq_place(&self, base: &Expr) -> bool {
        Place::from_expr(base).is_some() && self.ty(base).is_some_and(is_seq_or_slice)
    }
    /// Whether the elements of the sequence place `base` are copy.
    fn copy_elements(&self, base: &Expr) -> bool {
        self.ty(base)
            .and_then(elem)
            .is_some_and(|t| *t != Type::Infer && self.world.mode(t, &self.bounds) == Mode::Copy)
    }

    /// The span of the element `xs[i]` that the field/index chain `e` goes
    /// through, nearest the root: `e` is `xs[i]` followed by at least one
    /// field or index. `None` for other expressions.
    fn bottom(&self, e: &Expr) -> Option<Span> {
        let mut cur = e;
        let mut found = None;
        loop {
            match &cur.kind {
                ExprKind::Field(b, _) => cur = b,
                ExprKind::Index(b, _) if interval(cur).is_none() => {
                    if !std::ptr::eq(cur, e) && self.seq_place(b) {
                        found = Some(cur.span.clone());
                    }
                    cur = b;
                }
                _ => return found,
            }
        }
    }

    /// The pass with which method `m` borrows a receiver of type `t`.
    fn receiver_pass(&self, receiver: &Expr, m: &str) -> Pass {
        if let Some(t) = self.ty(receiver)
            && (is_seq_or_slice(t) || crate::strings::is_string_type(t))
            && let Some(op) = crate::seq::resolve(m, Some(t))
        {
            return op.receiver_pass();
        }
        self.methods.get(m).copied().unwrap_or(Pass::In)
    }

    /// Whether `e`, an operand of an anchor that views elements of the
    /// places `roots`, must be evaluated before the view starts.
    fn hoisted(e: &Expr, roots: &[String]) -> bool {
        !matches!(e.kind, ExprKind::Literal(_))
            && crate::check::free_names(e)
                .iter()
                .any(|n| roots.contains(n))
    }

    fn expr(&mut self, e: &mut Expr, place_position: bool) {
        if self.rewrite(e, place_position) {
            self.changed = true;
        }
        self.children(e);
    }

    /// Visits the children of `e`, telling each whether it is in a place
    /// position its parent borrows (an assignment target, an explicit `in`
    /// or `inout` argument, a method receiver, the base of a field).
    fn children(&mut self, e: &mut Expr) {
        if let Some(cs) = e.kind.control_children_mut() {
            for c in cs {
                self.expr(c, false);
            }
            return;
        }
        match &mut e.kind {
            ExprKind::Literal(_) | ExprKind::Name(_) => {}
            ExprKind::Tuple(es) => es.iter_mut().for_each(|e| self.expr(e, false)),
            ExprKind::Struct { fields, .. } => {
                fields.iter_mut().for_each(|(_, e)| self.expr(e, false))
            }
            ExprKind::Block { statements, tail } => {
                for s in statements {
                    match s {
                        Statement::Let { value, .. } => self.expr(value, false),
                        Statement::Expr(e) => self.expr(e, false),
                    }
                }
                if let Some(t) = tail {
                    self.expr(t, false);
                }
            }
            ExprKind::Call { callee, args, .. } => {
                match &mut callee.kind {
                    ExprKind::Field(receiver, _) => self.expr(receiver, true),
                    _ => self.expr(callee, false),
                }
                for a in args {
                    let place = a.explicit && a.pass != Pass::Sink;
                    self.expr(&mut a.expr, place);
                }
            }
            ExprKind::Field(b, _) => self.expr(b, true),
            ExprKind::Try(b) | ExprKind::Unary(_, b) => self.expr(b, false),
            ExprKind::Index(a, b) => {
                self.expr(a, true);
                self.expr(b, false);
            }
            ExprKind::Binary(_, a, b) => {
                self.expr(a, false);
                self.expr(b, false);
            }
            ExprKind::Assign(_, a, b) => {
                self.expr(a, true);
                self.expr(b, false);
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                self.expr(condition, false);
                self.expr(then, false);
                if let Some(o) = otherwise {
                    self.expr(o, false);
                }
            }
            ExprKind::Match { value, arms } => {
                // A matched place is borrowed by the match (§4.3).
                self.expr(value, true);
                arms.iter_mut().for_each(|(_, e)| self.expr(e, false));
            }
            ExprKind::Closure { body, .. } => self.expr(body, false),
            ExprKind::Handle {
                state,
                body,
                ret,
                ops,
                ..
            } => {
                if let Some((_, e)) = state {
                    self.expr(e, false);
                }
                self.expr(body, false);
                self.expr(&mut ret.body, false);
                ops.iter_mut().for_each(|c| self.expr(&mut c.body, false));
            }
            ExprKind::Loop(_)
            | ExprKind::For { .. }
            | ExprKind::Range { .. }
            | ExprKind::Break(_)
            | ExprKind::Continue
            | ExprKind::Return(_) => {}
        }
    }

    /// Rewrites `e` itself when it is one of the forms in the module
    /// documentation. Returns whether it changed.
    fn rewrite(&mut self, e: &mut Expr, place_position: bool) -> bool {
        match &e.kind {
            ExprKind::Assign(_, target, _) => {
                let Some(span) = self.bottom(target) else {
                    return false;
                };
                self.wrap(e, vec![(span, Pass::Inout)]);
                true
            }
            ExprKind::Call { callee, args, .. } => {
                if let ExprKind::Field(base, m) = &callee.kind
                    && m == "swap"
                    && args.len() == 2
                    && self.seq_place(base)
                {
                    self.seq_swap(e);
                    return true;
                }
                let grouped = self.distinct_groups(e);
                let mut sites = vec![];
                if let ExprKind::Call { callee, args, .. } = &e.kind {
                    if let ExprKind::Field(receiver, m) = &callee.kind
                        && let Some(span) = self.bottom(receiver)
                    {
                        let pass = self.receiver_pass(receiver, m);
                        if pass == Pass::Inout || !self.copy_bottom(receiver, &span) {
                            sites.push((span, pass));
                        }
                    }
                    for a in args {
                        if a.explicit
                            && a.pass != Pass::Sink
                            && let Some(span) = self.bottom(&a.expr)
                            && (a.pass == Pass::Inout || !self.copy_bottom(&a.expr, &span))
                        {
                            sites.push((span, a.pass));
                        }
                    }
                }
                if !sites.is_empty() {
                    self.wrap(e, sites);
                    return true;
                }
                grouped
            }
            ExprKind::Match { value, .. } => {
                let Some(span) = self.bottom(value) else {
                    return false;
                };
                if self.copy_bottom(value, &span) {
                    return false;
                }
                self.wrap(e, vec![(span, Pass::In)]);
                true
            }
            ExprKind::Field(..) | ExprKind::Index(..) if !place_position => {
                let Some(span) = self.bottom(e) else {
                    return false;
                };
                if self.copy_bottom(e, &span) {
                    return false;
                }
                self.wrap(e, vec![(span, Pass::In)]);
                true
            }
            _ => false,
        }
    }

    /// Whether the element at `span` in the chain `e` has a copy type (so
    /// reading through it needs no view).
    fn copy_bottom(&self, e: &Expr, span: &Span) -> bool {
        let mut cur = e;
        loop {
            match &cur.kind {
                ExprKind::Field(b, _) => cur = b,
                ExprKind::Index(b, _) => {
                    if cur.span == *span {
                        return self.copy_elements(b);
                    }
                    cur = b;
                }
                _ => return false,
            }
        }
    }

    /// Wraps `e` in one element view per site (outermost first), after
    /// binding the operands that must be evaluated before the views.
    fn wrap(&mut self, e: &mut Expr, sites: Vec<(Span, Pass)>) {
        let mut lets = vec![];
        let mut taken = vec![];
        for (span, pass) in sites {
            let name = self.fresh_name(VIEW_PREFIX);
            let fresh = self.fresh_span();
            let Some((base, mut index)) = replace_bottom(e, &span, &name, fresh) else {
                continue;
            };
            if !matches!(index.kind, ExprKind::Literal(_) | ExprKind::Name(_)) {
                let bound = self.fresh_name(OPERAND_PREFIX);
                lets.push(self.let_statement(&bound, Type::named(INDEX_TYPE), index));
                index = self.fresh(ExprKind::Name(bound));
            }
            taken.push(Site {
                base,
                index,
                pass,
                name,
                span,
            });
        }
        let roots: Vec<String> = taken
            .iter()
            .filter_map(|s| Place::from_expr(&s.base).map(|p| p.root))
            .collect();
        // Value operands that read a viewed sequence run before the view.
        let mut operands: Vec<&mut Expr> = vec![];
        match &mut e.kind {
            ExprKind::Assign(_, _, v) => operands.push(v),
            ExprKind::Call { args, .. } => operands.extend(
                args.iter_mut()
                    .filter(|a| !(a.explicit && a.pass != Pass::Sink))
                    .map(|a| &mut a.expr),
            ),
            _ => {}
        }
        let mut hoists = vec![];
        for v in operands {
            if Self::hoisted(v, &roots) {
                hoists.push(v);
            }
        }
        for v in hoists {
            let bound = self.fresh_name(OPERAND_PREFIX);
            let span = self.fresh_span();
            let value = std::mem::replace(
                v,
                Expr {
                    kind: ExprKind::Name(bound.clone()),
                    span,
                },
            );
            lets.push(self.let_statement(&bound, Type::Infer, value));
        }
        let mut body = std::mem::replace(
            e,
            Expr {
                kind: ExprKind::Tuple(vec![]),
                span: 0..0,
            },
        );
        for site in taken.into_iter().rev() {
            body = self.view(site, body);
        }
        if !lets.is_empty() {
            body = Expr {
                span: self.fresh_span(),
                kind: ExprKind::Block {
                    statements: lets,
                    tail: Some(Box::new(body)),
                },
            };
        }
        *e = body;
    }

    fn let_statement(&mut self, name: &str, ty: Type, value: Expr) -> Statement {
        Statement::Let {
            pat: Pattern {
                kind: PatternKind::Bind(name.into()),
                span: self.fresh_span(),
            },
            ty,
            value,
            mutable: false,
        }
    }

    /// `Fail::raise(RuntimeError::IndexOutOfBounds)` for the source
    /// expression at `at`. The call takes the span `at` without its last
    /// byte, which no other node has, so a diagnostic about the raise (an
    /// undeclared `Fail[RuntimeError]`) points into the source.
    fn raise(&mut self, at: &Span) -> Expr {
        let error = self.fresh(ExprKind::Name("RuntimeError::IndexOutOfBounds".into()));
        let callee = self.fresh(ExprKind::Name("Fail::raise".into()));
        Expr {
            span: at.start..at.end.saturating_sub(1).max(at.start + 1),
            kind: ExprKind::Call {
            callee: Box::new(callee),
            types: vec![],
                args: vec![Arg {
                    pass: Pass::In,
                    explicit: false,
                    expr: error,
                }],
            },
        }
    }

    /// `match base.try_get[_mut](index) { Some(name) => body, None => raise }`.
    fn view(&mut self, site: Site, body: Expr) -> Expr {
        let method = if site.pass == Pass::Inout {
            "try_get_mut"
        } else {
            "try_get"
        };
        let callee = self.fresh(ExprKind::Field(Box::new(site.base), method.into()));
        // The scrutinee takes the span of the source's `xs[i]`, so a
        // diagnostic about the view points at the element.
        let scrutinee = Expr {
            span: site.span.clone(),
            kind: ExprKind::Call {
                callee: Box::new(callee),
                types: vec![],
                args: vec![Arg {
                    pass: Pass::In,
                    explicit: false,
                    expr: site.index,
                }],
            },
        };
        let some = Pattern {
            kind: PatternKind::Constructor {
                name: "Option::Some".into(),
                fields: vec![(
                    "0".into(),
                    Pattern {
                        kind: PatternKind::Bind(site.name),
                        span: self.fresh_span(),
                    },
                )],
                named: false,
            },
            span: self.fresh_span(),
        };
        let none = Pattern {
            kind: PatternKind::Constructor {
                name: "Option::None".into(),
                fields: vec![],
                named: false,
            },
            span: self.fresh_span(),
        };
        let raise = self.raise(&site.span);
        self.fresh(ExprKind::Match {
            value: Box::new(scrutinee),
            arms: vec![(some, body), (none, raise)],
        })
    }

    /// A copy of the place expression `e` with fresh spans (the original
    /// keeps its own).
    fn respan(&mut self, e: &Expr) -> Expr {
        let kind = match &e.kind {
            ExprKind::Field(b, f) => ExprKind::Field(Box::new(self.respan(b)), f.clone()),
            k => k.clone(),
        };
        self.fresh(kind)
    }

    /// `base.swap(i, j)`:
    /// `{ let a = i; let b = j; if a == b { if a >= base.len() { raise } } else { swap(inout base[a], inout base[b]) } }`.
    fn seq_swap(&mut self, e: &mut Expr) {
        let at = e.span.clone();
        let ExprKind::Call { callee, args, .. } = std::mem::replace(&mut e.kind, ExprKind::Tuple(vec![]))
        else {
            unreachable!()
        };
        let ExprKind::Field(base, _) = callee.kind else {
            unreachable!()
        };
        let group = self.fresh_name("");
        let names = [0, 1].map(|m| format!("{DISTINCT_PREFIX}{group}_{m}"));
        let mut statements = vec![];
        for (name, a) in names.iter().zip(args) {
            statements.push(self.let_statement(name, Type::named(INDEX_TYPE), a.expr));
        }
        let name = |r: &mut Self, n: &str| r.fresh(ExprKind::Name(n.into()));
        let a = name(self, &names[0]);
        let b = name(self, &names[1]);
        let equal = self.fresh(ExprKind::Binary("==".into(), Box::new(a), Box::new(b)));
        let a = name(self, &names[0]);
        let len_base = self.respan(&base);
        let len_callee = self.fresh(ExprKind::Field(Box::new(len_base), "len".into()));
        let len = self.fresh(ExprKind::Call {
            callee: Box::new(len_callee),
            types: vec![],
            args: vec![],
        });
        let past = self.fresh(ExprKind::Binary(">=".into(), Box::new(a), Box::new(len)));
        let raise = self.raise(&at);
        let check = self.fresh(ExprKind::If {
            condition: Box::new(past),
            then: Box::new(raise),
            otherwise: None,
        });
        let element = |r: &mut Self, base: Expr, n: &str| {
            let index = name(r, n);
            let element = r.fresh(ExprKind::Index(Box::new(base), Box::new(index)));
            Arg {
                pass: Pass::Inout,
                explicit: true,
                expr: element,
            }
        };
        let other_base = self.respan(&base);
        let first = element(self, *base, &names[0]);
        let second = element(self, other_base, &names[1]);
        let swap_callee = self.fresh(ExprKind::Name("swap".into()));
        let swap = self.fresh(ExprKind::Call {
            callee: Box::new(swap_callee),
            types: vec![],
            args: vec![first, second],
        });
        let branch = self.fresh(ExprKind::If {
            condition: Box::new(equal),
            then: Box::new(check),
            otherwise: Some(Box::new(swap)),
        });
        e.kind = ExprKind::Block {
            statements,
            tail: Some(Box::new(branch)),
        };
    }

    /// Binds the indices of the elements a call borrows from one sequence
    /// (at least two, one of them `inout`, not all distinct literals) to a
    /// distinct group, checked pairwise unequal before the call. Returns
    /// whether it changed `e`.
    fn distinct_groups(&mut self, e: &mut Expr) -> bool {
        let ExprKind::Call { args, .. } = &e.kind else {
            return false;
        };
        let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, a) in args.iter().enumerate() {
            if !(a.explicit && a.pass != Pass::Sink) {
                continue;
            }
            if let ExprKind::Index(base, _) = &a.expr.kind
                && interval(&a.expr).is_none()
                && self.seq_place(base)
                && let Some(p) = Place::from_expr(base)
            {
                groups.entry(p.display()).or_default().push(i);
            }
        }
        let mut statements = vec![];
        let mut checks: Vec<Expr> = vec![];
        let mut changed = false;
        for members in groups.into_values() {
            let ExprKind::Call { args, .. } = &e.kind else {
                unreachable!()
            };
            if members.len() < 2 || members.iter().all(|&i| args[i].pass == Pass::In) {
                continue;
            }
            let literals: Vec<Option<i128>> = members
                .iter()
                .map(|&i| match &args[i].expr.kind {
                    ExprKind::Index(_, index) => match index.kind {
                        ExprKind::Literal(Literal::Int(n)) => Some(n),
                        _ => None,
                    },
                    _ => None,
                })
                .collect();
            if literals.iter().all(Option::is_some) {
                // Distinct literals are disjoint; equal ones stay refused.
                continue;
            }
            if members.iter().any(|&i| {
                matches!(&args[i].expr.kind, ExprKind::Index(_, index)
                    if matches!(&index.kind, ExprKind::Name(n) if is_distinct_index(n)))
            }) {
                // Already a checked group.
                continue;
            }
            changed = true;
            let group = self.fresh_name("");
            let mut names = vec![];
            for (m, &i) in members.iter().enumerate() {
                let name = format!("{DISTINCT_PREFIX}{group}_{m}");
                let span = self.fresh_span();
                let ExprKind::Call { args, .. } = &mut e.kind else {
                    unreachable!()
                };
                let ExprKind::Index(_, index) = &mut args[i].expr.kind else {
                    unreachable!()
                };
                let value = std::mem::replace(
                    &mut **index,
                    Expr {
                        kind: ExprKind::Name(name.clone()),
                        span,
                    },
                );
                statements.push(self.let_statement(&name, Type::named(INDEX_TYPE), value));
                names.push(name);
            }
            for x in 0..names.len() {
                for y in x + 1..names.len() {
                    let a = self.fresh(ExprKind::Name(names[x].clone()));
                    let b = self.fresh(ExprKind::Name(names[y].clone()));
                    checks.push(self.fresh(ExprKind::Binary(
                        "==".into(),
                        Box::new(a),
                        Box::new(b),
                    )));
                }
            }
        }
        if !changed {
            return false;
        }
        let mut condition = checks.remove(0);
        for c in checks {
            condition = self.fresh(ExprKind::Binary(
                "||".into(),
                Box::new(condition),
                Box::new(c),
            ));
        }
        let at = e.span.clone();
        let raise = self.raise(&at);
        let check = self.fresh(ExprKind::If {
            condition: Box::new(condition),
            then: Box::new(raise),
            otherwise: None,
        });
        statements.push(Statement::Expr(check));
        let call = std::mem::replace(
            e,
            Expr {
                kind: ExprKind::Tuple(vec![]),
                span: 0..0,
            },
        );
        *e = Expr {
            span: self.fresh_span(),
            kind: ExprKind::Block {
                statements,
                tail: Some(Box::new(call)),
            },
        };
        true
    }
}

/// Replaces the element `xs[i]` at `span` inside the field/index chain `e`
/// with the name `name` (at the span `fresh`), and returns `xs` and `i`.
fn replace_bottom(e: &mut Expr, span: &Span, name: &str, fresh: Span) -> Option<(Expr, Expr)> {
    if e.span == *span && matches!(e.kind, ExprKind::Index(..)) {
        let ExprKind::Index(base, index) =
            std::mem::replace(&mut e.kind, ExprKind::Name(name.into()))
        else {
            unreachable!()
        };
        e.span = fresh;
        return Some((*base, *index));
    }
    match &mut e.kind {
        ExprKind::Field(b, _) | ExprKind::Index(b, _) => replace_bottom(b, span, name, fresh),
        ExprKind::Assign(_, target, _) => replace_bottom(target, span, name, fresh),
        ExprKind::Match { value, .. } => replace_bottom(value, span, name, fresh),
        ExprKind::Call { callee, args, .. } => {
            if let ExprKind::Field(receiver, _) = &mut callee.kind
                && let Some(found) = replace_bottom(receiver, span, name, fresh.clone())
            {
                return Some(found);
            }
            args.iter_mut()
                .find_map(|a| replace_bottom(&mut a.expr, span, name, fresh.clone()))
        }
        _ => None,
    }
}
