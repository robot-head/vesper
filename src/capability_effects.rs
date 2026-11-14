//! Built-in source capability effects `ConstTime`, `Realtime` and `DetFloat`
//! (design §23.2, §32.2, §36.2).
//!
//! A capability effect is a stateless lexical property of a function body,
//! written in its effect row (`eff [Realtime]`). Unlike ordinary effects it is
//! never requested from the caller: calling a `Realtime` function from plain
//! code enters the realtime discipline, it does not make the caller realtime.
//! The discipline flows the other way, to callees:
//!
//! * a body carrying capability `C` may only call functions that carry `C`
//!   (constructors and the `wrapping_*` intrinsics are allowed: they are
//!   single primitive operations), and only through function values whose
//!   type carries `C`;
//! * no handler can discharge a capability effect (`handle Realtime` is an
//!   error) and no source declaration may redefine one;
//! * every function type carrying `C` is inhabited only by code checked under
//!   `C`: a closure literal flowing into such a position must declare `C`
//!   itself, and a named function must carry it.
//!
//! On top of that each capability adds its own rules, all checked on the
//! surface HIR of an already well-typed `World`:
//!
//! * `Realtime`: no effect named by the design as allocating, blocking or
//!   unbounded (`Alloc`, `Async`, `Net`, `Storage`, `Scope`, mutexes, `Ffi`,
//!   ...) in the row of a realtime function or handled inside it, no open
//!   effect-row tail, no recursion (the core language's only unbounded
//!   iteration: it has no loop construct, so no loop can carry a static
//!   bound), and no handler of a resumable (linear) effect, whose captured
//!   continuation may need a suspended fiber stack.
//! * `DetFloat`: float operations are limited to the correctly rounded IEEE
//!   754 basic operations (`+ - * /`, negation, comparisons). Float remainder
//!   and host-implemented effect operations with float signatures are
//!   rejected. The surface language has no fused multiply-add or libm call;
//!   contraction in the backend is Stage 5's job (Kinetic's verifier), fed by
//!   [`capability_policies`].
//! * `ConstTime`: values whose type mentions a declared secret type
//!   (`Secret[T]`, or the design's `SecU8`, `SecU32`, `SecU64`, `SecMask64`)
//!   are secret, and so is everything computed from them. Inside
//!   `eff [ConstTime]` a secret may not decide a branch (`if`, a refutable
//!   `match`, `&&`/`||`, `?`), an index, a division or remainder, or a checked
//!   operation that may abort; it may not flow into a public parameter, an
//!   effect operation's public argument or a non-secret result, except through
//!   an explicit declassification gate (a `ConstTime` function named
//!   `declassify` or `declassify_*`). Outside `ConstTime`, the payload of a
//!   secret value cannot be read at all, and secret values are never compared
//!   with `==`, `!=`, `<`, `<=`, `>` or `>=`.
//!
//! These are source typing rules enforced by the checker. They are not a proof
//! that the emitted machine code is constant time, realtime safe or bit
//! deterministic: that is the Stage 5 verifiers' job.
use crate::ast::*;
use crate::check::{Checked, World};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// `ConstTime` (§23.2).
pub const CONST_TIME: &str = "ConstTime";
/// `Realtime` (§32.2).
pub const REALTIME: &str = "Realtime";
/// `DetFloat` (§36.2).
pub const DET_FLOAT: &str = "DetFloat";

/// Declared type names that make a value secret. `Secret[T]` is §22.4's
/// spelling; the others are §23.2's opaque scalars. Only names declared as
/// ADTs in the module count.
pub const SECRET_TYPES: [&str; 5] = ["Secret", "SecU8", "SecU32", "SecU64", "SecMask64"];

/// Effects a realtime body may not reach: allocation, blocking, suspension
/// and lock acquisition, as named by §32.1 and §32.5.
pub const REALTIME_FORBIDDEN: [&str; 12] = [
    "Alloc", "Async", "Net", "Storage", "Scope", "Mutex", "Lock", "Block", "Blocking", "Ffi", "Io",
    "Sleep",
];

/// Schema of [`CapabilityPolicies`].
pub const POLICY_SCHEMA: &str = "vesper.capability-policies/1";

/// A built-in capability effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum Capability {
    ConstTime,
    Realtime,
    DetFloat,
}
impl Capability {
    pub const ALL: [Capability; 3] = [
        Capability::ConstTime,
        Capability::Realtime,
        Capability::DetFloat,
    ];
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            CONST_TIME => Some(Self::ConstTime),
            REALTIME => Some(Self::Realtime),
            DET_FLOAT => Some(Self::DetFloat),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::ConstTime => CONST_TIME,
            Self::Realtime => REALTIME,
            Self::DetFloat => DET_FLOAT,
        }
    }
}

/// Whether `name` is a built-in capability effect.
pub fn is_capability_effect(name: &str) -> bool {
    // `Isa[P]` (§14.2, lane 28) is erased the same way: it has no
    // operations or handlers and is never requested from the caller's row.
    // Its own rules live in `crate::vector`, not in this module's discipline.
    Capability::from_name(name).is_some() || crate::vector::is_isa_effect(name)
}

/// The capability effects in a row, in canonical order.
pub fn capabilities(effects: &Effects) -> BTreeSet<Capability> {
    effects
        .instances
        .iter()
        .filter_map(|e| Capability::from_name(&e.name))
        .collect()
}

/// Diagnostic for a source declaration of a reserved capability effect.
pub(crate) fn reserved_declaration(name: &str, span: Span) -> Diagnostic {
    Diagnostic::new(
        "E0100",
        format!("'{name}' is a reserved built-in capability effect"),
        span,
    )
    .with_help("remove the declaration; ConstTime, Realtime and DetFloat are built in")
}

/// Diagnostics for one capability entry of an effect row.
pub(crate) fn row_entry(effect: &Effect, span: Span) -> Option<Diagnostic> {
    if crate::vector::is_isa_effect(&effect.name) {
        return crate::vector::isa_row_entry(effect, span);
    }
    (!effect.args.is_empty()).then(|| {
        Diagnostic::new(
            "E0400",
            format!(
                "capability effect '{}' takes no type arguments",
                effect.name
            ),
            span,
        )
    })
}

/// Diagnostic for `handle ConstTime in ...` and friends.
pub(crate) fn handler_rejected(effect: &Effect, span: Span) -> Diagnostic {
    Diagnostic::new(
        "E0701",
        format!(
            "capability effect '{}' cannot be handled: no handler can discharge it",
            effect.name
        ),
        span,
    )
    .with_help("call a function carrying the capability instead of installing a handler")
}

