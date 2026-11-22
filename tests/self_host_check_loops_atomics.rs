//! Self-hosting SH2 lane 10 (#1182): `@vesper/check-loops-atomics` against
//! the Rust checker. Each task renders its diagnostics (code, span,
//! message, help; related labels are printed too, so a missing one is a
//! difference) or its report as text in both languages, and the texts must
//! be equal:
//!
//! - the control rules of `src/loops/check.rs` and `src/loops/try_option.rs`
//!   (E0213, E0200) over every corpus world, two mutants of each that make
//!   the rules fire (jumps out of closures, handled bodies, clauses and
//!   functions, `break` values in `for`, stray ranges; non-range `for`
//!   iterables), the loop fixtures and the `?` sources of the checker tests;
//! - the atomics checks of `src/atomics.rs` (`needs_prelude`,
//!   `needs_workers`, `check_world`: E0820-E0824, `check_program`: E0822)
//!   over the corpus, a generated program per cell type with every
//!   operation and order, the worker sources, and world and IR mutants;
//! - the stack-bound report of `src/stack_bound.rs` (with the shared Silicon
//!   analysis, the eBPF kernel rules and `render`) over generated call
//!   graphs of both origins.
#[path = "common/check_harness.rs"]
mod check_harness;
#[path = "common/stack_words.rs"]
mod stack_words;

use check_harness::*;
use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::OnceLock;
use vesper::ast::*;
use vesper::check::World;
use vesper::stack_bound::{
    GraphOrigin, StackBoundPolicy, StackBudget, StackExternal, StackFn, StackGraph,
};

const PACKAGE: &str = "packages/vesper-check-loops-atomics";

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load(PACKAGE))
}

// ------------------------------------------------------------ AST encoding

/// Writes function bodies for `ast.ves` (`read_expr`, `read_pat`,
/// `read_fn`): every node in pre-order as kind, start, end, then its words.
struct AstEnc<'a> {
    enc: &'a mut Enc,
    out: &'a mut Vec<u64>,
    /// The checked world, for what the checker decides from declarations:
    /// region blocks, handlers it skips and closures' expected results.
    world: Option<&'a World>,
}

