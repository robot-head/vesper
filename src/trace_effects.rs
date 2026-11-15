//! Source Beacon (design §22): the `Trace` capability effect, linear
//! `ActiveSpan` tokens, context propagation and `Telemetrable` coherence.
//!
//! A module that names `Trace`, `SpanContext`, `ActiveSpan`, `TelemetryAttr`
//! or `Telemetrable` without declaring any of them gets the built-in Beacon
//! prelude ([`PRELUDE`]): the §22.2 declarations, with `StaticStr` spelled
//! `String`, two additive operations (`start_child`, which starts a span under
//! an explicit parent, and `emit_attr`, which finishes one with a typed
//! attribute) and a `Telemetrable` trait whose method returns a
//! [`TelemetryAttr`](PRELUDE) instead of writing into an `AttrWriter`. A
//! module that declares one of those names itself keeps its own declarations
//! and none of the rules below apply (as with `Region`, see `regions`).
//!
//! The ordinary checker already treats `ActiveSpan` as a `Mode::Linear`
//! value: a span that is not finished on every path is E0309, a `?` or other
//! abortable effect while a span is live is E0402, and calling a `Trace`
//! function from a body without `Trace` in its row or a handler is E0401.
//! This module adds the §22 rules on top, on the checked and elaborated
//! World:
//!
//! * E0750: a span cannot leave the function that started it. No function,
//!   closure or ADT field type mentions `ActiveSpan`, no value wraps one
//!   (`Some(span)`, `(span, x)`), and only `Trace::start_span` and
//!   `Trace::start_child` produce one.
//! * E0751: an `ActiveSpan` literal outside a `Trace` handler clause forges a
//!   span. E0752: destructuring one outside a handler clause discards it
//!   without finishing it.
//! * E0753: dropping a trace context. A `SpanContext` result used as a
//!   statement or bound to `_`, a `SpanContext` binding, parameter or
//!   handler-clause parameter that is never read.
//! * E0754: a `handle Trace` installed where `Trace` is already in scope must
//!   propagate the enclosing context: its `current_ctx` and `start_span`
//!   clauses must perform an outer `Trace` operation or read a context from
//!   outside the handler. Otherwise the handled block starts a disconnected
//!   trace.
//! * E0756: at most one `Telemetrable` instance (canonical `impl` or named
//!   `model`) per type, so a type has one exported encoding.
//! * E0757: no canonical `Telemetrable` impl for a secret type
//!   (`capability_effects::SECRET_TYPES`) or a bare type parameter; secrets
//!   need an explicit, named redacting model.
//! * E0758: a `Telemetrable` method may not read a secret: an expression whose
//!   type contains a secret type may only be the base of a field projection
//!   onto a non-secret field.
//!
//! [`host`] runs a checked program on the evaluator with a `Trace` host that
//! records spans through `vesper-beacon`'s `SpanPool` and `TelemetryRing` and
//! exports them as a span forest.
//!
//! These are source typing rules enforced by the checker and a reference
//! host. Rust ownership in the host is not evidence that the rules hold for
//! Vesper programs, and nothing here is a proof about emitted code.
use crate::ast::*;
use crate::check::Checked;
use std::borrow::Cow;

pub mod host;
pub mod native_runtime;
mod rules;
mod telemetry;

/// The trace effect (§22.2).
pub const TRACE: &str = "Trace";
/// The linear span token.
pub const ACTIVE_SPAN: &str = "ActiveSpan";
/// The propagated context.
pub const SPAN_CONTEXT: &str = "SpanContext";
/// Typed span attributes.
pub const TELEMETRY_ATTR: &str = "TelemetryAttr";
/// The export trait (§22.4).
pub const TELEMETRABLE: &str = "Telemetrable";

/// Names the prelude declares.
pub const PRELUDE_NAMES: [&str; 5] = [
    TRACE,
    ACTIVE_SPAN,
    SPAN_CONTEXT,
    TELEMETRY_ATTR,
    TELEMETRABLE,
];