/// Checks the capability rules on a well-typed World. Returns diagnostics
/// in source order.
pub(crate) fn check_world(world: &World) -> Vec<Diagnostic> {
    let mut diagnostics = vec![];
    let mut graph: BTreeMap<String, Vec<(String, Span)>> = BTreeMap::new();
    let mut indirect = BTreeSet::new();
    for f in world.functions.values() {
        let caps = capabilities(&f.effects);
        let mut w = Walker::new(world, f, caps.clone());
        w.function_rules(f);
        if let Some(body) = &f.body {
            let tainted = w.expr(body);
            w.coerce(body, &f.result);
            if caps.contains(&Capability::ConstTime)
                && tainted
                && !w.secret_type(&f.result)
                && !carries_no_information(&f.result)
                && !is_declassify_gate(&f.name)
            {
                w.error(
                    "E0706",
                    format!(
                        "eff [ConstTime] function '{}' returns a secret-derived value as non-secret '{}'",
                        f.name, f.result
                    ),
                    tail_span(body),
                    "wrap the result in a secret type or pass it through an explicit declassify gate",
                );
            }
        }
        for (node, edges) in std::mem::take(&mut w.edges) {
            graph.entry(node).or_default().extend(edges);
        }
        indirect.extend(std::mem::take(&mut w.indirect_targets));
        diagnostics.extend(w.diagnostics);
    }
    graph.insert(
        INDIRECT.to_string(),
        indirect.into_iter().map(|n| (n, 0..0)).collect(),
    );
    diagnostics.extend(realtime_recursion(&graph));
    diagnostics.sort_by(|a, b| {
        (a.span.start, a.span.end, &a.code).cmp(&(b.span.start, b.span.end, &b.code))
    });
    diagnostics
}

/// Whether `name` (or its last path segment) is a declassification gate.
pub fn is_declassify_gate(name: &str) -> bool {
    let short = name.rsplit("::").next().unwrap_or(name);
    short == "declassify" || short.starts_with("declassify_")
}

fn carries_no_information(t: &Type) -> bool {
    matches!(t, Type::Tuple(ts) if ts.is_empty()) || matches!(t.name(), "Unit" | "Never")
}

fn tail_span(e: &Expr) -> Span {
    match &e.kind {
        ExprKind::Block { tail: Some(t), .. } => tail_span(t),
        _ => e.span.clone(),
    }
}

/// Synthetic call-graph node standing for every call through a function
/// value: it has an edge to each realtime function whose value is taken and
/// to each realtime closure.
const INDIRECT: &str = "$indirect";
/// Prefix of the call-graph node of a realtime closure in plain code.
const CLOSURE_NODE: &str = "$closure@";

/// Rejects call cycles among realtime functions: recursion has no static
/// bound in the core language.
fn realtime_recursion(graph: &BTreeMap<String, Vec<(String, Span)>>) -> Vec<Diagnostic> {
    let mut out = vec![];
    let mut reported = BTreeSet::new();
    for (root, edges) in graph {
        if root == INDIRECT {
            continue;
        }
        let shown = if root.starts_with(CLOSURE_NODE) {
            "a realtime closure"
        } else {
            root.as_str()
        };
        for (callee, span) in edges {
            if !graph.contains_key(callee) || reported.contains(&(span.start, span.end)) {
                continue;
            }
            // Does `callee` reach `root`? Bounded by the graph size.
            let mut seen = BTreeSet::new();
            let mut stack = vec![callee.as_str()];
            let mut cycle = false;
            while let Some(n) = stack.pop() {
                if n == root {
                    cycle = true;
                    break;
                }
                if !seen.insert(n) {
                    continue;
                }
                if let Some(next) = graph.get(n) {
                    stack.extend(next.iter().map(|(c, _)| c.as_str()));
                }
            }
            if cycle {
                reported.insert((span.start, span.end));
                let message = if callee == INDIRECT {
                    format!(
                        "eff [Realtime] call through a function value in '{shown}' may reach it again; recursion has no static bound"
                    )
                } else {
                    format!(
                        "eff [Realtime] call from '{shown}' to '{callee}' is recursive; recursion has no static bound"
                    )
                };
                out.push(
                    Diagnostic::new(
                        "E0722",
                        message,
                        span.clone(),
                    )
                    .with_help("realtime code must have a statically bounded call depth; unroll or bound the iteration"),
                );
            }
        }
    }
    out
}

#[derive(Debug, Clone)]
struct Local {
    tainted: bool,
    /// A closure literal or continuation bound in this lexical context: its
    /// body is checked under the same capabilities.
    lexical_callable: bool,
    /// A parameter that is not secret by type: secrets may not be written to
    /// it.
    public_param: bool,
    ty: Option<Type>,
}

struct Walker<'w> {
    world: &'w World,
    function: &'w Function,
    caps: BTreeSet<Capability>,
    scopes: Vec<BTreeMap<String, Local>>,
    diagnostics: Vec<Diagnostic>,
    direct_calls: Vec<(String, Span)>,
    /// Realtime call-graph node the walk is in (`None` outside realtime
    /// code), its outgoing edges, and the nodes reachable through function
    /// values (address-taken realtime functions and realtime closures).
    node: Option<String>,
    edges: BTreeMap<String, Vec<(String, Span)>>,
    indirect_targets: BTreeSet<String>,
}

enum Callee<'w> {
    Constructor,
    Intrinsic,
    Function(&'w Function),
    EffectOp(&'w EffectDecl, &'w Function),
    Value(Option<Type>, bool),
    Unknown,
}