impl AstEnc<'_> {
    fn w(&mut self, x: u64) {
        self.out.push(x);
    }
    fn name(&mut self, s: &str) {
        let i = self.enc.name(s);
        self.w(i);
    }
    fn ty(&mut self, t: &Type) {
        let i = self.enc.ty(t);
        self.w(i);
    }

    fn pat(&mut self, p: &Pattern) {
        let kind = match &p.kind {
            PatternKind::Wild => 0,
            PatternKind::Bind(_) => 1,
            PatternKind::Literal(_) => 2,
            PatternKind::Tuple(_) => 3,
            PatternKind::Constructor { .. } => 4,
        };
        self.w(kind);
        self.w(p.span.start as u64);
        self.w(p.span.end as u64);
        match &p.kind {
            PatternKind::Bind(n) => self.name(n),
            PatternKind::Tuple(ps) => {
                self.w(ps.len() as u64);
                for q in ps {
                    self.pat(q);
                }
            }
            PatternKind::Constructor { name, fields, .. } => {
                self.name(name);
                self.w(fields.len() as u64);
                for (f, q) in fields {
                    self.name(f);
                    self.pat(q);
                }
            }
            PatternKind::Wild | PatternKind::Literal(_) => {}
        }
    }

    /// Whether a `Handle` is a region block, as the checker's
    /// `region_block` decides.
    fn region(&self, effect: &Effect, ret: &Clause) -> bool {
        self.world.is_some_and(|w| {
            effect.name == vesper::regions::REGION_EFFECT
                && vesper::regions::active(w)
                && vesper::regions::block_capacity(effect, ret).is_some()
        })
    }

    /// Whether `Checker::handle` returns before checking the handler: a
    /// capability effect, a hand-written `Region` handler (E0797) or an
    /// unknown effect (E0400).
    fn handle_skipped(&self, effect: &Effect, ret: &Clause) -> bool {
        self.world.is_some_and(|w| {
            vesper::capability_effects::is_capability_effect(&effect.name)
                || (effect.name == vesper::regions::REGION_EFFECT
                    && vesper::regions::active(w)
                    && vesper::regions::block_capacity(effect, ret).is_none())
                || !w.effects.contains_key(&effect.name)
        })
    }

    /// Whether `Checker::handle` checks an operation clause's body: it
    /// skips a clause for an operation the effect does not declare (E0405).
    fn clause_checked(&self, effect: &Effect, op: &str) -> bool {
        self.world.is_none_or(|w| {
            w.effects
                .get(&effect.name)
                .is_none_or(|d| d.ops.iter().any(|f| f.name == op))
        })
    }

    /// The function type a call argument is checked against, where it is
    /// known without inference: a parameter of a non-generic world function
    /// called by name, not `inout` and without projections.
    fn arg_expected(&self, callee: &Expr, types: &[Type], i: usize) -> Option<Type> {
        let ExprKind::Name(n) = &callee.kind else {
            return None;
        };
        let f = self.world?.functions.get(n)?;
        let p = f.params.get(i)?;
        (f.generics.is_empty()
            && types.is_empty()
            && p.pass != Pass::Inout
            && p.projections.is_empty()
            && matches!(p.ty, Type::Function { .. }))
        .then(|| p.ty.clone())
    }

    fn expr_with(&mut self, e: &Expr) {
        self.expr_hinted(e, None);
    }

    /// `hint`: the type the checker passes as the expected type of `e`,
    /// where the encoder knows it (an annotated `let`, a call argument);
    /// only a closure reads it, for the result its function frame expects.
    fn expr_hinted(&mut self, e: &Expr, hint: Option<&Type>) {
        let kind = match &e.kind {
            ExprKind::Literal(_) => 0,
            ExprKind::Name(_) => 1,
            ExprKind::Tuple(_) => 2,
            ExprKind::Struct { .. } => 3,
            ExprKind::Block { .. } => 4,
            ExprKind::Call { .. } => 5,
            ExprKind::Field(..) => 6,
            ExprKind::Try(_) => 7,
            ExprKind::Unary(..) => 8,
            ExprKind::Index(..) => 9,
            ExprKind::Binary(..) => 10,
            ExprKind::Assign(..) => 11,
            ExprKind::If { .. } => 12,
            ExprKind::Match { .. } => 13,
            ExprKind::Closure { .. } => 14,
            ExprKind::Handle { .. } => 15,
            ExprKind::Loop(_) => 16,
            ExprKind::For { .. } => 17,
            ExprKind::Range { .. } => 18,
            ExprKind::Break(_) => 19,
            ExprKind::Continue => 20,
            ExprKind::Return(_) => 21,
        };
        self.w(kind);
        self.w(e.span.start as u64);
        self.w(e.span.end as u64);
        let sub = |s: &mut Self, x: &Expr| s.expr_with(x);
        match &e.kind {
            ExprKind::Literal(_) | ExprKind::Continue => {}
            ExprKind::Name(n) => self.name(n),
            ExprKind::Tuple(es) => {
                self.w(es.len() as u64);
                for x in es {
                    sub(self, x);
                }
            }
            ExprKind::Struct { name, fields } => {
                self.name(name);
                self.w(fields.len() as u64);
                for (_, x) in fields {
                    sub(self, x);
                }
            }
            ExprKind::Block { statements, tail } => {
                self.w(statements.len() as u64);
                for s in statements {
                    match s {
                        Statement::Let { pat, ty, value, .. } => {
                            self.w(0);
                            self.pat(pat);
                            self.ty(ty);
                            self.expr_hinted(value, Some(ty));
                        }
                        Statement::Expr(x) => {
                            self.w(1);
                            sub(self, x);
                        }
                    }
                }
                match tail {
                    Some(t) => {
                        self.w(1);
                        sub(self, t);
                    }
                    None => self.w(0),
                }
            }
            ExprKind::Call {
                callee,
                types,
                args,
            } => {
                sub(self, callee);
                self.w(types.len() as u64);
                for t in types {
                    self.ty(t);
                }
                self.w(args.len() as u64);
                for (i, a) in args.iter().enumerate() {
                    self.w(pass_word(a.pass));
                    let hint = self.arg_expected(callee, types, i);
                    self.expr_hinted(&a.expr, hint.as_ref());
                }
            }
            ExprKind::Field(x, n) => {
                sub(self, x);
                self.name(n);
            }
            ExprKind::Try(x) | ExprKind::Unary(_, x) | ExprKind::Loop(x) => sub(self, x),
            ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Assign(_, a, b) => {
                sub(self, a);
                sub(self, b);
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                sub(self, condition);
                sub(self, then);
                match otherwise {
                    Some(o) => {
                        self.w(1);
                        sub(self, o);
                    }
                    None => self.w(0),
                }
            }
            ExprKind::Match { value, arms } => {
                sub(self, value);
                self.w(arms.len() as u64);
                for (p, x) in arms {
                    self.pat(p);
                    sub(self, x);
                }
            }
            ExprKind::Closure {
                params,
                result,
                body,
                ..
            } => {
                self.w(params.len() as u64);
                for p in params {
                    self.ty(&p.ty);
                }
                self.ty(result);
                // The checker's closure rule: an `Infer` result takes the
                // expected function type's result.
                let expected = match hint {
                    Some(Type::Function { result: r, .. }) if *result == Type::Infer => r,
                    _ => result,
                };
                self.ty(expected);
                sub(self, body);
            }
            ExprKind::Handle {
                effect,
                state,
                body,
                ret,
                ops,
            } => {
                let flags = u64::from(state.is_some())
                    | (u64::from(self.region(effect, ret)) << 1)
                    | (u64::from(self.handle_skipped(effect, ret)) << 2);
                self.w(flags);
                if let Some((_, s)) = state {
                    sub(self, s);
                }
                sub(self, body);
                sub(self, &ret.body);
                self.w(ops.len() as u64);
                for c in ops {
                    let checked = self.clause_checked(effect, &c.name);
                    self.w(u64::from(checked));
                    sub(self, &c.body);
                }
            }
            ExprKind::For { pat, iter, body } => {
                self.pat(pat);
                sub(self, iter);
                sub(self, body);
            }
            ExprKind::Range {
                start,
                end,
                inclusive,
            } => {
                self.w(u64::from(*inclusive));
                sub(self, start);
                sub(self, end);
            }
            ExprKind::Break(v) | ExprKind::Return(v) => match v {
                Some(x) => {
                    self.w(1);
                    sub(self, x);
                }
                None => self.w(0),
            },
        }
    }

    fn function(&mut self, f: &Function) {
        self.name(&f.name);
        self.w(f.generics.len() as u64);
        // Whether the row may run on a `parallel_for` worker, as
        // `atomics::worker_row` decides from the world's effects.
        self.w(u64::from(match self.world {
            Some(w) => vesper::atomics::worker_row(w, &f.effects),
            None => f.effects.instances.is_empty() && f.effects.tail.is_none(),
        }));
        self.w(f.params.len() as u64);
        for p in &f.params {
            self.w(pass_word(p.pass));
            self.ty(&p.ty);
        }
        self.ty(&f.result);
        match &f.body {
            Some(b) => {
                self.w(1);
                self.expr_with(b);
            }
            None => self.w(0),
        }
    }
}

/// The world's functions, in `world.functions` order (`read_fns`), with
/// `Handle`s marked as region blocks the way the checker marks them.
fn world_functions(enc: &mut Enc, out: &mut Vec<u64>, w: &World) {
    let mut a = AstEnc {
        enc,
        out,
        world: Some(w),
    };
    a.w(w.functions.len() as u64);
    for f in w.functions.values() {
        a.function(f);
    }
}

/// The module items (`read_items` in atomics.ves).
fn module_items(enc: &mut Enc, out: &mut Vec<u64>, m: &Module) {
    let mut a = AstEnc {
        enc,
        out,
        world: None,
    };
    a.w(m.items.len() as u64);
    for item in &m.items {
        let fns = |a: &mut AstEnc, fs: &[Function]| {
            a.w(fs.len() as u64);
            for f in fs {
                a.function(f);
            }
        };
        match item {
            Item::Adt(adt) => {
                a.w(0);
                a.name(&adt.name);
                let fields: Vec<&Type> = adt
                    .variants
                    .iter()
                    .flat_map(|v| &v.fields)
                    .map(|f| &f.ty)
                    .collect();
                a.w(fields.len() as u64);
                for t in fields {
                    a.ty(t);
                }
            }
            Item::Function(f) => {
                a.w(1);
                a.function(f);
            }
            Item::Effect(e) => {
                a.w(2);
                fns(&mut a, &e.ops);
            }
            Item::Impl { ty, methods, .. } => {
                a.w(3);
                a.ty(ty);
                fns(&mut a, methods);
            }
            Item::Trait(t) => {
                a.w(4);
                fns(&mut a, &t.methods);
            }
            Item::Conformance(c) => {
                a.w(5);
                fns(&mut a, &c.methods);
            }
        }
    }
}