/// `Trace` operations, in declaration order.
pub const OPERATIONS: [&str; 6] = [
    "current_ctx",
    "start_span",
    "start_child",
    "emit_ok",
    "emit_err",
    "emit_attr",
];

/// The built-in Beacon declarations.
pub const PRELUDE: &str = "copy struct SpanContext { trace_id_hi: u64, trace_id_lo: u64, span_id: u64, flags: u8 }
linear struct ActiveSpan { ctx: SpanContext, parent_span_id: u64, start_mono_ns: u64, schema_id: u32 }
copy enum TelemetryAttr { NoAttr, Count(u64), Bytes(u64), Redacted }
linear effect Trace {
    fn current_ctx() -> SpanContext;
    fn start_span(schema_id: u32, name: String) -> ActiveSpan;
    fn start_child(parent: SpanContext, schema_id: u32, name: String) -> ActiveSpan;
    fn emit_ok(sink span: ActiveSpan, attr_u64: u64) -> Unit;
    fn emit_err(sink span: ActiveSpan, err_code: u32, msg: String) -> Unit;
    fn emit_attr(sink span: ActiveSpan, attr: TelemetryAttr) -> Unit;
}";

/// The `Telemetrable` trait, added only when the module names it.
pub const TELEMETRABLE_PRELUDE: &str =
    "trait Telemetrable { fn telemetry_attr(in self) -> TelemetryAttr; }";

/// A module together with whether it runs under the built-in Beacon prelude.
pub struct BeaconModule<'a> {
    module: Cow<'a, Module>,
    active: bool,
}

impl BeaconModule<'_> {
    /// The module to check (with the prelude items first when active).
    pub fn module(&self) -> &Module {
        &self.module
    }
    /// Whether the Beacon prelude and rules apply.
    pub fn active(&self) -> bool {
        self.active
    }
    /// Adds the §22 diagnostics to an otherwise clean check result.
    pub fn check(&self, checked: &mut Checked) {
        if !self.active || !checked.diagnostics.is_empty() {
            return;
        }
        let mut diagnostics = rules::check(checked);
        diagnostics.extend(telemetry::check(&self.module, checked));
        diagnostics.sort_by(|a, b| {
            (a.span.start, a.span.end, &a.code).cmp(&(b.span.start, b.span.end, &b.code))
        });
        diagnostics.dedup();
        checked.diagnostics.extend(diagnostics);
    }
}

/// Adds the Beacon prelude to `module` when it uses Beacon names without
/// declaring any of them.
pub fn with_prelude(module: &Module) -> BeaconModule<'_> {
    let declares = module.items.iter().any(|item| match item {
        Item::Adt(a) => PRELUDE_NAMES.contains(&a.name.as_str()),
        Item::Effect(e) => PRELUDE_NAMES.contains(&e.name.as_str()),
        Item::Trait(t) => PRELUDE_NAMES.contains(&t.name.as_str()),
        _ => false,
    });
    let mut uses = Uses::default();
    if !declares {
        uses.module(module);
    }
    if declares || !(uses.beacon || uses.telemetrable) {
        return BeaconModule {
            module: Cow::Borrowed(module),
            active: false,
        };
    }
    let mut items = crate::parser::parse(PRELUDE).module.items;
    if uses.telemetrable {
        items.extend(crate::parser::parse(TELEMETRABLE_PRELUDE).module.items);
    }
    let mut module = module.clone();
    items.append(&mut module.items);
    module.items = items;
    BeaconModule {
        module: Cow::Owned(module),
        active: true,
    }
}