impl<'w> Walker<'w> {
    fn new(world: &'w World, function: &'w Function, caps: BTreeSet<Capability>) -> Self {
        let mut w = Self {
            world,
            function,
            caps,
            scopes: vec![BTreeMap::new()],
            diagnostics: vec![],
            direct_calls: vec![],
            node: None,
            edges: BTreeMap::new(),
            indirect_targets: BTreeSet::new(),
        };
        if w.has(Capability::Realtime) {
            w.node = Some(function.name.clone());
            w.edges.insert(function.name.clone(), vec![]);
        }
        for p in &function.params {
            let secret = w.secret_type(&p.ty);
            w.bind(
                &p.name,
                Local {
                    tainted: secret,
                    lexical_callable: false,
                    public_param: !secret,
                    ty: Some(p.ty.clone()),
                },
            );
        }
        w
    }
    fn error(&mut self, code: &str, message: String, span: Span, help: &str) {
        self.diagnostics
            .push(Diagnostic::new(code, message, span).with_help(help));
    }
    fn has(&self, c: Capability) -> bool {
        self.caps.contains(&c)
    }
    fn bind(&mut self, name: &str, local: Local) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string(), local);
        }
    }
    fn local(&self, name: &str) -> Option<&Local> {
        self.scopes.iter().rev().find_map(|s| s.get(name))
    }
    fn local_mut(&mut self, name: &str) -> Option<&mut Local> {
        self.scopes.iter_mut().rev().find_map(|s| s.get_mut(name))
    }
    /// The checked type of `e`: recorded by the checker, or derived from
    /// the local and field types for places the checker records elsewhere.
    fn ty(&self, e: &Expr) -> Option<Type> {
        if let Some(t) = self.world.types.get(&(e.span.start, e.span.end))
            && *t != Type::Infer
        {
            return Some(t.clone());
        }
        match &e.kind {
            ExprKind::Name(n) => self.local(n).and_then(|l| l.ty.clone()),
            ExprKind::Field(base, f) => self.world.field(&self.ty(base)?, std::slice::from_ref(f)),
            _ => None,
        }
    }
    fn secret_type(&self, t: &Type) -> bool {
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
        visit(self.world, t, &mut BTreeSet::new())
    }
    fn caps_list(&self) -> String {
        self.caps
            .iter()
            .map(|c| c.name())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Signature-level rules of a capability function.
    fn function_rules(&mut self, f: &Function) {
        if !self.has(Capability::Realtime) {
            return;
        }
        for e in &f.effects.instances {
            if REALTIME_FORBIDDEN.contains(&e.name.as_str()) {
                self.error(
                    "E0720",
                    format!(
                        "eff [Realtime] function '{}' carries '{}', which may allocate, block or suspend",
                        f.name, e.name
                    ),
                    f.span.clone(),
                    "realtime rows exclude Alloc, Async, Net, Storage, Scope, locks, Ffi and blocking I/O",
                );
            }
        }
        if let Some(tail) = &f.effects.tail {
            self.error(
                "E0721",
                format!(
                    "eff [Realtime] function '{}' has an open effect row '{tail}', which may admit blocking effects",
                    f.name
                ),
                f.span.clone(),
                "list the realtime function's effects explicitly",
            );
        }
    }

    fn with_scope<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        self.scopes.push(BTreeMap::new());
        let r = f(self);
        self.scopes.pop();
        r
    }

    fn bind_pattern(&mut self, p: &Pattern, tainted: bool, scrutinee: Option<&Type>) {
        if let Some(t) = scrutinee {
            self.secret_access_pattern(p, t);
        }
        let whole = matches!(p.kind, PatternKind::Bind(_));
        for name in pattern_bindings(p) {
            self.bind(
                &name,
                Local {
                    tainted,
                    lexical_callable: false,
                    public_param: false,
                    ty: if whole { scrutinee.cloned() } else { None },
                },
            );
        }
    }

    /// Outside ConstTime, destructuring a secret exposes its payload.
    fn secret_access_pattern(&mut self, p: &Pattern, t: &Type) {
        if self.has(Capability::ConstTime) || !self.secret_type(t) {
            return;
        }
        // Recurse into the subpatterns with their component types.
        match (&p.kind, t) {
            (PatternKind::Tuple(ps), Type::Tuple(ts)) => {
                for (p, t) in ps.iter().zip(ts) {
                    self.secret_access_pattern(p, t);
                }
            }
            (PatternKind::Constructor { name, fields, .. }, Type::Named(_, args)) => {
                if let Some((a, v)) = self.world.constructor(name) {
                    let vars: BTreeMap<String, Type> = a
                        .generics
                        .iter()
                        .zip(args)
                        .map(|(g, t)| (g.name.clone(), t.clone()))
                        .collect();
                    for (i, (field, sub)) in fields.iter().enumerate() {
                        let fty = v
                            .fields
                            .iter()
                            .find(|f| &f.name == field)
                            .or_else(|| v.fields.get(i).filter(|_| field.parse::<usize>().is_ok()))
                            .map(|f| substitute(&f.ty, &vars));
                        if let Some(fty) = fty {
                            self.secret_access_pattern(sub, &fty);
                        }
                    }
                }
            }
            _ => {}
        }
        if let PatternKind::Constructor { name, .. } = &p.kind
            && self
                .world
                .constructor(name)
                .is_some_and(|(a, _)| SECRET_TYPES.contains(&a.name.as_str()))
        {
            self.error(
                "E0707",
                format!(
                    "the payload of secret '{t}' can only be read inside eff [ConstTime] (in '{}')",
                    self.function.name
                ),
                p.span.clone(),
                "move the computation into an eff [ConstTime] function",
            );
        }
    }

    /// Whether a match over `arms` branches at run time.
    fn refutable(&self, arms: &[(Pattern, Expr)]) -> bool {
        if arms.len() != 1 {
            return true;
        }
        fn refutable(w: &World, p: &Pattern) -> bool {
            match &p.kind {
                PatternKind::Wild | PatternKind::Bind(_) => false,
                PatternKind::Literal(_) => true,
                PatternKind::Tuple(ps) => ps.iter().any(|p| refutable(w, p)),
                PatternKind::Constructor { name, fields, .. } => {
                    w.constructor(name)
                        .is_none_or(|(a, _)| a.variants.len() != 1)
                        || fields.iter().any(|(_, p)| refutable(w, p))
                }
            }
        }
        refutable(self.world, &arms[0].0)
    }

    fn branch_on_secret(&mut self, what: &str, span: Span) {
        self.error(
            "E0702",
            format!(
                "eff [ConstTime] {what} depends on a secret value (in '{}')",
                self.function.name
            ),
            span,
            "select with constant-time arithmetic and declassify only the final verdict",
        );
    }

    /// Walks `e`; returns whether its value is secret-derived.
    fn expr(&mut self, e: &Expr) -> bool {
        let ct = self.has(Capability::ConstTime);
        let typed_secret = self.ty(e).is_some_and(|t| self.secret_type(&t));
        let tainted = match &e.kind {
            ExprKind::Literal(_) => false,
            ExprKind::Name(n) => {
                if self.local(n).is_none()
                    && let Some(f) = self.world.functions.get(n)
                    && capabilities(&f.effects).contains(&Capability::Realtime)
                {
                    self.indirect_targets.insert(n.clone());
                }
                self.local(n).is_some_and(|l| l.tainted)
            }
            ExprKind::Tuple(es) => es.iter().fold(false, |t, e| self.expr(e) | t),
            ExprKind::Struct { name, fields } => {
                let adt = self.world.constructor(name);
                let mut t = false;
                for (field, value) in fields {
                    t |= self.expr(value);
                    if let Some((a, v)) = adt
                        && let Some(fd) = v.fields.iter().find(|f| &f.name == field)
                    {
                        let vars = self
                            .ty(e)
                            .map(|ty| generic_vars(&a.generics, &ty))
                            .unwrap_or_default();
                        self.coerce(value, &substitute(&fd.ty, &vars));
                    }
                }
                t
            }
            ExprKind::Block { statements, tail } => self.with_scope(|w| {
                for s in statements {
                    match s {
                        Statement::Let { pat, ty, value } => {
                            let t = w.expr(value);
                            if *ty != Type::Infer {
                                w.coerce(value, ty);
                            }
                            let lexical = matches!(value.kind, ExprKind::Closure { .. });
                            let scrutinee = if *ty != Type::Infer {
                                Some(ty.clone())
                            } else {
                                w.ty(value)
                            };
                            w.bind_pattern(pat, t, scrutinee.as_ref());
                            if lexical && let PatternKind::Bind(n) = &pat.kind {
                                w.bind(
                                    n,
                                    Local {
                                        tainted: t,
                                        lexical_callable: true,
                                        public_param: false,
                                        ty: scrutinee,
                                    },
                                );
                            }
                        }
                        Statement::Expr(e) => {
                            w.expr(e);
                        }
                    }
                }
                tail.as_ref().is_some_and(|t| w.expr(t))
            }),
            ExprKind::Call { callee, args, .. } => self.call(e, callee, args),
            ExprKind::Field(base, _) => {
                let t = self.expr(base);
                if !ct
                    && let Some(bt) = self.ty(base)
                    && let Type::Named(n, _) = &bt
                    && SECRET_TYPES.contains(&n.as_str())
                    && self.world.adts.contains_key(n)
                {
                    self.error(
                        "E0707",
                        format!(
                            "the payload of secret '{bt}' can only be read inside eff [ConstTime] (in '{}')",
                            self.function.name
                        ),
                        e.span.clone(),
                        "move the computation into an eff [ConstTime] function",
                    );
                }
                t
            }
            ExprKind::Index(base, index) => {
                let b = self.expr(base);
                let i = self.expr(index);
                if ct && i {
                    self.error(
                        "E0704",
                        format!(
                            "eff [ConstTime] index depends on a secret value (in '{}')",
                            self.function.name
                        ),
                        index.span.clone(),
                        "secret-indexed loads leak through the cache; select over all elements instead",
                    );
                }
                b | i
            }
            ExprKind::Try(v) => {
                let t = self.expr(v);
                if ct && t {
                    self.branch_on_secret("'?' propagation", e.span.clone());
                }
                t
            }
            ExprKind::Unary(op, v) => {
                let t = self.expr(v);
                if self.has(Capability::ConstTime)
                    && t
                    && self
                        .world
                        .overflow_ops
                        .contains(&(e.span.start, e.span.end))
                {
                    self.error(
                        "E0703",
                        format!(
                            "eff [ConstTime] checked '{op}' on a secret value may abort depending on the secret (in '{}')",
                            self.function.name
                        ),
                        e.span.clone(),
                        "use wrapping_sub(0, x) on secrets",
                    );
                }
                t
            }
            ExprKind::Binary(op, l, r) => {
                let a = self.expr(l);
                let b = self.expr(r);
                let t = a | b;
                self.binary(e, op, l, r, a, t);
                t
            }
            ExprKind::Assign(op, place, value) => {
                let p = self.expr(place);
                let v = self.expr(value);
                if let Some(target) = self.ty(place) {
                    self.coerce(value, &target);
                }
                // A local that held a lexically checked closure no longer
                // does once anything else is assigned to it.
                if let ExprKind::Name(n) = &place.kind
                    && !matches!(value.kind, ExprKind::Closure { .. })
                    && let Some(l) = self.local_mut(n)
                {
                    l.lexical_callable = false;
                }
                if op != "=" {
                    self.binary(e, op.trim_end_matches('='), place, value, p, p | v);
                }
                if v && let Some(root) = place_root(place) {
                    let public = self.local(&root).is_some_and(|l| l.public_param);
                    let param_inout = self
                        .function
                        .params
                        .iter()
                        .any(|q| q.name == root && q.pass == Pass::Inout);
                    if ct && public && param_inout {
                        self.error(
                            "E0705",
                            format!(
                                "eff [ConstTime] writes a secret-derived value into public inout parameter '{root}'"
                            ),
                            e.span.clone(),
                            "give the parameter a secret type or declassify explicitly",
                        );
                    }
                    if let Some(l) = self.local_mut(&root) {
                        l.tainted = true;
                    }
                }
                false
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                let c = self.expr(condition);
                if ct && c {
                    self.branch_on_secret("'if' condition", condition.span.clone());
                }
                let a = self.with_scope(|w| w.expr(then));
                let b = otherwise
                    .as_ref()
                    .is_some_and(|o| self.with_scope(|w| w.expr(o)));
                c | a | b
            }
            ExprKind::Match { value, arms } => {
                let s = self.expr(value);
                if ct && s && self.refutable(arms) {
                    self.branch_on_secret("'match' scrutinee", value.span.clone());
                }
                let scrutinee = self.ty(value);
                let mut t = s;
                for (pat, body) in arms {
                    t |= self.with_scope(|w| {
                        w.bind_pattern(pat, s, scrutinee.as_ref());
                        w.expr(body)
                    });
                }
                t
            }
            ExprKind::Closure {
                params,
                effects,
                body,
                result,
                ..
            } => {
                let outer = self.caps.clone();
                // Lexically nested closures run under the enclosing
                // capabilities and their own.
                self.caps.extend(capabilities(effects));
                if !self.caps.contains(&Capability::Realtime) {
                    // Nothing extra.
                } else if let Some(tail) = &effects.tail {
                    self.error(
                        "E0721",
                        format!(
                            "eff [Realtime] closure has an open effect row '{tail}', which may admit blocking effects"
                        ),
                        e.span.clone(),
                        "list the closure's effects explicitly",
                    );
                }
                if self.caps.contains(&Capability::Realtime) {
                    for eff in &effects.instances {
                        if REALTIME_FORBIDDEN.contains(&eff.name.as_str()) {
                            self.error(
                                "E0720",
                                format!(
                                    "eff [Realtime] closure carries '{}', which may allocate, block or suspend",
                                    eff.name
                                ),
                                e.span.clone(),
                                "realtime rows exclude Alloc, Async, Net, Storage, Scope, locks, Ffi and blocking I/O",
                            );
                        }
                    }
                }
                let outer_node = self.node.clone();
                if self.caps.contains(&Capability::Realtime) {
                    let node = outer_node
                        .clone()
                        .unwrap_or_else(|| format!("{CLOSURE_NODE}{}", e.span.start));
                    self.edges.entry(node.clone()).or_default();
                    self.indirect_targets.insert(node.clone());
                    self.node = Some(node);
                }
                let t = self.with_scope(|w| {
                    for p in params {
                        let secret = w.secret_type(&p.ty);
                        w.bind(
                            &p.name,
                            Local {
                                tainted: secret,
                                lexical_callable: false,
                                public_param: false,
                                ty: Some(p.ty.clone()),
                            },
                        );
                    }
                    w.expr(body)
                });
                if self.caps.contains(&Capability::ConstTime) && t {
                    let declared = match self.ty(e) {
                        Some(Type::Function { result, .. }) => Some(*result),
                        _ => (*result != Type::Infer).then(|| result.clone()),
                    };
                    if declared
                        .as_ref()
                        .is_none_or(|r| !self.secret_type(r) && !carries_no_information(r))
                    {
                        self.error(
                            "E0706",
                            "eff [ConstTime] closure returns a secret-derived value as non-secret"
                                .to_string(),
                            tail_span(body),
                            "wrap the result in a secret type or pass it through an explicit declassify gate",
                        );
                    }
                }
                self.node = outer_node;
                self.caps = outer;
                // A closure's value is secret only through its captures,
                // which its own result boundary has already checked.
                false
            }
            ExprKind::Handle {
                effect,
                state,
                body,
                ret,
                ops,
            } => self.handle(e, effect, state, body, ret, ops),
        };
        tainted | typed_secret
    }

    fn binary(&mut self, e: &Expr, op: &str, l: &Expr, r: &Expr, left: bool, tainted: bool) {
        let ct = self.has(Capability::ConstTime);
        if ct && left && matches!(op, "&&" | "||") {
            self.branch_on_secret("short-circuit operand", l.span.clone());
        }
        if ct && tainted && matches!(op, "/" | "%") {
            self.error(
                "E0703",
                format!(
                    "eff [ConstTime] '{op}' on a secret value has data-dependent latency (in '{}')",
                    self.function.name
                ),
                e.span.clone(),
                "secret division and remainder are variable time; restructure without them",
            );
        } else if ct
            && tainted
            && (self
                .world
                .overflow_ops
                .contains(&(e.span.start, e.span.end))
                || self.world.partial_ops.contains(&(e.span.start, e.span.end)))
        {
            self.error(
                "E0703",
                format!(
                    "eff [ConstTime] checked '{op}' on a secret value may abort depending on the secret (in '{}')",
                    self.function.name
                ),
                e.span.clone(),
                "use wrapping_add, wrapping_sub or wrapping_mul on secrets",
            );
        }
        let secret_operand = [l, r]
            .iter()
            .any(|x| self.ty(x).is_some_and(|t| self.secret_type(&t)));
        if secret_operand && matches!(op, "==" | "!=" | "<" | "<=" | ">" | ">=") {
            self.error(
                "E0707",
                format!(
                    "secret values cannot be compared with '{op}' (in '{}')",
                    self.function.name
                ),
                e.span.clone(),
                "secret types omit Eq and Ord; compare payloads inside eff [ConstTime] and declassify the verdict",
            );
        }
        if self.has(Capability::DetFloat)
            && op == "%"
            && [l, r]
                .iter()
                .any(|x| self.ty(x).is_some_and(|t| t.name() == "f64"))
        {
            self.error(
                "E0730",
                format!(
                    "eff [DetFloat] float remainder is not a correctly rounded basic operation (in '{}')",
                    self.function.name
                ),
                e.span.clone(),
                "only + - * / and comparisons are bit deterministic across targets",
            );
        }
    }

    fn handle(
        &mut self,
        e: &Expr,
        effect: &Effect,
        state: &Option<(Pattern, Box<Expr>)>,
        body: &Expr,
        ret: &Clause,
        ops: &[Clause],
    ) -> bool {
        if self.has(Capability::Realtime) {
            if REALTIME_FORBIDDEN.contains(&effect.name.as_str()) {
                self.error(
                    "E0720",
                    format!(
                        "eff [Realtime] body handles '{}', which may allocate, block or suspend",
                        effect.name
                    ),
                    e.span.clone(),
                    "move this handler outside the realtime callback",
                );
            } else if self
                .world
                .effects
                .get(&effect.name)
                .is_some_and(|d| d.linear)
            {
                self.error(
                    "E0723",
                    format!(
                        "eff [Realtime] body installs a resumable handler for '{}'; its continuation may need a suspended fiber stack",
                        effect.name
                    ),
                    e.span.clone(),
                    "realtime bodies may only handle abortive effects such as Fail",
                );
            }
        }
        let mut state_taint = state.as_ref().is_some_and(|(_, v)| self.expr(v));
        let b = self.with_scope(|w| w.expr(body));
        let mut t = b;
        // Operation clauses resume with the next state, so a clause that
        // computes anything secret may make the state secret. Taint is a
        // single bit, so a second pass reaches the fixpoint (duplicate
        // diagnostics are removed by the checker).
        for _ in 0..2 {
            let ops_taint = self.op_clauses(effect, state, ops, state_taint);
            t |= ops_taint;
            if state.is_none() || state_taint || !ops_taint {
                break;
            }
            state_taint = true;
        }
        // The return clause binds the body's value and, with state, the
        // final state: both carry their taint.
        let final_state = state.is_some() && state_taint;
        t |= self.with_scope(|w| {
            match (ret.pats.as_slice(), state.is_some()) {
                // `return (value, state)`, written as two patterns or as one
                // pair: each component keeps its own taint.
                ([value, st], true) => {
                    w.bind_pattern(value, b, None);
                    w.bind_pattern(st, final_state, None);
                }
                ([p], true)
                    if let PatternKind::Tuple(ps) = &p.kind
                        && let [value, st] = ps.as_slice() =>
                {
                    w.bind_pattern(value, b, None);
                    w.bind_pattern(st, final_state, None);
                }
                (pats, _) => {
                    for p in pats {
                        w.bind_pattern(p, b || final_state, None);
                    }
                }
            }
            w.expr(&ret.body)
        });
        t
    }

    fn op_clauses(
        &mut self,
        effect: &Effect,
        state: &Option<(Pattern, Box<Expr>)>,
        ops: &[Clause],
        state_taint: bool,
    ) -> bool {
        let mut t = false;
        for op in ops {
            t |= self.with_scope(|w| {
                let n = op.pats.len();
                for (i, p) in op.pats.iter().enumerate() {
                    if i + 1 == n {
                        if let PatternKind::Bind(k) = &p.kind {
                            w.bind(
                                k,
                                Local {
                                    tainted: false,
                                    lexical_callable: true,
                                    public_param: false,
                                    ty: None,
                                },
                            );
                        }
                    } else {
                        // Operation arguments are secret only when their
                        // declared type is; state carries its own taint.
                        let secret = w
                            .world
                            .effects
                            .get(&effect.name)
                            .and_then(|d| d.ops.iter().find(|f| f.name == op.name))
                            .and_then(|f| f.params.get(i))
                            .is_some_and(|q| w.secret_type(&q.ty));
                        w.bind_pattern(p, secret || (state.is_some() && state_taint), None);
                    }
                }
                w.expr(&op.body)
            });
        }
        t
    }

    fn resolve(&self, callee: &Expr) -> Callee<'w> {
        match &callee.kind {
            ExprKind::Name(n) if self.local(n).is_some() => Callee::Value(
                self.ty(callee),
                self.local(n).is_some_and(|l| l.lexical_callable),
            ),
            ExprKind::Name(n) => {
                if self.world.constructor(n).is_some() {
                    return Callee::Constructor;
                }
                if crate::numeric::wrapping_intrinsic(n).is_some() {
                    return Callee::Intrinsic;
                }
                if let Some(f) = self.world.functions.get(n) {
                    return Callee::Function(f);
                }
                if let Some((eff, op)) = n.split_once("::")
                    && let Some(d) = self.world.effects.get(eff)
                    && let Some(f) = d.ops.iter().find(|f| f.name == op)
                {
                    return Callee::EffectOp(d, f);
                }
                Callee::Unknown
            }
            ExprKind::Field(base, method) => {
                if let ExprKind::Name(label) = &base.kind
                    && self.local(label).is_none()
                    && let Some(eff) = self
                        .function
                        .effects
                        .instances
                        .iter()
                        .find(|e| e.label.as_deref() == Some(label.as_str()))
                    && let Some(d) = self.world.effects.get(&eff.name)
                    && let Some(f) = d.ops.iter().find(|f| &f.name == method)
                {
                    return Callee::EffectOp(d, f);
                }
                if let Some(Type::Named(t, _)) = self.ty(base).as_ref()
                    && let Some(f) = self.world.functions.get(&format!("{t}::{method}"))
                {
                    return Callee::Function(f);
                }
                match self.ty(callee) {
                    Some(t @ Type::Function { .. }) => Callee::Value(Some(t), false),
                    _ => Callee::Unknown,
                }
            }
            _ => Callee::Value(self.ty(callee), false),
        }
    }

    fn call(&mut self, e: &Expr, callee: &Expr, args: &[Arg]) -> bool {
        let callee_taint = match &callee.kind {
            ExprKind::Field(base, _) => self.expr(base),
            ExprKind::Name(n) => self.local(n).is_some_and(|l| l.tainted),
            _ => self.expr(callee),
        };
        let resolved = self.resolve(callee);
        let receiver = matches!(
            (&callee.kind, &resolved),
            (ExprKind::Field(..), Callee::Function(_))
        );
        let taints: Vec<bool> = args.iter().map(|a| self.expr(&a.expr)).collect();
        let any = callee_taint && receiver || taints.iter().any(|t| *t);
        let vars = self
            .world
            .calls
            .get(&(e.span.start, e.span.end))
            .cloned()
            .unwrap_or_default();
        let span = e.span.clone();
        match resolved {
            Callee::Constructor | Callee::Intrinsic => any,
            Callee::Function(f) => {
                self.direct_calls.push((f.name.clone(), span.clone()));
                self.edge(&f.name, span.clone());
                self.require_carried(&f.name, &f.effects, span.clone());
                let mut params: Vec<Type> =
                    f.params.iter().map(|p| substitute(&p.ty, &vars)).collect();
                let mut flows: Vec<(bool, &Expr)> = vec![];
                if receiver && let ExprKind::Field(base, _) = &callee.kind {
                    flows.push((callee_taint, base));
                }
                flows.extend(taints.iter().copied().zip(args.iter().map(|a| &a.expr)));
                for ((t, arg), p) in flows.iter().zip(params.iter_mut()) {
                    self.coerce(arg, p);
                    if *t && self.has(Capability::ConstTime) && !self.secret_type(p) {
                        self.error(
                            "E0705",
                            format!(
                                "eff [ConstTime] passes a secret-derived value to public parameter '{p}' of '{}'",
                                f.name
                            ),
                            arg.span.clone(),
                            "give the parameter a secret type, or declassify explicitly",
                        );
                    }
                }
                if is_declassify_gate(&f.name)
                    && capabilities(&f.effects).contains(&Capability::ConstTime)
                {
                    false
                } else {
                    any
                }
            }
            Callee::EffectOp(d, op) => {
                self.effect_op(e, d, op, args, &taints);
                any
            }
            Callee::Value(ty, lexical) => {
                if !lexical {
                    self.edge(INDIRECT, span.clone());
                }
                let effects = match &ty {
                    Some(Type::Function { effects, .. }) => Some(effects.clone()),
                    _ => None,
                };
                if !self.caps.is_empty() && !lexical {
                    match &effects {
                        Some(effs) => {
                            let name = match &callee.kind {
                                ExprKind::Name(n) => n.clone(),
                                _ => "function value".into(),
                            };
                            self.require_carried(&name, effs, span.clone());
                            if self.has(Capability::Realtime) && effs.tail.is_some() {
                                self.error(
                                    "E0721",
                                    format!("eff [Realtime] calls '{name}' through an open effect row"),
                                    span.clone(),
                                    "list the function value's effects explicitly",
                                );
                            }
                        }
                        None => self.error(
                            "E0709",
                            format!(
                                "eff [{}] body calls a value whose capabilities cannot be determined",
                                self.caps_list()
                            ),
                            span.clone(),
                            "call a named function carrying the capability",
                        ),
                    }
                }
                if let Some(Type::Function { params, .. }) = &ty {
                    for ((t, a), (_, p)) in taints.iter().zip(args).zip(params) {
                        self.coerce(&a.expr, p);
                        if *t && self.has(Capability::ConstTime) && !lexical && !self.secret_type(p)
                        {
                            self.error(
                                "E0705",
                                format!("eff [ConstTime] passes a secret-derived value to public parameter '{p}' of a function value"),
                                a.expr.span.clone(),
                                "give the parameter a secret type, or declassify explicitly",
                            );
                        }
                    }
                }
                any
            }
            Callee::Unknown => {
                if !self.caps.is_empty() {
                    self.error(
                        "E0709",
                        format!(
                            "eff [{}] body calls a callee that cannot be resolved statically",
                            self.caps_list()
                        ),
                        span,
                        "call a named function carrying the capability",
                    );
                }
                any
            }
        }
    }

    fn effect_op(
        &mut self,
        e: &Expr,
        d: &EffectDecl,
        op: &Function,
        args: &[Arg],
        taints: &[bool],
    ) {
        if self.has(Capability::Realtime) && REALTIME_FORBIDDEN.contains(&d.name.as_str()) {
            self.error(
                "E0720",
                format!(
                    "eff [Realtime] body performs '{}::{}', which may allocate, block or suspend",
                    d.name, op.name
                ),
                e.span.clone(),
                "realtime rows exclude Alloc, Async, Net, Storage, Scope, locks, Ffi and blocking I/O",
            );
        }
        if self.has(Capability::DetFloat)
            && !capabilities(&op.effects).contains(&Capability::DetFloat)
            && (op.params.iter().any(|p| mentions_float(&p.ty)) || mentions_float(&op.result))
        {
            self.error(
                "E0731",
                format!(
                    "eff [DetFloat] calls host-implemented float operation '{}::{}', whose rounding is platform dependent",
                    d.name, op.name
                ),
                e.span.clone(),
                "compute the value with DetFloat functions built from + - * /",
            );
        }
        if self.has(Capability::ConstTime) {
            // Instantiate the effect's generics from the active row, then
            // from the call: a generic parameter is secret only when its
            // instantiation is.
            let mut vars: BTreeMap<String, Type> = self
                .function
                .effects
                .instances
                .iter()
                .find(|i| i.name == d.name && i.args.len() == d.generics.len())
                .map(|i| {
                    d.generics
                        .iter()
                        .zip(&i.args)
                        .map(|(g, t)| (g.name.clone(), t.clone()))
                        .collect()
                })
                .unwrap_or_default();
            vars.extend(generic_vars_from_call(self.world, e));
            for ((t, a), p) in taints.iter().zip(args).zip(&op.params) {
                if *t && !self.secret_type(&substitute(&p.ty, &vars)) {
                    self.error(
                        "E0705",
                        format!(
                            "eff [ConstTime] passes a secret-derived value to effect operation '{}::{}'",
                            d.name, op.name
                        ),
                        a.expr.span.clone(),
                        "handlers are not checked as constant time; pass only secret-typed values",
                    );
                }
            }
        }
    }

    fn edge(&mut self, callee: &str, span: Span) {
        if let Some(node) = &self.node {
            self.edges
                .entry(node.clone())
                .or_default()
                .push((callee.to_string(), span));
        }
    }

    /// The call discipline: inside a capability body, callees carry every
    /// active capability.
    fn require_carried(&mut self, name: &str, effects: &Effects, span: Span) {
        let carried = capabilities(effects);
        let missing: Vec<_> = self
            .caps
            .iter()
            .filter(|c| !carried.contains(c))
            .map(|c| c.name())
            .collect();
        if !missing.is_empty() {
            self.error(
                "E0700",
                format!(
                    "eff [{}] body calls '{name}', which does not carry {}",
                    self.caps_list(),
                    missing.join(", ")
                ),
                span,
                "a capability body may only call functions that carry the same capability",
            );
        }
    }

    /// `value` flows into a position of type `target`: a function type
    /// carrying capabilities must be inhabited by code checked under them.
    fn coerce(&mut self, value: &Expr, target: &Type) {
        let Type::Function { effects, .. } = target else {
            return;
        };
        let required = capabilities(effects);
        if required.is_empty() {
            return;
        }
        match &value.kind {
            ExprKind::Block { tail: Some(t), .. } => self.coerce(t, target),
            ExprKind::If {
                then, otherwise, ..
            } => {
                self.coerce(then, target);
                if let Some(o) = otherwise {
                    self.coerce(o, target);
                }
            }
            ExprKind::Match { arms, .. } => {
                for (_, body) in arms {
                    self.coerce(body, target);
                }
            }
            ExprKind::Closure { effects: own, .. } => {
                let own = capabilities(own);
                let missing: Vec<_> = required
                    .iter()
                    .filter(|c| !own.contains(c) && !self.caps.contains(c))
                    .map(|c| c.name())
                    .collect();
                if !missing.is_empty() {
                    self.coerce_error(value, &missing);
                }
            }
            _ => {
                let actual = match &value.kind {
                    ExprKind::Name(n) if self.local(n).is_none() => self
                        .world
                        .functions
                        .get(n)
                        .map(|f| capabilities(&f.effects)),
                    _ => match self.ty(value).as_ref() {
                        Some(Type::Function { effects, .. }) => Some(capabilities(effects)),
                        _ => None,
                    },
                };
                let lexical = matches!(&value.kind, ExprKind::Name(n) if self.local(n).is_some_and(|l| l.lexical_callable));
                let missing: Vec<_> = required
                    .iter()
                    .filter(|c| {
                        !actual.as_ref().is_some_and(|a| a.contains(c))
                            && !(lexical && self.caps.contains(c))
                    })
                    .map(|c| c.name())
                    .collect();
                if !missing.is_empty() {
                    self.coerce_error(value, &missing);
                }
            }
        }
    }
    fn coerce_error(&mut self, value: &Expr, missing: &[&str]) {
        self.error(
            "E0708",
            format!(
                "this function value is not checked under {}, which its expected type requires",
                missing.join(", ")
            ),
            value.span.clone(),
            "declare the capability on the closure (|..| -> T eff [..] { .. }) or pass a function carrying it",
        );
    }
}