/// The Place IR `check_program` reads (`read_ir`): per function its locals,
/// parameters and value types, and its `Read`, `Write` and `Project`
/// instructions (the only ones the rule looks at).
fn ir_section(enc: &mut Enc, out: &mut Vec<u64>, program: Option<&vesper::ir::Program>) {
    use vesper::ir::Op;
    let Some(p) = program else {
        out.push(0);
        return;
    };
    out.push(p.functions.len() as u64);
    for f in p.functions.values() {
        out.push(enc.name(&f.name));
        out.push(f.locals.len() as u64);
        for (n, t) in &f.locals {
            out.push(enc.name(n));
            out.push(enc.ty(t));
        }
        out.push(f.params.len() as u64);
        for p in &f.params {
            out.push(enc.name(&p.name));
            out.push(enc.ty(&p.ty));
        }
        out.push(f.values.len() as u64);
        for (v, t) in &f.values {
            out.push(u64::from(*v));
            out.push(enc.ty(t));
        }
        let insts: Vec<_> = f
            .blocks
            .iter()
            .flat_map(|b| &b.instructions)
            .filter(|i| matches!(i.op, Op::Read { .. } | Op::Write { .. } | Op::Project(..)))
            .collect();
        out.push(insts.len() as u64);
        for i in insts {
            let span = [i.span.start as u64, i.span.end as u64];
            match &i.op {
                Op::Read { root, path, .. } => {
                    out.push(0);
                    out.extend(span);
                    out.push(enc.name(root));
                    out.push(path.first().map_or(0, |s| enc.name(s) + 1));
                }
                Op::Write { place, .. } => {
                    out.push(1);
                    out.extend(span);
                    out.push(enc.name(place));
                }
                Op::Project(v, field) => {
                    out.push(2);
                    out.extend(span);
                    out.push(u64::from(*v));
                    out.push(enc.name(field));
                }
                _ => unreachable!(),
            }
        }
    }
}

// ------------------------------------------------------------ rendering

fn render(ds: &[Diagnostic]) -> String {
    let mut s = String::new();
    for d in ds {
        writeln!(
            s,
            "{} {} {} {}",
            d.code, d.span.start, d.span.end, d.message
        )
        .unwrap();
        if let Some(h) = &d.help {
            writeln!(s, "  help: {h}").unwrap();
        }
        for l in &d.related {
            writeln!(s, "  related {} {} {}", l.span.start, l.span.end, l.message).unwrap();
        }
    }
    s
}

/// Lines sorted and unique: the checker reports in typing order and drops
/// repeats; the port walks in source order.
fn canonical(text: &str) -> String {
    let mut lines: Vec<&str> = text.lines().collect();
    lines.sort_unstable();
    lines.dedup();
    lines.iter().map(|l| format!("{l}\n")).collect()
}

// ------------------------------------------------------------ control

/// The syntactic E0213 rules. Which iterables a `for` takes is a typed
/// rule (#1243) and stays out, with its messages.
const CONTROL_MESSAGES: [&str; 13] = [
    "'break' cannot leave a handled body; return a value from the body instead",
    "'break' cannot leave a handler clause",
    "'break' cannot leave a closure",
    "'break' outside of a loop",
    "'continue' cannot leave a handled body",
    "'continue' cannot leave a handler clause",
    "'continue' cannot leave a closure",
    "'continue' outside of a loop",
    "'return' cannot leave a region block",
    "'return' cannot leave a handler clause",
    "'return' outside of a function",
    "a range is only supported as the iterable of a 'for' loop",
    "'break' in a 'for' loop takes no value",
];
const TRY_OPTION: &str = "'?' on Option[T] requires a function returning Option";

fn control_rust(ds: &[Diagnostic]) -> String {
    let kept: Vec<Diagnostic> = ds
        .iter()
        .filter(|d| {
            (d.code == "E0213" && CONTROL_MESSAGES.contains(&d.message.as_str()))
                || (d.code == "E0200" && d.message == TRY_OPTION)
        })
        .cloned()
        .collect();
    canonical(&render(&kept))
}

fn control_vesper(world: &World) -> String {
    let input = encode(world, |e, tail| world_functions(e, tail, world));
    canonical(&control_entry().text(&input))
}

fn control_entry() -> &'static Entry {
    static E: OnceLock<Entry> = OnceLock::new();
    E.get_or_init(|| package().entry("check_loops_control", true))
}

/// Fresh spans past every span of the module.
struct Fresh(usize);
impl Fresh {
    fn span(&mut self) -> Span {
        let s = self.0..self.0 + 1;
        self.0 += 2;
        s
    }
    fn expr(&mut self, kind: ExprKind) -> Expr {
        Expr {
            kind,
            span: self.span(),
        }
    }
    /// `{ s1; s2; ...; e }`.
    fn wrap(&mut self, e: &mut Expr, stmts: &[fn(&mut Fresh) -> Expr]) {
        // Each in `if true { s }`, so one jump does not end the block (the
        // checker stops a block at a statement of type `Never`).
        let statements = stmts
            .iter()
            .map(|s| {
                let jump = s(self);
                let condition = self.expr(ExprKind::Literal(Literal::Bool(true)));
                let then = self.expr(ExprKind::Block {
                    statements: vec![Statement::Expr(jump)],
                    tail: None,
                });
                Statement::Expr(self.expr(ExprKind::If {
                    condition: Box::new(condition),
                    then: Box::new(then),
                    otherwise: None,
                }))
            })
            .collect();
        let span = self.span();
        let inner = std::mem::replace(
            e,
            Expr {
                kind: ExprKind::Tuple(vec![]),
                span: span.clone(),
            },
        );
        *e = Expr {
            kind: ExprKind::Block {
                statements,
                tail: Some(Box::new(inner)),
            },
            span,
        };
    }
}

fn jump_break(f: &mut Fresh) -> Expr {
    f.expr(ExprKind::Break(None))
}
fn jump_continue(f: &mut Fresh) -> Expr {
    f.expr(ExprKind::Continue)
}
fn jump_return(f: &mut Fresh) -> Expr {
    f.expr(ExprKind::Return(None))
}
fn break_value(f: &mut Fresh) -> Expr {
    let v = f.expr(ExprKind::Literal(Literal::Int(0)));
    f.expr(ExprKind::Break(Some(Box::new(v))))
}
fn stray_range(f: &mut Fresh) -> Expr {
    let a = f.expr(ExprKind::Literal(Literal::Int(0)));
    let b = f.expr(ExprKind::Literal(Literal::Int(1)));
    f.expr(ExprKind::Range {
        start: Box::new(a),
        end: Box::new(b),
        inclusive: false,
    })
}
/// `|| { break; continue; }` inside a loop: the closure starts afresh.
fn closure_jumps(f: &mut Fresh) -> Expr {
    let mut body = f.expr(ExprKind::Tuple(vec![]));
    f.wrap(&mut body, &[jump_break, jump_continue]);
    f.expr(ExprKind::Closure {
        params: vec![],
        mode: None,
        result: Type::Infer,
        effects: Effects::default(),
        infer_effects: true,
        body: Box::new(body),
    })
}

