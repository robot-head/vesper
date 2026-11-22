//! Second-class trait objects: `in dyn`, `inout dyn` and `sink dyn`
//! parameters (manuscript §7.1, port wave P3a lane 1, survey feature F7b).
//!
//! # Elaboration
//!
//! The frontend ([`lower`]) turns every parameter `p: dyn Trait[args]` into
//! a hidden type parameter `__dyn__p` with the bound `dyn$p: Trait[args]`, so
//! the body is checked with the ordinary bounded-generic rules: a method
//! call on `p` resolves to the bound's Pact assumption. Unlike a static
//! bound, a `dyn` parameter is never monomorphized. Its calls name a
//! *vtable stub* `vtable$Trait$method` (bodiless, kept in the World), and
//! each call that passes a concrete `T` where a `dyn` parameter is expected
//! selects the conformance `T: Trait[args]` at that call (under its
//! lexical `use model` scopes, as for a static bound) and records one
//! [`VTable`] per (object type, `T`). A `dyn` function is compiled once.
//!
//! # Second class
//!
//! A `dyn` value never escapes: its name may appear only as the receiver
//! of a method call, the first argument of a trait path call, or a direct
//! argument for another `dyn` parameter ([`check_forwards`]). It is never
//! bound, stored in a field, returned, captured or passed to a non-`dyn`
//! parameter, and `dyn` is written nowhere but as the whole type of an
//! `in`, `inout` or `sink` parameter of a function or inherent method.
//!
//! # Object safety
//!
//! A trait is usable as `dyn` when it has no associated types and every
//! method has no type parameters, takes `self` first by `in`, `inout` or
//! `sink`, mentions `Self` nowhere else, and has a closed effect row (after
//! the object's trait arguments are substituted). Methods may perform
//! handler effects (#1063): native code specializes those into the code
//! that reaches the handler, so such a slot holds no function and a call
//! through it dispatches on the vtable's identity over the program's
//! vtables for the object type (`dyn_call_effectful` in
//! [`crate::native`]).
//!
//! # Native lowering
//!
//! Native code ([`crate::native`]) erases each `__dyn__p` to the object type
//! `dyn Trait[args]`, whose layout is the 16-byte pair `{data, vtable}`.
//! The vtable is a read-only data object, emitted once per (object type,
//! `T`) in a compilation:
//!
//! | word | contents |
//! |---|---|
//! | 0 | drop: `fn(data)` that drops a `T` in place, or 0 when `T` owns nothing |
//! | 1 | move: 0, meaning a bitwise move of `size` bytes (every Stage 1 type is bitwise movable) |
//! | 2 | size of `T` in bytes |
//! | 3 | alignment of `T` in bytes |
//! | 4 + i | method `i` in trait declaration order: `fn(data, args...)` |
//!
//! `data` points at the caller's value (an `in` or `inout` argument's place,
//! or the moved value of a `sink` argument), so the object allocates
//! nothing. A slot takes `self` by address whatever its pass mode; a
//! method whose `self` is a by-value scalar is reached through a generated
//! adapter that loads it. A `sink dyn` parameter the callee does not
//! consume is dropped through word 0 once; consuming it (a `sink self`
//! method or a `sink dyn` argument) empties the parameter.
//!
//! # Boxed objects
//!
//! `Box[dyn Trait[args]]` (manuscript §7.3, #1057) is a first-class owned
//! type, allowed wherever a type is written. `Box::new(value)` expecting it
//! (or `Box::new[dyn Trait](value)`) is the coercion: the frontend records
//! one vtable per (object type, value type) exactly as for a `dyn` argument.
//! Calls on a boxed receiver resolve to the vtable stubs; lending one to a
//! `dyn` parameter reuses its vtable. Natively the box is one word, the
//! address of a block `{vtable, value}` ([`BOXED_HEADER`]); a call passes
//! the value's address to the slot, and a drop calls the vtable's drop word
//! and frees the block. The evaluator keeps the value in a shared cell
//! beside its vtable (`runtime::Value::Object`).
use crate::ast::*;
use std::collections::{BTreeMap, BTreeSet};