fn pattern_bindings(p: &Pattern) -> Vec<String> {
    match &p.kind {
        PatternKind::Bind(n) => vec![n.clone()],
        PatternKind::Tuple(ps) => ps.iter().flat_map(pattern_bindings).collect(),
        PatternKind::Constructor { fields, .. } => fields
            .iter()
            .flat_map(|(_, p)| pattern_bindings(p))
            .collect(),
        _ => vec![],
    }
}

fn generic_vars(generics: &[Generic], ty: &Type) -> BTreeMap<String, Type> {
    match ty {
        Type::Named(_, args) => generics
            .iter()
            .zip(args)
            .map(|(g, t)| (g.name.clone(), t.clone()))
            .collect(),
        _ => BTreeMap::new(),
    }
}

fn generic_vars_from_call(world: &World, e: &Expr) -> BTreeMap<String, Type> {
    world
        .calls
        .get(&(e.span.start, e.span.end))
        .cloned()
        .unwrap_or_default()
}

fn mentions_float(t: &Type) -> bool {
    match t {
        Type::Named(n, args) => n == "f64" || args.iter().any(mentions_float),
        Type::Tuple(ts) => ts.iter().any(mentions_float),
        Type::Function { params, result, .. } => {
            params.iter().any(|(_, t)| mentions_float(t)) || mentions_float(result)
        }
        _ => false,
    }
}