/// The direct subexpressions of `e`, mutably.
fn children(e: &mut Expr) -> Vec<&mut Expr> {
    match &mut e.kind {
        ExprKind::Literal(_) | ExprKind::Name(_) | ExprKind::Continue => vec![],
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
        ExprKind::Field(e, _) | ExprKind::Try(e) | ExprKind::Unary(_, e) | ExprKind::Loop(e) => {
            vec![&mut **e]
        }
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
        ExprKind::For { iter, body, .. } => vec![&mut **iter, &mut **body],
        ExprKind::Range { start, end, .. } => vec![&mut **start, &mut **end],
        ExprKind::Break(v) | ExprKind::Return(v) => v.iter_mut().map(|e| &mut **e).collect(),
    }
}

/// Mutant 1 adds jumps where each rule fires; mutant 2 makes every `for`
/// iterate a non-range.
fn control_mutate(e: &mut Expr, m: usize, fresh: &mut Fresh) {
    for c in children(e) {
        control_mutate(c, m, fresh);
    }
    match (&mut e.kind, m) {
        (ExprKind::Closure { body, .. }, 1) => {
            fresh.wrap(body, &[jump_break, jump_continue, jump_return]);
        }
        (ExprKind::Handle { body, ret, ops, .. }, 1) => {
            fresh.wrap(body, &[jump_break, jump_continue, jump_return]);
            fresh.wrap(&mut ret.body, &[jump_break, jump_continue, jump_return]);
            for c in ops {
                fresh.wrap(&mut c.body, &[jump_break, jump_continue, jump_return]);
            }
        }
        (ExprKind::For { body, .. }, 1) => fresh.wrap(body, &[break_value]),
        (ExprKind::Loop(body), 1) => fresh.wrap(body, &[closure_jumps, stray_range]),
        (ExprKind::For { iter, .. }, 2) => {
            if let ExprKind::Range { start, .. } = &mut iter.kind {
                let start = std::mem::replace(
                    &mut **start,
                    Expr {
                        kind: ExprKind::Tuple(vec![]),
                        span: 0..0,
                    },
                );
                **iter = start;
            }
        }
        _ => {}
    }
}

fn functions_mut(m: &mut Module) -> Vec<&mut Function> {
    let mut out = vec![];
    for item in &mut m.items {
        match item {
            Item::Function(f) => out.push(&mut **f),
            Item::Impl { methods, .. } => out.extend(methods.iter_mut()),
            Item::Trait(t) => out.extend(t.methods.iter_mut()),
            Item::Conformance(c) => out.extend(c.methods.iter_mut()),
            Item::Adt(_) | Item::Effect(_) => {}
        }
    }
    out
}

fn max_end(e: &mut Expr) -> usize {
    let own = e.span.end;
    children(e).into_iter().map(max_end).fold(own, usize::max)
}

/// Past every span of the module's items and bodies.
fn module_end(m: &mut Module) -> usize {
    let mut end = 0;
    for item in &mut m.items {
        match item {
            Item::Adt(a) => end = end.max(a.span.end),
            Item::Effect(e) => end = end.max(e.span.end),
            _ => {}
        }
    }
    for f in functions_mut(m) {
        end = end.max(f.span.end);
        if let Some(b) = &mut f.body {
            end = end.max(max_end(b));
        }
    }
    end
}

fn control_mutant(module: &Module, m: usize) -> Module {
    let mut module = module.clone();
    let end = module_end(&mut module);
    let mut fresh = Fresh(end + 1000);
    for f in functions_mut(&mut module) {
        if let Some(body) = &mut f.body {
            control_mutate(body, m, &mut fresh);
            if m == 1 {
                fresh.wrap(body, &[jump_break, jump_continue, stray_range]);
            }
        }
    }
    module
}

/// Checks `module` (catching a checker panic) and returns both texts.
fn control_case(module: &Module) -> Option<(String, String)> {
    let quiet = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let checked = std::panic::catch_unwind(|| vesper::check::check(module));
    std::panic::set_hook(quiet);
    let checked = checked.ok()?;
    Some((
        control_rust(&checked.diagnostics),
        control_vesper(&checked.world),
    ))
}

fn source_module(src: &str) -> Module {
    let p = vesper::parser::parse(src);
    assert!(p.diagnostics.is_empty(), "{src}\n{:?}", p.diagnostics);
    p.module
}