/// Prefix of the hidden type parameter of a `dyn` parameter: `__dyn__p`.
pub const DYN_GENERIC: &str = "__dyn__";
/// Prefix of a vtable stub, `vtable$Trait$method`.
pub const VTABLE_STUB: &str = "vtable$";
/// Bytes of the vtable header (drop, move, size, align) before the methods.
pub const VTABLE_HEADER: i32 = 32;
/// Bytes of a boxed object's block before its value: the vtable address.
pub const BOXED_HEADER: i64 = 8;

/// Whether `name` is the hidden type parameter of a `dyn` parameter.
pub fn is_dyn_generic(name: &str) -> bool {
    name.starts_with(DYN_GENERIC)
}

/// The object type `dyn trait_ty`.
pub fn object(trait_ty: Type) -> Type {
    Type::Named(DYN_TYPE.into(), vec![trait_ty])
}

/// The trait of an object type `dyn Trait[args]`.
pub fn object_trait(t: &Type) -> Option<&Type> {
    match t {
        Type::Named(n, a) if n == DYN_TYPE && a.len() == 1 => Some(&a[0]),
        _ => None,
    }
}

/// Whether `t` is an object type.
pub fn is_object(t: &Type) -> bool {
    object_trait(t).is_some()
}

/// `dyn` metadata of a checked World (empty without `dyn`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DynObjects {
    /// The `dyn` parameters of each function (World name, instances
    /// included): hidden type parameter to its trait `Named(Trait, args)`,
    /// whose arguments may mention the function's other type parameters.
    pub params: BTreeMap<String, BTreeMap<String, Type>>,
    /// Vtable stub to (trait, method slot).
    pub stubs: BTreeMap<String, (String, usize)>,
    /// Vtables the program needs.
    pub vtables: Vec<VTable>,
}

/// The vtable of one (object type, implementing type) pair.
#[derive(Debug, Clone, PartialEq)]
pub struct VTable {
    /// The closed object type `dyn Trait[args]`.
    pub object: Type,
    /// The implementing type.
    pub self_ty: Type,
    /// The conformance function of each method, in slot order, with the
    /// type arguments it is called at.
    pub methods: Vec<(String, BTreeMap<String, Type>)>,
    /// The selected conformance (an index into the frontend's
    /// conformances): one program has one vtable per (object, type), so a
    /// second coercion selecting another model is refused (E0633).
    pub conformance: usize,
    /// The first coercion that built this vtable.
    pub span: Span,
}

impl DynObjects {
    pub fn is_empty(&self) -> bool {
        self.params.is_empty() && self.stubs.is_empty() && self.vtables.is_empty()
    }

    /// The vtable of `self_ty` as `object`.
    pub fn vtable(&self, object: &Type, self_ty: &Type) -> Option<&VTable> {
        let object = crate::numeric::default_type(object);
        let self_ty = crate::numeric::default_type(self_ty);
        self.vtables
            .iter()
            .find(|v| v.object == object && v.self_ty == self_ty)
    }

    /// Call type arguments for `callee` with every `dyn` parameter erased to
    /// its object type: a `dyn` function has one native instance.
    pub fn erase(&self, callee: &str, vars: &BTreeMap<String, Type>) -> BTreeMap<String, Type> {
        let Some(params) = self.params.get(callee) else {
            return vars.clone();
        };
        let mut out = vars.clone();
        for (g, tr) in params {
            out.insert(g.clone(), object(substitute(tr, vars)));
        }
        out
    }

    /// The vtables a call of `callee` at `vars` (the call's own type
    /// arguments, before [`Self::erase`]) coerces to: one per `dyn`
    /// parameter given a concrete type.
    pub fn coercions(
        &self,
        callee: &str,
        vars: &BTreeMap<String, Type>,
    ) -> Vec<(String, Option<&VTable>)> {
        let Some(params) = self.params.get(callee) else {
            return vec![];
        };
        let mut out = vec![];
        for (g, tr) in params {
            let Some(actual) = vars.get(g) else { continue };
            if is_object(actual) {
                continue;
            }
            let object = object(substitute(tr, vars));
            out.push((g.clone(), self.vtable(&object, actual)));
        }
        out
    }
}

