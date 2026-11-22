//! Function objects (manuscript §7.3 "Function Objects", port wave P3b lane
//! 1, #1063): `dyn Fn(A) -> R eff E`, `dyn FnMut(..)` and `dyn FnOnce(..)`
//! as second-class parameters and inside `Box[..]`.
//!
//! # Elaboration
//!
//! Function objects are trait objects. Before the `dyn` lowering of
//! [`crate::dyn_trait`], this pass turns each distinct function-object type
//! into a hidden object-safe trait with one method `call`, whose receiver is
//! `in self` (`Fn`), `inout self` (`FnMut`) or `sink self` (`FnOnce`) and
//! whose other parameters, result and effect row are the signature's. A
//! closure literal written where a function object is expected becomes a
//! hidden struct holding its captures and a conformance of that struct to
//! the trait, whose `call` body is the closure's body reading the captures
//! from `self`. Everything after that (vtables, the `{data, vtable}` pair,
//! boxing, drops, the evaluator) is the ordinary trait-object machinery.
//!
//! Captures move into the object, as for any closure (`[T-Abs]`): a
//! `FnMut` object mutates its own copy, which persists across its calls.
//!
//! # Where a closure becomes an object
//!
//! The pass runs before type checking, so it finds the expected type
//! syntactically: an argument of a function, method or path call whose
//! declared parameter is a function object, `Box::new(closure)` where a
//! `Box[dyn Fn..]` is expected (a `let` annotation, a call parameter, a
//! struct field, a function result) or written (`Box::new[dyn FnMut(..)](..)`),
//! and the element of `s.push(Box::new(..))` on a sequence of boxed
//! function objects. A capture's type comes from the parameter or `let` that
//! binds it (annotated, or a literal, struct literal, call of a declared
//! function or `Box::new[T]`); otherwise the capture must be annotated
//! (E0102).
//!
//! A call `f(args)` on a name whose type is a function object, `self.f(args)`
//! on a function-object field, and `s[i](args)` on a sequence of them are
//! calls of `call`; `x.call(args)` works on any function-object expression.
use crate::ast::*;
use std::collections::{BTreeMap, BTreeSet};

/// The three function-object traits.
pub const FN_TRAITS: [&str; 3] = ["Fn", "FnMut", "FnOnce"];
/// The method a function object is called through.
pub const CALL: &str = "call";
/// Prefix of the hidden trait of a function-object type.
pub const HIDDEN_TRAIT: &str = "__FnObject";
/// Prefix of the hidden struct of a closure lowered to an object.
pub const HIDDEN_CLOSURE: &str = "__Closure";

/// The kind and signature of a function-object type `dyn Fn(..)` (not
/// boxed): `Named(dyn, [Named(Fn, [Function{..}])])`.
pub fn fn_object(t: &Type) -> Option<(&str, &Type)> {
    match t {
        Type::Named(d, args) if d == DYN_TYPE && args.len() == 1 => match &args[0] {
            Type::Named(k, sig)
                if FN_TRAITS.contains(&k.as_str())
                    && sig.len() == 1
                    && matches!(sig[0], Type::Function { .. }) =>
            {
                Some((k.as_str(), &sig[0]))
            }
            _ => None,
        },
        _ => None,
    }
}

/// The function object a `Box[dyn Fn..]` holds.
fn boxed_fn_object(t: &Type) -> Option<(&str, &Type)> {
    match t {
        Type::Named(b, args) if b == "Box" && args.len() == 1 => fn_object(&args[0]),
        _ => None,
    }
}

/// A function object, boxed or not.
fn any_fn_object(t: &Type) -> Option<(&str, &Type)> {
    fn_object(t).or_else(|| boxed_fn_object(t))
}

fn names(e: &Effects) -> String {
    let mut v = e
        .instances
        .iter()
        .map(|i| i.name.clone())
        .collect::<Vec<_>>();
    v.extend(e.tail.clone());
    v.join(", ")
}

/// An expression whose evaluation has no effect to order: a literal or a
/// path of names, fields and literal indices.
fn trivial(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Literal(_) | ExprKind::Name(_) => true,
        ExprKind::Field(base, _) => trivial(base),
        ExprKind::Index(base, index) => trivial(base) && trivial(index),
        _ => false,
    }
}

fn mentions_fn_object(t: &Type) -> bool {
    if fn_object(t).is_some() {
        return true;
    }
    match t {
        Type::Named(_, args) | Type::Tuple(args) => args.iter().any(mentions_fn_object),
        Type::Function { params, result, .. } => {
            params.iter().any(|(_, t)| mentions_fn_object(t)) || mentions_fn_object(result)
        }
        _ => false,
    }
}

/// Whether the module writes a function-object type anywhere.
pub fn uses_fn_objects(module: &Module) -> bool {
    let mut found = false;
    let mut probe = module.clone();
    for_each_type(&mut probe, &mut |t| found |= mentions_fn_object(t));
    found
}