/// The loop fixtures of the checker tests and `?` sources of
/// tests/try_option.rs, plus sources for each control rule.
fn control_sources() -> Vec<(String, String)> {
    let mut out = vec![];
    let dir = repo().join("tests/fixtures/loops/reject");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    paths.sort();
    for p in paths {
        out.push((
            p.display().to_string(),
            std::fs::read_to_string(&p).unwrap(),
        ));
    }
    let num = "fn num(in s: Slice[u8], i: u64) -> Option[u64] { match s.try_get(i) { Option::Some(b) => Option::Some(widen_u64(b)), Option::None => Option::None } }\n";
    for (name, src) in [
        (
            "try in u64 function",
            format!("{num}fn f(in s: Slice[u8]) -> u64 {{ num(in s, 0)? }}\nfn main() -> u64 {{ 0 }}"),
        ),
        (
            "try in Option function",
            format!("{num}fn f(in s: Slice[u8]) -> Option[u64] {{ let x = num(in s, 0)?; Option::Some(x) }}\nfn main() -> u64 {{ 0 }}"),
        ),
        (
            "try mixed",
            format!("{num}fn f(in s: Slice[u8]) -> Option[u64] {{ let x = num(in s, 0)?; Option::Some(x) }}\nfn g(in s: Slice[u8]) -> u64 {{ num(in s, 1)? }}\nfn main() -> u64 {{ 0 }}"),
        ),
        (
            "try in closure",
            format!("{num}fn f(in s: Slice[u8]) -> u64 {{ let c = |x: u64| -> u64 {{ num(in s, x)? }}; c(0) }}\nfn main() -> u64 {{ 0 }}"),
        ),
        (
            "try in a loop",
            format!("{num}fn f(in s: Slice[u8], n: u64) -> u64 {{ let mut t: u64 = 0; for i in 0..n {{ t = wrapping_add(t, num(in s, i)?); }} t }}\nfn main() -> u64 {{ 0 }}"),
        ),
        (
            "break outside",
            "fn main() -> i64 {\n    break;\n    0\n}".to_string(),
        ),
        (
            "continue outside",
            "fn main() -> i64 {\n    continue;\n    0\n}".to_string(),
        ),
        (
            "jumps",
            "effect Ask {\n    fn ask() -> i64;\n}\nfn main() -> i64 {\n    for i in 0..3 {\n        break 1;\n    }\n    loop {\n        let c = || {\n            break;\n        };\n        let v = handle Ask in {\n            continue;\n            Ask::ask()\n        } with {\n            return value => {\n                break;\n                value\n            },\n            ask(resume) => {\n                continue;\n                resume(1)\n            },\n        };\n    }\n    0\n}".to_string(),
        ),
        (
            "returns",
            "effect Ask {\n    fn ask() -> i64;\n}\nfn main() -> i64 {\n    handle Ask in {\n        return 1;\n    } with {\n        return value => {\n            return value;\n        },\n        ask(resume) => {\n            return 2;\n        },\n    }\n}".to_string(),
        ),
        (
            "non-range for",
            "fn main() -> i64 { let n: i64 = 3; for i in n { break; } 0 }".to_string(),
        ),
        (
            "try in an annotated closure",
            "fn f() -> u64 { let c: fn(u64) -> u64 = |x| { let v: Option[u64] = Option::Some(x); v? }; c(1) }\nfn main() -> u64 { 0 }".to_string(),
        ),
        (
            "try in an Option closure",
            "fn f() -> u64 { let c: fn(u64) -> Option[u64] = |x| { let v: Option[u64] = Option::Some(x); let y = v?; Option::Some(y) }; 0 }\nfn main() -> u64 { 0 }".to_string(),
        ),
        (
            "try in a closure argument",
            "fn apply(sink c: fn(u64) -> u64) -> u64 { c(1) }\nfn f() -> u64 { apply(|x| { let v: Option[u64] = Option::Some(x); v? }) }\nfn main() -> u64 { 0 }".to_string(),
        ),
        (
            "undeclared operation clause",
            "effect Ask {\n    fn ask() -> i64;\n}\nfn main() -> i64 {\n    handle Ask in {\n        Ask::ask()\n    } with {\n        return value => value,\n        ask(resume) => resume(1),\n        missing(resume) => {\n            return 0;\n        },\n    }\n}".to_string(),
        ),
        (
            "unknown handled effect",
            "fn main() -> i64 {\n    handle Missing in {\n        break;\n        0\n    } with {\n        return value => {\n            continue;\n            value\n        },\n    }\n}".to_string(),
        ),
    ] {
        out.push((name.to_string(), src));
    }
    out
}

#[test]
fn control_rules_match_rust() {
    let mut bad = 0;
    let mut lines = 0;
    let mut worlds = 0;
    let mut skipped = 0;
    let mut check = |name: &str, module: &Module| match control_case(module) {
        Some((r, v)) => {
            worlds += 1;
            lines += r.lines().count();
            if let Some(d) = first_difference(&r, &v) {
                bad += 1;
                if bad <= 8 {
                    eprintln!("control: {name} differs at {d}");
                }
            }
        }
        None => skipped += 1,
    };
    for (name, src) in control_sources() {
        let m = source_module(&src);
        check(&name, &m);
        check(&format!("{name}#m1"), &control_mutant(&m, 1));
    }
    for c in corpus() {
        check(&c.name, &c.module);
        check(&format!("{}#m1", c.name), &control_mutant(&c.module, 1));
        check(&format!("{}#m2", c.name), &control_mutant(&c.module, 2));
    }
    eprintln!(
        "control: {bad} of {worlds} worlds differ ({lines} diagnostic lines, {skipped} checker panics skipped)"
    );
    assert_eq!(bad, 0);
    assert!(
        lines > 1000,
        "the mutants must make the rules fire: {lines}"
    );
}

// ------------------------------------------------------------ atomics

fn atomics_entry() -> &'static Entry {
    static E: OnceLock<Entry> = OnceLock::new();
    E.get_or_init(|| package().entry("check_atomics", true))
}

fn atomics_rust(module: &Module, world: &World, program: Option<&vesper::ir::Program>) -> String {
    let mut s = format!(
        "prelude {} workers {}\n",
        u8::from(vesper::atomics::needs_prelude(module)),
        u8::from(vesper::atomics::needs_workers(module))
    );
    s.push_str(&render(&vesper::atomics::check_world(world)));
    if let Some(p) = program {
        s.push_str(&render(&vesper::atomics::check_program(world, p)));
    }
    s
}

fn atomics_vesper(module: &Module, world: &World, program: Option<&vesper::ir::Program>) -> String {
    let input = encode(world, |e, tail| {
        tail.push(u64::from(world.atomic_prelude));
        tail.push(u64::from(world.workers_prelude));
        world_functions(e, tail, world);
        module_items(e, tail, module);
        ir_section(e, tail, program);
    });
    atomics_entry().text(&input)
}

fn lower(world: &World) -> Option<vesper::ir::Program> {
    let quiet = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let p = std::panic::catch_unwind(|| vesper::ir::lower_world(world)).ok();
    std::panic::set_hook(quiet);
    p
}

const KINDS: [(&str, &str); 14] = [
    ("u8", "1"),
    ("u16", "1"),
    ("u32", "1"),
    ("u64", "1"),
    ("usize", "1"),
    ("i8", "1"),
    ("i16", "1"),
    ("i32", "1"),
    ("i64", "1"),
    ("isize", "1"),
    ("bool", "true"),
    ("f64", "1.0"),
    ("(u64, u64)", "(1, 1)"),
    ("Option[u64]", "Option::Some(1)"),
];

