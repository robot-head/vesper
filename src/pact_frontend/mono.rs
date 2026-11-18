//! Bounded generics by monomorphization (port wave P0, lane 5; F7a).
//!
//! A function, inherent method or conformance with trait bounds
//! (`fn max[T: Ord]`, `impl[K: Ord, V] Tree[K, V]`, `impl[T] Ord for
//! Pair[T] where T: Ord`) is checked once, generically. In the first pass a
//! call on a bounded parameter (`a.cmp(b)` with `a: T`) is discharged by the
//! bound's Pact assumption and becomes an ordinary call of the bound's
//! *stub*: a private generic function `bound$Trait$method[$Self, ...]` with
//! the trait method's signature. The checker therefore types bounded bodies
//! with the ordinary generic rules and records each call's type arguments.
//!
//! After the second rewrite, [`Elaborator::monomorphize`]:
//!
//! 1. checks every call of a bounded function: each bound, instantiated with
//!    the call's type arguments, must be provable at the call (under the
//!    caller's own bounds); a missing conformance is E0608 at the call, an
//!    ambiguous one E0609. Types whose parameters carry bounds are checked
//!    the same way wherever a function mentions them.
//! 2. starting from every unbounded function, instantiates each bounded
//!    callee at its concrete type arguments. The bound's evidence is chosen
//!    at the instance site (so a lexical `use model` there applies) and is
//!    passed down to nested instances that use the same bound. Each
//!    instance is a private clone `f$mono$<types>` whose stub calls name the
//!    selected conformance's generated function (itself instantiated when
//!    that conformance is bounded). Polymorphic recursion that would create
//!    unboundedly many instances is E0615.
//! 3. drops the generic originals and the stubs, so no backend, the Lean
//!    extractor included, ever sees a bounded generic or a dictionary.
//!
//! Every clone gets its own span range (see [`SpanMap`]), because checked
//! Worlds key expression types by span. Diagnostics are mapped back to the
//! source span they were cloned from.
use super::*;
use serde::{Serialize, de::DeserializeOwned};

/// Name prefix of bound stubs.
const STUB: &str = "bound$";
/// Instances allowed per package, and nesting depth of one instance chain.
const MAX_INSTANCES: usize = 4096;
const MAX_DEPTH: usize = 64;
/// Type nodes allowed in one instance's type arguments.
const MAX_TYPE_SIZE: usize = 256;

fn type_size(t: &Type) -> usize {
    match t {
        Type::Named(_, args) | Type::Tuple(args) => 1 + args.iter().map(type_size).sum::<usize>(),
        Type::Function { params, result, .. } => {
            1 + params.iter().map(|(_, t)| type_size(t)).sum::<usize>() + type_size(result)
        }
        _ => 1,
    }
}

// ---------------------------------------------------------------------------
// Span ranges

/// Span ranges past the end of the user's source, for builtin declarations,
/// stubs and clones, and the way back to the source span.
pub(super) struct SpanMap {
    regions: RefCell<Vec<Region>>,
    next: RefCell<usize>,
    /// Spans of the bodiless stub declarations.
    stubs: RefCell<BTreeSet<(usize, usize)>>,
}

#[derive(Clone, Copy)]
struct Region {
    start: usize,
    end: usize,
    /// Where `start` came from; `None` for builtin source.
    base: Option<usize>,
}

fn max_span_end<T: Serialize>(x: &T) -> usize {
    let mut max = 0;
    if let Ok(v) = serde_json::to_value(x) {
        visit_spans(&v, &mut |s, e| max = max.max(s).max(e));
    }
    max
}

fn min_span_start<T: Serialize>(x: &T) -> usize {
    let mut min = usize::MAX;
    if let Ok(v) = serde_json::to_value(x) {
        visit_spans(&v, &mut |s, _| min = min.min(s));
    }
    if min == usize::MAX { 0 } else { min }
}

/// AST spans serialize as `{"start": n, "end": m}`; nothing else in the AST
/// has exactly those two fields.
fn span_of(m: &serde_json::Map<String, serde_json::Value>) -> Option<(usize, usize)> {
    if m.len() != 2 {
        return None;
    }
    Some((
        m.get("start")?.as_u64()? as usize,
        m.get("end")?.as_u64()? as usize,
    ))
}