fn d(code: &str, msg: impl Into<String>, span: &Span) -> Diagnostic {
    Diagnostic::new(code, msg, span.clone())
}

fn mentions_dyn(t: &Type) -> bool {
    match t {
        Type::Named(n, a) => n == DYN_TYPE || a.iter().any(mentions_dyn),
        Type::Tuple(ts) => ts.iter().any(mentions_dyn),
        Type::Function {
            params,
            result,
            effects,
            ..
        } => {
            params.iter().any(|(_, t)| mentions_dyn(t))
                || mentions_dyn(result)
                || effects
                    .instances
                    .iter()
                    .any(|e| e.args.iter().any(mentions_dyn))
        }
        Type::Row(e) => e.instances.iter().any(|e| e.args.iter().any(mentions_dyn)),
        _ => false,
    }
}

/// The trait of a boxed object type `Box[dyn Trait[args]]`.
pub fn boxed_trait(t: &Type) -> Option<&Type> {
    match t {
        Type::Named(n, a) if n == crate::fip::BOX_TYPE && a.len() == 1 => object_trait(&a[0]),
        _ => None,
    }
}

/// Whether `t` is a boxed object type `Box[dyn Trait[args]]`.
pub fn is_boxed(t: &Type) -> bool {
    boxed_trait(t).is_some()
}

/// The object type `dyn Trait[args]` of a boxed object type.
pub fn boxed_object(t: &Type) -> Option<&Type> {
    match t {
        Type::Named(n, a) if n == crate::fip::BOX_TYPE && a.len() == 1 && is_object(&a[0]) => {
            Some(&a[0])
        }
        _ => None,
    }
}

/// Whether `t` mentions `dyn` other than as a boxed object `Box[dyn
/// Trait[args]]` (whose trait arguments mention no `dyn`).
fn mentions_bare_dyn(t: &Type) -> bool {
    if let Some(Type::Named(_, args)) = boxed_trait(t) {
        return args.iter().any(mentions_dyn);
    }
    match t {
        Type::Named(n, a) => n == DYN_TYPE || a.iter().any(mentions_bare_dyn),
        Type::Tuple(ts) => ts.iter().any(mentions_bare_dyn),
        Type::Function {
            params,
            result,
            effects,
            ..
        } => {
            params.iter().any(|(_, t)| mentions_bare_dyn(t))
                || mentions_bare_dyn(result)
                || effects
                    .instances
                    .iter()
                    .any(|e| e.args.iter().any(mentions_bare_dyn))
        }
        Type::Row(e) => e
            .instances
            .iter()
            .any(|e| e.args.iter().any(mentions_bare_dyn)),
        _ => false,
    }
}

/// The traits `Trait[args]` of every boxed object type in `t`.
fn boxed_traits(t: &Type, out: &mut Vec<Type>) {
    if let Some(tr) = boxed_trait(t) {
        out.push(tr.clone());
        return;
    }
    match t {
        Type::Named(_, a) | Type::Tuple(a) => a.iter().for_each(|t| boxed_traits(t, out)),
        Type::Function {
            params,
            result,
            effects,
            ..
        } => {
            params.iter().for_each(|(_, t)| boxed_traits(t, out));
            boxed_traits(result, out);
            for e in &effects.instances {
                e.args.iter().for_each(|t| boxed_traits(t, out));
            }
        }
        Type::Row(e) => {
            for e in &e.instances {
                e.args.iter().for_each(|t| boxed_traits(t, out));
            }
        }
        _ => {}
    }
}

fn mentions(t: &Type, name: &str) -> bool {
    match t {
        Type::Named(n, a) => n == name || a.iter().any(|t| mentions(t, name)),
        Type::Tuple(ts) => ts.iter().any(|t| mentions(t, name)),
        Type::Function {
            params,
            result,
            effects,
            ..
        } => {
            params.iter().any(|(_, t)| mentions(t, name))
                || mentions(result, name)
                || effects
                    .instances
                    .iter()
                    .any(|e| e.args.iter().any(|t| mentions(t, name)))
        }
        Type::Row(e) => e
            .instances
            .iter()
            .any(|e| e.args.iter().any(|t| mentions(t, name))),
        _ => false,
    }
}