fn for_each_type(module: &mut Module, g: &mut dyn FnMut(&mut Type)) {
    for item in &mut module.items {
        match item {
            Item::Adt(a) => {
                for v in &mut a.variants {
                    for f in &mut v.fields {
                        g(&mut f.ty);
                    }
                }
            }
            Item::Function(f) => crate::pact_frontend::walk_function(f, g),
            Item::Impl { ty, methods, .. } => {
                g(ty);
                for m in methods {
                    crate::pact_frontend::walk_function(m, g);
                }
            }
            Item::Trait(t) => {
                for m in &mut t.methods {
                    crate::pact_frontend::walk_function(m, g);
                }
            }
            Item::Conformance(c) => {
                g(&mut c.self_ty);
                for a in &mut c.trait_ref.args {
                    g(a);
                }
                for m in &mut c.methods {
                    crate::pact_frontend::walk_function(m, g);
                }
            }
            Item::Effect(e) => {
                for op in &mut e.ops {
                    crate::pact_frontend::walk_function(op, g);
                }
            }
        }
    }
}

fn d(code: &str, msg: impl Into<String>, span: &Span) -> Diagnostic {
    Diagnostic::new(code, msg, span.clone())
}

fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Declarations the pass reads: functions and methods by name, struct
/// fields by type name.
#[derive(Default)]
struct Decls {
    functions: BTreeMap<String, Function>,
    /// Inherent and conformance methods: (type name, method) -> method.
    methods: BTreeMap<(String, String), Function>,
    fields: BTreeMap<String, BTreeMap<String, Type>>,
    /// Types declared `linear`.
    linear: BTreeSet<String>,
}

impl Decls {
    fn of(module: &Module) -> Self {
        let mut out = Self::default();
        for item in &module.items {
            match item {
                Item::Function(f) => {
                    out.functions.insert(f.name.clone(), (**f).clone());
                }
                Item::Impl { ty, methods, .. } => {
                    for m in methods {
                        out.methods
                            .insert((ty.name().to_owned(), m.name.clone()), m.clone());
                    }
                }
                Item::Conformance(c) => {
                    for m in &c.methods {
                        out.methods
                            .entry((c.self_ty.name().to_owned(), m.name.clone()))
                            .or_insert_with(|| m.clone());
                    }
                }
                Item::Adt(a) if a.mode == Mode::Linear => {
                    out.linear.insert(a.name.clone());
                    if !a.is_enum {
                        let fields = a
                            .variants
                            .iter()
                            .flat_map(|v| &v.fields)
                            .map(|f| (f.name.clone(), f.ty.clone()))
                            .collect();
                        out.fields.insert(a.name.clone(), fields);
                    }
                }
                Item::Adt(a) if !a.is_enum => {
                    let fields = a
                        .variants
                        .iter()
                        .flat_map(|v| &v.fields)
                        .map(|f| (f.name.clone(), f.ty.clone()))
                        .collect();
                    out.fields.insert(a.name.clone(), fields);
                }
                _ => {}
            }
        }
        out
    }

    /// The methods named `m` of any type, for a receiver of unknown type.
    fn methods_named(&self, m: &str) -> Vec<&Function> {
        self.methods
            .iter()
            .filter(|((_, n), _)| n == m)
            .map(|(_, f)| f)
            .collect()
    }
}

/// One closure lowered to an object.
struct Generated {
    adt: Adt,
    conformance: Conformance,
}

struct Lowering<'a> {
    decls: &'a Decls,
    /// Hidden trait name per (kind, signature).
    traits: BTreeMap<String, TraitDecl>,
    generated: Vec<Generated>,
    errors: Vec<Diagnostic>,
    next_span: usize,
    /// The source span the current generated code stands for.
    origin: Span,
    origins: Vec<(usize, Span)>,
    /// Generic parameters of the function being lowered.
    generics: BTreeSet<String>,
    /// Result type of the function being lowered.
    result: Type,
}

type Env = BTreeMap<String, Type>;