fn visit_spans(v: &serde_json::Value, f: &mut dyn FnMut(usize, usize)) {
    match v {
        serde_json::Value::Object(m) => {
            if let Some((s, e)) = span_of(m) {
                f(s, e);
            } else {
                m.values().for_each(|v| visit_spans(v, f));
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|v| visit_spans(v, f)),
        _ => {}
    }
}

fn shift_spans(v: &mut serde_json::Value, by: isize) {
    match v {
        serde_json::Value::Object(m) => {
            if let Some((s, e)) = span_of(m) {
                m.insert("start".into(), (s as isize + by).into());
                m.insert("end".into(), (e as isize + by).into());
            } else {
                m.values_mut().for_each(|v| shift_spans(v, by));
            }
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(|v| shift_spans(v, by)),
        _ => {}
    }
}

fn shifted<T: Serialize + DeserializeOwned + Clone>(x: &T, by: isize) -> T {
    let Ok(mut v) = serde_json::to_value(x) else {
        return x.clone();
    };
    shift_spans(&mut v, by);
    serde_json::from_value(v).unwrap_or_else(|_| x.clone())
}

impl SpanMap {
    pub(super) fn new(module: &Module) -> Self {
        SpanMap {
            regions: RefCell::new(vec![]),
            next: RefCell::new(max_span_end(module) + 1),
            stubs: RefCell::new(BTreeSet::new()),
        }
    }

    /// Moves `x` into a fresh range; `base` is where its spans came from
    /// (`None`: builtin source).
    fn relocate_value<T: Serialize + DeserializeOwned + Clone>(&self, x: &T, builtin: bool) -> T {
        let lo = min_span_start(x);
        let hi = max_span_end(x).max(lo);
        let start = *self.next.borrow();
        let out = shifted(x, start as isize - lo as isize);
        let end = start + (hi - lo);
        self.regions.borrow_mut().push(Region {
            start,
            end,
            base: (!builtin).then_some(lo),
        });
        *self.next.borrow_mut() = end + 1;
        out
    }

    /// `module` with the builtin trait declarations appended (in their own
    /// span range).
    /// `module` with the builtin `items` appended. A derived conformance
    /// keeps the span of the empty impl that asked for it (`owners`), so it
    /// counts as package source; its method bodies stay builtin.
    pub(super) fn with_builtins(
        &self,
        module: &Module,
        items: Vec<Item>,
        owners: &[Option<Span>],
    ) -> Module {
        let mut out = module.clone();
        let mut items = self.relocate_value(&items, true);
        for (item, owner) in items.iter_mut().zip(owners) {
            if let (Item::Conformance(c), Some(span)) = (item, owner) {
                c.span = span.clone();
            }
        }
        out.items.extend(items);
        out
    }

    /// Whether `span` lies in builtin source (prelude declarations, stubs
    /// and their clones).
    pub(super) fn is_builtin(&self, span: &Span) -> bool {
        let mut s = span.start;
        loop {
            let Some(r) = self.region(s) else {
                return false;
            };
            match r.base {
                None => return true,
                Some(b) => s = s - r.start + b,
            }
        }
    }

    fn region(&self, s: usize) -> Option<Region> {
        self.regions
            .borrow()
            .iter()
            .find(|r| r.start <= s && s <= r.end)
            .copied()
    }

    /// Maps a span inside a clone back to the source span it was cloned
    /// from (builtin source maps to `0..0`).
    fn source_span(&self, span: &Span) -> Span {
        let (mut s, mut e) = (span.start, span.end);
        while let Some(r) = self.region(s) {
            match r.base {
                None => return 0..0,
                Some(b) => {
                    let len = e.saturating_sub(s);
                    s = s - r.start + b;
                    e = s + len;
                }
            }
        }
        s..e
    }

    /// Drops the checker's "requires a body" errors for stub declarations
    /// (stubs only provide signatures and are removed before backends).
    pub(super) fn drop_stub_errors(&self, diagnostics: &mut Vec<Diagnostic>) {
        let stubs = self.stubs.borrow();
        diagnostics.retain(|d| !(d.code == "E0103" && stubs.contains(&(d.span.start, d.span.end))));
    }

    pub(super) fn relocate(&self, diagnostics: &mut [Diagnostic]) {
        for d in diagnostics {
            if self.region(d.span.start).is_some() {
                d.span = self.source_span(&d.span);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Expression walking

/// Visits every expression of `e` in pre-order, children after the parent
/// (so a visitor may rewrite a node before its children are visited).
fn each_expr(e: &mut Expr, f: &mut dyn FnMut(&mut Expr)) {
    f(e);
    match &mut e.kind {
        ExprKind::Literal(_) | ExprKind::Name(_) => {}
        ExprKind::Tuple(es) => es.iter_mut().for_each(|e| each_expr(e, f)),
        ExprKind::Struct { fields, .. } => fields.iter_mut().for_each(|(_, e)| each_expr(e, f)),
        ExprKind::Block { statements, tail } => {
            for s in statements {
                match s {
                    Statement::Let { value, .. } => each_expr(value, f),
                    Statement::Expr(e) => each_expr(e, f),
                }
            }
            if let Some(t) = tail {
                each_expr(t, f);
            }
        }
        ExprKind::Call { callee, args, .. } => {
            each_expr(callee, f);
            for a in args {
                each_expr(&mut a.expr, f);
            }
        }
        ExprKind::Field(b, _) | ExprKind::Try(b) | ExprKind::Unary(_, b) => each_expr(b, f),
        ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Assign(_, a, b) => {
            each_expr(a, f);
            each_expr(b, f);
        }
        ExprKind::If {
            condition,
            then,
            otherwise,
        } => {
            each_expr(condition, f);
            each_expr(then, f);
            if let Some(o) = otherwise {
                each_expr(o, f);
            }
        }
        ExprKind::Match { value, arms } => {
            each_expr(value, f);
            for (_, a) in arms {
                each_expr(a, f);
            }
        }
        ExprKind::Closure { body, .. } => each_expr(body, f),
        ExprKind::Handle {
            state,
            body,
            ret,
            ops,
            ..
        } => {
            if let Some((_, s)) = state {
                each_expr(s, f);
            }
            each_expr(body, f);
            each_expr(&mut ret.body, f);
            for o in ops {
                each_expr(&mut o.body, f);
            }
        }
        ExprKind::Loop(_)
        | ExprKind::For { .. }
        | ExprKind::Range { .. }
        | ExprKind::Break(_)
        | ExprKind::Return(_) => {
            for c in e.kind.control_children_mut().into_iter().flatten() {
                each_expr(c, f);
            }
        }
    }
}

fn each_body_expr(func: &mut Function, f: &mut dyn FnMut(&mut Expr)) {
    if let Some(b) = &mut func.body {
        each_expr(b, f);
    }
}

/// Names referenced by `func` (callees and values).
fn referenced_names(func: &Function) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut func = func.clone();
    each_body_expr(&mut func, &mut |e| {
        if let ExprKind::Name(n) = &e.kind {
            out.insert(n.clone());
        }
    });
    out
}

fn closed(t: &Type) -> bool {
    !has_infer(t) && !matches!(t, Type::IntLiteral(_))
}

fn substitute_function(f: &mut Function, vars: &BTreeMap<String, Type>) {
    walk_function(f, &mut |t| *t = substitute(t, vars));
    f.effects = substitute_effects(&f.effects, vars);
    let mut body_effects = |e: &mut Expr| {
        if let ExprKind::Closure { effects, .. } = &mut e.kind {
            *effects = substitute_effects(effects, vars);
        }
    };
    each_body_expr(f, &mut body_effects);
    f.generics.retain(|g| !vars.contains_key(&g.name));
}

// ---------------------------------------------------------------------------
// Stubs

/// Whether `b` is the bound of a `dyn` parameter's hidden type parameter.
fn is_dyn_bound(b: &Bound) -> bool {
    matches!(&b.ty, Type::Named(n, a) if a.is_empty() && crate::dyn_trait::is_dyn_generic(n))
}

fn stub_name(key: &TraitKey, method: &str) -> String {
    format!("{STUB}{}${method}", key.1)
}

/// One instance: origin, type arguments in generic order (as injective
/// key segments), evidence (a conformance index per bound of the origin).
type InstanceKey = (String, Vec<String>, Vec<usize>);

/// The context a body is instantiated in.
struct Ctx {
    origin: String,
    vars: BTreeMap<String, Type>,
    evidence: Vec<usize>,
    depth: usize,
}

struct Mono<'a> {
    el: &'a Elaborator,
    world: &'a World,
    /// Every checked function of the module by World name (inherent
    /// methods as `Type::m`, with the impl's generics and `self` type).
    funcs: BTreeMap<String, Function>,
    /// Functions instantiated by monomorphization: bounded functions and
    /// generic functions that pass their own parameters to one.
    bounded: BTreeSet<String>,
    instances: BTreeMap<InstanceKey, String>,
    names: BTreeSet<String>,
    clones: Vec<(String, Function)>,
    errors: Vec<Diagnostic>,
}

impl Elaborator {
    /// The static (monomorphized) bounds of `function`: a `dyn`
    /// parameter's bound is discharged by a vtable instead.
    fn bounds_of(&self, function: &str) -> Vec<Bound> {
        self.fe
            .bounds
            .get(function)
            .map(|bs| bs.iter().filter(|b| !is_dyn_bound(b)).cloned().collect())
            .unwrap_or_default()
    }

    /// Whether a call of `function` takes a `dyn` parameter.
    fn dyn_params_of(&self, function: &str) -> Option<&BTreeMap<String, Type>> {
        self.dyn_params.get(function)
    }

    /// The stub a call of `method` on a parameter bounded by `key` names.
    pub(super) fn bound_stub(
        &self,
        key: &TraitKey,
        method: &str,
        span: &Span,
    ) -> Result<String, Diagnostic> {
        let t = &self.fe.traits[key].decl;
        let Some(m) = t.methods.iter().find(|m| m.name == method) else {
            return Err(d(
                "E0608",
                format!("trait '{}' has no method '{method}'", key.1),
                span,
            ));
        };
        let mut probe = m.clone();
        let mut projection = false;
        walk_function(&mut probe, &mut |t| projection |= contains_projection(t));
        if projection {
            return Err(d(
                "E0607",
                format!(
                    "'{}::{method}' mentions an associated type; calling it on a bounded type parameter is not supported yet",
                    key.1
                ),
                span,
            ));
        }
        Ok(stub_name(key, method))
    }

    /// Stub declarations for every method of every local trait named by a
    /// bound (only their signatures matter: they never reach a World).
    pub(super) fn stubs(&self) -> Vec<Function> {
        let keys: BTreeSet<&TraitKey> = self
            .fe
            .bounds
            .values()
            .flatten()
            .filter(|b| !is_dyn_bound(b))
            .map(|b| &b.key)
            .filter(|k| k.0 == self.fe.identity)
            .collect();
        self.stub_declarations(keys, stub_name)
    }

    /// Vtable stub declarations for every trait a `dyn` parameter names
    /// ([`crate::dyn_trait`]): they stay bodiless in the World, and native
    /// code calls them through the object's vtable.
    pub(super) fn dyn_stubs(&self) -> Vec<Function> {
        let keys: BTreeSet<&TraitKey> = self
            .fe
            .bounds
            .values()
            .flatten()
            .filter(|b| is_dyn_bound(b))
            .map(|b| &b.key)
            .filter(|k| k.0 == self.fe.identity)
            .collect();
        self.stub_declarations(keys, |key, method| {
            crate::dyn_trait::stub_name(&key.1, method)
        })
    }

    fn stub_declarations(
        &self,
        keys: BTreeSet<&TraitKey>,
        name_of: impl Fn(&TraitKey, &str) -> String,
    ) -> Vec<Function> {
        let mut out = vec![];
        for key in keys {
            let t = &self.fe.traits[key].decl;
            for m in &t.methods {
                let mut probe = m.clone();
                let mut projection = false;
                walk_function(&mut probe, &mut |t| projection |= contains_projection(t));
                if projection || !m.generics.is_empty() {
                    continue;
                }
                let name = name_of(key, &m.name);
                let vars: BTreeMap<String, Type> =
                    [("Self".to_owned(), Type::named("$Self"))].into();
                let mut f = m.clone();
                f.name = name.clone();
                f.generics = std::iter::once(Generic {
                    name: "$Self".into(),
                    bound: Mode::Linear,
                    row: false,
                })
                .chain(t.generics.iter().cloned())
                .collect();
                if let Some(p) = f.params.first_mut()
                    && p.ty == Type::Infer
                {
                    p.ty = Type::named("$Self");
                }
                walk_function(&mut f, &mut |t| *t = substitute(t, &vars));
                // A declaration only: the checker types calls by the
                // signature, and stubs never reach a World.
                f.body = None;
                let f = self.spans.relocate_value(&f, true);
                self.spans
                    .stubs
                    .borrow_mut()
                    .insert((f.span.start, f.span.end));
                out.push(f);
            }
        }
        out
    }

    /// Replaces bounded generics by their instances (see the module docs).
    pub(super) fn monomorphize(
        &self,
        mut module: Module,
        world: &World,
    ) -> Result<Module, Vec<Diagnostic>> {
        let mut funcs = BTreeMap::new();
        for item in &module.items {
            match item {
                Item::Function(f) => {
                    funcs.insert(f.name.clone(), (**f).clone());
                }
                Item::Impl {
                    ty,
                    generics,
                    methods,
                } => {
                    for m in methods {
                        let mut f = m.clone();
                        f.name = format!("{}::{}", ty.name(), m.name);
                        f.generics.splice(0..0, generics.clone());
                        for p in &mut f.params {
                            if p.name == "self" && p.ty == Type::Infer {
                                p.ty = ty.clone();
                            }
                        }
                        funcs.insert(f.name.clone(), f);
                    }
                }
                _ => {}
            }
        }
        if self.fe.bounds.is_empty() && self.fe.type_bounds.is_empty() {
            return Ok(self.prune(module));
        }
        let mut m = Mono {
            el: self,
            world,
            bounded: funcs
                .keys()
                .filter(|n| !self.bounds_of(n).is_empty() || n.starts_with(STUB))
                .cloned()
                .collect(),
            funcs,
            instances: BTreeMap::new(),
            names: BTreeSet::new(),
            clones: vec![],
            errors: vec![],
        };
        m.close_bounded();
        m.check_obligations();
        if !m.errors.is_empty() {
            return Err(m.errors);
        }
        // Instantiate from every unbounded function.
        let roots: Vec<String> = m
            .funcs
            .keys()
            .filter(|n| !m.bounded.contains(*n))
            .cloned()
            .collect();
        let mut bodies = BTreeMap::new();
        for root in roots {
            let mut f = m.funcs[&root].clone();
            let cx = Ctx {
                origin: root.clone(),
                vars: BTreeMap::new(),
                evidence: vec![],
                depth: 0,
            };
            m.instantiate_body(&mut f, &cx);
            bodies.insert(root, f.body);
        }
        if !m.errors.is_empty() {
            return Err(m.errors);
        }
        let bounded = m.bounded.clone();
        let mut items = vec![];
        for mut item in std::mem::take(&mut module.items) {
            match &mut item {
                Item::Function(f) => {
                    if bounded.contains(&f.name) {
                        continue;
                    }
                    if let Some(b) = bodies.remove(&f.name) {
                        f.body = b;
                    }
                }
                Item::Impl { ty, methods, .. } => {
                    let head = ty.name().to_owned();
                    methods.retain(|f| !bounded.contains(&format!("{head}::{}", f.name)));
                    for f in methods.iter_mut() {
                        if let Some(b) = bodies.remove(&format!("{head}::{}", f.name)) {
                            f.body = b;
                        }
                    }
                }
                _ => {}
            }
            items.push(item);
        }
        items.extend(
            self.dyn_thunks
                .borrow_mut()
                .drain(..)
                .map(|f| Item::Function(Box::new(f))),
        );
        let mut instances = self.instances.borrow_mut();
        for (origin, f) in std::mem::take(&mut m.clones) {
            instances.insert(f.name.clone(), origin);
            items.push(Item::Function(Box::new(
                self.spans.relocate_value(&f, false),
            )));
        }
        drop(instances);
        module.items = items;
        Ok(self.prune(module))
    }

    /// Drops builtin functions (builtin conformances, helpers and their
    /// instances) that no package function reaches.
    fn prune(&self, mut module: Module) -> Module {
        let mut builtin: BTreeSet<String> = self
            .fe
            .confs
            .iter()
            .filter(|c| c.local && self.spans.is_builtin(&c.decl.span))
            .flat_map(|c| c.generated.values().cloned())
            .collect();
        for item in &module.items {
            if let Item::Function(f) = item
                && f.name.starts_with(builtin_traits::HELPER)
                && self.spans.is_builtin(&f.span)
            {
                builtin.insert(f.name.clone());
            }
        }
        let instances = self.instances.borrow();
        for (clone, origin) in instances.iter() {
            if builtin.contains(origin) {
                builtin.insert(clone.clone());
            }
        }
        drop(instances);
        let mut bodies: BTreeMap<String, &Function> = BTreeMap::new();
        for item in &module.items {
            match item {
                Item::Function(f) => {
                    bodies.insert(f.name.clone(), f);
                }
                Item::Impl { ty, methods, .. } => {
                    for f in methods {
                        bodies.insert(format!("{}::{}", ty.name(), f.name), f);
                    }
                }
                _ => {}
            }
        }
        let mut reached: BTreeSet<String> = BTreeSet::new();
        let mut todo: Vec<String> = bodies
            .keys()
            .filter(|n| !builtin.contains(*n))
            .cloned()
            .collect();
        while let Some(n) = todo.pop() {
            if !reached.insert(n.clone()) {
                continue;
            }
            if let Some(f) = bodies.get(&n) {
                for r in referenced_names(f) {
                    if bodies.contains_key(&r) && !reached.contains(&r) {
                        todo.push(r);
                    }
                }
            }
        }
        module.items.retain(|i| match i {
            Item::Function(f) => !builtin.contains(&f.name) || reached.contains(&f.name),
            _ => true,
        });
        module
    }

    /// The vtable slot function of method `m` of `decl` for `self_ty`
    /// (object trait arguments `args`): a private function with the trait
    /// method's signature that calls the conformance function `target`.
    /// Every slot of one method therefore has one calling convention,
    /// whatever effects the implementation leaves out.
    fn dyn_thunk(
        &self,
        decl: &TraitDecl,
        m: &Function,
        args: &[Type],
        self_ty: &Type,
        target: &str,
    ) -> String {
        let mut vars: BTreeMap<String, Type> = decl
            .generics
            .iter()
            .zip(args)
            .map(|(g, a)| (g.name.clone(), a.clone()))
            .collect();
        vars.insert("Self".into(), self_ty.clone());
        let mut thunks = self.dyn_thunks.borrow_mut();
        let mut name = format!(
            "{}${}",
            crate::dyn_trait::stub_name(&decl.name, &m.name),
            key_segment(self_ty)
        );
        if thunks.iter().any(|f| f.name == name) {
            name = format!("{name}${}", thunks.len());
        }
        let mut next = 0usize;
        let mut span = || {
            next += 1;
            next - 1..next
        };
        let mut f = m.clone();
        f.name = name.clone();
        f.generics = vec![];
        for (i, p) in f.params.iter_mut().enumerate() {
            p.ty = if i == 0 {
                self_ty.clone()
            } else {
                substitute(&p.ty, &vars)
            };
            p.span = span();
        }
        f.result = substitute(&f.result, &vars);
        f.effects = substitute_effects(&f.effects, &vars);
        let call_args = f
            .params
            .iter()
            .map(|p| Arg {
                pass: p.pass,
                explicit: true,
                expr: Expr {
                    kind: ExprKind::Name(p.name.clone()),
                    span: span(),
                },
            })
            .collect();
        let callee = Expr {
            kind: ExprKind::Name(target.to_owned()),
            span: span(),
        };
        let call = span();
        f.body = Some(Expr {
            kind: ExprKind::Call {
                callee: Box::new(callee),
                types: vec![],
                args: call_args,
            },
            span: call,
        });
        f.span = 0..next;
        thunks.push(self.spans.relocate_value(&f, true));
        name
    }

    /// The `dyn` metadata native code needs ([`crate::dyn_trait::DynObjects`]).
    pub(super) fn dyn_metadata(&self, world: &mut World) {
        if self.dyn_params.is_empty() {
            return;
        }
        let mut objects = crate::dyn_trait::DynObjects::default();
        let instances = self.instances.borrow();
        for name in world.functions.keys() {
            let origin = instances.get(name).unwrap_or(name);
            if let Some(p) = self.dyn_params.get(origin) {
                objects.params.insert(name.clone(), p.clone());
            }
        }
        for (key, t) in &self.fe.traits {
            for (slot, m) in t.decl.methods.iter().enumerate() {
                let stub = crate::dyn_trait::stub_name(&key.1, &m.name);
                if world.functions.contains_key(&stub) {
                    objects.stubs.insert(stub, (key.1.clone(), slot));
                }
            }
        }
        objects.vtables = self.dyn_vtables.borrow().clone();
        world.dyn_objects = objects;
    }

    /// Records each instance as a private generated function.
    pub(super) fn instance_metadata(&self, world: &mut World) {
        for (clone, origin) in self.instances.borrow().iter() {
            if !world.functions.contains_key(clone) {
                continue;
            }
            let (conformance, trait_name) = self
                .fe
                .confs
                .iter()
                .find(|c| c.local && c.generated.values().any(|g| g == origin))
                .map(|c| (c.path.to_string(), c.trait_key.1.clone()))
                .unwrap_or_default();
            world.elaboration.generated.insert(
                clone.clone(),
                GeneratedFunction {
                    conformance,
                    trait_name,
                    method: origin.clone(),
                    public: false,
                },
            );
        }
    }
}

impl Mono<'_> {
    /// The checked function a call expression applies, if it is a
    /// function of the module.
    fn callee(&self, e: &Expr) -> Option<String> {
        let ExprKind::Call { callee, .. } = &e.kind else {
            return None;
        };
        let name = match &callee.kind {
            ExprKind::Name(n) => n.clone(),
            ExprKind::Field(base, method) => {
                let t = self.world.types.get(&(base.span.start, base.span.end))?;
                format!("{}::{method}", t.name())
            }
            _ => return None,
        };
        (self.funcs.contains_key(&name)
            && self.world.calls.contains_key(&(e.span.start, e.span.end)))
        .then_some(name)
    }

    fn call_vars(&self, e: &Expr) -> BTreeMap<String, Type> {
        self.world
            .calls
            .get(&(e.span.start, e.span.end))
            .cloned()
            .unwrap_or_default()
    }

    /// Calls of module functions in `f`: (call span, callee, type args).
    fn calls_in(&self, f: &Function) -> Vec<(Span, String, BTreeMap<String, Type>)> {
        let mut out = vec![];
        let mut f = f.clone();
        each_body_expr(&mut f, &mut |e| {
            if let Some(c) = self.callee(e) {
                out.push((e.span.clone(), c, self.call_vars(e)));
            }
        });
        out
    }

    /// Adds generic functions that call an instantiated function with their
    /// own type parameters (they must be instantiated as well), and rejects
    /// instantiated functions used as values.
    fn close_bounded(&mut self) {
        loop {
            let mut added = vec![];
            for (name, f) in &self.funcs {
                if self.bounded.contains(name) || f.generics.is_empty() {
                    continue;
                }
                let open = self.calls_in(f).into_iter().any(|(_, c, vars)| {
                    self.bounded.contains(&c)
                        && vars
                            .iter()
                            .filter(|(k, _)| !crate::dyn_trait::is_dyn_generic(k))
                            .any(|(_, t)| {
                                f.generics.iter().any(|g| {
                                    !crate::dyn_trait::is_dyn_generic(&g.name)
                                        && mentions(t, &g.name)
                                })
                            })
                });
                if open {
                    added.push(name.clone());
                }
            }
            if added.is_empty() {
                break;
            }
            self.bounded.extend(added);
        }
        let mut errors = vec![];
        for f in self.funcs.values() {
            let mut callees = BTreeSet::new();
            let mut probe = f.clone();
            each_body_expr(&mut probe, &mut |e| {
                if let ExprKind::Call { callee, .. } = &e.kind {
                    callees.insert((callee.span.start, callee.span.end));
                }
            });
            each_body_expr(&mut probe, &mut |e| {
                if let ExprKind::Name(n) = &e.kind
                    && self.bounded.contains(n)
                    && !callees.contains(&(e.span.start, e.span.end))
                    && matches!(
                        self.world.types.get(&(e.span.start, e.span.end)),
                        Some(Type::Function { .. })
                    )
                {
                    errors.push(d(
                        "E0607",
                        format!(
                            "'{n}' has trait-bounded type parameters and cannot be used as a function value; call it directly (dictionary passing is not supported)"
                        ),
                        &e.span,
                    ));
                }
            });
        }
        self.errors.extend(errors);
    }

    /// Bounds instantiated at each call, and bounded types wherever they
    /// occur, must be provable where they appear.
    fn check_obligations(&mut self) {
        let funcs: Vec<Function> = self
            .funcs
            .iter()
            .filter(|(n, _)| !n.starts_with(STUB))
            .map(|(_, f)| f.clone())
            .collect();
        let mut seen = BTreeSet::new();
        for f in &funcs {
            for (span, callee, vars) in self.calls_in(f) {
                let vars: BTreeMap<String, Type> = vars
                    .into_iter()
                    .map(|(k, v)| (k, crate::numeric::default_type(&v)))
                    .collect();
                for b in self.el.bounds_of(&callee) {
                    let ty = substitute(&b.ty, &vars);
                    let args: Vec<Type> = b.args.iter().map(|a| substitute(a, &vars)).collect();
                    let what = format!("'{}' requires {ty}: {}", callee, b.key.1);
                    if !closed(&ty) {
                        self.errors.push(d(
                            "E0607",
                            format!("{what}, but the type argument cannot be inferred; annotate the arguments"),
                            &span,
                        ));
                        continue;
                    }
                    self.obligation(&b.key, &ty, &args, f, &span, &what, &mut seen);
                }
            }
            // Types with bounded parameters.
            let mut types = vec![];
            let mut probe = f.clone();
            walk_function(&mut probe, &mut |t| types.push(t.clone()));
            if !self.el.spans.is_builtin(&f.span) {
                types.extend(
                    self.world
                        .types
                        .range((f.span.start, 0)..=(f.span.end, usize::MAX))
                        .map(|(_, t)| t.clone()),
                );
            }
            for t in types {
                self.type_obligations(&t, f, &mut seen);
            }
        }
    }

    fn type_obligations(
        &mut self,
        t: &Type,
        f: &Function,
        seen: &mut BTreeSet<((usize, usize), String)>,
    ) {
        match t {
            Type::Named(n, args) => {
                for a in args {
                    self.type_obligations(a, f, seen);
                }
                let Some(bounds) = self.el.fe.type_bounds.get(n).cloned() else {
                    return;
                };
                for (i, b) in bounds {
                    let Some(arg) = args.get(i) else { continue };
                    let arg = crate::numeric::default_type(arg);
                    if !closed(&arg) {
                        continue;
                    }
                    let what = format!("type '{t}' requires {arg}: {}", b.key.1);
                    let vars: BTreeMap<String, Type> = self
                        .el
                        .fe
                        .base_world
                        .adts
                        .get(n)
                        .map(|a| {
                            a.generics
                                .iter()
                                .zip(args)
                                .map(|(g, a)| (g.name.clone(), a.clone()))
                                .collect()
                        })
                        .unwrap_or_default();
                    let targs: Vec<Type> = b.args.iter().map(|a| substitute(a, &vars)).collect();
                    let span = f.span.clone();
                    self.obligation(&b.key, &arg, &targs, f, &span, &what, seen);
                }
            }
            Type::Tuple(ts) => ts.iter().for_each(|a| self.type_obligations(a, f, seen)),
            Type::Function { params, result, .. } => {
                for (_, p) in params {
                    self.type_obligations(p, f, seen);
                }
                self.type_obligations(result, f, seen);
            }
            _ => {}
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "The inputs mirror one obligation and where it is reported"
    )]
    fn obligation(
        &mut self,
        key: &TraitKey,
        ty: &Type,
        args: &[Type],
        f: &Function,
        span: &Span,
        what: &str,
        seen: &mut BTreeSet<((usize, usize), String)>,
    ) {
        match self
            .el
            .select(key, ty, None, args, &f.generics, &f.name, span)
        {
            Ok(_) | Err((5, _)) => {}
            Err(e) => {
                let generic = f.generics.iter().any(|g| mentions(ty, &g.name));
                let mut diag = self.el.failure(&key.1, ty, e, span);
                diag.message = if generic && matches!(diag.code.as_str(), "E0607" | "E0608") {
                    diag.code = "E0608".into();
                    format!(
                        "{what}: {}; add the bound `{ty}: {}` to '{}'",
                        diag.message, key.1, f.name
                    )
                } else {
                    format!("{what}: {}", diag.message)
                };
                if seen.insert(((diag.span.start, diag.span.end), diag.message.clone())) {
                    self.errors.push(diag);
                }
            }
        }
    }

    /// The conformance that discharges `ty: key[args]` inside `cx`: the
    /// evidence of a matching bound of the instance's origin, otherwise the
    /// one selected at `span` (under its lexical `use model` scopes).
    fn evidence(
        &mut self,
        cx: &Ctx,
        key: &TraitKey,
        ty: &Type,
        args: &[Type],
        span: &Span,
    ) -> Option<usize> {
        for (i, b) in self.el.bounds_of(&cx.origin).iter().enumerate() {
            if b.key == *key
                && let Some(ev) = cx.evidence.get(i)
                && crate::numeric::default_type(&substitute(&b.ty, &cx.vars)) == *ty
                && b.args
                    .iter()
                    .zip(args)
                    .all(|(a, x)| same_type(&substitute(a, &cx.vars), x))
            {
                return Some(*ev);
            }
        }
        match self.el.select(key, ty, None, args, &[], "", span) {
            Ok((index, _, _)) => Some(index),
            Err(e) => {
                let diag = self.el.failure(&key.1, ty, e, span);
                self.errors.push(diag);
                None
            }
        }
    }

    /// The instance of `origin` at closed `vars` with `evidence`, created on
    /// first use.
    fn instance(
        &mut self,
        origin: &str,
        vars: BTreeMap<String, Type>,
        evidence: Vec<usize>,
        depth: usize,
        span: &Span,
    ) -> String {
        let f = self.funcs[origin].clone();
        let types: Vec<Type> = f
            .generics
            .iter()
            .filter_map(|g| vars.get(&g.name).cloned())
            .collect();
        let segments: Vec<String> = types.iter().map(key_segment).collect();
        // `key_segment` is lossy (every function type is "5"), so the cache
        // keys on the full structural identity; names stay readable and
        // collisions get a numeric suffix below.
        let identities: Vec<String> = types
            .iter()
            .map(|t| serde_json::to_string(t).unwrap_or_default())
            .collect();
        let key = (origin.to_owned(), identities, evidence.clone());
        if let Some(n) = self.instances.get(&key) {
            return n.clone();
        }
        if depth > MAX_DEPTH
            || self.instances.len() >= MAX_INSTANCES
            || types.iter().map(type_size).sum::<usize>() > MAX_TYPE_SIZE
        {
            self.errors.push(d(
                "E0615",
                format!(
                    "instantiating '{origin}' recursively creates unboundedly many instances (polymorphic recursion); monomorphization stops after {MAX_DEPTH} nested instances or type arguments of {MAX_TYPE_SIZE} nodes"
                ),
                span,
            ));
            return origin.to_owned();
        }
        let mut name = format!("{origin}$mono${}", segments.join("."));
        for ev in &evidence {
            if let ConformanceKind::Model { name: model, .. } = &self.el.fe.confs[*ev].decl.kind {
                name.push('$');
                name.push_str(model);
            }
        }
        if self.names.contains(&name) {
            name = format!("{name}${}", self.instances.len());
        }
        self.names.insert(name.clone());
        self.instances.insert(key, name.clone());
        let mut clone = f;
        substitute_function(&mut clone, &vars);
        clone.name = name.clone();
        let cx = Ctx {
            origin: origin.to_owned(),
            vars,
            evidence,
            depth,
        };
        self.instantiate_body(&mut clone, &cx);
        self.clones.push((origin.to_owned(), clone));
        name
    }

    /// Rewrites the calls of a body (of `cx.origin`, already substituted)
    /// to instances.
    fn instantiate_body(&mut self, f: &mut Function, cx: &Ctx) {
        let mut rewrites: Vec<(Span, String)> = vec![];
        // Spans of `f` are still the origin's, so the probe World's call
        // records apply.
        let origin = self.funcs[&cx.origin].clone();
        for (span, callee, vars) in self.calls_in(&origin) {
            let mut vars: BTreeMap<String, Type> = vars
                .into_iter()
                .map(|(k, v)| (k, crate::numeric::default_type(&substitute(&v, &cx.vars))))
                .collect();
            if let Some(params) = self.el.dyn_params_of(&callee).cloned() {
                self.dyn_coercions(cx, &params, &vars, &span);
                // A `dyn` parameter is not instantiated: the instance keeps
                // it generic, and the second check infers it again.
                vars.retain(|k, _| !crate::dyn_trait::is_dyn_generic(k));
            }
            if let Some(rest) = callee.strip_prefix(STUB) {
                let Some((trait_name, method)) = rest.split_once('$') else {
                    continue;
                };
                let key = (self.el.fe.identity.clone(), trait_name.to_owned());
                let Some(ty) = vars.get("$Self").cloned() else {
                    continue;
                };
                let tr = self.el.fe.traits[&key].decl.clone();
                let args: Vec<Type> = tr
                    .generics
                    .iter()
                    .map(|g| vars.get(&g.name).cloned().unwrap_or(Type::Infer))
                    .collect();
                let Some(index) = self.evidence(cx, &key, &ty, &args, &span) else {
                    continue;
                };
                let generated = match self.el.conformance_function(index, method, &span) {
                    Ok(g) => g,
                    Err(e) => {
                        self.errors.push(e);
                        continue;
                    }
                };
                let c = self.el.fe.confs[index].clone();
                let mut cvars = BTreeMap::new();
                match_type(&c.decl.self_ty, &ty, &c.decl.generics, &mut cvars);
                // Conformance parameters may appear only in the trait
                // arguments (`impl[T] Pick[T] for i64`).
                for (pattern, actual) in c.decl.trait_ref.args.iter().zip(&args) {
                    if !closed(actual) {
                        continue;
                    }
                    match_type(pattern, actual, &c.decl.generics, &mut cvars);
                }
                let target = if self.bounded.contains(&generated) {
                    let evidence = self.callee_evidence(cx, &generated, &cvars, &span);
                    self.instance(&generated, cvars, evidence, cx.depth + 1, &span)
                } else {
                    generated
                };
                rewrites.push((span, target));
            } else if self.bounded.contains(&callee) {
                if vars.values().any(|t| !closed(t)) {
                    self.errors.push(d(
                        "E0607",
                        format!(
                            "cannot infer the type arguments of '{callee}' here; annotate them"
                        ),
                        &span,
                    ));
                    continue;
                }
                let evidence = self.callee_evidence(cx, &callee, &vars, &span);
                let target = self.instance(&callee, vars, evidence, cx.depth + 1, &span);
                rewrites.push((span, target));
            }
        }
        if rewrites.is_empty() {
            return;
        }
        let rewrites: BTreeMap<(usize, usize), String> = rewrites
            .into_iter()
            .map(|(s, n)| ((s.start, s.end), n))
            .collect();
        let passes: BTreeMap<String, Pass> = self
            .funcs
            .iter()
            .filter_map(|(n, f)| Some((n.clone(), f.params.first()?.pass)))
            .collect();
        let instances = &self.instances;
        each_body_expr(f, &mut |e| {
            let Some(target) = rewrites.get(&(e.span.start, e.span.end)) else {
                return;
            };
            let ExprKind::Call {
                callee,
                args,
                types,
            } = &mut e.kind
            else {
                return;
            };
            // The instance is monomorphic: explicit type arguments of the
            // generic call (`max[i64](1, 2)`) no longer apply.
            types.clear();
            if let ExprKind::Field(base, _) = &callee.kind {
                // An inherent method call: the receiver is the first argument.
                let origin = instances
                    .iter()
                    .find(|(_, n)| *n == target)
                    .map(|(k, _)| k.0.clone())
                    .unwrap_or_else(|| target.clone());
                args.insert(
                    0,
                    Arg {
                        pass: passes.get(&origin).copied().unwrap_or_default(),
                        explicit: false,
                        expr: (**base).clone(),
                    },
                );
            }
            let span = callee.span.clone();
            **callee = Expr {
                kind: ExprKind::Name(target.clone()),
                span,
            };
        });
    }

    /// Selects the vtable of each concrete type a call passes to a `dyn`
    /// parameter (`params`, the callee's), with the conformance chosen in
    /// `cx` at `span`, and instantiates its methods.
    fn dyn_coercions(
        &mut self,
        cx: &Ctx,
        params: &BTreeMap<String, Type>,
        vars: &BTreeMap<String, Type>,
        span: &Span,
    ) {
        for (g, tr) in params {
            let Some(ty) = vars.get(g).cloned() else {
                continue;
            };
            if matches!(&ty, Type::Named(n, a) if a.is_empty() && crate::dyn_trait::is_dyn_generic(n))
            {
                // Forwarding a `dyn` parameter: its caller chose the vtable.
                continue;
            }
            let tr = crate::numeric::default_type(&substitute(tr, vars));
            let origin_generics = &self.funcs[&cx.origin].generics;
            let open = !closed(&ty)
                || !closed(&tr)
                || origin_generics
                    .iter()
                    .filter(|g| !cx.vars.contains_key(&g.name))
                    .any(|g| mentions(&ty, &g.name) || mentions(&tr, &g.name));
            if open {
                self.errors.push(d(
                    "E0633",
                    format!(
                        "cannot build the vtable of {ty} as `dyn {tr}` here: the type is not known before instantiation; bound the type parameter by the trait so the function is monomorphized"
                    ),
                    span,
                ));
                continue;
            }
            let Type::Named(trait_name, args) = &tr else {
                continue;
            };
            let key = (self.el.fe.identity.clone(), trait_name.clone());
            let Some(decl) = self.el.fe.traits.get(&key).map(|t| t.decl.clone()) else {
                continue;
            };
            let object = crate::dyn_trait::object(tr.clone());
            if self
                .el
                .dyn_vtables
                .borrow()
                .iter()
                .any(|v| v.object == object && v.self_ty == ty)
            {
                continue;
            }
            let Some(index) = self.evidence(cx, &key, &ty, args, span) else {
                continue;
            };
            let c = self.el.fe.confs[index].clone();
            let mut cvars = BTreeMap::new();
            match_type(&c.decl.self_ty, &ty, &c.decl.generics, &mut cvars);
            for (pattern, actual) in c.decl.trait_ref.args.iter().zip(args) {
                match_type(pattern, actual, &c.decl.generics, &mut cvars);
            }
            let mut methods = vec![];
            for m in &decl.methods {
                let generated = match self.el.conformance_function(index, &m.name, span) {
                    Ok(g) => g,
                    Err(e) => {
                        self.errors.push(e);
                        return;
                    }
                };
                let target = if self.bounded.contains(&generated) {
                    let evidence = self.callee_evidence(cx, &generated, &cvars, span);
                    self.instance(&generated, cvars.clone(), evidence, cx.depth + 1, span)
                } else {
                    generated
                };
                let thunk = self.el.dyn_thunk(&decl, m, args, &ty, &target);
                methods.push((thunk, BTreeMap::new()));
            }
            self.el
                .dyn_vtables
                .borrow_mut()
                .push(crate::dyn_trait::VTable {
                    object,
                    self_ty: ty,
                    methods,
                });
        }
    }

    /// Evidence for each bound of `callee` instantiated at `vars`.
    fn callee_evidence(
        &mut self,
        cx: &Ctx,
        callee: &str,
        vars: &BTreeMap<String, Type>,
        span: &Span,
    ) -> Vec<usize> {
        let mut out = vec![];
        for b in self.el.bounds_of(callee) {
            let ty = crate::numeric::default_type(&substitute(&b.ty, vars));
            let args: Vec<Type> = b.args.iter().map(|a| substitute(a, vars)).collect();
            match self.evidence(cx, &b.key, &ty, &args, span) {
                Some(e) => out.push(e),
                None => return out,
            }
        }
        out
    }
}