/// Every type written in `f` (signature and body), with a span to report.
fn function_types(f: &Function) -> Vec<(Type, Span)> {
    let mut out = vec![];
    let mut probe = f.clone();
    crate::pact_frontend::walk_function_spans(&mut probe, &mut |t, s| {
        out.push((t.clone(), s.clone()));
    });
    out
}

/// A type written in `f` that mentions `dyn` other than as a boxed object
/// or the object type `Box::new[dyn Trait](x)` boxes (§7.3).
fn bare_dyn_type(f: &Function) -> Option<(Type, Span)> {
    let mut probe = f.clone();
    if let Some(body) = &mut probe.body {
        crate::pact_frontend::each_expr(body, &mut |e| {
            if let ExprKind::Call { callee, types, .. } = &mut e.kind
                && matches!(&callee.kind, ExprKind::Name(n) if n == crate::fip::BOX_NEW)
                && let [t] = types.as_slice()
                && object_trait(t).is_some_and(|tr| !mentions_dyn(tr))
            {
                types.clear();
            }
        });
    }
    function_types(&probe)
        .into_iter()
        .find(|(t, _)| mentions_bare_dyn(t))
}

/// Whether the module writes `dyn` anywhere.
pub fn uses_dyn(module: &Module) -> bool {
    let fns = |f: &Function| function_types(f).iter().any(|(t, _)| mentions_dyn(t));
    module.items.iter().any(|item| match item {
        Item::Function(f) => fns(f),
        Item::Impl { methods, ty, .. } => mentions_dyn(ty) || methods.iter().any(fns),
        Item::Trait(t) => t.methods.iter().any(fns),
        Item::Conformance(c) => c.methods.iter().any(fns) || mentions_dyn(&c.self_ty),
        Item::Effect(e) => e.ops.iter().any(fns),
        Item::Adt(a) => a
            .variants
            .iter()
            .flat_map(|v| &v.fields)
            .any(|f| mentions_dyn(&f.ty)),
    })
}