/// A program per cell type with every operation in every order, both call
/// forms, a non-literal order, `into_inner`, and a struct literal.
fn atomic_sources() -> Vec<(String, String)> {
    let mut out = vec![];
    for (t, v) in KINDS {
        let mut body = format!("    let c: Atomic[{t}] = Atomic::new({v});\n");
        for o in vesper::atomics::ORDERS {
            let o = format!("MemoryOrder::{o}");
            writeln!(body, "    let l = c.load({o});").unwrap();
            writeln!(body, "    c.store({v}, {o});").unwrap();
            writeln!(body, "    let s = c.swap({v}, {o});").unwrap();
            for op in [
                "fetch_add",
                "fetch_sub",
                "fetch_and",
                "fetch_or",
                "fetch_xor",
            ] {
                writeln!(body, "    let x = c.{op}({v}, {o});").unwrap();
            }
            for p in vesper::atomics::ORDERS {
                writeln!(
                    body,
                    "    let r = c.compare_exchange({v}, {v}, {o}, MemoryOrder::{p});"
                )
                .unwrap();
            }
            writeln!(body, "    let p = Atomic::load(in c, {o});").unwrap();
            writeln!(body, "    let q = Atomic::fetch_add(in c, {v}, {o});").unwrap();
        }
        body.push_str("    let o = MemoryOrder::Relaxed;\n    let n = c.load(o);\n");
        body.push_str("    let i = c.into_inner();\n    0\n");
        out.push((
            format!("cell {t}"),
            format!("fn main() -> u64 {{\n{body}}}\n"),
        ));
    }
    out.push((
        "struct literal".into(),
        "fn main() -> u64 { let c = Atomic { cell: ChunkSeq::new[u64]() }; c.load(MemoryOrder::SeqCst) }".into(),
    ));
    out.push((
        "order in a let annotation only".into(),
        "fn f(o: u64) -> u64 { let order: MemoryOrder = o; 0 }\nfn main() -> u64 { 0 }".into(),
    ));
    out.push((
        "private cell".into(),
        "fn main() -> u64 { let c: Atomic[u64] = Atomic::new(1); let n = c.cell.len(); n }".into(),
    ));
    let shared = "struct Shared { hits: Atomic[u64] }\n";
    for (name, src) in [
        (
            "workers",
            "fn body(in s: Shared, i: u64) { let x = s.hits.fetch_add(i, MemoryOrder::Relaxed); }\nfn main() -> u64 { let s = Shared { hits: Atomic::new(0) }; parallel_for(in s, 8, body); s.hits.load(MemoryOrder::Acquire) }",
        ),
        (
            "inout data",
            "fn body(in s: Shared, i: u64) { }\nfn main() -> u64 { let mut s = Shared { hits: Atomic::new(0) }; parallel_for(inout s, 8, body); 0 }",
        ),
        (
            "inout body",
            "fn body(inout s: Shared, i: u64) { }\nfn main() -> u64 { let s = Shared { hits: Atomic::new(0) }; parallel_for(in s, 8, body); 0 }",
        ),
        (
            "closure body",
            "fn main() -> u64 { let s = Shared { hits: Atomic::new(0) }; parallel_for(in s, 8, |d: Shared, i: u64| { }); 0 }",
        ),
        (
            "generic body",
            "fn body[T](in s: T, i: u64) { }\nfn main() -> u64 { let s = Shared { hits: Atomic::new(0) }; parallel_for(in s, 8, body); 0 }",
        ),
        (
            "effectful body",
            "effect Log { fn log(x: u64); }\nfn body(in s: Shared, i: u64) eff [Log] { Log::log(i) }\nfn main() -> u64 { let s = Shared { hits: Atomic::new(0) }; parallel_for(in s, 8, body); 0 }",
        ),
        (
            "no workers",
            "fn parallel_for(n: u64) -> u64 { n }\nfn main() -> u64 { parallel_for(1) }",
        ),
        (
            "declared atomic",
            "struct Atomic { x: u64 }\nfn main() -> u64 { let a = Atomic { x: 1 }; a.x }",
        ),
    ] {
        out.push((name.into(), format!("{shared}{src}")));
    }
    out
}

/// World mutants that make each `check_world` rule fire: every literal
/// order rotated by `m` (1-4), orders made non-literal (5), `Atomic::new`
/// calls made struct literals (6), `parallel_for` data made `inout` (7) and
/// its body a name that is not a function (8).
fn atomic_mutant(world: &World, m: usize) -> World {
    fn walk(e: &mut Expr, m: usize) {
        for c in children(e) {
            walk(c, m);
        }
        match &mut e.kind {
            ExprKind::Name(n) if (1..=5).contains(&m) => {
                if let Some(o) = n.strip_prefix("MemoryOrder::") {
                    let orders = vesper::atomics::ORDERS;
                    if let Some(i) = orders.iter().position(|x| *x == o) {
                        *n = if m == 5 {
                            "order".into()
                        } else {
                            format!("MemoryOrder::{}", orders[(i + m) % orders.len()])
                        };
                    }
                }
            }
            ExprKind::Call { callee, args, .. } => {
                let name = match &callee.kind {
                    ExprKind::Name(n) => n.clone(),
                    _ => String::new(),
                };
                if m == 6 && name == "Atomic::new" && args.len() == 1 {
                    let value = args[0].expr.clone();
                    e.kind = ExprKind::Struct {
                        name: "Atomic".into(),
                        fields: vec![("cell".into(), value)],
                    };
                } else if name == "parallel_for" && args.len() == 3 {
                    if m == 7 {
                        args[0].pass = Pass::Inout;
                    } else if m == 8 {
                        args[2].expr.kind = ExprKind::Name("not_a_function".into());
                    }
                }
            }
            _ => {}
        }
    }
    let mut w = world.clone();
    for f in w.functions.values_mut() {
        if let Some(b) = &mut f.body {
            walk(b, m);
        }
    }
    w
}

/// The IR mutant: every read, write and projection of an `Atomic` root or
/// value names the private cell.
fn cell_mutant(world: &World, p: &vesper::ir::Program) -> vesper::ir::Program {
    use vesper::ir::Op;
    let is_cell = |t: Option<&Type>| t.and_then(vesper::atomics::cell_type).is_some();
    let mut p = p.clone();
    for f in p.functions.values_mut() {
        let locals = f.locals.clone();
        let params = f.params.clone();
        let values = f.values.clone();
        let root_type = |root: &str| {
            locals
                .get(root)
                .or_else(|| params.iter().find(|q| q.name == root).map(|q| &q.ty))
                .cloned()
        };
        for b in &mut f.blocks {
            for i in &mut b.instructions {
                match &mut i.op {
                    Op::Read { root, path, .. } if is_cell(root_type(root).as_ref()) => {
                        *path = vec!["cell".into()];
                    }
                    Op::Write { place, .. } => {
                        let root = place.split('.').next().unwrap_or_default().to_string();
                        if is_cell(root_type(&root).as_ref()) {
                            *place = format!("{root}.cell");
                        }
                    }
                    Op::Project(v, field) if is_cell(values.get(v)) => *field = "cell".into(),
                    _ => {}
                }
            }
        }
    }
    let _ = world;
    p
}