fn place_root(e: &Expr) -> Option<String> {
    crate::check::Place::from_expr(e).map(|p| p.root)
}

// ---------------------------------------------------------------------------
// Verifier policy inputs.

/// Inputs for the Stage 5 verifiers built from source annotations. Plain
/// data: it deliberately does not depend on `vesper-vault`,
/// `vesper-resonance` or `vesper-kinetic`. Parameter indices are source
/// parameter positions; mapping them to Cranelift block parameters is the
/// backend hook's job.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CapabilityPolicies {
    pub schema: &'static str,
    /// Inputs for Vault's `CtPolicy`, one per `eff [ConstTime]` function.
    pub const_time: Vec<ConstTimeInput>,
    /// Inputs for Resonance's `RealtimePolicy`.
    pub realtime: RealtimeInput,
    /// Inputs for Kinetic's `DetFloatPolicy`, one per `eff [DetFloat]`
    /// function.
    pub det_float: Vec<DetFloatInput>,
}

/// Source facts for one constant-time function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConstTimeInput {
    pub function: String,
    pub span: Span,
    /// Source parameter indices whose types are secret (`CtPolicy::secret_params`).
    pub secret_params: Vec<usize>,
    /// Whether the result type is secret.
    pub secret_result: bool,
    /// Whether this function is itself a declassification gate.
    pub declassify_gate: bool,
    /// Direct callees, all of which carry ConstTime
    /// (`CtPolicy::constant_time_callees`).
    pub constant_time_callees: Vec<String>,
    /// Direct callees that are declassification gates (`CtPolicy::declassify`).
    pub declassify_callees: Vec<String>,
}