/// A direct use of a `dyn` parameter as a call argument, checked against
/// the callee once calls are resolved ([`check_forwards`]).
#[derive(Debug, Clone)]
pub struct Forward {
    /// The parameter.
    pub name: String,
    /// The call expression's span.
    pub call: Span,
    /// The callee as written.
    pub callee: Callee,
    /// The argument's position (receiver excluded).
    pub index: usize,
    /// The argument's span.
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum Callee {
    Name(String),
    /// `base.method(...)`, with the base's span.
    Method(Span, String),
}

/// The module with `dyn` parameters lowered ([`lower`]).
pub struct Lowered {
    pub module: Module,
    /// Hidden type parameter to trait, by owning function (World name).
    pub params: BTreeMap<String, BTreeMap<String, Type>>,
    pub forwards: Vec<Forward>,
    /// Traits used as boxed objects (`Box[dyn Trait]`) anywhere.
    pub boxed: BTreeSet<String>,
}

/// Lowers `dyn` parameters to hidden bounded type parameters and checks
/// that `dyn` appears nowhere else, that every object trait is object
/// safe, and that no `dyn` value escapes syntactically.
pub fn lower(module: &Module) -> Result<Lowered, Vec<Diagnostic>> {
    let mut out = module.clone();
    let mut errors = vec![];
    let mut params = BTreeMap::new();
    let mut forwards = vec![];
    let traits: BTreeMap<String, TraitDecl> = module
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Trait(t) => Some((t.name.clone(), t.clone())),
            _ => None,
        })
        .collect();
    let mut checked_objects = BTreeSet::new();
    let elsewhere = |what: &str, span: &Span| {
        d(
            "E0632",
            format!(
                "`dyn Trait` is second-class: it may only be the whole type of an `in`, `inout` or `sink` parameter of a function or inherent method, not {what}"
            ),
            span,
        )
    };
    let mut bounds = vec![];
    let mut boxed = BTreeSet::new();
    {
        let mut found: Vec<(Type, Span)> = vec![];
        let mut types = |t: &Type, s: &Span| {
            let mut ts = vec![];
            // `Box::new[dyn Trait](x)` names the object type alone.
            if let Some(tr) = object_trait(t) {
                ts.push(tr.clone());
            }
            boxed_traits(t, &mut ts);
            found.extend(ts.into_iter().map(|t| (t, s.clone())));
        };
        for item in &module.items {
            let fns: Vec<&Function> = match item {
                Item::Function(f) => vec![&**f],
                Item::Impl { methods, .. } => methods.iter().collect(),
                Item::Trait(t) => t.methods.iter().collect(),
                Item::Conformance(c) => c.methods.iter().collect(),
                Item::Effect(e) => e.ops.iter().collect(),
                Item::Adt(a) => {
                    for f in a.variants.iter().flat_map(|v| &v.fields) {
                        types(&f.ty, &a.span);
                    }
                    vec![]
                }
            };
            for f in fns {
                for (t, s) in function_types(f) {
                    types(&t, &s);
                }
            }
        }
        for (tr, span) in found {
            let Type::Named(trait_name, args) = &tr else {
                continue;
            };
            if let Some(decl) = traits.get(trait_name) {
                if checked_objects.insert(tr.to_string()) {
                    errors.extend(object_safety(decl, args, &span));
                }
                boxed.insert(trait_name.clone());
            }
        }
    }
    for item in &mut out.items {
        match item {
            Item::Function(f) => {
                let owner = f.name.clone();
                lower_function(
                    f,
                    &owner,
                    &traits,
                    &mut checked_objects,
                    &mut params,
                    &mut bounds,
                    &mut forwards,
                    &mut errors,
                );
            }
            Item::Impl { ty, methods, .. } => {
                if mentions_dyn(ty) {
                    errors.push(elsewhere("an impl self type", &(0..0)));
                }
                let head = ty.name().to_owned();
                for f in methods {
                    let owner = format!("{head}::{}", f.name);
                    lower_function(
                        f,
                        &owner,
                        &traits,
                        &mut checked_objects,
                        &mut params,
                        &mut bounds,
                        &mut forwards,
                        &mut errors,
                    );
                }
            }
            Item::Trait(t) => {
                for f in &t.methods {
                    if let Some((_, ref s)) = bare_dyn_type(f) {
                        errors.push(elsewhere("in a trait method signature", s));
                    }
                }
            }
            Item::Conformance(c) => {
                if mentions_dyn(&c.self_ty) {
                    errors.push(elsewhere("a conformance self type", &c.span));
                }
                for f in &c.methods {
                    if let Some((_, ref s)) = bare_dyn_type(f) {
                        errors.push(elsewhere("in a conformance method", s));
                    }
                }
            }
            Item::Effect(e) => {
                for f in &e.ops {
                    if let Some((_, ref s)) = bare_dyn_type(f) {
                        errors.push(elsewhere("in an effect operation", s));
                    }
                }
            }
            Item::Adt(a) => {
                for f in a.variants.iter().flat_map(|v| &v.fields) {
                    if mentions_bare_dyn(&f.ty) {
                        errors.push(elsewhere("the type of a field", &a.span));
                    }
                }
            }
        }
    }
    out.trait_bounds.extend(bounds);
    if errors.is_empty() {
        Ok(Lowered {
            module: out,
            params,
            forwards,
            boxed,
        })
    } else {
        Err(errors)
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "one function's lowering writes every output of the module pass"
)]
fn lower_function(
    f: &mut Function,
    owner: &str,
    traits: &BTreeMap<String, TraitDecl>,
    checked: &mut BTreeSet<String>,
    params: &mut BTreeMap<String, BTreeMap<String, Type>>,
    bounds: &mut Vec<TraitBoundSyntax>,
    forwards: &mut Vec<Forward>,
    errors: &mut Vec<Diagnostic>,
) {
    let mut names = BTreeSet::new();
    let mut own = BTreeMap::new();
    for p in &mut f.params {
        let Some(tr) = object_trait(&p.ty).cloned() else {
            continue;
        };
        if !matches!(p.pass, Pass::In | Pass::Inout | Pass::Sink) {
            errors.push(d(
                "E0632",
                format!(
                    "`{}: dyn {tr}` is an `out` parameter; a trait object is passed `in`, `inout` or `sink`",
                    p.name
                ),
                &p.span,
            ));
            continue;
        }
        let Type::Named(trait_name, args) = &tr else {
            continue;
        };
        if args.iter().any(mentions_dyn) {
            errors.push(d(
                "E0632",
                "`dyn Trait` cannot be a trait argument of another `dyn` type",
                &p.span,
            ));
            continue;
        }
        if let Some(decl) = traits.get(trait_name) {
            let key = tr.to_string();
            if checked.insert(key) {
                errors.extend(object_safety(decl, args, &p.span));
            }
        }
        let g = format!("{DYN_GENERIC}{}", p.name);
        f.generics.push(Generic {
            name: g.clone(),
            // A borrowed object may be linear; a consumed one may be
            // dropped by the callee, so `sink dyn` admits affine types.
            bound: if p.pass == Pass::Sink {
                Mode::Affine
            } else {
                Mode::Linear
            },
            row: false,
        });
        bounds.push(TraitBoundSyntax {
            param: g.clone(),
            trait_ref: TraitRef {
                package: None,
                name: trait_name.clone(),
                args: args.clone(),
                span: p.span.clone(),
            },
            span: p.span.clone(),
            owner: owner.to_owned(),
        });
        p.ty = Type::named(&g);
        names.insert(p.name.clone());
        own.insert(g, tr);
    }
    if let Some((_, ref s)) = bare_dyn_type(f) {
        errors.push(d(
            "E0632",
            "`dyn Trait` is second-class unless boxed: it may only be the whole type of an `in`, `inout` or `sink` parameter or the argument of `Box[dyn Trait]`, not a result, a `let` annotation, a type argument, a closure parameter or part of another type",
            s,
        ));
    }
    if own.is_empty() {
        return;
    }
    if let Some(body) = &f.body {
        escapes(body, &names, false, forwards, errors);
    }
    params.insert(owner.to_owned(), own);
}