#[test]
fn atomics_rules_match_rust() {
    let mut bad = 0;
    let mut worlds = 0;
    let mut lines = 0;
    let mut compare =
        |name: &str, module: &Module, world: &World, program: Option<&vesper::ir::Program>| {
            let r = atomics_rust(module, world, program);
            let v = atomics_vesper(module, world, program);
            worlds += 1;
            lines += r.lines().count();
            if let Some(d) = first_difference(&r, &v) {
                bad += 1;
                if bad <= 8 {
                    eprintln!("atomics: {name} differs at {d}");
                }
            }
        };
    let mut cases: Vec<(String, Module, World)> = vec![];
    for (name, src) in atomic_sources() {
        let m = source_module(&src);
        let c = vesper::check::check(&m);
        cases.push((name, m, c.world));
    }
    for c in corpus() {
        cases.push((c.name.clone(), c.module.clone(), c.world.clone()));
    }
    for (name, m, w) in &cases {
        let program = lower(w);
        compare(name, m, w, program.as_ref());
        if !(w.atomic_prelude || w.workers_prelude) {
            continue;
        }
        if let Some(p) = &program {
            compare(&format!("{name}#cell"), m, w, Some(&cell_mutant(w, p)));
        }
        for k in 1..=8 {
            compare(&format!("{name}#m{k}"), m, &atomic_mutant(w, k), None);
        }
    }
    eprintln!("atomics: {bad} of {worlds} worlds differ ({lines} lines)");
    assert_eq!(bad, 0);
    let fired = cases
        .iter()
        .map(|(_, _, w)| vesper::atomics::check_world(w).len())
        .sum::<usize>();
    assert!(
        fired > 100,
        "the generated programs must make the rules fire: {fired}"
    );
}

// ------------------------------------------------------------ stack bound

/// A deterministic generator (xorshift).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

const EXTERNALS: [&str; 4] = ["libcall:Memcpy", "ext_b", "ext_a", "abort"];

fn random_graph(rng: &mut Rng) -> (StackGraph, StackBoundPolicy) {
    let n = 1 + rng.below(10) as u32;
    let origin = if rng.chance(50) {
        GraphOrigin::Ebpf
    } else {
        GraphOrigin::Cranelift
    };
    let name = |i: u32, rng: &mut Rng| {
        if rng.chance(5) {
            "dup".to_string()
        } else {
            format!("f{i}")
        }
    };
    let mut functions: Vec<StackFn> = (0..n)
        .map(|i| StackFn {
            id: i,
            name: name(i, rng),
            frame_bytes: 16 * rng.below(20) as u32,
        })
        .collect();
    if rng.chance(3) {
        let i = rng.below(u64::from(n)) as usize;
        functions[i].id += 1;
    }
    let edges = rng.below(u64::from(n) * 2);
    let mut calls = vec![];
    for _ in 0..edges {
        let a = rng.below(u64::from(n)) as u32;
        let mut b = rng.below(u64::from(n)) as u32;
        // mostly acyclic: callees after callers
        if b <= a && rng.chance(85) {
            b = (u64::from(a) + 1 + rng.below(u64::from(n))) as u32 % (n + 1);
            if b >= n {
                continue;
            }
        }
        calls.push((a, b));
    }
    if rng.chance(3) {
        calls.push((0, n + rng.below(3) as u32));
    }
    let mut indirect = vec![];
    if rng.chance(25) {
        for _ in 0..1 + rng.below(2) {
            let inst = rng.chance(50).then(|| rng.below(40) as u32);
            indirect.push((rng.below(u64::from(n)) as u32, inst));
        }
    }
    let mut external = vec![];
    if origin == GraphOrigin::Cranelift && rng.chance(40) {
        for _ in 0..1 + rng.below(3) {
            external.push(StackExternal {
                caller: rng.below(u64::from(n)) as u32,
                symbol: EXTERNALS[rng.below(4) as usize].to_string(),
                inst: rng.chance(50).then(|| rng.below(40) as u32),
            });
        }
    }
    let mut roots = vec![];
    for _ in 0..1 + rng.below(3) {
        let function = if rng.chance(8) {
            "missing".to_string()
        } else {
            functions[rng.below(u64::from(n)) as usize].name.clone()
        };
        roots.push(StackBudget {
            function,
            budget_bytes: 64 * rng.below(12) as u32,
        });
    }
    let mut external_bounds = BTreeMap::new();
    for e in EXTERNALS {
        if rng.chance(50) {
            external_bounds.insert(e.to_string(), 8 * rng.below(64) as u32);
        }
    }
    let mut indirect_targets = BTreeMap::new();
    for (c, _) in &indirect {
        if rng.chance(70) {
            let mut ts = vec![];
            for _ in 0..1 + rng.below(3) {
                ts.push(match rng.below(3) {
                    0 => functions[rng.below(u64::from(n)) as usize].name.clone(),
                    1 => EXTERNALS[rng.below(4) as usize].to_string(),
                    _ => "nowhere".to_string(),
                });
            }
            indirect_targets.insert(functions[*c as usize % functions.len()].name.clone(), ts);
        }
    }
    (
        StackGraph {
            origin,
            functions,
            calls,
            indirect,
            external,
        },
        StackBoundPolicy {
            roots,
            external_bounds,
            indirect_targets,
        },
    )
}

fn stack_rust(g: &StackGraph, p: &StackBoundPolicy) -> String {
    let report = match vesper::stack_bound::stack_bound_report(g, p) {
        Ok(r) => r,
        Err(e) => return format!("error: {e}\n"),
    };
    let mut s = String::new();
    for r in &report.roots {
        let worst = r
            .worst_case_bytes
            .map_or("-".to_string(), |w| w.to_string());
        writeln!(
            s,
            "root {} budget {} worst {worst} within {} path {}",
            r.root,
            r.budget_bytes,
            u8::from(r.within_budget),
            r.worst_path.join(",")
        )
        .unwrap();
    }
    for f in &report.findings {
        let inst = f.inst.map_or("-".to_string(), |i| i.to_string());
        writeln!(
            s,
            "finding {} inst {inst} {}: {}",
            f.function, f.rule, f.message
        )
        .unwrap();
    }
    s.push_str(&vesper::stack_bound::render(&report));
    s.push_str(if report.accepted() {
        "accepted\n"
    } else {
        "rejected\n"
    });
    s
}

fn stack_input(g: &StackGraph, p: &StackBoundPolicy) -> Vec<u8> {
    let world = World::default();
    encode(&world, |e, t| {
        t.push(u64::from(g.origin == GraphOrigin::Ebpf));
        t.push(g.functions.len() as u64);
        for f in &g.functions {
            t.extend([u64::from(f.id), e.name(&f.name), u64::from(f.frame_bytes)]);
        }
        t.push(g.calls.len() as u64);
        for (a, b) in &g.calls {
            t.extend([u64::from(*a), u64::from(*b)]);
        }
        t.push(g.indirect.len() as u64);
        for (c, inst) in &g.indirect {
            t.extend([u64::from(*c), inst.map_or(0, |i| u64::from(i) + 1)]);
        }
        t.push(g.external.len() as u64);
        for x in &g.external {
            let s = e.name(&x.symbol);
            t.extend([
                u64::from(x.caller),
                s,
                x.inst.map_or(0, |i| u64::from(i) + 1),
            ]);
        }
        t.push(p.roots.len() as u64);
        for r in &p.roots {
            let n = e.name(&r.function);
            t.extend([n, u64::from(r.budget_bytes)]);
        }
        t.push(p.external_bounds.len() as u64);
        for (s, b) in &p.external_bounds {
            let n = e.name(s);
            t.extend([n, u64::from(*b)]);
        }
        t.push(p.indirect_targets.len() as u64);
        for (c, ts) in &p.indirect_targets {
            let n = e.name(c);
            t.extend([n, ts.len() as u64]);
            for x in ts {
                let n = e.name(x);
                t.push(n);
            }
        }
    })
}