/// One realtime function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RealtimeFunction {
    pub function: String,
    pub span: Span,
    /// True when no other realtime function calls it: a `RealtimeRoot`.
    pub root: bool,
    /// The function's effect row names (realtime rows never contain the
    /// forbidden effects in an accepted program).
    pub effects: Vec<String>,
    /// `@realtime(stack_budget_bytes = ..)` has no source syntax yet.
    pub stack_budget_bytes: Option<u64>,
}

/// Source facts for Resonance's realtime confinement verifier.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RealtimeInput {
    pub functions: Vec<RealtimeFunction>,
    /// Direct call edges among realtime functions, by name. Acyclic in an
    /// accepted program.
    pub calls: Vec<(String, String)>,
    /// Effect names the source rules allow in realtime rows' place of
    /// `RealtimePolicy::allowed_effects`: everything except these.
    pub forbidden_effects: Vec<String>,
    /// The source has no loops; recursion is rejected. Backend loops are
    /// therefore compiler-introduced and must still be bounded.
    pub reject_unbounded_loops: bool,
}

/// Source facts for one bit-deterministic float function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DetFloatInput {
    pub function: String,
    pub span: Span,
    /// Callees that carry DetFloat (`DetFloatPolicy::allowed_float_calls`).
    pub allowed_float_calls: Vec<String>,
    /// The source has no explicit fused multiply-add, so none is allowed.
    pub allow_fma: bool,
}