/// Object-safety rules (module docs) for `decl` used at trait arguments
/// `args`.
fn object_safety(decl: &TraitDecl, args: &[Type], span: &Span) -> Vec<Diagnostic> {
    let mut out = vec![];
    let unsafe_ = |why: String| {
        d(
            "E0630",
            format!("trait '{}' is not object safe: {why}", decl.name),
            span,
        )
    };
    if let Some(a) = decl.assoc.first() {
        out.push(unsafe_(format!(
            "it declares the associated type '{}'",
            a.name
        )));
    }
    let vars: BTreeMap<String, Type> = decl
        .generics
        .iter()
        .zip(args)
        .map(|(g, a)| (g.name.clone(), a.clone()))
        .collect();
    for m in &decl.methods {
        if !m.generics.is_empty() {
            out.push(unsafe_(format!("method '{}' has type parameters", m.name)));
        }
        match m.params.first() {
            Some(p) if p.name == "self" => {
                if !matches!(p.pass, Pass::In | Pass::Inout | Pass::Sink) {
                    out.push(unsafe_(format!("method '{}' takes `out self`", m.name)));
                }
            }
            _ => out.push(unsafe_(format!(
                "method '{}' has no `self` receiver",
                m.name
            ))),
        }
        let by_self = m.params.iter().skip(1).any(|p| mentions(&p.ty, "Self"))
            || mentions(&m.result, "Self")
            || m.effects
                .instances
                .iter()
                .any(|e| e.args.iter().any(|t| mentions(t, "Self")));
        if by_self {
            out.push(unsafe_(format!(
                "method '{}' mentions `Self` outside its receiver",
                m.name
            )));
        }
        let effects = substitute_effects(&m.effects, &vars);
        if effects.tail.is_some() {
            out.push(unsafe_(format!(
                "method '{}' has an open effect row",
                m.name
            )));
        }
    }
    out
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
        PatternKind::Wild | PatternKind::Literal(_) => {}
    }
}