fn stack_entry() -> &'static Entry {
    static E: OnceLock<Entry> = OnceLock::new();
    E.get_or_init(|| package().entry("check_stack_bound", true))
}

fn stack_cases() -> Vec<(StackGraph, StackBoundPolicy)> {
    let mut rng = Rng(0x9e3779b97f4a7c15);
    (0..4000).map(|_| random_graph(&mut rng)).collect()
}

fn stack_words_entry() -> &'static Entry {
    static E: OnceLock<Entry> = OnceLock::new();
    E.get_or_init(|| package().entry("check_stack_words", true))
}

/// The hybrid build's path (crates/vesper-hybrid/modules/stack_bound): the
/// input of tests/common/stack_words.rs and the decoded `report_words`
/// stream must give exactly `rust_stack_bound_report`'s report, error or
/// not, on every generated graph.
#[test]
fn stack_bound_words_match_rust() {
    let mut bad = 0;
    let cases = stack_cases();
    for (i, (g, p)) in cases.iter().enumerate() {
        let rust = vesper::stack_bound::rust_stack_bound_report(g, p);
        let words = stack_words_entry().words(&stack_words::stack_input(g, p));
        let vesper = stack_words::decode_stack_report(g.origin, &words);
        if vesper != rust {
            bad += 1;
            if bad <= 4 {
                eprintln!("stack words: graph {i}\n  rust:   {rust:?}\n  vesper: {vesper:?}");
            }
        }
    }
    eprintln!("stack words: {bad} of {} graphs differ", cases.len());
    assert_eq!(bad, 0);
}

#[test]
fn stack_bound_matches_rust() {
    let mut bad = 0;
    let mut kinds = BTreeMap::<String, usize>::new();
    let cases = stack_cases();
    for (i, (g, p)) in cases.iter().enumerate() {
        let r = stack_rust(g, p);
        let v = stack_entry().text(&stack_input(g, p));
        for l in r.lines() {
            let key = if let Some(rest) = l.strip_prefix("finding ") {
                rest.split(' ').nth(3).unwrap_or("").to_string()
            } else {
                l.split(' ').next().unwrap_or("").to_string()
            };
            *kinds.entry(key).or_default() += 1;
        }
        if let Some(d) = first_difference(&r, &v) {
            bad += 1;
            if bad <= 8 {
                eprintln!("stack: graph {i} differs at {d}\n{g:?}\n{p:?}");
            }
        }
    }
    eprintln!(
        "stack: {bad} of {} graphs differ; lines by kind {kinds:?}",
        cases.len()
    );
    assert_eq!(bad, 0);
    for rule in [
        "silicon.stack.recursion:",
        "silicon.stack.unresolved_indirect_call:",
        "silicon.stack.unbounded_external_call:",
        "silicon.stack.budget_exceeded:",
        "silicon.stack.unknown_root:",
        "silicon.stack.unknown_indirect_target:",
        "ebpf.stack.kernel_limit:",
        "error:",
        "accepted",
    ] {
        assert!(
            kinds.get(rule).copied().unwrap_or(0) > 0,
            "{rule} never fired: {kinds:?}"
        );
    }
}

// ------------------------------------------------------------ timing

/// ABBA timing of each task against the Rust code it replaces
/// (`CHECK_HARNESS_SPEED=1`; ignored by default). Vesper times include
/// decoding the input, not encoding it.
#[test]
#[ignore]
fn loops_atomics_speed() {
    if !speed() {
        return;
    }
    let corpus = corpus();
    let worlds: Vec<&World> = corpus.iter().map(|c| &c.world).collect();
    // control: the Rust rules run inside the checker, so its side is the
    // whole check of the module; reported for scale only.
    let inputs: Vec<Vec<u8>> = worlds
        .iter()
        .map(|w| encode(w, |e, t| world_functions(e, t, w)))
        .collect();
    let f = control_entry();
    abba(
        "control (rust = whole check)",
        &mut || {
            time(|| {
                for c in corpus {
                    std::hint::black_box(vesper::check::check(&c.module));
                }
            })
        },
        &mut || {
            time(|| {
                for i in &inputs {
                    std::hint::black_box(f.words(i));
                }
            })
        },
    );
    let programs: Vec<Option<vesper::ir::Program>> = worlds.iter().map(|w| lower(w)).collect();
    let inputs: Vec<Vec<u8>> = corpus
        .iter()
        .zip(&programs)
        .map(|(c, p)| {
            encode(&c.world, |e, t| {
                t.push(u64::from(c.world.atomic_prelude));
                t.push(u64::from(c.world.workers_prelude));
                world_functions(e, t, &c.world);
                module_items(e, t, &c.module);
                ir_section(e, t, p.as_ref());
            })
        })
        .collect();
    let f = atomics_entry();
    abba(
        "atomics",
        &mut || {
            time(|| {
                for (c, p) in corpus.iter().zip(&programs) {
                    std::hint::black_box(vesper::atomics::needs_prelude(&c.module));
                    std::hint::black_box(vesper::atomics::needs_workers(&c.module));
                    std::hint::black_box(vesper::atomics::check_world(&c.world));
                    if let Some(p) = p {
                        std::hint::black_box(vesper::atomics::check_program(&c.world, p));
                    }
                }
            })
        },
        &mut || {
            time(|| {
                for i in &inputs {
                    std::hint::black_box(f.words(i));
                }
            })
        },
    );
    let cases = stack_cases();
    let inputs: Vec<Vec<u8>> = cases.iter().map(|(g, p)| stack_input(g, p)).collect();
    let f = stack_entry();
    abba(
        "stack bound",
        &mut || {
            time(|| {
                for (g, p) in &cases {
                    std::hint::black_box(vesper::stack_bound::stack_bound_report(g, p).ok());
                }
            })
        },
        &mut || {
            time(|| {
                for i in &inputs {
                    std::hint::black_box(f.words(i));
                }
            })
        },
    );
}