impl Lowering<'_> {
    fn span(&mut self) -> Span {
        let s = self.next_span;
        self.next_span += 2;
        self.origins.push((s, self.origin.clone()));
        s..s + 1
    }

    fn expr(&mut self, kind: ExprKind) -> Expr {
        let span = self.span();
        Expr { kind, span }
    }

    /// The hidden trait of a function-object kind and signature.
    fn hidden_trait(&mut self, kind: &str, sig: &Type) -> String {
        let Type::Function {
            params,
            result,
            effects,
            ..
        } = sig
        else {
            return String::new();
        };
        let key = format!("{kind}|{params:?}|{result:?}|{effects:?}");
        let name = format!("{HIDDEN_TRAIT}{kind}_{:016x}", fnv(&key));
        if !self.traits.contains_key(&name) {
            let receiver = match kind {
                "Fn" => Pass::In,
                "FnMut" => Pass::Inout,
                _ => Pass::Sink,
            };
            let mut ps = vec![Param {
                name: "self".into(),
                pass: receiver,
                projections: vec![],
                ty: Type::Infer,
                span: 0..0,
            }];
            for (i, (pass, t)) in params.iter().enumerate() {
                ps.push(Param {
                    name: format!("a{i}"),
                    pass: *pass,
                    projections: vec![],
                    ty: t.clone(),
                    span: 0..0,
                });
            }
            self.traits.insert(
                name.clone(),
                TraitDecl {
                    name: name.clone(),
                    public: false,
                    generics: vec![],
                    assoc: vec![],
                    methods: vec![Function {
                        name: CALL.into(),
                        fip: false,
                        generics: vec![],
                        params: ps,
                        result: (**result).clone(),
                        effects: effects.clone(),
                        body: None,
                        span: 0..0,
                    }],
                    span: 0..0,
                },
            );
        }
        name
    }

    /// The syntactic type of `e`, when the pass can tell it.
    fn type_of(&self, e: &Expr, env: &Env) -> Option<Type> {
        match &e.kind {
            ExprKind::Name(n) => env.get(n).cloned(),
            ExprKind::Field(base, f) => {
                let t = self.type_of(base, env)?;
                self.decls.fields.get(t.name())?.get(f).cloned()
            }
            ExprKind::Index(base, _) => match self.type_of(base, env)? {
                Type::Named(_, args) if args.len() == 1 => Some(args[0].clone()),
                _ => None,
            },
            _ => None,
        }
    }

    /// The type a `let` without an annotation binds, when it is evident.
    fn infer(&self, e: &Expr, env: &Env) -> Type {
        match &e.kind {
            ExprKind::Literal(Literal::Int(_)) => Type::named("i64"),
            ExprKind::Literal(Literal::Bool(_)) => Type::named("bool"),
            ExprKind::Literal(Literal::Float(_)) => Type::named("f64"),
            ExprKind::Literal(Literal::String(_)) => Type::named("String"),
            ExprKind::Struct { name, .. } if self.decls.fields.contains_key(name) => {
                Type::Named(name.clone(), vec![])
            }
            ExprKind::Call { callee, types, .. } => match &callee.kind {
                ExprKind::Name(n) if n == "Box::new" && types.len() == 1 => {
                    Type::Named("Box".into(), types.clone())
                }
                ExprKind::Name(n) if n.ends_with("::new") && types.len() == 1 => {
                    Type::Named(n.trim_end_matches("::new").into(), types.clone())
                }
                ExprKind::Name(n) if !env.contains_key(n) => self
                    .decls
                    .functions
                    .get(n)
                    .filter(|f| f.generics.is_empty())
                    .map(|f| f.result.clone())
                    .unwrap_or(Type::Infer),
                _ => Type::Infer,
            },
            _ => self.type_of(e, env).unwrap_or(Type::Infer),
        }
    }

    /// The declared parameter types of the call `callee(args)`, aligned
    /// with its arguments.
    fn callee_params(&self, callee: &Expr, env: &Env) -> Option<Vec<Type>> {
        match &callee.kind {
            ExprKind::Name(n) if !env.contains_key(n) => {
                if let Some(f) = self.decls.functions.get(n) {
                    return Some(f.params.iter().map(|p| p.ty.clone()).collect());
                }
                let (ty, m) = n.rsplit_once("::")?;
                let f = self.decls.methods.get(&(ty.to_owned(), m.to_owned()))?;
                Some(f.params.iter().map(|p| p.ty.clone()).collect())
            }
            ExprKind::Field(recv, m) => {
                let recv_ty = match self.infer(recv, env) {
                    Type::Infer => None,
                    t => Some(t),
                };
                let f = match recv_ty {
                    Some(t) => self
                        .decls
                        .methods
                        .get(&(t.name().to_owned(), m.clone()))?
                        .clone(),
                    None => {
                        let all = self.decls.methods_named(m);
                        let first = (*all.first()?).clone();
                        // Every candidate must agree on its function-object
                        // parameters, type for type.
                        let shape = |f: &Function| {
                            f.params
                                .iter()
                                .map(|p| any_fn_object(&p.ty).map(|_| p.ty.clone()))
                                .collect::<Vec<_>>()
                        };
                        if all.iter().any(|f| shape(f) != shape(&first)) {
                            return None;
                        }
                        first
                    }
                };
                Some(f.params.iter().skip(1).map(|p| p.ty.clone()).collect())
            }
            _ => None,
        }
    }

    fn function(&mut self, f: &mut Function, self_ty: Option<&Type>) {
        let mut env = Env::new();
        for p in &f.params {
            let ty = match (&p.ty, self_ty) {
                (Type::Infer, Some(s)) if p.name == "self" => s.clone(),
                (Type::Named(n, a), Some(s)) if n == "Self" && a.is_empty() => s.clone(),
                (t, _) => t.clone(),
            };
            env.insert(p.name.clone(), ty);
        }
        if let Some(s) = self_ty {
            env.entry("self".into()).or_insert_with(|| s.clone());
        }
        self.generics = f.generics.iter().map(|g| g.name.clone()).collect();
        self.result = f.result.clone();
        let result = f.result.clone();
        if let Some(body) = &mut f.body {
            self.lower(body, &mut env, Some(&result));
        }
    }

    /// Lowers `e` where a value of `expected` is wanted (when known).
    fn lower(&mut self, e: &mut Expr, env: &mut Env, expected: Option<&Type>) {
        if let Some(t) = expected
            && let Some((kind, sig)) = fn_object(t)
            && matches!(e.kind, ExprKind::Closure { .. })
        {
            let (kind, sig) = (kind.to_owned(), sig.clone());
            self.coerce(e, env, &kind, &sig);
            return;
        }
        if let ExprKind::Call {
            callee,
            types,
            args,
        } = &mut e.kind
            && matches!(&callee.kind, ExprKind::Name(n) if n == crate::fip::BOX_NEW && !env.contains_key(n))
            && args.len() == 1
            && matches!(args[0].expr.kind, ExprKind::Closure { .. })
        {
            let object = match types.first() {
                Some(t) if fn_object(t).is_some() => Some(t.clone()),
                Some(_) => None,
                None => expected.and_then(boxed_fn_object).map(|_| match expected {
                    Some(Type::Named(_, a)) => a[0].clone(),
                    _ => Type::Infer,
                }),
            };
            if let Some(object) = object
                && let Some((kind, sig)) = fn_object(&object)
            {
                let (kind, sig) = (kind.to_owned(), sig.clone());
                self.coerce(&mut args[0].expr, env, &kind, &sig);
                if types.is_empty() {
                    types.push(object);
                }
                return;
            }
        }
        match &mut e.kind {
            ExprKind::Literal(_) | ExprKind::Name(_) => {}
            ExprKind::Tuple(es) => {
                let parts = match expected {
                    Some(Type::Tuple(ts)) if ts.len() == es.len() => ts.clone(),
                    _ => vec![Type::Infer; es.len()],
                };
                for (x, t) in es.iter_mut().zip(&parts) {
                    self.lower(x, env, Some(t));
                }
            }
            ExprKind::Struct { name, fields } => {
                let declared = self.decls.fields.get(name).cloned().unwrap_or_default();
                for (f, x) in fields {
                    let t = declared.get(f).cloned();
                    self.lower(x, env, t.as_ref());
                }
            }
            ExprKind::Block { statements, tail } => {
                let mut scope = env.clone();
                for s in statements.iter_mut() {
                    match s {
                        Statement::Let { pat, ty, value, .. } => {
                            let want = (*ty != Type::Infer).then(|| ty.clone());
                            self.lower(value, &mut scope, want.as_ref());
                            let bound = match want {
                                Some(t) => t,
                                None => self.infer(value, &scope),
                            };
                            bind(pat, &bound, &mut scope);
                        }
                        Statement::Expr(x) => self.lower(x, &mut scope, None),
                    }
                }
                if let Some(t) = tail {
                    self.lower(t, &mut scope, expected);
                }
            }
            ExprKind::Call { .. } => self.call(e, env),
            ExprKind::Field(b, _) | ExprKind::Try(b) | ExprKind::Unary(_, b) => {
                self.lower(b, env, None)
            }
            ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) => {
                self.lower(a, env, None);
                self.lower(b, env, None);
            }
            ExprKind::Assign(_, a, b) => {
                let t = self.type_of(a, env);
                self.lower(a, env, None);
                self.lower(b, env, t.as_ref());
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                self.lower(condition, env, None);
                self.lower(then, env, expected);
                if let Some(o) = otherwise {
                    self.lower(o, env, expected);
                }
            }
            ExprKind::Match { value, arms } => {
                self.lower(value, env, None);
                for (p, a) in arms {
                    let mut scope = env.clone();
                    bind(p, &Type::Infer, &mut scope);
                    self.lower(a, &mut scope, expected);
                }
            }
            ExprKind::Closure { params, body, .. } => {
                let mut scope = env.clone();
                for p in params.iter() {
                    scope.insert(p.name.clone(), p.ty.clone());
                }
                self.lower(body, &mut scope, None);
            }
            ExprKind::Handle {
                state,
                body,
                ret,
                ops,
                ..
            } => {
                if let Some((_, s)) = state {
                    self.lower(s, env, None);
                }
                self.lower(body, env, None);
                for c in std::iter::once(&mut **ret).chain(ops.iter_mut()) {
                    let mut scope = env.clone();
                    for p in &c.pats {
                        bind(p, &Type::Infer, &mut scope);
                    }
                    self.lower(&mut c.body, &mut scope, None);
                }
            }
            ExprKind::For { pat, iter, body } => {
                self.lower(iter, env, None);
                let mut scope = env.clone();
                let t = if matches!(iter.kind, ExprKind::Range { .. }) {
                    Type::named("i64")
                } else {
                    Type::Infer
                };
                bind(pat, &t, &mut scope);
                self.lower(body, &mut scope, None);
            }
            ExprKind::Return(Some(v)) => {
                let result = self.result.clone();
                self.lower(v, env, Some(&result));
            }
            k => {
                for c in k.control_children_mut().into_iter().flatten() {
                    self.lower(c, env, None);
                }
            }
        }
    }

    /// A call: closure arguments for function-object parameters become
    /// objects, and a call of a function object becomes a call of `call`.
    fn call(&mut self, e: &mut Expr, env: &mut Env) {
        let ExprKind::Call { callee, args, .. } = &mut e.kind else {
            return;
        };
        let params = self.callee_params(callee, env);
        let call_span = e.span.clone();
        // `f(x)` on a function object, `self.f(x)` on a function-object
        // field and `s[i](x)` on a sequence of them call `call`.
        let callee_object = match &callee.kind {
            ExprKind::Name(_) | ExprKind::Index(..) => self
                .type_of(callee, env)
                .is_some_and(|t| any_fn_object(&t).is_some()),
            // A field named `call` is a callback too; `f.call(x)` on the
            // object itself is not a field access, so it is left alone.
            ExprKind::Field(recv, m) => {
                let has_method = self.type_of(recv, env).is_some_and(|t| {
                    self.decls
                        .methods
                        .contains_key(&(t.name().to_owned(), m.clone()))
                });
                !has_method
                    && self
                        .type_of(callee, env)
                        .is_some_and(|t| any_fn_object(&t).is_some())
            }
            _ => false,
        };
        // `s.push(Box::new(closure))` on a sequence of boxed objects.
        let push_element = match &callee.kind {
            ExprKind::Field(recv, m) if m == "push" => match self.type_of(recv, env) {
                Some(Type::Named(_, a)) if a.len() == 1 && boxed_fn_object(&a[0]).is_some() => {
                    Some(a[0].clone())
                }
                _ => None,
            },
            _ => None,
        };
        self.lower(callee, env, None);
        self.origin = call_span.clone();
        let mut hoisted = vec![];
        let wants = (0..args.len())
            .map(|i| {
                push_element
                    .clone()
                    .or_else(|| params.as_ref().and_then(|ps| ps.get(i).cloned()))
            })
            .collect::<Vec<_>>();
        let lends = args
            .iter()
            .zip(&wants)
            .map(|(a, want)| {
                a.pass == Pass::Inout
                    && matches!(a.expr.kind, ExprKind::Closure { .. })
                    && want.as_ref().is_some_and(|t| fn_object(t).is_some())
            })
            .collect::<Vec<_>>();
        let last_lend = lends.iter().rposition(|&l| l);
        for (i, a) in args.iter_mut().enumerate() {
            let want = wants[i].clone();
            let lend_mut = lends[i];
            self.lower(&mut a.expr, env, want.as_ref());
            self.origin = call_span.clone();
            if !lend_mut
                && a.pass != Pass::Inout
                && last_lend.is_some_and(|l| i < l)
                && !trivial(&a.expr)
            {
                // An argument before a hoisted closure is evaluated first
                // too, so the closure captures what it left behind.
                let name = format!("$arg{}", a.expr.span.start);
                let value = std::mem::replace(&mut a.expr.kind, ExprKind::Name(name.clone()));
                let value = Expr {
                    kind: value,
                    span: a.expr.span.clone(),
                };
                a.expr.span = self.span();
                let pat_span = self.span();
                hoisted.push(Statement::Let {
                    pat: Pattern {
                        kind: PatternKind::Bind(name),
                        span: pat_span,
                    },
                    ty: Type::Infer,
                    value,
                    mutable: false,
                });
                continue;
            }
            if lend_mut {
                // An `inout` lend needs a place: the object is a local of
                // the call's own block.
                let name = format!("$closure{}", a.expr.span.start);
                let value = std::mem::replace(&mut a.expr.kind, ExprKind::Name(name.clone()));
                let value = Expr {
                    kind: value,
                    span: a.expr.span.clone(),
                };
                a.expr.span = self.span();
                let pat_span = self.span();
                hoisted.push(Statement::Let {
                    pat: Pattern {
                        kind: PatternKind::Bind(name),
                        span: pat_span,
                    },
                    ty: Type::Infer,
                    value,
                    mutable: true,
                });
            }
        }
        self.origin = call_span.clone();
        if callee_object {
            let old = std::mem::replace(
                &mut **callee,
                Expr {
                    kind: ExprKind::Literal(Literal::Bool(false)),
                    span: 0..0,
                },
            );
            let span = self.span();
            **callee = Expr {
                kind: ExprKind::Field(Box::new(old), CALL.into()),
                span,
            };
        }
        if !hoisted.is_empty() {
            let call = std::mem::replace(
                e,
                Expr {
                    kind: ExprKind::Literal(Literal::Bool(false)),
                    span: 0..0,
                },
            );
            let span = self.span();
            *e = Expr {
                kind: ExprKind::Block {
                    statements: hoisted,
                    tail: Some(Box::new(call)),
                },
                span,
            };
        }
    }

    /// Replaces the closure literal `e` by a value of a hidden struct that
    /// conforms to the hidden trait of `kind` and `sig`.
    fn coerce(&mut self, e: &mut Expr, env: &mut Env, kind: &str, sig: &Type) {
        let ExprKind::Closure {
            params,
            mode,
            body,
            result: written_result,
            effects: written_effects,
            infer_effects,
        } = &mut e.kind
        else {
            return;
        };
        let Type::Function {
            params: sig_params,
            result,
            effects,
            ..
        } = sig
        else {
            return;
        };
        let span = e.span.clone();
        self.origin = span.clone();
        if params.len() != sig_params.len() {
            self.errors.push(d(
                "E0207",
                format!(
                    "the closure takes {} parameters; `dyn {kind}` takes {}",
                    params.len(),
                    sig_params.len()
                ),
                &span,
            ));
            return;
        }
        // What the closure writes must agree with the object's signature:
        // the signature replaces it below.
        for (p, (pass, t)) in params.iter().zip(sig_params) {
            if p.ty != Type::Infer && p.ty != *t {
                self.errors.push(d(
                    "E0200",
                    format!(
                        "the closure's parameter '{}' is written `{}`; `dyn {kind}` passes `{t}`",
                        p.name, p.ty
                    ),
                    &p.span,
                ));
                return;
            }
            if p.pass != Pass::In && p.pass != *pass {
                self.errors.push(d(
                    "E0200",
                    format!(
                        "the closure takes '{}' by `{:?}`; `dyn {kind}` passes it by `{pass:?}`",
                        p.name, p.pass
                    ),
                    &p.span,
                ));
                return;
            }
        }
        if *written_result != Type::Infer && *written_result != **result {
            self.errors.push(d(
                "E0200",
                format!("the closure returns `{written_result}`; `dyn {kind}` returns `{result}`"),
                &span,
            ));
            return;
        }
        // A written effect row restricts the body; it must fit the object's.
        let body_effects = if *infer_effects {
            effects.clone()
        } else {
            let fits = written_effects.tail.is_none()
                && written_effects
                    .instances
                    .iter()
                    .all(|w| effects.instances.iter().any(|e| e == w));
            if !fits {
                self.errors.push(d(
                    "E0401",
                    format!(
                        "the closure's written effects [{}] are not within those of `dyn {kind}`: [{}]",
                        names(written_effects),
                        names(effects)
                    ),
                    &span,
                ));
                return;
            }
            written_effects.clone()
        };
        if matches!(mode, Some(Mode::Linear)) && kind != "FnOnce" {
            self.errors.push(d(
                "E0310",
                format!(
                    "a linear closure can be called once: it is a `dyn FnOnce`, not a `dyn {kind}`"
                ),
                &span,
            ));
        }
        // Lower nested objects inside the body first, with the closure's
        // parameters in scope.
        let mut inner = env.clone();
        for (p, (_, t)) in params.iter().zip(sig_params) {
            inner.insert(p.name.clone(), t.clone());
        }
        let saved_result = std::mem::replace(&mut self.result, (**result).clone());
        self.lower(body, &mut inner, Some(result));
        self.result = saved_result;
        let mut free = crate::check::free_names(body);
        for p in params.iter() {
            free.remove(&p.name);
        }
        let captures: Vec<(String, Type)> = free
            .into_iter()
            .filter_map(|n| env.get(&n).map(|t| (n, t.clone())))
            .collect();
        for (n, t) in &captures {
            if *t == Type::Infer || contains_infer(t) {
                self.errors.push(d(
                    "E0102",
                    format!(
                        "the closure captures '{n}', whose type is not written where it is bound; annotate it (`let {n}: T = ..`) to lower the closure to `dyn {kind}`"
                    ),
                    &span,
                ));
                return;
            }
            if self.decls.linear.contains(t.name()) {
                let (code, why) = if kind == "FnOnce" {
                    (
                        "E0205",
                        "a `dyn FnOnce` object may be dropped uncalled, so it cannot own a linear value",
                    )
                } else {
                    (
                        "E0310",
                        "a closure capturing a linear value can be called once only: it is never a `dyn Fn` or `dyn FnMut`",
                    )
                };
                self.errors.push(d(
                    code,
                    format!("the closure captures the linear '{n}' ({t}); {why}"),
                    &span,
                ));
                return;
            }
            if mentions_any(t, &self.generics)
                || sig_params
                    .iter()
                    .any(|(_, s)| mentions_any(s, &self.generics))
                || mentions_any(result, &self.generics)
            {
                self.errors.push(d(
                    "E0633",
                    format!(
                        "a closure lowered to `dyn {kind}` cannot mention the type parameters of its enclosing function yet ('{n}': {t})"
                    ),
                    &span,
                ));
                return;
            }
        }
        let mut erased = sig.clone();
        erase(&mut erased, self);
        let trait_name = self.hidden_trait(kind, &erased);
        let index = self.generated.len();
        let struct_name = format!("{HIDDEN_CLOSURE}{index}_{}", span.start);
        let field = |n: &str| {
            if n == "self" {
                "self_".to_owned()
            } else {
                n.to_owned()
            }
        };
        let adt = Adt {
            name: struct_name.clone(),
            mode: Mode::Affine,
            generics: vec![],
            variants: vec![Variant {
                name: struct_name.clone(),
                fields: captures
                    .iter()
                    .map(|(n, t)| Field {
                        name: field(n),
                        ty: t.clone(),
                        modality: None,
                        temperature: None,
                    })
                    .collect(),
                named: true,
            }],
            is_enum: false,
            tile_width: None,
            span: self.span(),
        };
        let receiver = match kind {
            "Fn" => Pass::In,
            "FnMut" => Pass::Inout,
            _ => Pass::Sink,
        };
        let mut method_params = vec![Param {
            name: "self".into(),
            pass: receiver,
            projections: vec![],
            ty: Type::Infer,
            span: self.span(),
        }];
        for (p, (pass, t)) in params.iter().zip(sig_params) {
            method_params.push(Param {
                name: p.name.clone(),
                pass: *pass,
                projections: vec![],
                ty: t.clone(),
                span: p.span.clone(),
            });
        }
        let body = std::mem::replace(
            &mut **body,
            Expr {
                kind: ExprKind::Tuple(vec![]),
                span: 0..0,
            },
        );
        let body = if receiver == Pass::Sink {
            // `FnOnce`: the object is consumed whole, binding each capture
            // under its own name (`self` as `$self`).
            let renamed: BTreeMap<String, String> = captures
                .iter()
                .filter(|(n, _)| n == "self")
                .map(|(n, _)| (n.clone(), "$self".to_owned()))
                .collect();
            let mut body = body;
            self.substitute(&mut body, &renamed, false);
            let fields = captures
                .iter()
                .map(|(n, _)| {
                    let span = self.span();
                    (
                        field(n),
                        Pattern {
                            kind: PatternKind::Bind(renamed.get(n).cloned().unwrap_or(n.clone())),
                            span,
                        },
                    )
                })
                .collect();
            let pat_span = self.span();
            let self_expr = self.expr(ExprKind::Name("self".into()));
            let span = self.span();
            Expr {
                kind: ExprKind::Block {
                    statements: vec![Statement::Let {
                        pat: Pattern {
                            kind: PatternKind::Constructor {
                                name: struct_name.clone(),
                                fields,
                                named: true,
                            },
                            span: pat_span,
                        },
                        ty: Type::Infer,
                        value: self_expr,
                        // The object owns its captures: a one-shot body may
                        // mutate them before it consumes them.
                        mutable: true,
                    }],
                    tail: Some(Box::new(body)),
                },
                span,
            }
        } else {
            let map = captures
                .iter()
                .map(|(n, _)| (n.clone(), field(n)))
                .collect();
            let mut body = body;
            self.substitute(&mut body, &map, true);
            body
        };
        let method = Function {
            name: CALL.into(),
            fip: false,
            generics: vec![],
            params: method_params,
            result: (**result).clone(),
            effects: body_effects,
            body: Some(body),
            span: self.span(),
        };
        let conformance = Conformance {
            kind: ConformanceKind::Canonical,
            generics: vec![],
            trait_ref: TraitRef {
                package: None,
                name: trait_name,
                args: vec![],
                span: self.span(),
            },
            self_ty: Type::Named(struct_name.clone(), vec![]),
            where_clauses: vec![],
            assoc: vec![],
            methods: vec![method],
            span: self.span(),
        };
        self.generated.push(Generated { adt, conformance });
        let fields = captures
            .iter()
            .map(|(n, _)| (field(n), self.expr(ExprKind::Name(n.clone()))))
            .collect();
        e.kind = ExprKind::Struct {
            name: struct_name,
            fields,
        };
    }

    /// Rewrites the free uses of `map`'s names in `e`: to `self.field`
    /// (`fields`), or to another name.
    fn substitute(&mut self, e: &mut Expr, map: &BTreeMap<String, String>, fields: bool) {
        if map.is_empty() {
            return;
        }
        match &mut e.kind {
            ExprKind::Name(n) => {
                if let Some(to) = map.get(n) {
                    if fields {
                        let base = self.expr(ExprKind::Name("self".into()));
                        e.kind = ExprKind::Field(Box::new(base), to.clone());
                    } else {
                        *n = to.clone();
                    }
                }
            }
            ExprKind::Literal(_) => {}
            ExprKind::Tuple(es) => es.iter_mut().for_each(|x| self.substitute(x, map, fields)),
            ExprKind::Struct { fields: fs, .. } => {
                fs.iter_mut()
                    .for_each(|(_, x)| self.substitute(x, map, fields));
            }
            ExprKind::Block { statements, tail } => {
                let mut live = map.clone();
                for s in statements.iter_mut() {
                    match s {
                        Statement::Let { pat, value, .. } => {
                            self.substitute(value, &live, fields);
                            let mut bound = BTreeSet::new();
                            pattern_names(pat, &mut bound);
                            live.retain(|k, _| !bound.contains(k));
                        }
                        Statement::Expr(x) => self.substitute(x, &live, fields),
                    }
                }
                if let Some(t) = tail {
                    self.substitute(t, &live, fields);
                }
            }
            ExprKind::Call { callee, args, .. } => {
                // A path or function name is not a capture.
                if !matches!(callee.kind, ExprKind::Name(_)) {
                    self.substitute(callee, map, fields);
                } else if let ExprKind::Name(n) = &callee.kind
                    && map.contains_key(n)
                {
                    self.substitute(callee, map, fields);
                }
                for a in args {
                    self.substitute(&mut a.expr, map, fields);
                }
            }
            ExprKind::Field(b, _) | ExprKind::Try(b) | ExprKind::Unary(_, b) => {
                self.substitute(b, map, fields)
            }
            ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Assign(_, a, b) => {
                self.substitute(a, map, fields);
                self.substitute(b, map, fields);
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                self.substitute(condition, map, fields);
                self.substitute(then, map, fields);
                if let Some(o) = otherwise {
                    self.substitute(o, map, fields);
                }
            }
            ExprKind::Match { value, arms } => {
                self.substitute(value, map, fields);
                for (p, a) in arms {
                    let mut bound = BTreeSet::new();
                    pattern_names(p, &mut bound);
                    let live = map
                        .iter()
                        .filter(|(k, _)| !bound.contains(*k))
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    self.substitute(a, &live, fields);
                }
            }
            ExprKind::Closure { params, body, .. } => {
                let live = map
                    .iter()
                    .filter(|(k, _)| !params.iter().any(|p| &p.name == *k))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                self.substitute(body, &live, fields);
            }
            ExprKind::Handle {
                state,
                body,
                ret,
                ops,
                ..
            } => {
                if let Some((_, s)) = state {
                    self.substitute(s, map, fields);
                }
                self.substitute(body, map, fields);
                for c in std::iter::once(&mut **ret).chain(ops.iter_mut()) {
                    let mut bound = BTreeSet::new();
                    for p in &c.pats {
                        pattern_names(p, &mut bound);
                    }
                    let live = map
                        .iter()
                        .filter(|(k, _)| !bound.contains(*k))
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    self.substitute(&mut c.body, &live, fields);
                }
            }
            ExprKind::For { pat, iter, body } => {
                self.substitute(iter, map, fields);
                let mut bound = BTreeSet::new();
                pattern_names(pat, &mut bound);
                let live = map
                    .iter()
                    .filter(|(k, _)| !bound.contains(*k))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                self.substitute(body, &live, fields);
            }
            k => {
                for c in k.control_children_mut().into_iter().flatten() {
                    self.substitute(c, map, fields);
                }
            }
        }
    }
}