fn without(live: &BTreeSet<String>, bound: &BTreeSet<String>) -> BTreeSet<String> {
    live.difference(bound).cloned().collect()
}

/// Syntactic second-class rule over `e` with the `dyn` parameters `live`
/// in scope; `closure` is set inside a closure or handler body.
fn escapes(
    e: &Expr,
    live: &BTreeSet<String>,
    closure: bool,
    forwards: &mut Vec<Forward>,
    errors: &mut Vec<Diagnostic>,
) {
    if live.is_empty() {
        return;
    }
    let is_live = |x: &Expr| matches!(&x.kind, ExprKind::Name(n) if live.contains(n));
    let rec = |x: &Expr, forwards: &mut Vec<Forward>, errors: &mut Vec<Diagnostic>| {
        escapes(x, live, closure, forwards, errors)
    };
    match &e.kind {
        ExprKind::Name(n) if live.contains(n) => errors.push(d(
            "E0631",
            format!(
                "the `dyn` parameter '{n}' is second-class: use it only as a method receiver or as the argument of another `dyn` parameter (it cannot be bound, stored, returned or captured)"
            ),
            &e.span,
        )),
        ExprKind::Literal(_) | ExprKind::Name(_) => {}
        ExprKind::Call { callee, args, .. } => {
            let callee_desc = match &callee.kind {
                ExprKind::Field(base, m) if is_live(base) => {
                    if closure {
                        rec(base, forwards, errors);
                    }
                    Some(Callee::Method(base.span.clone(), m.clone()))
                }
                ExprKind::Field(base, m) => {
                    rec(base, forwards, errors);
                    Some(Callee::Method(base.span.clone(), m.clone()))
                }
                ExprKind::Name(n) => Some(Callee::Name(n.clone())),
                _ => {
                    rec(callee, forwards, errors);
                    None
                }
            };
            for (index, a) in args.iter().enumerate() {
                match (&a.expr.kind, &callee_desc) {
                    (ExprKind::Name(n), Some(c)) if live.contains(n) && !closure => {
                        forwards.push(Forward {
                            name: n.clone(),
                            call: e.span.clone(),
                            callee: c.clone(),
                            index,
                            span: a.expr.span.clone(),
                        });
                    }
                    _ => rec(&a.expr, forwards, errors),
                }
            }
        }
        ExprKind::Block { statements, tail } => {
            let mut live = live.clone();
            for s in statements {
                match s {
                    Statement::Let { pat, value, .. } => {
                        escapes(value, &live, closure, forwards, errors);
                        let mut bound = BTreeSet::new();
                        pattern_names(pat, &mut bound);
                        live = without(&live, &bound);
                    }
                    Statement::Expr(x) => escapes(x, &live, closure, forwards, errors),
                }
            }
            if let Some(t) = tail {
                escapes(t, &live, closure, forwards, errors);
            }
        }
        ExprKind::Tuple(es) => es.iter().for_each(|x| rec(x, forwards, errors)),
        ExprKind::Struct { fields, .. } => {
            fields.iter().for_each(|(_, x)| rec(x, forwards, errors));
        }
        ExprKind::Field(b, _) | ExprKind::Try(b) | ExprKind::Unary(_, b) => {
            rec(b, forwards, errors);
        }
        ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Assign(_, a, b) => {
            rec(a, forwards, errors);
            rec(b, forwards, errors);
        }
        ExprKind::If {
            condition,
            then,
            otherwise,
        } => {
            rec(condition, forwards, errors);
            rec(then, forwards, errors);
            if let Some(o) = otherwise {
                rec(o, forwards, errors);
            }
        }
        ExprKind::Match { value, arms } => {
            rec(value, forwards, errors);
            for (p, a) in arms {
                let mut bound = BTreeSet::new();
                pattern_names(p, &mut bound);
                escapes(a, &without(live, &bound), closure, forwards, errors);
            }
        }
        ExprKind::Closure { params, body, .. } => {
            let bound = params.iter().map(|p| p.name.clone()).collect();
            escapes(body, &without(live, &bound), true, forwards, errors);
        }
        ExprKind::Handle {
            state, body, ret, ops, ..
        } => {
            if let Some((_, s)) = state {
                rec(s, forwards, errors);
            }
            rec(body, forwards, errors);
            for c in std::iter::once(&**ret).chain(ops) {
                let mut bound = BTreeSet::new();
                for p in &c.pats {
                    pattern_names(p, &mut bound);
                }
                escapes(&c.body, &without(live, &bound), true, forwards, errors);
            }
        }
        ExprKind::For { pat, iter, body } => {
            rec(iter, forwards, errors);
            let mut bound = BTreeSet::new();
            pattern_names(pat, &mut bound);
            escapes(body, &without(live, &bound), closure, forwards, errors);
        }
        k => {
            for c in k.control_children().into_iter().flatten() {
                rec(c, forwards, errors);
            }
        }
    }
}