/// Whether the World's Beacon declarations match the built-in ABI.
pub fn active(world: &crate::check::World) -> bool {
    fn same_type(a: &Type, b: &Type) -> bool {
        let is_unit = |t: &Type| {
            *t == Type::unit()
                || matches!(t, Type::Named(n, args) if n == "Unit" && args.is_empty())
        };
        if is_unit(a) && is_unit(b) {
            return true;
        }
        match (a, b) {
            (Type::IntLiteral(x), Type::IntLiteral(y)) => x == y,
            (Type::Named(na, aa), Type::Named(nb, ab)) => {
                na == nb && aa.len() == ab.len() && aa.iter().zip(ab).all(|(x, y)| same_type(x, y))
            }
            (Type::Tuple(ta), Type::Tuple(tb)) => {
                ta.len() == tb.len() && ta.iter().zip(tb).all(|(x, y)| same_type(x, y))
            }
            (
                Type::Function {
                    params: pa,
                    result: ra,
                    effects: ea,
                    mode: ma,
                },
                Type::Function {
                    params: pb,
                    result: rb,
                    effects: eb,
                    mode: mb,
                },
            ) => {
                ma == mb
                    && ea == eb
                    && pa.len() == pb.len()
                    && pa
                        .iter()
                        .zip(pb)
                        .all(|((psa, ta), (psb, tb))| psa == psb && same_type(ta, tb))
                    && same_type(ra, rb)
            }
            (Type::Row(ea), Type::Row(eb)) => ea == eb,
            (Type::Infer, Type::Infer) => true,
            _ => false,
        }
    }
    fn same_adt(a: &Adt, b: &Adt) -> bool {
        a.name == b.name
            && a.mode == b.mode
            && a.generics == b.generics
            && a.is_enum == b.is_enum
            && a.variants.len() == b.variants.len()
            && a.variants.iter().zip(&b.variants).all(|(va, vb)| {
                va.name == vb.name
                    && va.named == vb.named
                    && va.fields.len() == vb.fields.len()
                    && va.fields.iter().zip(&vb.fields).all(|(fa, fb)| {
                        fa.name == fb.name
                            && fa.modality == fb.modality
                            && same_type(&fa.ty, &fb.ty)
                    })
            })
    }
    fn same_effect(a: &EffectDecl, b: &EffectDecl) -> bool {
        a.name == b.name
            && a.linear == b.linear
            && a.generics == b.generics
            && a.ops.len() == b.ops.len()
            && a.ops.iter().zip(&b.ops).all(|(oa, ob)| {
                oa.name == ob.name
                    && oa.generics == ob.generics
                    && oa.effects == ob.effects
                    && oa.params.len() == ob.params.len()
                    && oa.params.iter().zip(&ob.params).all(|(pa, pb)| {
                        pa.pass == pb.pass
                            && pa.projections == pb.projections
                            && same_type(&pa.ty, &pb.ty)
                    })
                    && same_type(&oa.result, &ob.result)
            })
    }

    let prelude = crate::parser::parse(PRELUDE).module;
    let mut adt_count = 0usize;
    let mut effect_count = 0usize;
    for item in &prelude.items {
        match item {
            Item::Adt(expected) => {
                adt_count += 1;
                if !world
                    .adts
                    .get(&expected.name)
                    .is_some_and(|actual| same_adt(actual, expected))
                {
                    return false;
                }
            }
            Item::Effect(expected) => {
                effect_count += 1;
                if !world
                    .effects
                    .get(&expected.name)
                    .is_some_and(|actual| same_effect(actual, expected))
                {
                    return false;
                }
            }
            _ => {}
        }
    }
    adt_count == 3 && effect_count == 1
}

/// Whether `t` mentions `name` in a data position (function types are not
/// data: a `resume` continuation taking a span does not hold one).
pub(crate) fn mentions(t: &Type, name: &str) -> bool {
    match t {
        Type::Named(n, args) => n == name || args.iter().any(|a| mentions(a, name)),
        Type::Tuple(ts) => ts.iter().any(|t| mentions(t, name)),
        _ => false,
    }
}

pub(crate) fn is_named(t: &Type, name: &str) -> bool {
    matches!(t, Type::Named(n, args) if n == name && args.is_empty())
}