fn contains_infer(t: &Type) -> bool {
    match t {
        Type::Infer => true,
        Type::Named(_, a) | Type::Tuple(a) => a.iter().any(contains_infer),
        _ => false,
    }
}

fn mentions_any(t: &Type, names: &BTreeSet<String>) -> bool {
    if names.is_empty() {
        return false;
    }
    match t {
        Type::Named(n, a) => {
            (a.is_empty() && names.contains(n)) || a.iter().any(|t| mentions_any(t, names))
        }
        Type::Tuple(a) => a.iter().any(|t| mentions_any(t, names)),
        Type::Function { params, result, .. } => {
            params.iter().any(|(_, t)| mentions_any(t, names)) || mentions_any(result, names)
        }
        _ => false,
    }
}

fn pattern_names(p: &Pattern, out: &mut BTreeSet<String>) {
    match &p.kind {
        PatternKind::Bind(n) => {
            out.insert(n.clone());
        }
        PatternKind::Tuple(ps) => ps.iter().for_each(|p| pattern_names(p, out)),
        PatternKind::Constructor { fields, .. } => {
            fields.iter().for_each(|(_, p)| pattern_names(p, out));
        }
        _ => {}
    }
}

/// Binds the names of `p` in `env`: a plain binding at `ty`, anything else
/// at an unknown type.
fn bind(p: &Pattern, ty: &Type, env: &mut Env) {
    match &p.kind {
        PatternKind::Bind(n) => {
            env.insert(n.clone(), ty.clone());
        }
        PatternKind::Tuple(ps) => {
            for (i, q) in ps.iter().enumerate() {
                let t = match ty {
                    Type::Tuple(ts) => ts.get(i).cloned().unwrap_or(Type::Infer),
                    _ => Type::Infer,
                };
                bind(q, &t, env);
            }
        }
        _ => {
            let mut names = BTreeSet::new();
            pattern_names(p, &mut names);
            for n in names {
                env.insert(n, Type::Infer);
            }
        }
    }
}