/// [`capability_policies`] was given a program that did not check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyError {
    /// The program has this many diagnostics; policies would be unsound.
    Unchecked { diagnostics: usize },
}
impl std::fmt::Display for PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unchecked { diagnostics } => write!(
                f,
                "capability policies need a checked program; it has {diagnostics} diagnostic(s)"
            ),
        }
    }
}
impl std::error::Error for PolicyError {}

/// Builds the verifier policy inputs from the capability annotations of an
/// accepted program.
pub fn capability_policies(checked: &Checked) -> Result<CapabilityPolicies, PolicyError> {
    if !checked.diagnostics.is_empty() {
        return Err(PolicyError::Unchecked {
            diagnostics: checked.diagnostics.len(),
        });
    }
    let world = &checked.world;
    let mut out = CapabilityPolicies {
        schema: POLICY_SCHEMA,
        realtime: RealtimeInput {
            forbidden_effects: REALTIME_FORBIDDEN.iter().map(|s| s.to_string()).collect(),
            reject_unbounded_loops: true,
            ..RealtimeInput::default()
        },
        ..CapabilityPolicies::default()
    };
    let mut realtime_called = BTreeSet::new();
    for f in world.functions.values() {
        let caps = capabilities(&f.effects);
        if caps.is_empty() {
            continue;
        }
        let mut w = Walker::new(world, f, caps.clone());
        if let Some(body) = &f.body {
            w.expr(body);
        }
        let callees: BTreeSet<String> = w.direct_calls.iter().map(|(n, _)| n.clone()).collect();
        if caps.contains(&Capability::ConstTime) {
            out.const_time.push(ConstTimeInput {
                function: f.name.clone(),
                span: f.span.clone(),
                secret_params: f
                    .params
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| w.secret_type(&p.ty))
                    .map(|(i, _)| i)
                    .collect(),
                secret_result: w.secret_type(&f.result),
                declassify_gate: is_declassify_gate(&f.name),
                constant_time_callees: callees
                    .iter()
                    .filter(|c| !is_declassify_gate(c))
                    .cloned()
                    .collect(),
                declassify_callees: callees
                    .iter()
                    .filter(|c| is_declassify_gate(c))
                    .cloned()
                    .collect(),
            });
        }
        if caps.contains(&Capability::Realtime) {
            for c in &callees {
                out.realtime.calls.push((f.name.clone(), c.clone()));
                realtime_called.insert(c.clone());
            }
            out.realtime.functions.push(RealtimeFunction {
                function: f.name.clone(),
                span: f.span.clone(),
                root: false,
                effects: f.effects.instances.iter().map(|e| e.name.clone()).collect(),
                stack_budget_bytes: None,
            });
        }
        if caps.contains(&Capability::DetFloat) {
            out.det_float.push(DetFloatInput {
                function: f.name.clone(),
                span: f.span.clone(),
                allowed_float_calls: callees.into_iter().collect(),
                allow_fma: false,
            });
        }
    }
    for r in &mut out.realtime.functions {
        r.root = !realtime_called.contains(&r.function);
    }
    Ok(out)
}