#[derive(Default)]
struct Uses {
    beacon: bool,
    telemetrable: bool,
}

impl Uses {
    fn name(&mut self, n: &str) {
        let head = n.split("::").next().unwrap_or(n);
        if head == TELEMETRABLE {
            self.telemetrable = true;
        } else if PRELUDE_NAMES.contains(&head) {
            self.beacon = true;
        }
    }
    fn ty(&mut self, t: &Type) {
        match t {
            Type::Named(n, args) => {
                self.name(n);
                args.iter().for_each(|a| self.ty(a));
            }
            Type::Tuple(ts) => ts.iter().for_each(|t| self.ty(t)),
            Type::Function {
                params,
                result,
                effects,
                ..
            } => {
                params.iter().for_each(|(_, t)| self.ty(t));
                self.ty(result);
                self.effects(effects);
            }
            Type::Row(effects) => self.effects(effects),
            Type::IntLiteral(_) | Type::Infer => {}
        }
    }
    fn effects(&mut self, effects: &Effects) {
        for e in &effects.instances {
            self.name(&e.name);
            e.args.iter().for_each(|a| self.ty(a));
        }
    }
    fn function(&mut self, f: &Function) {
        f.params.iter().for_each(|p| self.ty(&p.ty));
        self.ty(&f.result);
        self.effects(&f.effects);
        if let Some(body) = &f.body {
            self.expr(body);
        }
    }
    fn pattern(&mut self, p: &Pattern) {
        match &p.kind {
            PatternKind::Tuple(ps) => ps.iter().for_each(|p| self.pattern(p)),
            PatternKind::Constructor { name, fields, .. } => {
                self.name(name);
                fields.iter().for_each(|(_, p)| self.pattern(p));
            }
            _ => {}
        }
    }
    fn expr(&mut self, e: &Expr) {
        crate::regions::visit_expr(e, &mut |e| match &e.kind {
            ExprKind::Name(n) | ExprKind::Struct { name: n, .. } => self.name(n),
            ExprKind::Call { types, .. } => types.iter().for_each(|t| self.ty(t)),
            ExprKind::Block { statements, .. } => {
                for s in statements {
                    if let Statement::Let { pat, ty, .. } = s {
                        self.pattern(pat);
                        self.ty(ty);
                    }
                }
            }
            ExprKind::Match { arms, .. } => arms.iter().for_each(|(p, _)| self.pattern(p)),
            ExprKind::Closure {
                params,
                result,
                effects,
                ..
            } => {
                params.iter().for_each(|p| self.ty(&p.ty));
                self.ty(result);
                self.effects(effects);
            }
            ExprKind::Handle {
                effect,
                state,
                ret,
                ops,
                ..
            } => {
                self.name(&effect.name);
                effect.args.iter().for_each(|a| self.ty(a));
                if let Some((p, _)) = state {
                    self.pattern(p);
                }
                ret.pats
                    .iter()
                    .chain(ops.iter().flat_map(|c| &c.pats))
                    .for_each(|p| self.pattern(p));
            }
            _ => {}
        });
    }
    fn module(&mut self, module: &Module) {
        for item in &module.items {
            match item {
                Item::Adt(a) => a
                    .variants
                    .iter()
                    .flat_map(|v| &v.fields)
                    .for_each(|f| self.ty(&f.ty)),
                Item::Function(f) => self.function(f),
                Item::Effect(e) => e.ops.iter().for_each(|f| self.function(f)),
                Item::Impl { ty, methods, .. } => {
                    self.ty(ty);
                    methods.iter().for_each(|f| self.function(f));
                }
                Item::Trait(t) => t.methods.iter().for_each(|f| self.function(f)),
                Item::Conformance(c) => {
                    self.name(&c.trait_ref.name);
                    self.ty(&c.self_ty);
                    c.methods.iter().for_each(|f| self.function(f));
                }
            }
        }
    }
}