/// Checks each forwarded `dyn` argument against its resolved callee: the
/// parameter it reaches must be a `dyn` parameter (or the receiver of a
/// vtable stub).
pub(crate) fn check_forwards(
    lowered: &Lowered,
    world: &crate::check::World,
    resolved: &BTreeMap<(usize, usize), String>,
) -> Vec<Diagnostic> {
    let mut out = vec![];
    for fw in &lowered.forwards {
        let key = (fw.call.start, fw.call.end);
        let (name, offset) = match &fw.callee {
            Callee::Name(n) => (resolved.get(&key).cloned().unwrap_or_else(|| n.clone()), 0),
            Callee::Method(base, m) => (
                resolved.get(&key).cloned().unwrap_or_else(|| {
                    let head = world
                        .types
                        .get(&(base.start, base.end))
                        .map(|t| t.name().to_owned())
                        .unwrap_or_default();
                    format!("{head}::{m}")
                }),
                1,
            ),
        };
        let index = fw.index + offset;
        let ok = world.functions.get(&name).is_some_and(|f| {
            f.params.get(index).is_some_and(|p| {
                matches!(&p.ty, Type::Named(g, a) if a.is_empty() && is_dyn_generic(g))
                    || (index == 0 && name.starts_with(VTABLE_STUB))
            })
        });
        if !ok {
            out.push(d(
                "E0631",
                format!(
                    "the `dyn` parameter '{}' is second-class: it may be passed only to a `dyn` parameter, and '{name}' does not take one here",
                    fw.name
                ),
                &fw.span,
            ));
        }
    }
    out
}

/// The vtable stub of `method` of `trait_name`.
pub(crate) fn stub_name(trait_name: &str, method: &str) -> String {
    format!("{VTABLE_STUB}{trait_name}${method}")
}

/// A typed refusal for backends without `dyn` support.
pub fn refuse(world: &crate::check::World, backend: &str) -> Result<(), Diagnostic> {
    if world.dyn_objects.is_empty() {
        return Ok(());
    }
    let param = world
        .dyn_objects
        .params
        .keys()
        .next()
        .and_then(|n| world.functions.get(n))
        .map(|f| (f.name.clone(), f.span.clone()));
    let (what, span) = match (param, world.dyn_objects.vtables.first()) {
        (Some((name, span)), _) => (
            format!("`dyn Trait` parameters yet ('{name}' takes one)"),
            span,
        ),
        (None, Some(v)) => (
            format!(
                "boxed trait objects yet (`Box[{}]` holds {})",
                v.object, v.self_ty
            ),
            v.span.clone(),
        ),
        (None, None) => ("`dyn Trait` yet".to_owned(), 0..0),
    };
    Err(d(
        "E0634",
        format!("the {backend} does not support {what}; use the native backend or the JIT"),
        &span,
    ))
}