/// Replaces every function-object type by its hidden trait's object type.
fn erase(t: &mut Type, traits: &mut Lowering<'_>) {
    if let Some((kind, sig)) = fn_object(t) {
        let (kind, mut sig) = (kind.to_owned(), sig.clone());
        if let Type::Function { params, result, .. } = &mut sig {
            for (_, p) in params.iter_mut() {
                erase(p, traits);
            }
            erase(result, traits);
        }
        let name = traits.hidden_trait(&kind, &sig);
        *t = Type::Named(DYN_TYPE.into(), vec![Type::Named(name, vec![])]);
        return;
    }
    match t {
        Type::Named(_, a) | Type::Tuple(a) => a.iter_mut().for_each(|t| erase(t, traits)),
        Type::Function { params, result, .. } => {
            params.iter_mut().for_each(|(_, t)| erase(t, traits));
            erase(result, traits);
        }
        _ => {}
    }
}

/// Lowers every function object of `module` (module docs).
/// A module with its function objects lowered, and where its generated
/// code came from.
pub struct Lowered {
    pub module: Module,
    /// The first synthetic span start's lower bound (`serde_json`'s length
    /// of the module plus 1,000,000).
    pub base: usize,
    /// End of the synthetic spans (the checker's own generated spans follow).
    pub end: usize,
    /// (first synthetic span start, source span), in increasing order.
    pub origins: Vec<(usize, Span)>,
}

impl Lowered {
    /// Reports diagnostics at generated code at the source it came from.
    pub fn relocate(&self, diagnostics: &mut [Diagnostic]) {
        let at = |span: &mut Span| {
            if span.start < self.base || span.start >= self.end {
                return;
            }
            let i = self.origins.partition_point(|(s, _)| *s <= span.start);
            if let Some((_, origin)) = i.checked_sub(1).and_then(|i| self.origins.get(i)) {
                *span = origin.clone();
            }
        };
        for d in diagnostics {
            at(&mut d.span);
            for r in &mut d.related {
                at(&mut r.span);
            }
        }
    }
}

pub fn lower(module: &Module) -> Result<Lowered, Vec<Diagnostic>> {
    let decls = Decls::of(module);
    let base = serde_json::to_string(module).map(|s| s.len()).unwrap_or(0) + 1_000_000;
    let mut l = Lowering {
        decls: &decls,
        traits: BTreeMap::new(),
        generated: vec![],
        errors: vec![],
        next_span: base.next_multiple_of(2),
        origin: 0..0,
        origins: vec![],
        generics: BTreeSet::new(),
        result: Type::unit(),
    };
    let mut out = module.clone();
    for item in &mut out.items {
        match item {
            Item::Function(f) => l.function(f, None),
            Item::Impl { ty, methods, .. } => {
                let ty = ty.clone();
                for m in methods {
                    l.function(m, Some(&ty));
                }
            }
            Item::Conformance(c) => {
                let ty = c.self_ty.clone();
                for m in &mut c.methods {
                    l.function(m, Some(&ty));
                }
            }
            _ => {}
        }
    }
    for g in std::mem::take(&mut l.generated) {
        out.items.push(Item::Adt(g.adt));
        out.items.push(Item::Conformance(g.conformance));
    }
    // Every function-object type, in source and generated items alike,
    // becomes its hidden trait's object type.
    for_each_type(&mut out, &mut |t| erase(t, &mut l));
    if !l.errors.is_empty() {
        return Err(l.errors);
    }
    let traits = std::mem::take(&mut l.traits);
    let mut hidden: Vec<Item> = traits.into_values().map(Item::Trait).collect();
    hidden.extend(out.items);
    out.items = hidden;
    Ok(Lowered {
        module: out,
        base,
        end: l.next_span,
        origins: l.origins,
    })
}
