//! Bidirectional type/mode inference and path-sensitive place capability checking.
use crate::ast::*;
use std::collections::{BTreeMap, BTreeSet};

#[path = "regions/check.rs"]
mod region_rules;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct World {
    pub adts: BTreeMap<String, Adt>,
    pub functions: BTreeMap<String, Function>,
    pub effects: BTreeMap<String, EffectDecl>,
    pub types: TypeMap,
    pub moves: BTreeMap<(usize, usize), Vec<Place>>,
    pub function_results: BTreeMap<String, Type>,
    pub handler_effects: BTreeMap<(usize, usize), Effects>,
    /// Primitive operations which can raise Fail[RuntimeError].
    pub partial_ops: BTreeSet<(usize, usize)>,
    /// Checked integer operations which can raise Fail[ArithOverflow]. Spans
    /// absent here were proved in range (see `vesper.arith.checked/1`).
    pub overflow_ops: BTreeSet<(usize, usize)>,
    pub string_capacity: usize,
    pub calls: BTreeMap<(usize, usize), BTreeMap<String, Type>>,
    /// Source trait/model elaboration metadata (empty without traits).
    pub elaboration: Elaboration,
}
impl World {
    pub fn collect(module: &Module) -> (Self, Vec<Diagnostic>) {
        let mut w = Self::default();
        let mut errors = vec![];
        let mut builtins = crate::parser::parse(&format!("copy enum Option[T] {{ None, Some(T) }} copy enum Result[T, E] {{ Ok(T), Err(E) }} copy enum RuntimeError {{ DivisionByZero, IndexOutOfBounds }} {} effect Fail[E] {{ fn raise(sink error: E) -> Never; }}", crate::numeric::OVERFLOW_DECLARATION)).module;
        if crate::regions::needs_prelude(module) {
            builtins.items.extend(crate::regions::prelude());
        }
        let builtin_count = builtins.items.len();
        for (index, item) in builtins.items.iter().chain(module.items.iter()).enumerate() {
            match item {
                Item::Adt(a) => {
                    if matches!(a.name.as_str(), "Unit" | "Never" | "String") {
                        errors.push(Diagnostic::new(
                            "E0100",
                            format!("'{}' is a reserved builtin type", a.name),
                            a.span.clone(),
                        ));
                    }
                    if w.adts.insert(a.name.clone(), a.clone()).is_some() {
                        errors.push(Diagnostic::new(
                            "E0100",
                            format!("duplicate type '{}'", a.name),
                            a.span.clone(),
                        ));
                    }
                }
                Item::Effect(e) => {
                    if index >= builtin_count
                        && e.name == crate::regions::REGION_EFFECT
                        && builtin_count > 0
                        && crate::regions::needs_prelude(module)
                    {
                        errors.push(
                            Diagnostic::new(
                                "E0797",
                                "'Region' is the built-in Strata region effect",
                                e.span.clone(),
                            )
                            .with_help("open a lexical region with `region r { ... }`"),
                        );
                        continue;
                    }
                    if crate::capability_effects::is_capability_effect(&e.name) {
                        errors.push(crate::capability_effects::reserved_declaration(
                            &e.name,
                            e.span.clone(),
                        ));
                        continue;
                    }
                    if w.effects.insert(e.name.clone(), e.clone()).is_some() {
                        errors.push(Diagnostic::new(
                            "E0100",
                            format!("duplicate effect '{}'", e.name),
                            e.span.clone(),
                        ));
                    }
                }
                Item::Function(f) => {
                    if crate::numeric::wrapping_intrinsic(&f.name).is_some() {
                        errors.push(Diagnostic::new(
                            "E0100",
                            format!("'{}' is a reserved arithmetic intrinsic", f.name),
                            f.span.clone(),
                        ));
                    }
                    if w.functions.insert(f.name.clone(), (**f).clone()).is_some() {
                        errors.push(Diagnostic::new(
                            "E0100",
                            format!("duplicate function '{}'", f.name),
                            f.span.clone(),
                        ));
                    }
                }
                // Elaborated by `pact_frontend` into generated functions.
                Item::Trait(_) | Item::Conformance(_) => {}
                Item::Impl {
                    ty,
                    generics,
                    methods,
                } => {
                    for method in methods {
                        let mut f = method.clone();
                        f.name = format!("{}::{}", ty.name(), f.name);
                        f.generics.splice(0..0, generics.clone());
                        for p in &mut f.params {
                            if p.name == "self" && p.ty == Type::Infer {
                                p.ty = ty.clone();
                            }
                        }
                        if w.functions.insert(f.name.clone(), f.clone()).is_some() {
                            errors.push(Diagnostic::new(
                                "E0100",
                                format!("duplicate method '{}'", f.name),
                                f.span.clone(),
                            ));
                        }
                    }
                }
            }
        }
        if crate::regions::active(&w) {
            // Region labels in signatures name the function's own capabilities.
            for f in w.functions.values_mut() {
                let labels = f
                    .effects
                    .instances
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.name == crate::regions::REGION_EFFECT)
                    .filter_map(|(i, e)| Some((e.label.clone()?, function_capability(&f.name, i))))
                    .collect();
                for p in &mut f.params {
                    p.ty = crate::regions::normalize(&p.ty, &labels);
                }
                f.result = crate::regions::normalize(&f.result, &labels);
            }
        }
        (w, errors)
    }
    pub fn constructor(&self, name: &str) -> Option<(&Adt, &Variant)> {
        let short = name.rsplit("::").next().unwrap_or(name);
        self.adts.values().find_map(|a| {
            a.variants
                .iter()
                .find(|v| {
                    v.name == short
                        && (!name.contains("::") || name.starts_with(&format!("{}::", a.name)))
                })
                .map(|v| (a, v))
        })
    }
    pub fn mode(&self, ty: &Type, bounds: &BTreeMap<String, Mode>) -> Mode {
        fn visit(
            w: &World,
            ty: &Type,
            bounds: &BTreeMap<String, Mode>,
            seen: &mut BTreeSet<String>,
        ) -> Mode {
            match ty {
                Type::Infer | Type::IntLiteral(_) => Mode::Copy,
                Type::Row(_) => Mode::Copy,
                Type::Tuple(ts) => ts
                    .iter()
                    .fold(Mode::Copy, |m, t| m.join(visit(w, t, bounds, seen))),
                Type::Function { mode, .. } => *mode,
                Type::Named(n, args) => {
                    if let Some(m) = bounds.get(n) {
                        return *m;
                    }
                    if matches!(
                        n.as_str(),
                        "u8" | "u32" | "u64" | "i64" | "f64" | "bool" | "Unit" | "Never"
                    ) {
                        return Mode::Copy;
                    }
                    if n == "String" {
                        return Mode::Affine;
                    }
                    // RegionBox[r, T] has the mode of T (§15.2).
                    if n == crate::regions::BOX_TYPE {
                        return args
                            .get(1)
                            .map_or(Mode::Linear, |t| visit(w, t, bounds, seen));
                    }
                    let Some(a) = w.adts.get(n) else {
                        return Mode::Linear;
                    };
                    let argument_modes = args
                        .iter()
                        .map(|t| visit(w, t, bounds, seen))
                        .collect::<Vec<_>>();
                    let key = format!("{n}:{argument_modes:?}");
                    if !seen.insert(key.clone()) {
                        return a.mode;
                    }
                    let component_bounds = a
                        .generics
                        .iter()
                        .zip(argument_modes)
                        .map(|(g, m)| (g.name.clone(), m))
                        .collect();
                    let mut m = a.mode;
                    for f in a.variants.iter().flat_map(|v| &v.fields) {
                        m = m.join(visit(w, &f.ty, &component_bounds, seen).meet(f.modality));
                    }
                    seen.remove(&key);
                    m
                }
            }
        }
        visit(self, ty, bounds, &mut BTreeSet::new())
    }
    pub fn field(&self, ty: &Type, path: &[String]) -> Option<Type> {
        let mut ty = ty.clone();
        for name in path {
            ty = match &ty {
                Type::Named(n, args) => {
                    let a = self.adts.get(n)?;
                    if a.is_enum {
                        return None;
                    }
                    let f = a.variants[0].fields.iter().find(|f| &f.name == name)?;
                    substitute(
                        &f.ty,
                        &a.generics
                            .iter()
                            .zip(args)
                            .map(|(p, t)| (p.name.clone(), t.clone()))
                            .collect(),
                    )
                }
                Type::Tuple(ts) => ts.get(name.parse::<usize>().ok()?)?.clone(),
                _ => return None,
            };
        }
        Some(ty)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub struct Place {
    pub root: String,
    pub path: Vec<String>,
}
impl Place {
    pub fn from_expr(e: &Expr) -> Option<Self> {
        match &e.kind {
            ExprKind::Name(n) => Some(Self {
                root: n.clone(),
                path: vec![],
            }),
            ExprKind::Field(e, f) => {
                let mut p = Self::from_expr(e)?;
                p.path.push(f.clone());
                Some(p)
            }
            _ => None,
        }
    }
    pub fn overlaps(&self, other: &Self) -> bool {
        self.root == other.root
            && (self.path.starts_with(&other.path) || other.path.starts_with(&self.path))
    }
    pub fn display(&self) -> String {
        std::iter::once(self.root.as_str())
            .chain(self.path.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(".")
    }
}
// Diagnostic-only evidence. This is deliberately excluded from ownership state
// comparisons and all published World/IR schemas. Missing/capped evidence stays
// absent rather than inventing a last use. Each binding retains at most four
// source events and only bounded place paths.
const MAX_MOVE_CAUSES: usize = 4;
#[derive(Debug, Clone)]
struct MoveCause {
    path: Vec<String>,
    span: Span,
}
#[derive(Debug, Clone)]
struct Binding {
    ty: Type,
    pass: Pass,
    projections: Vec<Vec<String>>,
    moved: Vec<Vec<String>>,
    move_causes: Vec<MoveCause>,
    span: Span,
    integer: Option<i128>,
    nonzero: bool,
}
impl Binding {
    fn remember_move(&mut self, path: &[String], span: Span) {
        if self.move_causes.len() >= MAX_MOVE_CAUSES
            || path.len() > 64
            || path
                .iter()
                .try_fold(0usize, |n, p| n.checked_add(p.len()))
                .is_none_or(|n| n > 1024)
            || self
                .move_causes
                .iter()
                .any(|c| c.path == path && c.span == span)
        {
            return;
        }
        self.move_causes.push(MoveCause {
            path: path.to_vec(),
            span,
        });
    }
}
#[derive(Debug, Clone, Default)]
struct Env {
    bindings: BTreeMap<String, Binding>,
}
impl Env {
    fn invalidate_borrowed_facts(&mut self) {
        for binding in self.bindings.values_mut().filter(|b| b.pass != Pass::Sink) {
            binding.integer = None;
            binding.nonzero = false;
        }
    }
}
#[derive(Clone, Copy)]
enum Usage {
    Own,
    Borrow(Pass),
}
#[derive(Debug, Clone, PartialEq)]
pub struct Checked {
    pub world: World,
    pub diagnostics: Vec<Diagnostic>,
    pub expression_types: TypeMap,
}
/// Checks a module. Source `trait`/`impl ... for`/`model` declarations,
/// projections and `use model` scopes are elaborated by
/// [`crate::pact_frontend`] into ordinary core functions and calls first;
/// the returned World then contains only the existing core schema.
pub fn check(module: &Module) -> Checked {
    crate::pact_frontend::check_module(module)
}

/// Static resolution of trait method calls during the frontend's first
/// checking pass. Implemented by `pact_frontend`.
pub(crate) trait TraitCalls {
    /// `None` when `call` is not a trait call; otherwise the generated
    /// function the coherent Pact session selected, or a diagnostic.
    fn resolve(&self, call: &TraitCall<'_>) -> Option<Result<String, Diagnostic>>;
}
/// A receiver (`x.m(...)`) or path (`Trait::m(...)`, `Model::m(...)`) call.
pub(crate) struct TraitCall<'a> {
    /// Method name of a receiver call, or the full path of a path call.
    pub callee: &'a str,
    pub path: bool,
    /// Type of the receiver or first argument (`Infer` when unknown).
    pub receiver: Type,
    /// Generic parameters in scope at the call.
    pub generics: &'a [Generic],
    /// Name of the function containing the call.
    pub function: &'a str,
    pub span: Span,
}

/// The core checker. With `traits`, unresolved trait calls are resolved
/// through it and recorded by call span; the Place-IR validation is then
/// skipped because the module still contains surface trait calls.
pub(crate) fn check_core(
    module: &Module,
    traits: Option<&dyn TraitCalls>,
) -> (Checked, BTreeMap<(usize, usize), String>) {
    let (mut world, collection_errors) = World::collect(module);
    let mut resolved;
    let mut diagnostics = vec![];
    let mut iteration = 0;
    loop {
        iteration += 1;
        let mut c = Checker {
            world: &world,
            diagnostics: collection_errors.clone(),
            types: BTreeMap::new(),
            calls: BTreeMap::new(),
            moves: BTreeMap::new(),
            results: BTreeMap::new(),
            handler_effects: BTreeMap::new(),
            observations: vec![],
            state_guards: vec![],
            partial_ops: BTreeSet::new(),
            overflow_ops: BTreeSet::new(),
            dead: 0,
            string_capacity: 0,
            bounds: BTreeMap::new(),
            generics: vec![],
            allowed: Effects::default(),
            caps: vec![],
            cap_aliases: BTreeMap::new(),
            loans: vec![],
            effect_inference: vec![],
            traits,
            current: String::new(),
            resolved: BTreeMap::new(),
        };
        c.declarations();
        for f in world.functions.values() {
            c.function(f);
        }
        let Checker {
            diagnostics: errors,
            types,
            calls,
            moves,
            results,
            handler_effects,
            partial_ops,
            overflow_ops,
            string_capacity,
            resolved: trait_calls,
            ..
        } = c;
        resolved = trait_calls;
        let changed = world.function_results != results || world.handler_effects != handler_effects;
        world.types = types;
        world.calls = calls;
        world.moves = moves;
        world.function_results = results;
        world.handler_effects = handler_effects;
        world.partial_ops = partial_ops;
        world.overflow_ops = overflow_ops;
        world.string_capacity = string_capacity;
        diagnostics = errors;
        if !changed {
            break;
        }
        if iteration > world.functions.len() + world.handler_effects.len() * 2 + 4 {
            diagnostics.push(Diagnostic::new("E0408","capability and continuation effect inference did not converge; add explicit type/effect annotations",0..0));
            break;
        }
    }
    if diagnostics.is_empty() && traits.is_none() {
        diagnostics.extend(crate::capability_effects::check_world(&world));
    }
    if diagnostics.is_empty() && traits.is_none() {
        diagnostics.extend(crate::regions::check_world(&world));
    }
    if diagnostics.is_empty() && traits.is_none() {
        diagnostics.extend(crate::ir_check::validate(
            &world,
            &crate::ir::lower_world(&world),
        ));
    }
    let mut seen = BTreeSet::new();
    diagnostics
        .retain(|d| seen.insert((d.code.clone(), d.span.start, d.span.end, d.message.clone())));
    (
        Checked {
            expression_types: world.types.clone(),
            world,
            diagnostics,
        },
        resolved,
    )
}
struct Checker<'a> {
    world: &'a World,
    diagnostics: Vec<Diagnostic>,
    types: TypeMap,
    calls: BTreeMap<(usize, usize), BTreeMap<String, Type>>,
    moves: BTreeMap<(usize, usize), Vec<Place>>,
    results: BTreeMap<String, Type>,
    handler_effects: BTreeMap<(usize, usize), Effects>,
    observations: Vec<(usize, Effects)>,
    state_guards: Vec<(usize, usize, Type, Span)>,
    partial_ops: BTreeSet<(usize, usize)>,
    overflow_ops: BTreeSet<(usize, usize)>,
    dead: usize,
    string_capacity: usize,
    bounds: BTreeMap<String, Mode>,
    generics: Vec<Generic>,
    allowed: Effects,
    caps: Vec<Effect>,
    cap_aliases: BTreeMap<String, String>,
    loans: Vec<(Place, Pass, Span)>,
    effect_inference: Vec<(bool, usize, Effects, String)>,
    traits: Option<&'a dyn TraitCalls>,
    current: String,
    resolved: BTreeMap<(usize, usize), String>,
}
impl Checker<'_> {
    fn error(&mut self, code: &str, msg: impl Into<String>, span: Span) {
        self.diagnostics.push(Diagnostic::new(code, msg, span));
    }
    // Called immediately after the unchanged primary producer. Fixed messages
    // and at most eight labels bound new metadata independently of source names.
    fn related(&mut self, span: Span, message: &'static str) {
        if let Some(d) = self.diagnostics.last_mut()
            && d.related.len() < 8
        {
            d.related.push(DiagnosticLabel {
                span,
                message: message.into(),
            });
        }
    }
    fn help(&mut self, message: &'static str) {
        if let Some(d) = self.diagnostics.last_mut() {
            d.help = Some(message.into());
        }
    }
    fn mode(&self, t: &Type) -> Mode {
        self.world.mode(t, &self.bounds)
    }
    fn declarations(&mut self) {
        for a in self.world.adts.values() {
            self.valid_generics(&a.generics, a.span.clone());
            self.bounds = a
                .generics
                .iter()
                .filter(|g| !g.row)
                .map(|g| (g.name.clone(), g.bound))
                .collect();
            let mut names = BTreeSet::new();
            for v in &a.variants {
                if !names.insert(v.name.clone()) {
                    self.error("E0100", "duplicate variant", a.span.clone());
                }
                let mut fields = BTreeSet::new();
                for f in &v.fields {
                    if !fields.insert(&f.name) {
                        self.error("E0100", "duplicate field", a.span.clone());
                    }
                    self.valid_type(&f.ty, &a.generics, a.span.clone());
                    if self.mode(&f.ty) > f.modality {
                        self.error(
                            "E0105",
                            format!(
                                "field '{}' cannot relax {:?} ownership to {:?}",
                                f.name,
                                self.mode(&f.ty),
                                f.modality
                            ),
                            a.span.clone(),
                        );
                    }
                }
            }
        }
        for effect in self.world.effects.values() {
            self.valid_generics(&effect.generics, effect.span.clone());
            self.bounds = effect
                .generics
                .iter()
                .filter(|g| !g.row)
                .map(|g| (g.name.clone(), g.bound))
                .collect();
            let mut names = BTreeSet::new();
            for op in &effect.ops {
                if !names.insert(&op.name) {
                    self.error("E0100", "duplicate effect operation", op.span.clone());
                }
                for p in &op.params {
                    self.valid_type(&p.ty, &effect.generics, p.span.clone());
                }
                self.valid_type(&op.result, &effect.generics, op.span.clone());
            }
        }
    }
    fn valid_type(&mut self, t: &Type, g: &[Generic], span: Span) {
        if crate::regions::active(self.world)
            && let Some(errors) = crate::regions::validate_type(t, span.clone())
        {
            self.diagnostics.extend(errors);
            // The capability argument is checked by `validate_type` itself.
            if let Type::Named(_, args) = t {
                for a in args.iter().skip(1) {
                    self.valid_type(a, g, span.clone());
                }
            }
            return;
        }
        match t {
            Type::Named(n, args) => {
                let expected = self
                    .world
                    .adts
                    .get(n)
                    .map(|a| a.generics.len())
                    .or_else(|| {
                        if matches!(
                            n.as_str(),
                            "u8" | "u32"
                                | "u64"
                                | "i64"
                                | "f64"
                                | "bool"
                                | "Unit"
                                | "Never"
                                | "String"
                        ) || g.iter().any(|p| &p.name == n && !p.row)
                        {
                            Some(0)
                        } else {
                            None
                        }
                    });
                if let Some(arity) = expected {
                    if args.len() != arity {
                        self.error(
                            "E0101",
                            format!("{n} expects {arity} type arguments, found {}", args.len()),
                            span.clone(),
                        );
                    }
                } else if !matches!(
                    n.as_str(),
                    "u8" | "u32" | "u64" | "i64" | "f64" | "bool" | "Unit" | "Never" | "String"
                ) && !g.iter().any(|p| &p.name == n)
                {
                    self.error("E0101", format!("unknown type '{n}'"), span.clone());
                }
                if let Some(decl) = self.world.adts.get(n) {
                    let vars = decl
                        .generics
                        .iter()
                        .zip(args)
                        .map(|(g, t)| (g.name.clone(), t.clone()))
                        .collect();
                    self.generic_bounds(&decl.generics, &vars, span.clone());
                }
                for a in args {
                    self.valid_type(a, g, span.clone());
                }
            }
            Type::Tuple(ts) => {
                for t in ts {
                    self.valid_type(t, g, span.clone());
                }
            }
            Type::Function {
                params,
                result,
                effects,
                ..
            } => {
                for (_, t) in params {
                    self.valid_type(t, g, span.clone());
                }
                self.valid_type(result, g, span.clone());
                self.valid_effects(effects, g, span);
            }
            Type::Row(effects) => self.valid_effects(effects, g, span),
            _ => {}
        }
    }
    fn valid_generics(&mut self, gs: &[Generic], span: Span) {
        let mut seen = BTreeSet::new();
        for g in gs {
            if !seen.insert(&g.name) {
                self.error("E0100", "duplicate generic parameter", span.clone());
            }
        }
    }
    fn valid_effects(&mut self, effects: &Effects, gs: &[Generic], span: Span) {
        if let Some(row) = &effects.tail
            && !gs.iter().any(|g| g.row && &g.name == row)
        {
            self.error("E0406", format!("unknown effect row '{row}'"), span.clone());
        }
        let mut labels = BTreeSet::new();
        for e in &effects.instances {
            if let Some(label) = &e.label
                && !labels.insert(label)
            {
                self.error("E0400", "duplicate lexical capability label", span.clone());
            }
            if crate::capability_effects::is_capability_effect(&e.name) {
                self.diagnostics
                    .extend(crate::capability_effects::row_entry(e, span.clone()));
                continue;
            }
            if let Some(decl) = self.world.effects.get(&e.name) {
                if e.args.len() != decl.generics.len() {
                    self.error(
                        "E0400",
                        format!(
                            "effect '{}' expects {} type arguments",
                            e.name,
                            decl.generics.len()
                        ),
                        span.clone(),
                    );
                }
                let vars = decl
                    .generics
                    .iter()
                    .zip(&e.args)
                    .map(|(g, t)| (g.name.clone(), t.clone()))
                    .collect();
                self.generic_bounds(&decl.generics, &vars, span.clone());
                for t in &e.args {
                    self.valid_type(t, gs, span.clone());
                }
            } else {
                self.error(
                    "E0400",
                    format!("unknown effect '{}'", e.name),
                    span.clone(),
                );
            }
        }
    }
    fn function(&mut self, f: &Function) {
        self.current = f.name.clone();
        self.bounds = f
            .generics
            .iter()
            .filter(|g| !g.row)
            .map(|g| (g.name.clone(), g.bound))
            .collect();
        self.generics = f.generics.clone();
        self.valid_generics(&f.generics, f.span.clone());
        self.allowed = f.effects.clone();
        self.caps = f
            .effects
            .instances
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let mut e = e.clone();
                e.label = Some(function_capability(&f.name, i));
                e
            })
            .collect();
        self.cap_aliases = f
            .effects
            .instances
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                e.label
                    .clone()
                    .map(|label| (label, function_capability(&f.name, i)))
            })
            .collect();
        self.loans.clear();
        self.valid_type(&f.result, &f.generics, f.span.clone());
        let mut env = Env::default();
        for p in &f.params {
            if p.ty == Type::Infer {
                self.error(
                    "E0102",
                    "function parameters require a type",
                    p.span.clone(),
                );
            }
            self.valid_type(&p.ty, &f.generics, p.span.clone());
            for path in &p.projections {
                if self.world.field(&p.ty, path).is_none() {
                    self.error(
                        "E0300",
                        format!("invalid projection '{}.{}'", p.name, path.join(".")),
                        p.span.clone(),
                    );
                }
            }
            for (i, a) in p.projections.iter().enumerate() {
                for b in &p.projections[i + 1..] {
                    if a.starts_with(b) || b.starts_with(a) {
                        self.error("E0301", "overlapping parameter projections", p.span.clone());
                    }
                }
            }
            if env
                .bindings
                .insert(
                    p.name.clone(),
                    Binding {
                        ty: bind_signature_capabilities(&p.ty, f),
                        pass: p.pass,
                        projections: p.projections.clone(),
                        moved: vec![],
                        move_causes: vec![],
                        span: p.span.clone(),
                        integer: None,
                        nonzero: false,
                    },
                )
                .is_some()
            {
                self.error("E0100", "duplicate parameter", p.span.clone());
            }
        }
        self.valid_effects(&f.effects, &f.generics, f.span.clone());
        if let Some(body) = &f.body {
            let actual = self.expr_expected(body, &mut env, Usage::Own, &f.result);
            self.expect_type(&f.result, &actual, body.span.clone());
            self.results
                .insert(f.name.clone(), refine_annotation(&f.result, &actual));
            self.exhaust(
                &env,
                env.bindings.keys().cloned().collect(),
                body.span.clone(),
            );
        } else {
            self.error(
                "E0103",
                "a function outside an effect declaration requires a body",
                f.span.clone(),
            );
        }
    }
    fn join_types(&mut self, a: &Type, b: &Type, span: Span) -> Type {
        if a.name() == "Never" || *a == Type::Infer {
            return b.clone();
        }
        if b.name() == "Never" || *b == Type::Infer {
            return a.clone();
        }
        match (a, b) {
            (Type::IntLiteral(x), Type::IntLiteral(y)) => {
                let n = if (*x).max(*y) > i64::MAX as i128 {
                    (*x).max(*y)
                } else if (*x).min(*y) < 0 {
                    (*x).min(*y)
                } else {
                    (*x).max(*y)
                };
                let kind = crate::numeric::IntegerKind::default_for(n);
                if !kind.fits(*x) || !kind.fits(*y) {
                    self.error("E0200", "branches have no common integer type", span);
                }
                Type::IntLiteral(n)
            }
            (Type::IntLiteral(_), _) => b.clone(),
            (_, Type::IntLiteral(_)) => a.clone(),
            (Type::Named(n, aa), Type::Named(m, bb)) if n == m && aa.len() == bb.len() => {
                Type::Named(
                    n.clone(),
                    aa.iter()
                        .zip(bb)
                        .map(|(a, b)| self.join_types(a, b, span.clone()))
                        .collect(),
                )
            }
            (Type::Tuple(aa), Type::Tuple(bb)) if aa.len() == bb.len() => Type::Tuple(
                aa.iter()
                    .zip(bb)
                    .map(|(a, b)| self.join_types(a, b, span.clone()))
                    .collect(),
            ),
            (
                Type::Function {
                    params,
                    result,
                    effects,
                    mode,
                },
                Type::Function {
                    result: br,
                    effects: be,
                    mode: bm,
                    ..
                },
            ) => {
                let mut row = effects.clone();
                union_effects(&mut row, be);
                Type::Function {
                    params: params.clone(),
                    result: Box::new(self.join_types(result, br, span)),
                    effects: row,
                    mode: mode.join(*bm),
                }
            }
            _ => a.clone(),
        }
    }
    fn expect_type(&mut self, expected: &Type, actual: &Type, span: Span) {
        if !compatible(expected, actual) {
            if let Some(d) = crate::regions::mismatch(expected, actual, span.clone()) {
                self.diagnostics.push(d);
                return;
            }
            self.error(
                "E0200",
                format!("expected {expected}, found {actual}"),
                span,
            );
        }
    }
    fn exhaust(&mut self, env: &Env, names: Vec<String>, span: Span) {
        for n in names {
            if let Some(b) = env.bindings.get(&n)
                && b.pass == Pass::Sink
                && self.mode(&b.ty) == Mode::Linear
                && !b.moved.iter().any(Vec::is_empty)
            {
                self.error("E0302",format!("linear value '{n}' must be consumed exactly once; destructure it or pass it to a sink parameter"),span.clone());
                self.related(b.span.clone(), "linear binding introduced here");
                self.help("Every owned linear binding must be consumed exactly once on every returning path.");
            }
        }
    }
    fn place(&mut self, p: &Place, env: &mut Env, usage: Usage, span: Span) -> Type {
        let Some(b) = env.bindings.get(&p.root).cloned() else {
            self.error("E0201", format!("unknown binding '{}'", p.root), span);
            return Type::Infer;
        };
        let mut transfer_allowed = true;
        if b.moved
            .iter()
            .any(|path| path.starts_with(&p.path) || p.path.starts_with(path))
        {
            transfer_allowed = false;
            self.error(
                "E0303",
                format!("use of moved place '{}'", p.display()),
                span.clone(),
            );
            self.related(b.span.clone(), "binding introduced here");
            for cause in &b.move_causes {
                if cause.path.starts_with(&p.path) || p.path.starts_with(&cause.path) {
                    self.related(
                        cause.span.clone(),
                        "earlier ownership-taking use of this place",
                    );
                }
            }
            self.help("A moved non-copy place cannot be used again, including through an overlapping projection.");
        }
        if !b.projections.is_empty() && !b.projections.iter().any(|path| p.path.starts_with(path)) {
            transfer_allowed = false;
            self.error(
                "E0304",
                format!(
                    "'{}' is outside the parameter's declared projections",
                    p.display()
                ),
                span.clone(),
            );
        }
        let ty = self.world.field(&b.ty, &p.path).unwrap_or_else(|| {
            transfer_allowed = false;
            self.error(
                "E0202",
                format!("unknown field in '{}'", p.display()),
                span.clone(),
            );
            Type::Infer
        });
        for (loan, mode, origin) in self.loans.clone() {
            let exclusive = mode != Pass::In
                || matches!(usage, Usage::Borrow(Pass::Inout))
                || (matches!(usage, Usage::Own) && self.mode(&ty) != Mode::Copy);
            if exclusive && loan.overlaps(p) {
                transfer_allowed = false;
                self.error(
                    "E0301",
                    format!(
                        "access to '{}' conflicts with active argument loan '{}'",
                        p.display(),
                        loan.display()
                    ),
                    span.clone(),
                );
                self.related(origin, "conflicting argument access requested here");
                self.related(b.span.clone(), "accessed binding introduced here");
                self.help("An exclusive argument loan cannot overlap another access while the call's arguments are evaluated.");
            }
        }
        match usage {
            Usage::Own if self.mode(&ty) != Mode::Copy => {
                if b.pass != Pass::Sink {
                    transfer_allowed = false;
                    self.error(
                        "E0305",
                        format!("cannot move '{}' through a borrowed parameter", p.display()),
                        span.clone(),
                    );
                    self.related(b.span.clone(), "borrowed parameter declared here");
                    self.help("Borrowed parameters do not transfer ownership of non-copy values.");
                }
                if !p.path.is_empty() && self.mode(&b.ty) == Mode::Linear {
                    transfer_allowed = false;
                    self.error(
                        "E0306",
                        "destructure a linear aggregate before moving its fields",
                        span.clone(),
                    );
                    self.related(b.span.clone(), "linear aggregate binding introduced here");
                }
                if let Some(b) = env.bindings.get_mut(&p.root) {
                    b.moved.push(p.path.clone());
                    // Keep error recovery unchanged, but failed move attempts
                    // are not witnesses of a preceding ownership transfer.
                    if transfer_allowed {
                        b.remember_move(&p.path, span.clone());
                    }
                }
            }
            Usage::Borrow(Pass::Inout) if b.pass == Pass::In => {
                self.error(
                    "E0307",
                    format!("cannot mutate shared parameter '{}'", p.root),
                    span,
                );
                self.related(b.span.clone(), "shared parameter declared here");
            }
            _ => {}
        }
        ty
    }
    fn expr(&mut self, e: &Expr, env: &mut Env, usage: Usage) -> Type {
        self.expr_expected(e, env, usage, &Type::Infer)
    }
    fn expr_expected(&mut self, e: &Expr, env: &mut Env, usage: Usage, expected: &Type) -> Type {
        let before = if matches!(e.kind, ExprKind::Handle { .. } | ExprKind::Closure { .. }) {
            Some(env.clone())
        } else {
            None
        };
        let ty = match &e.kind {
            ExprKind::Literal(l) => {
                if let Literal::String(s) = l {
                    self.string_capacity = self.string_capacity.max(s.len());
                }
                let t = literal_type(l);
                if let Type::IntLiteral(n) = t {
                    if let Some(kind) = crate::numeric::IntegerKind::from_type(expected) {
                        if !kind.fits(n) {
                            self.error(
                                "E0209",
                                format!("integer literal {n} is outside the range of {expected}"),
                                e.span.clone(),
                            );
                        }
                        kind.ty()
                    } else {
                        if !crate::numeric::IntegerKind::default_for(n).fits(n) {
                            self.error(
                                "E0209",
                                "integer literal is outside the supported ranges",
                                e.span.clone(),
                            );
                        }
                        t
                    }
                } else {
                    t
                }
            }
            ExprKind::Name(n) => {
                if env.bindings.contains_key(n) {
                    self.place(
                        &Place {
                            root: n.clone(),
                            path: vec![],
                        },
                        env,
                        usage,
                        e.span.clone(),
                    )
                } else if let Some(f) = self.world.functions.get(n) {
                    function_value_type(f, self.world.function_results.get(&f.name))
                } else if let Some((a, v)) = self.world.constructor(n) {
                    if v.fields.is_empty() {
                        if let Type::Named(name, _) = expected
                            && name == &a.name
                        {
                            expected.clone()
                        } else {
                            Type::Named(
                                a.name.clone(),
                                a.generics.iter().map(|_| Type::Infer).collect(),
                            )
                        }
                    } else {
                        self.error("E0203", "constructor requires arguments", e.span.clone());
                        Type::Infer
                    }
                } else {
                    self.error("E0201", format!("unknown name '{n}'"), e.span.clone());
                    Type::Infer
                }
            }
            ExprKind::Tuple(es) => {
                Type::Tuple(es.iter().map(|e| self.expr(e, env, Usage::Own)).collect())
            }
            ExprKind::Struct { name, fields } => self.construct(
                name,
                fields.iter().map(|(n, e)| (n.clone(), e)).collect(),
                env,
                e.span.clone(),
                expected,
            ),
            ExprKind::Field(_, _) => {
                if let Some(p) = Place::from_expr(e) {
                    self.place(&p, env, usage, e.span.clone())
                } else {
                    let ExprKind::Field(base, f) = &e.kind else {
                        unreachable!()
                    };
                    let t = self.expr(base, env, Usage::Own);
                    if self.mode(&t) == Mode::Linear {
                        self.error(
                            "E0306",
                            "destructure a temporary linear aggregate before projecting its fields",
                            e.span.clone(),
                        );
                    }
                    self.world
                        .field(&t, std::slice::from_ref(f))
                        .unwrap_or_else(|| {
                            self.error("E0202", "unknown field", e.span.clone());
                            Type::Infer
                        })
                }
            }
            ExprKind::Index(base, index) => {
                let t = self.expr(base, env, Usage::Borrow(Pass::In));
                let i = self.expr(index, env, Usage::Own);
                if !integer(&i) {
                    self.error("E0200", "index must be an integer", index.span.clone());
                }
                match t {
                    Type::Tuple(ts) => {
                        if ts.is_empty() {
                            self.error("E0200", "cannot index an empty tuple", e.span.clone());
                        }
                        if !self
                            .integer_fact(index, env)
                            .is_some_and(|n| n >= 0 && n < ts.len() as i128)
                        {
                            self.primitive_failure(env, e.span.clone());
                        }
                        let mut ty = Type::Infer;
                        for t in &ts {
                            ty = self.join_types(&ty, t, e.span.clone());
                        }
                        if ts.iter().any(|t| !compatible(&ty, t)) || self.mode(&ty) != Mode::Copy {
                            self.error("E0311", "dynamic indexing requires homogeneous copy elements; destructure owned aggregates", e.span.clone());
                        }
                        for t in &ts {
                            self.expect_type(&ty, t, e.span.clone());
                        }
                        self.constrain_literals(base, &Type::Tuple(vec![ty.clone(); ts.len()]));
                        self.integer_fact(index, env)
                            .and_then(|n| usize::try_from(n).ok())
                            .and_then(|n| ts.get(n))
                            .cloned()
                            .unwrap_or(ty)
                    }
                    _ => {
                        self.error("E0200", "indexing requires a tuple", base.span.clone());
                        Type::Infer
                    }
                }
            }
            ExprKind::Block { statements, tail } => {
                let original = env.clone();
                let mut shadowed = BTreeMap::new();
                let mut result = Type::unit();
                for s in statements {
                    if result.name() == "Never" {
                        break;
                    }
                    match s {
                        Statement::Let { pat, ty, value } => {
                            let ty = &self.region_annotation(ty);
                            self.valid_type(ty, &self.generics.clone(), value.span.clone());
                            let actual = self.expr_expected(value, env, Usage::Own, ty);
                            self.expect_type(ty, &actual, value.span.clone());
                            let t = if *ty == Type::Infer {
                                crate::numeric::default_type(&actual)
                            } else {
                                refine_annotation(ty, &actual)
                            };
                            if unresolved(&t) {
                                self.error("E0208", "cannot infer an ownership-relevant type; add a type annotation", value.span.clone());
                            }
                            if !self.irrefutable(pat, &t) {
                                self.error(
                                    "E0210",
                                    "let pattern must be irrefutable; use match",
                                    pat.span.clone(),
                                );
                            }
                            for n in pattern_names(pat) {
                                if let (Some(before), Some(current)) =
                                    (original.bindings.get(&n), env.bindings.get(&n))
                                    && before.span == current.span
                                {
                                    shadowed.insert(n, current.clone());
                                }
                            }
                            let fact = self.integer_fact(value, env);
                            let nonzero = self.nonzero(value, env);
                            self.bind(pat, &t, env);
                            if let PatternKind::Bind(n) = &pat.kind {
                                let binding = env.bindings.get_mut(n).unwrap();
                                binding.integer = fact;
                                binding.nonzero = nonzero;
                            }
                        }
                        Statement::Expr(e) => {
                            let t = self.expr(e, env, Usage::Own);
                            if self.mode(&t) == Mode::Linear {
                                self.error(
                                    "E0302",
                                    "unused linear result must be consumed",
                                    e.span.clone(),
                                );
                            }
                            if t.name() == "Never" {
                                result = t;
                            }
                        }
                    }
                }
                if result.name() != "Never"
                    && let Some(tail) = tail
                {
                    result = self.expr_expected(tail, env, Usage::Own, expected);
                }
                let locals = env
                    .bindings
                    .iter()
                    .filter(|(n, b)| {
                        original
                            .bindings
                            .get(*n)
                            .is_none_or(|old| old.span != b.span)
                    })
                    .map(|(n, _)| n.clone())
                    .collect::<Vec<_>>();
                self.exhaust(env, locals.clone(), e.span.clone());
                for n in locals {
                    env.bindings.remove(&n);
                }
                for (n, b) in shadowed {
                    env.bindings.insert(n, b);
                }
                result
            }
            ExprKind::Call {
                callee,
                types,
                args,
            } => self.call(callee, types, args, env, e.span.clone(), expected),
            ExprKind::Unary(op, v) => {
                let t = self.expr_expected(v, env, Usage::Own, expected);
                let t = crate::numeric::default_type(&t);
                if op == "!" {
                    self.expect_type(&Type::named("bool"), &t, e.span.clone());
                    Type::named("bool")
                } else {
                    if !numeric(&t) {
                        self.error("E0200", "negation requires a number", e.span.clone());
                    }
                    if let Some(kind) = crate::numeric::IntegerKind::from_type(&t)
                        && self
                            .integer_fact(v, env)
                            .and_then(|n| kind.checked_neg(n))
                            .is_none()
                    {
                        self.overflow_failure(env, e.span.clone());
                    }
                    t
                }
            }
            ExprKind::Binary(op, l, r) => {
                let arithmetic = matches!(op.as_str(), "+" | "-" | "*" | "/" | "%");
                let goal = if matches!(op.as_str(), "&&" | "||") {
                    Type::named("bool")
                } else if arithmetic && numeric(expected) && *expected != Type::Infer {
                    expected.clone()
                } else {
                    Type::Infer
                };
                let a = self.expr_expected(l, env, Usage::Own, &goal);
                // The dividend's fact is the value already evaluated: the right
                // operand may reassign a binding the left one read.
                let left_fact = self.integer_fact(l, env);
                let right_goal = if goal != Type::Infer {
                    goal
                } else if !matches!(a, Type::IntLiteral(_)) {
                    a.clone()
                } else {
                    Type::Infer
                };
                let b = if matches!(op.as_str(), "&&" | "||") {
                    let before = env.clone();
                    let dead = matches!(
                        (&l.kind, op.as_str()),
                        (ExprKind::Literal(Literal::Bool(false)), "&&")
                            | (ExprKind::Literal(Literal::Bool(true)), "||")
                    );
                    self.dead += usize::from(dead);
                    let b = self.expr_expected(r, env, Usage::Own, &right_goal);
                    self.dead -= usize::from(dead);
                    let after = env.clone();
                    if dead {
                        *env = before;
                    } else {
                        self.merge(env, &before, &after, e.span.clone());
                    }
                    b
                } else {
                    self.expr_expected(r, env, Usage::Own, &right_goal)
                };
                let common = match (&a, &b) {
                    (Type::IntLiteral(x), Type::IntLiteral(y)) => {
                        crate::numeric::IntegerKind::default_for((*x).max(*y)).ty()
                    }
                    (Type::IntLiteral(_), t) => t.clone(),
                    (t, _) => t.clone(),
                };
                self.expect_type(&common, &a, l.span.clone());
                self.expect_type(&common, &b, r.span.clone());
                if matches!(a, Type::IntLiteral(_)) {
                    self.types
                        .insert((l.span.start, l.span.end), common.clone());
                }
                if matches!(b, Type::IntLiteral(_)) {
                    self.types
                        .insert((r.span.start, r.span.end), common.clone());
                }
                if (arithmetic || matches!(op.as_str(), "<" | "<=" | ">" | ">="))
                    && !numeric(&common)
                {
                    self.error(
                        "E0200",
                        "this operator requires numeric operands",
                        e.span.clone(),
                    );
                }
                if matches!(op.as_str(), "==" | "!=") && self.mode(&common) == Mode::Linear {
                    self.error("E0302","equality cannot discharge linear obligations; compare copy fields and explicitly consume their owners",e.span.clone());
                }
                if matches!(op.as_str(), "/" | "%") && integer(&common) && !self.nonzero(r, env) {
                    self.primitive_failure(env, e.span.clone());
                }
                if let Some(kind) = crate::numeric::IntegerKind::from_type(&common)
                    && crate::numeric::may_overflow(op, kind)
                    && !self.in_range(op, kind, left_fact, self.integer_fact(r, env))
                {
                    self.overflow_failure(env, e.span.clone());
                }
                if matches!(
                    op.as_str(),
                    "&&" | "||" | "==" | "!=" | "<" | "<=" | ">" | ">="
                ) {
                    Type::named("bool")
                } else {
                    common
                }
            }
            ExprKind::Assign(op, l, r) => {
                let Some(p) = Place::from_expr(l) else {
                    self.error("E0300", "assignment requires a place", l.span.clone());
                    return Type::Infer;
                };
                let expected = self.place(&p, env, Usage::Borrow(Pass::Inout), l.span.clone());
                self.types
                    .insert((l.span.start, l.span.end), expected.clone());
                let target_fact = |env: &Env| {
                    env.bindings
                        .get(&p.root)
                        .filter(|_| p.path.is_empty())
                        .and_then(|b| b.integer)
                };
                let before = target_fact(env);
                let actual = self.expr_expected(r, env, Usage::Own, &expected);
                self.expect_type(&expected, &actual, r.span.clone());
                if bound_capabilities(&expected) != bound_capabilities(&actual) {
                    self.error(
                        "E0403",
                        "assignment cannot change a place's lexical capability scope",
                        r.span.clone(),
                    );
                }
                if self.mode(&expected) == Mode::Linear {
                    self.error(
                        "E0302",
                        "assignment cannot discard an existing linear value",
                        l.span.clone(),
                    );
                }
                if op != "=" && !numeric(&expected) {
                    self.error(
                        "E0200",
                        "compound assignment requires numeric operands",
                        l.span.clone(),
                    );
                }
                if matches!(op.as_str(), "/=" | "%=") && integer(&expected) && !self.nonzero(r, env)
                {
                    self.primitive_failure(env, e.span.clone());
                }
                // If the right side reassigns the target, either value may be
                // the one combined, so only an unchanged fact is trusted.
                let old = target_fact(env).filter(|_| target_fact(env) == before);
                let value = self.integer_fact(r, env);
                let kind = crate::numeric::IntegerKind::from_type(&expected);
                if let Some(kind) = kind
                    && op != "="
                    && crate::numeric::may_overflow(op, kind)
                    && !self.in_range(&op[..1], kind, old, value)
                {
                    self.overflow_failure(env, e.span.clone());
                }
                let fact = if op == "=" {
                    value
                } else {
                    kind.zip(old)
                        .zip(value)
                        .and_then(|((k, a), b)| k.checked(&op[..1], a, b).ok())
                };
                let nonzero = fact.is_some_and(|n| n != 0) || (op == "=" && self.nonzero(r, env));
                // A write to a field never establishes a fact about its root.
                if let Some(binding) = env.bindings.get_mut(&p.root) {
                    binding.integer = fact.filter(|_| p.path.is_empty());
                    binding.nonzero = nonzero && p.path.is_empty();
                }
                Type::unit()
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                let t = self.expr(condition, env, Usage::Own);
                self.expect_type(&Type::named("bool"), &t, condition.span.clone());
                let mut left = env.clone();
                Self::refine_condition(condition, &mut left, true);
                let a = self.expr_expected(then, &mut left, Usage::Own, expected);
                let mut right = env.clone();
                Self::refine_condition(condition, &mut right, false);
                let b = otherwise
                    .as_ref()
                    .map(|e| self.expr_expected(e, &mut right, Usage::Own, expected))
                    .unwrap_or_else(Type::unit);
                let result = self.join_types(&a, &b, e.span.clone());
                self.expect_type(&result, &a, then.span.clone());
                self.expect_type(&result, &b, e.span.clone());
                self.constrain_literals(then, &result);
                if let Some(otherwise) = otherwise {
                    self.constrain_literals(otherwise, &result);
                }
                if a.name() == "Never" {
                    *env = right;
                } else if b.name() == "Never" {
                    *env = left;
                } else {
                    self.merge(env, &left, &right, e.span.clone());
                }
                result
            }
            ExprKind::Match { value, arms } => {
                let t = self.expr(value, env, Usage::Own);
                self.coverage(arms, &t, e.span.clone());
                let original = env.clone();
                let mut branches = vec![];
                let mut result = Type::named("Never");
                for (pat, body) in arms {
                    let mut arm = original.clone();
                    self.bind(pat, &t, &mut arm);
                    let arm_type = self.expr_expected(body, &mut arm, Usage::Own, expected);
                    let names = arm
                        .bindings
                        .iter()
                        .filter(|(n, b)| {
                            original
                                .bindings
                                .get(*n)
                                .is_none_or(|old| old.span != b.span)
                        })
                        .map(|(n, _)| n.clone())
                        .collect::<Vec<_>>();
                    self.exhaust(&arm, names.clone(), body.span.clone());
                    for n in names {
                        arm.bindings.remove(&n);
                        if let Some(old) = original.bindings.get(&n) {
                            arm.bindings.insert(n, old.clone());
                        }
                    }
                    if result.name() == "Never" {
                        result = arm_type.clone();
                    } else {
                        let common = self.join_types(&result, &arm_type, body.span.clone());
                        self.expect_type(&common, &result, body.span.clone());
                        self.expect_type(&common, &arm_type, body.span.clone());
                        result = common;
                    }
                    if arm_type.name() != "Never" {
                        branches.push(arm);
                    }
                }
                for (_, body) in arms {
                    self.constrain_literals(body, &result);
                }
                if let Some(first) = branches.first() {
                    *env = first.clone();
                    for branch in branches.iter().skip(1) {
                        let prev = env.clone();
                        self.merge(env, &prev, branch, e.span.clone());
                    }
                }
                result
            }
            ExprKind::Try(value) => {
                let target = if *expected != Type::Infer {
                    Type::Named("Result".into(), vec![expected.clone(), Type::Infer])
                } else {
                    Type::Infer
                };
                let t = self.expr_expected(value, env, Usage::Own, &target);
                if let Type::Named(n, args) = t {
                    if n == "Result" && args.len() == 2 {
                        self.require_effect(
                            &Effect {
                                label: None,
                                name: "Fail".into(),
                                args: vec![args[1].clone()],
                            },
                            env,
                            e.span.clone(),
                        );
                        args[0].clone()
                    } else {
                        self.error("E0200", "'?' requires Result[T, E]", e.span.clone());
                        Type::Infer
                    }
                } else {
                    self.error("E0200", "'?' requires Result[T, E]", e.span.clone());
                    Type::Infer
                }
            }
            ExprKind::Closure {
                params,
                mode,
                result,
                effects,
                infer_effects,
                body,
            } => {
                let mut params = params.clone();
                let mut result = result.clone();
                if let Type::Function {
                    params: expected_params,
                    result: expected_result,
                    ..
                } = expected
                {
                    if params.len() != expected_params.len() {
                        self.error(
                            "E0207",
                            "closure parameter count differs from expected function type",
                            e.span.clone(),
                        );
                    }
                    for (p, (pass, t)) in params.iter_mut().zip(expected_params) {
                        if p.pass != *pass {
                            self.error("E0308","closure parameter passing mode differs from expected function type",p.span.clone());
                        }
                        if p.ty == Type::Infer {
                            p.ty = t.clone();
                        }
                    }
                    if result == Type::Infer {
                        result = *expected_result.clone();
                    }
                }
                self.closure(
                    &params,
                    *mode,
                    &result,
                    effects,
                    *infer_effects,
                    body,
                    env,
                    e.span.clone(),
                )
            }
            ExprKind::Handle {
                effect,
                state,
                body,
                ret,
                ops,
            } => self.handle(effect, state, body, ret, ops, env, e.span.clone(), expected),
        };
        let ty = instantiate_latent_effects(&ty, expected);
        if *expected != Type::Infer {
            self.expect_type(expected, &ty, e.span.clone());
        }
        if let Some(before) = before {
            let mut moved = vec![];
            for (n, b) in &env.bindings {
                if let Some(old) = before.bindings.get(n)
                    && b.span == old.span
                {
                    for path in &b.moved {
                        if !old.moved.contains(path) {
                            moved.push(Place {
                                root: n.clone(),
                                path: path.clone(),
                            });
                        }
                    }
                }
            }
            self.moves.insert((e.span.start, e.span.end), moved);
        }
        self.types.insert((e.span.start, e.span.end), ty.clone());
        ty
    }
    fn construct(
        &mut self,
        name: &str,
        fields: Vec<(String, &Expr)>,
        env: &mut Env,
        span: Span,
        expected: &Type,
    ) -> Type {
        let Some((a, v)) = self.world.constructor(name) else {
            self.error("E0201", format!("unknown constructor '{name}'"), span);
            return Type::Infer;
        };
        let mut vars = BTreeMap::new();
        if *expected != Type::Infer {
            self.valid_type(expected, &self.generics.clone(), span.clone());
        }
        if let Type::Named(n, args) = expected
            && n == &a.name
        {
            vars.extend(
                a.generics
                    .iter()
                    .zip(args)
                    .map(|(g, t)| (g.name.clone(), t.clone())),
            );
        }
        let mut seen = BTreeSet::new();
        for (n, e) in fields {
            let mut context_vars = vars.clone();
            for g in &a.generics {
                context_vars.entry(g.name.clone()).or_insert(Type::Infer);
            }
            let target = v
                .fields
                .iter()
                .find(|f| f.name == n)
                .map(|f| substitute(&f.ty, &context_vars))
                .unwrap_or(Type::Infer);
            let t = self.expr_expected(e, env, Usage::Own, &target);
            if !seen.insert(n.clone()) {
                self.error("E0204", "duplicate constructor field", e.span.clone());
            }
            if let Some(f) = v.fields.iter().find(|f| f.name == n) {
                infer(&f.ty, &t, &a.generics, &mut vars);
                self.expect_type(&substitute(&f.ty, &vars), &t, e.span.clone());
            } else {
                self.error("E0202", format!("unknown field '{n}'"), e.span.clone());
            }
        }
        if seen.len() != v.fields.len() {
            self.error(
                "E0204",
                format!("constructor '{name}' requires {} fields", v.fields.len()),
                span.clone(),
            );
        }
        self.generic_bounds(&a.generics, &vars, span);
        Type::Named(
            a.name.clone(),
            a.generics
                .iter()
                .map(|g| vars.get(&g.name).cloned().unwrap_or(Type::Infer))
                .collect(),
        )
    }
    fn generic_bounds(&mut self, params: &[Generic], vars: &BTreeMap<String, Type>, span: Span) {
        for g in params {
            if let Some(t) = vars.get(&g.name)
                && *t != Type::Infer
                && g.row != matches!(t, Type::Row(_))
            {
                self.error(
                    "E0205",
                    "generic type and effect-row arguments have different kinds",
                    span.clone(),
                );
            }
            if let Some(t) = vars.get(&g.name)
                && !g.row
                && self.mode(t) > g.bound
            {
                self.error(
                    "E0205",
                    format!(
                        "type {t} has mode {:?}, exceeding {:?} bound for {}",
                        self.mode(t),
                        g.bound,
                        g.name
                    ),
                    span.clone(),
                );
            }
        }
    }
    fn call(
        &mut self,
        callee: &Expr,
        types: &[Type],
        args: &[Arg],
        env: &mut Env,
        span: Span,
        expected: &Type,
    ) -> Type {
        if let ExprKind::Name(name) = &callee.kind
            && !env.bindings.contains_key(name)
        {
            if self.world.constructor(name).is_some() {
                let t = self.construct(
                    name,
                    args.iter()
                        .enumerate()
                        .map(|(i, a)| (i.to_string(), &a.expr))
                        .collect(),
                    env,
                    span.clone(),
                    &if types.is_empty() {
                        expected.clone()
                    } else {
                        Type::Named(
                            self.world.constructor(name).unwrap().0.name.clone(),
                            types.to_vec(),
                        )
                    },
                );
                if !types.is_empty()
                    && let Type::Named(n, actual) = &t
                {
                    let expected = Type::Named(n.clone(), types.to_vec());
                    self.expect_type(&expected, &Type::Named(n.clone(), actual.clone()), span);
                    return expected;
                }
                return t;
            }
            if crate::numeric::wrapping_intrinsic(name).is_some() {
                return self.wrapping_call(name, types, args, env, span, expected);
            }
            if let Some(f) = self.world.functions.get(name) {
                return self.apply(f, types, args, None, env, span, expected);
            }
            if name
                .split_once("::")
                .is_some_and(|(e, _)| e == crate::regions::REGION_EFFECT)
                && crate::regions::active(self.world)
            {
                self.error(
                    "E0790",
                    "region operations are called through their capability, as in `r.box(value)`",
                    span,
                );
                for a in args {
                    self.expr(&a.expr, env, Usage::Own);
                }
                return Type::Infer;
            }
            if let Some((eff, op)) = name.split_once("::")
                && let Some(decl) = self.world.effects.get(eff)
                && let Some(f) = decl.ops.iter().find(|f| f.name == op)
            {
                let f = f.clone();
                let mut vars = BTreeMap::new();
                let mut f = f;
                f.generics = decl.generics.clone();
                let context = self
                    .active_effect(&Effect {
                        label: None,
                        name: eff.into(),
                        args: vec![],
                    })
                    .or_else(|| {
                        self.allowed
                            .instances
                            .iter()
                            .rev()
                            .find(|e| e.name == eff)
                            .cloned()
                    });
                let inferred_types = if types.is_empty() {
                    context.map(|e| e.args).unwrap_or_default()
                } else {
                    types.to_vec()
                };
                let result = self.apply_args(
                    &f,
                    &inferred_types,
                    args,
                    None,
                    env,
                    span.clone(),
                    &mut vars,
                );
                let effect = Effect {
                    label: None,
                    name: eff.into(),
                    args: decl
                        .generics
                        .iter()
                        .map(|g| vars.get(&g.name).cloned().unwrap_or(Type::Infer))
                        .collect(),
                };
                self.require_effect(&effect, env, span);
                return result;
            }
            if name.contains("::")
                && let Some(f) =
                    self.trait_call(name, true, args.first().map(|a| &a.expr), env, &span)
            {
                return match f {
                    Some(f) => self.apply(&f, types, args, None, env, span, expected),
                    None => Type::Infer,
                };
            }
        }
        if let ExprKind::Field(base, method) = &callee.kind {
            if let ExprKind::Name(label) = &base.kind
                && !env.bindings.contains_key(label)
                && let Some(effect) = self.region_capability(label)
            {
                return self.region_op(&effect, method, args, env, span, expected);
            }
            if let ExprKind::Name(label) = &base.kind
                && !env.bindings.contains_key(label)
                && let Some(effect) = self
                    .caps
                    .iter()
                    .rev()
                    .chain(self.allowed.instances.iter())
                    .find(|e| {
                        e.label.as_ref() == Some(self.cap_aliases.get(label).unwrap_or(label))
                    })
                    .cloned()
                && let Some(decl) = self.world.effects.get(&effect.name)
                && let Some(f) = decl.ops.iter().find(|f| &f.name == method)
            {
                let mut f = f.clone();
                f.generics = decl.generics.clone();
                let mut vars = BTreeMap::new();
                let result =
                    self.apply_args(&f, &effect.args, args, None, env, span.clone(), &mut vars);
                self.require_effect(&effect, env, span);
                return result;
            }
            let t = self.peek_type(base, env);
            let name = format!("{}::{method}", t.name());
            if let Some(f) = self.world.functions.get(&name) {
                return self.apply(f, types, args, Some(base), env, span, expected);
            }
            if let Some(f) = self.trait_call(method, false, Some(base), env, &span) {
                return match f {
                    Some(f) => self.apply(&f, types, args, Some(base), env, span, expected),
                    None => Type::Infer,
                };
            }
        }
        let t = self.expr(callee, env, Usage::Own);
        if let Type::Function {
            params,
            result,
            effects,
            mode,
        } = t
        {
            let f = Function {
                name: "<closure>".into(),
                generics: vec![],
                params: params
                    .iter()
                    .enumerate()
                    .map(|(i, (pass, ty))| Param {
                        name: i.to_string(),
                        pass: *pass,
                        ty: ty.clone(),
                        projections: vec![],
                        span: span.clone(),
                    })
                    .collect(),
                result: *result,
                effects,
                body: None,
                span: span.clone(),
            };
            let result = self.apply(&f, types, args, None, env, span, expected);
            if mode != Mode::Copy {
                env.invalidate_borrowed_facts();
            }
            result
        } else {
            self.error("E0206", format!("{t} is not callable"), span);
            Type::Infer
        }
    }
    /// Resolves a trait call through the frontend. `None`: not a trait call.
    /// `Some(None)`: a trait call that failed (diagnosed). `Some(Some(f))`:
    /// the selected generated function, recorded for elaboration.
    /// The static type of a trait-call receiver, determined without
    /// checking the expression (its ownership and effects are checked once,
    /// when the call is applied). Chained method calls are typed through
    /// the inherent method or the trait conformance their own receiver
    /// selects.
    fn receiver_type(&mut self, e: &Expr, env: &Env) -> Type {
        if let ExprKind::Literal(l) = &e.kind {
            return crate::numeric::default_type(&literal_type(l));
        }
        let t = self.peek_type(e, env);
        if t != Type::Infer {
            return t;
        }
        if let Some(t) = self.world.types.get(&(e.span.start, e.span.end))
            && *t != Type::Infer
        {
            return t.clone();
        }
        // Receivers that are not places: calls and struct literals have a
        // declared type.
        match &e.kind {
            ExprKind::Call { callee, .. } => match &callee.kind {
                ExprKind::Name(n) if !env.bindings.contains_key(n) => self
                    .world
                    .functions
                    .get(n)
                    .filter(|f| f.generics.is_empty())
                    .map(|f| f.result.clone())
                    .unwrap_or(Type::Infer),
                ExprKind::Field(base, method) => {
                    let base_ty = self.receiver_type(base, env);
                    if base_ty == Type::Infer {
                        return Type::Infer;
                    }
                    let inherent = format!("{}::{method}", base_ty.name());
                    let f = match self.world.functions.get(&inherent) {
                        Some(f) => Some(f.clone()),
                        None => self.traits.and_then(|traits| {
                            let generics = self.generics.clone();
                            let current = self.current.clone();
                            let call = TraitCall {
                                callee: method,
                                path: false,
                                receiver: base_ty.clone(),
                                generics: &generics,
                                function: &current,
                                span: e.span.clone(),
                            };
                            match traits.resolve(&call)? {
                                Ok(name) => self.world.functions.get(&name).cloned(),
                                Err(_) => None,
                            }
                        }),
                    };
                    let Some(f) = f else {
                        return Type::Infer;
                    };
                    let Some(first) = f.params.first() else {
                        return Type::Infer;
                    };
                    let mut vars = BTreeMap::new();
                    infer(&first.ty, &base_ty, &f.generics, &mut vars);
                    if f.generics.iter().any(|g| !vars.contains_key(&g.name)) {
                        return Type::Infer;
                    }
                    substitute(&f.result, &vars)
                }
                _ => Type::Infer,
            },
            ExprKind::Struct { name, .. } => self
                .world
                .adts
                .get(name)
                .filter(|a| a.generics.is_empty() && !a.is_enum)
                .map(|a| Type::named(&a.name))
                .unwrap_or(Type::Infer),
            _ => Type::Infer,
        }
    }
    fn trait_call(
        &mut self,
        callee: &str,
        path: bool,
        receiver: Option<&Expr>,
        env: &Env,
        span: &Span,
    ) -> Option<Option<Function>> {
        let traits = self.traits?;
        let receiver = match receiver {
            Some(e) => self.receiver_type(e, env),
            None => Type::Infer,
        };
        let generics = self.generics.clone();
        let current = self.current.clone();
        let call = TraitCall {
            callee,
            path,
            receiver,
            generics: &generics,
            function: &current,
            span: span.clone(),
        };
        match traits.resolve(&call)? {
            Ok(name) => match self.world.functions.get(&name) {
                Some(f) => {
                    self.resolved.insert((span.start, span.end), name);
                    Some(Some(f.clone()))
                }
                None => {
                    self.error(
                        "E0610",
                        format!("trait call resolved to missing function '{name}'"),
                        span.clone(),
                    );
                    Some(None)
                }
            },
            Err(d) => {
                self.diagnostics.push(d);
                Some(None)
            }
        }
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "The inputs mirror the call typing judgment"
    )]
    fn apply(
        &mut self,
        f: &Function,
        types: &[Type],
        args: &[Arg],
        receiver: Option<&Expr>,
        env: &mut Env,
        span: Span,
        expected: &Type,
    ) -> Type {
        let mut vars = BTreeMap::new();
        if *expected != Type::Infer {
            infer(&f.result, expected, &f.generics, &mut vars);
        }
        self.apply_args(f, types, args, receiver, env, span.clone(), &mut vars);
        let mut labels = BTreeMap::new();
        for (i, e) in f.effects.instances.iter().enumerate() {
            let mut e = e.clone();
            e.args = e.args.iter().map(|t| substitute(t, &vars)).collect();
            let bound = self.resolve_effect(&e);
            self.require_effect(&e, env, span.clone());
            let target = bound.and_then(|e| e.label);
            labels.insert(function_capability(&f.name, i), target.clone());
            if let Some(label) = e.label
                && label.starts_with("$latent:")
            {
                labels.insert(label, target);
            }
        }
        if let Some(row) = &f.effects.tail {
            if let Some(Type::Row(effects)) = vars.get(row).cloned() {
                let mut resolved = effects.clone();
                for e in &mut resolved.instances {
                    let old = e.label.clone();
                    let bound = self.resolve_effect(e);
                    self.require_effect(e, env, span.clone());
                    if let Some(bound) = bound {
                        e.label = bound.label;
                    }
                    if let Some(label) = old
                        && label.starts_with("$latent:")
                    {
                        labels.insert(label, e.label.clone());
                    }
                }
                if effects.tail.is_some() {
                    self.observe(&Effects {
                        instances: vec![],
                        tail: effects.tail.clone(),
                    });
                    self.check_abort_frame(env, row, span.clone());
                    if effects.tail != self.allowed.tail {
                        self.error(
                            "E0406",
                            "unresolved effect row cannot escape this call",
                            span.clone(),
                        );
                    }
                }
                vars.insert(row.clone(), Type::Row(resolved));
            } else if self.allowed.tail.as_ref() == Some(row) {
                self.observe(&Effects {
                    instances: vec![],
                    tail: Some(row.clone()),
                });
                self.check_abort_frame(env, row, span.clone());
            } else {
                self.error(
                    "E0406",
                    format!("cannot infer effect row '{row}'"),
                    span.clone(),
                );
            }
        }
        let result = self
            .world
            .function_results
            .get(&f.name)
            .unwrap_or(&f.result);
        rename_capabilities(&substitute(result, &vars), &labels)
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "The inputs mirror the call typing judgment"
    )]
    fn apply_args(
        &mut self,
        f: &Function,
        types: &[Type],
        args: &[Arg],
        receiver: Option<&Expr>,
        env: &mut Env,
        span: Span,
        vars: &mut BTreeMap<String, Type>,
    ) -> Type {
        let loan_base = self.loans.len();
        let mut fixed_rows = BTreeMap::new();
        let mut explicit = types.iter().peekable();
        for (index, g) in f.generics.iter().enumerate() {
            let Some(t) = explicit.next() else {
                break;
            };
            let t = if g.row {
                let mut row = match t {
                    Type::Row(e) => e.clone(),
                    Type::Named(n, args) if self.world.effects.contains_key(n) => Effects {
                        instances: vec![Effect {
                            label: None,
                            name: n.clone(),
                            args: args.clone(),
                        }],
                        tail: None,
                    },
                    _ => {
                        self.error(
                            "E0205",
                            "effect-row argument requires effects or a row variable",
                            span.clone(),
                        );
                        Effects::default()
                    }
                };
                // Effect and data names are resolved using the formal kind.
                // A sequence of effects belongs to this row until the next
                // explicitly typed formal parameter or row argument.
                while explicit.len() > f.generics.len() - index - 1 {
                    let Some(next) = explicit.peek() else {
                        break;
                    };
                    if let Type::Named(n, args) = next
                        && self.world.effects.contains_key(n)
                    {
                        row.instances.push(Effect {
                            label: None,
                            name: n.clone(),
                            args: args.clone(),
                        });
                        explicit.next();
                        continue;
                    }
                    break;
                }
                for e in &mut row.instances {
                    if let Some(bound) = self.resolve_effect(e) {
                        *e = bound;
                    }
                }
                fixed_rows.insert(g.name.clone(), row.clone());
                Type::Row(row)
            } else {
                t.clone()
            };
            self.valid_type(&t, &self.generics.clone(), span.clone());
            vars.insert(g.name.clone(), t);
        }
        if explicit.next().is_some() {
            self.error("E0205", "too many type arguments", span.clone());
        }
        let mut actuals = vec![];
        if let Some(e) = receiver {
            actuals.push((e, None));
        }
        actuals.extend(
            args.iter()
                .map(|a| (&a.expr, if a.explicit { Some(a.pass) } else { None })),
        );
        if actuals.len() != f.params.len() {
            self.error(
                "E0207",
                format!(
                    "'{}' expects {} arguments, found {}",
                    f.name,
                    f.params.len(),
                    actuals.len()
                ),
                span.clone(),
            );
        }
        let mut loans: Vec<(Place, Pass, Span)> = vec![];
        let mut constraints = vec![];
        for ((expr, explicit), param) in actuals.into_iter().zip(&f.params) {
            if let Some(pass) = explicit
                && pass != param.pass
            {
                self.error(
                    "E0308",
                    format!(
                        "argument mode {pass:?} does not match {:?} parameter",
                        param.pass
                    ),
                    expr.span.clone(),
                );
            }
            let place = Place::from_expr(expr);
            if param.pass == Pass::Inout && place.is_none() {
                self.error(
                    "E0300",
                    "inout argument requires a place",
                    expr.span.clone(),
                );
            }
            if let Some(p) = &place {
                let paths = if param.projections.is_empty() {
                    vec![p.clone()]
                } else {
                    param
                        .projections
                        .iter()
                        .map(|path| {
                            let mut p = p.clone();
                            p.path.extend(path.clone());
                            p
                        })
                        .collect()
                };
                for p in paths {
                    for (old, mode, origin) in &loans {
                        if p.overlaps(old) && !(param.pass == Pass::In && *mode == Pass::In) {
                            self.error("E0301",format!("overlapping arguments '{}' and '{}' violate alias XOR mutability",old.display(),p.display()),expr.span.clone());
                            self.related(
                                origin.clone(),
                                if origin == &expr.span {
                                    "overlapping projection of this argument introduced here"
                                } else {
                                    "earlier overlapping argument evaluated here"
                                },
                            );
                            self.help("Overlapping argument places are allowed only when both arguments use shared in passing.");
                        }
                    }
                    loans.push((p, param.pass, expr.span.clone()));
                }
            }
            let usage = if param.pass == Pass::Sink {
                Usage::Own
            } else {
                Usage::Borrow(param.pass)
            };
            let mut context_vars = vars.clone();
            for g in &f.generics {
                context_vars.entry(g.name.clone()).or_insert(Type::Infer);
            }
            let mut cap_labels = BTreeMap::new();
            for (i, e) in f.effects.instances.iter().enumerate() {
                let mut e = e.clone();
                e.args = e
                    .args
                    .iter()
                    .map(|t| substitute(t, &context_vars))
                    .collect();
                cap_labels.insert(
                    function_capability(&f.name, i),
                    self.resolve_effect(&e).and_then(|e| e.label),
                );
            }
            let target = rename_capabilities(
                &substitute(&bind_signature_capabilities(&param.ty, f), &context_vars),
                &cap_labels,
            );
            let mut actual = if !param.projections.is_empty() {
                if let Some(p) = &place {
                    for path in &param.projections {
                        let mut q = p.clone();
                        q.path.extend(path.clone());
                        self.place(&q, env, usage, expr.span.clone());
                    }
                    self.peek_type(expr, env)
                } else {
                    self.expr_expected(expr, env, usage, &target)
                }
            } else {
                if param.pass == Pass::Inout {
                    self.expr(expr, env, usage)
                } else {
                    self.expr_expected(expr, env, usage, &target)
                }
            };
            if param.pass != Pass::Sink && place.is_none() && self.mode(&actual) == Mode::Linear {
                self.error(
                    "E0302",
                    "bind a temporary linear value before borrowing it, then consume it explicitly",
                    expr.span.clone(),
                );
            }
            self.loans.truncate(loan_base);
            self.loans.extend(
                loans
                    .iter()
                    .filter(|(_, mode, _)| *mode != Pass::Sink)
                    .cloned(),
            );
            if param.pass == Pass::Inout {
                if let Some(place) = &place
                    && let Some(binding) = env.bindings.get_mut(&place.root)
                {
                    binding.integer = None;
                    binding.nonzero = false;
                }
                if bound_capabilities(&actual) != bound_capabilities(&target)
                    || !compatible(&actual, &target)
                {
                    self.error(
                        "E0403",
                        "inout arguments must preserve the place's capability scope and value type",
                        expr.span.clone(),
                    );
                }
            } else {
                actual = instantiate_latent_effects(&actual, &target);
            }
            infer(&param.ty, &actual, &f.generics, vars);
            for (row, fixed) in &fixed_rows {
                if let Some(Type::Row(inferred)) = vars.get(row) {
                    let closed = Effects {
                        instances: fixed.instances.clone(),
                        tail: None,
                    };
                    if !effects_include(
                        &closed,
                        &Effects {
                            instances: inferred.instances.clone(),
                            tail: None,
                        },
                    ) || inferred.tail.is_some() && inferred.tail != fixed.tail
                    {
                        self.error(
                            "E0406",
                            "argument effects exceed the explicitly supplied effect row",
                            expr.span.clone(),
                        );
                    }
                }
                vars.insert(row.clone(), Type::Row(fixed.clone()));
            }
            constraints.push((expr, param, actual.clone()));
            self.expect_type(
                &rename_capabilities(
                    &substitute(&bind_signature_capabilities(&param.ty, f), vars),
                    &cap_labels,
                ),
                &actual,
                expr.span.clone(),
            );
            if let (
                Type::Function {
                    effects: expected, ..
                },
                Type::Function {
                    effects: actual, ..
                },
            ) = (&target, &actual)
                && expected.tail.is_none()
            {
                for e in &actual.instances {
                    if !expected
                        .instances
                        .iter()
                        .any(|x| x.name == e.name && (x.label.is_none() || x.label == e.label))
                    {
                        self.error(
                            "E0401",
                            "function argument has unpermitted effects",
                            expr.span.clone(),
                        );
                    }
                }
            }
        }
        self.loans.truncate(loan_base);
        // A unique argument may contain a suspended continuation. Resuming it
        // can change the caller places aliased by this frame's borrowed inputs.
        if constraints
            .iter()
            .any(|(_, _, ty)| self.mode(ty) != Mode::Copy)
        {
            env.invalidate_borrowed_facts();
        }
        for ty in vars.values_mut() {
            *ty = crate::numeric::default_type(ty);
        }
        for (expr, param, actual) in constraints {
            let target = substitute(&param.ty, vars);
            let typed_target = bind_signature_capabilities(&target, f);
            let mut allowed_caps = bound_capabilities(&typed_target);
            // Region arguments of parameter types (`RegionBox[r, T]`) name the
            // callee's capabilities; they are instantiated like its row.
            let mut region_labels = BTreeMap::new();
            // Function-row binders are instantiated to the caller's evidence.
            for (index, effect) in f.effects.instances.iter().enumerate() {
                allowed_caps.remove(&function_capability(&f.name, index));
                let resolved = self.resolve_effect(effect).and_then(|e| e.label);
                if let Some(label) = &resolved {
                    allowed_caps.insert(label.clone());
                }
                region_labels.insert(function_capability(&f.name, index), resolved);
            }
            let target = rename_capabilities(&target, &region_labels);
            if !bound_capabilities(&actual).is_subset(&allowed_caps) {
                self.error("E0403","this parameter would erase a captured lexical capability; preserve it with an effect row or generic value type",expr.span.clone());
            }
            self.expect_type(&target, &actual, expr.span.clone());
            self.constrain_literals(expr, &target);
        }
        self.calls.insert((span.start, span.end), vars.clone());
        self.generic_bounds(&f.generics, vars, span);
        substitute(&f.result, vars)
    }
    fn constrain_literals(&mut self, e: &Expr, target: &Type) {
        let key = (e.span.start, e.span.end);
        if let Some(actual) = self.types.get(&key).cloned() {
            if matches!(actual, Type::IntLiteral(_))
                && crate::numeric::IntegerKind::from_type(target).is_some()
            {
                self.types.insert(key, target.clone());
            } else if matches!(actual, Type::Named(..) | Type::Tuple(..))
                && compatible(target, &actual)
            {
                self.types.insert(key, refine_annotation(target, &actual));
            }
        }
        match &e.kind {
            ExprKind::Tuple(es) => {
                if let Type::Tuple(ts) = target {
                    for (e, t) in es.iter().zip(ts) {
                        self.constrain_literals(e, t);
                    }
                }
            }
            ExprKind::Struct { name, fields } => {
                if let Some((a, v)) = self.world.constructor(name) {
                    let vars = if let Type::Named(_, ts) = target {
                        a.generics
                            .iter()
                            .zip(ts)
                            .map(|(g, t)| (g.name.clone(), t.clone()))
                            .collect()
                    } else {
                        BTreeMap::new()
                    };
                    for (n, e) in fields {
                        if let Some(f) = v.fields.iter().find(|f| &f.name == n) {
                            self.constrain_literals(e, &substitute(&f.ty, &vars));
                        }
                    }
                }
            }
            ExprKind::Call { callee, args, .. } => {
                if let ExprKind::Name(n) = &callee.kind
                    && let Some((a, v)) = self.world.constructor(n)
                {
                    let vars = if let Type::Named(_, ts) = target {
                        a.generics
                            .iter()
                            .zip(ts)
                            .map(|(g, t)| (g.name.clone(), t.clone()))
                            .collect()
                    } else {
                        BTreeMap::new()
                    };
                    for (a, f) in args.iter().zip(&v.fields) {
                        self.constrain_literals(&a.expr, &substitute(&f.ty, &vars));
                    }
                }
            }
            ExprKind::Block { tail: Some(t), .. } => self.constrain_literals(t, target),
            ExprKind::If {
                then, otherwise, ..
            } => {
                self.constrain_literals(then, target);
                if let Some(e) = otherwise {
                    self.constrain_literals(e, target);
                }
            }
            ExprKind::Match { arms, .. } => {
                for (_, e) in arms {
                    self.constrain_literals(e, target);
                }
            }
            _ => {}
        }
    }
    fn peek_type(&self, e: &Expr, env: &Env) -> Type {
        if let Some(p) = Place::from_expr(e) {
            env.bindings
                .get(&p.root)
                .and_then(|b| self.world.field(&b.ty, &p.path))
                .unwrap_or(Type::Infer)
        } else {
            self.types
                .get(&(e.span.start, e.span.end))
                .cloned()
                .unwrap_or(Type::Infer)
        }
    }
    fn active_effect(&self, effect: &Effect) -> Option<Effect> {
        let request = required_label(&effect.label);
        let label = request
            .as_ref()
            .map(|l| self.cap_aliases.get(l).unwrap_or(l));
        let mut candidates =
            self.caps.iter().rev().filter(|e| {
                e.name == effect.name && (label.is_none() || e.label.as_ref() == label)
            });
        // Typed instance selection: an unlabeled request with concrete
        // arguments prefers the innermost capability with exactly those
        // arguments. Every backend mirrors this with the same instance key.
        if label.is_none()
            && let Some(key) = crate::numeric::instance_key(&effect.name, &effect.args)
            && let Some(cap) = candidates
                .clone()
                .find(|c| crate::numeric::instance_key(&c.name, &c.args).as_ref() == Some(&key))
        {
            return Some(cap.clone());
        }
        candidates.next().cloned()
    }
    fn resolve_effect(&self, effect: &Effect) -> Option<Effect> {
        if let Some(cap) = self.active_effect(effect) {
            return Some(cap);
        }
        let label = required_label(&effect.label);
        if label.as_ref().is_some_and(|l| l.starts_with('$')) {
            return None;
        }
        if let Some((infer, _, _, prefix)) = self.effect_inference.last() {
            let allowed = self.allowed.instances.iter().any(|e| {
                e.name == effect.name
                    && required_label(&e.label) == label
                    && e.args.len() == effect.args.len()
                    && e.args
                        .iter()
                        .zip(&effect.args)
                        .all(|(a, b)| compatible(a, b))
            });
            if *infer || allowed {
                let mut e = effect.clone();
                e.label = label;
                e.label = Some(latent_capability(prefix, &e));
                return Some(e);
            }
        }
        None
    }
    fn observe(&mut self, row: &Effects) {
        let depth = self.effect_inference.len();
        if let Some((boundary, observed)) = self.observations.last_mut()
            && *boundary == depth
        {
            union_effects(observed, row);
        }
    }
    fn start_observing(&mut self) {
        self.observations
            .push((self.effect_inference.len(), Effects::default()));
    }
    fn finish_observing(&mut self) -> Effects {
        self.observations.pop().unwrap().1
    }
    fn require_effect(&mut self, effect: &Effect, env: &Env, span: Span) {
        // Built-in capability effects constrain callees, not callers: they are
        // never requested from the caller's row (see `capability_effects`).
        if self.dead != 0 || crate::capability_effects::is_capability_effect(&effect.name) {
            return;
        }
        let resolved = self.resolve_effect(effect);
        self.observe(&Effects {
            instances: vec![resolved.clone().unwrap_or_else(|| effect.clone())],
            tail: None,
        });
        let cap_index = self.caps.iter().rposition(|c| Some(c) == resolved.as_ref());
        let infer = self
            .effect_inference
            .last()
            .is_some_and(|(infer, _, _, _)| *infer);
        if let Some((_, boundary, observed, _)) = self.effect_inference.last_mut()
            && cap_index.is_none_or(|i| i < *boundary)
        {
            let effect = resolved.clone().unwrap_or_else(|| effect.clone());
            if !observed.instances.contains(&effect) {
                observed.instances.push(effect);
            }
        }
        if let Some(cap) = &resolved
            && (cap.args.len() != effect.args.len()
                || cap.args.iter().zip(&effect.args).any(|(a, b)| {
                    !compatible(a, b) || bound_capabilities(a) != bound_capabilities(b)
                }))
        {
            self.error(
                "E0400",
                format!(
                    "effect '{}' type arguments do not match its lexical capability",
                    effect.name
                ),
                span.clone(),
            );
        }
        if !infer
            && resolved.is_none()
            && !self.allowed.instances.iter().any(|e| {
                e.name == effect.name
                    && e.label == effect.label
                    && e.args.len() == effect.args.len()
                    && e.args
                        .iter()
                        .zip(&effect.args)
                        .all(|(a, b)| compatible(a, b))
            })
        {
            self.error("E0401",format!("unhandled effect '{}'{}; add it to the effect row or install a lexical handler",effect.name,effect.label.as_ref().map(|l|format!(" at '{l}'")).unwrap_or_default()),span.clone());
        }
        if self
            .world
            .effects
            .get(&effect.name)
            .is_some_and(|decl| !decl.linear)
        {
            self.check_abort_frame_for(env, &effect.name, span, cap_index);
        }
    }
    fn check_abort_frame(&mut self, env: &Env, effect: &str, span: Span) {
        self.check_abort_frame_for(env, effect, span, None);
    }
    fn check_abort_frame_for(
        &mut self,
        env: &Env,
        effect: &str,
        span: Span,
        target: Option<usize>,
    ) {
        for (index, depth, state, origin) in self.state_guards.clone() {
            if depth == self.effect_inference.len() && target.is_none_or(|target| index > target) {
                self.error(
                    "E0402",
                    format!("abortable effect '{effect}' may discard linear handler state {state}"),
                    span.clone(),
                );
                self.related(origin, "linear handler state initializer");
                self.help("An abortable checkpoint may discard the suspended continuation and its owned linear state.");
            }
        }
        if self.loans.iter().any(|(_, mode, _)| *mode == Pass::Inout) {
            self.error(
                "E0402",
                "abortable effect crosses an active inout argument loan",
                span.clone(),
            );
            let origins = self
                .loans
                .iter()
                .filter(|(_, mode, _)| *mode == Pass::Inout)
                .take(8)
                .map(|(_, _, origin)| origin.clone())
                .collect::<Vec<_>>();
            for origin in origins {
                self.related(origin, "inout argument access requested here");
            }
            self.help("An abortable checkpoint cannot cross an active exclusive argument loan.");
        }
        for (n, b) in &env.bindings {
            if b.pass == Pass::Sink
                && self.mode(&b.ty) == Mode::Linear
                && !b.moved.iter().any(Vec::is_empty)
            {
                self.error("E0402",format!("potentially abortable effect '{effect}' may discard live linear value '{n}'"),span.clone());
                self.related(b.span.clone(), "live linear binding introduced here");
                self.help("An abortable checkpoint may discard a continuation that still owns this linear binding.");
            }
            if b.pass == Pass::Inout {
                self.error("E0402",format!("potentially abortable effect '{effect}' crosses active inout parameter '{n}'"),span.clone());
                self.related(b.span.clone(), "active inout parameter declared here");
                self.help(
                    "An abortable checkpoint cannot cross an active exclusive parameter borrow.",
                );
            }
        }
    }
    fn merge(&mut self, env: &mut Env, a: &Env, b: &Env, span: Span) {
        for (n, binding) in &mut env.bindings {
            if let (Some(x), Some(y)) = (a.bindings.get(n), b.bindings.get(n)) {
                if self.mode(&binding.ty) == Mode::Linear && x.moved != y.moved {
                    self.error(
                        "E0309",
                        format!("linear value '{n}' is consumed on only some control-flow paths"),
                        span.clone(),
                    );
                    self.related(binding.span.clone(), "linear binding introduced here");
                    for branch in [x, y] {
                        if branch.span == binding.span {
                            for cause in &branch.move_causes {
                                // Incoming history is shared by both branches;
                                // it is not a branch-specific transfer witness.
                                if !binding.move_causes.iter().any(|before| {
                                    before.path == cause.path && before.span == cause.span
                                }) {
                                    self.related(
                                        cause.span.clone(),
                                        "ownership-taking use on one control-flow path",
                                    );
                                }
                            }
                        }
                    }
                    self.help(
                        "Returning control-flow paths must agree on linear ownership state; borrowed parameters do not transfer ownership.",
                    );
                }
                binding.moved = x.moved.iter().chain(&y.moved).cloned().collect();
                binding.moved.sort();
                binding.moved.dedup();
                binding.move_causes.clear();
                for branch in [x, y] {
                    if branch.span == binding.span {
                        for cause in &branch.move_causes {
                            binding.remember_move(&cause.path, cause.span.clone());
                        }
                    }
                }
                binding.integer = x.integer.filter(|n| Some(*n) == y.integer);
                binding.nonzero = x.nonzero && y.nonzero;
            }
        }
    }
    fn primitive_failure(&mut self, env: &Env, span: Span) {
        if self.dead != 0 {
            return;
        }
        self.partial_ops.insert((span.start, span.end));
        self.require_effect(
            &Effect {
                label: None,
                name: "Fail".into(),
                args: vec![Type::named("RuntimeError")],
            },
            env,
            span,
        );
    }
    /// Record a checked operation that may raise Fail[ArithOverflow].
    fn overflow_failure(&mut self, env: &Env, span: Span) {
        if self.dead != 0 {
            return;
        }
        self.overflow_ops.insert((span.start, span.end));
        self.require_effect(
            &Effect {
                label: None,
                name: "Fail".into(),
                args: vec![Type::named(crate::numeric::OVERFLOW_TYPE)],
            },
            env,
            span,
        );
    }
    /// Constant facts which prove a checked operation cannot overflow. A
    /// known divisor other than -1 (or dividend other than MIN) also proves
    /// signed division in range; zero divisors are RuntimeError's concern.
    fn in_range(
        &self,
        op: &str,
        kind: crate::numeric::IntegerKind,
        a: Option<i128>,
        b: Option<i128>,
    ) -> bool {
        match (a, b) {
            (Some(a), Some(b)) => {
                kind.checked(op, a, b) != Err(crate::numeric::ArithFailure::Overflow)
            }
            _ if op == "/" => b.is_some_and(|b| b != -1) || a.is_some_and(|a| a != kind.min()),
            _ => false,
        }
    }
    /// Typecheck `wrapping_add`, `wrapping_sub` or `wrapping_mul`: two
    /// by-value operands of one integer kind, with contextual literal typing.
    fn wrapping_call(
        &mut self,
        name: &str,
        types: &[Type],
        args: &[Arg],
        env: &mut Env,
        span: Span,
        expected: &Type,
    ) -> Type {
        if !types.is_empty() {
            self.error(
                "E0207",
                format!("'{name}' does not take type arguments"),
                span.clone(),
            );
        }
        if args.len() != 2 {
            self.error(
                "E0207",
                format!("'{name}' expects 2 arguments, found {}", args.len()),
                span.clone(),
            );
        }
        for a in args {
            if a.pass != Pass::In || a.explicit {
                self.error(
                    "E0308",
                    format!("'{name}' takes its operands by value"),
                    a.expr.span.clone(),
                );
            }
        }
        let goal = if crate::numeric::IntegerKind::from_type(expected).is_some()
            && !matches!(expected, Type::IntLiteral(_))
        {
            expected.clone()
        } else {
            Type::Infer
        };
        let mut common = goal.clone();
        let mut actual = vec![];
        for a in args {
            let goal = if matches!(common, Type::IntLiteral(_)) {
                Type::Infer
            } else {
                common.clone()
            };
            let t = self.expr_expected(&a.expr, env, Usage::Own, &goal);
            if matches!(common, Type::Infer | Type::IntLiteral(_)) {
                common = match (&common, &t) {
                    (Type::IntLiteral(x), Type::IntLiteral(y)) => Type::IntLiteral((*x).max(*y)),
                    _ => t.clone(),
                };
            }
            actual.push(t);
        }
        let common = crate::numeric::default_type(&common);
        if crate::numeric::IntegerKind::from_type(&common).is_none() {
            self.error(
                "E0200",
                format!("'{name}' requires integer operands of one kind, found {common}"),
                span.clone(),
            );
            return Type::Infer;
        }
        for (a, t) in args.iter().zip(&actual) {
            self.expect_type(&common, t, a.expr.span.clone());
            if matches!(t, Type::IntLiteral(_)) {
                self.types
                    .insert((a.expr.span.start, a.expr.span.end), common.clone());
            }
        }
        common
    }
    fn nonzero(&self, e: &Expr, env: &Env) -> bool {
        self.integer_fact(e, env).is_some_and(|n| n != 0)
            || matches!(&e.kind, ExprKind::Name(n) if env.bindings.get(n).is_some_and(|b| b.nonzero))
    }
    /// The exact value of a pure integer expression, under checked semantics:
    /// an operation that would overflow or fail has no fact.
    fn integer_fact(&self, e: &Expr, env: &Env) -> Option<i128> {
        let kind = |e: &Expr| {
            self.types
                .get(&(e.span.start, e.span.end))
                .and_then(crate::numeric::IntegerKind::from_type)
        };
        let n = match &e.kind {
            ExprKind::Literal(Literal::Int(n)) => *n,
            ExprKind::Name(n) => env.bindings.get(n)?.integer?,
            ExprKind::Unary(op, v) if op == "-" => {
                let k = kind(v).or_else(|| kind(e))?;
                k.checked_neg(self.integer_fact(v, env)?)?
            }
            ExprKind::Binary(op, a, b) => {
                let k = kind(a).or_else(|| kind(e))?;
                let (a, b) = (self.integer_fact(a, env)?, self.integer_fact(b, env)?);
                k.checked(op, a, b).ok()?
            }
            ExprKind::Call { callee, args, .. } => {
                let ExprKind::Name(name) = &callee.kind else {
                    return None;
                };
                let op = crate::numeric::wrapping_intrinsic(name)?;
                if env.bindings.contains_key(name) || args.len() != 2 {
                    return None;
                }
                let (a, b) = (
                    self.integer_fact(&args[0].expr, env)?,
                    self.integer_fact(&args[1].expr, env)?,
                );
                kind(e)?.wrapping(op, a, b)
            }
            _ => return None,
        };
        match kind(e) {
            Some(k) if !k.fits(n) => None,
            _ => Some(n),
        }
    }
    fn refine_condition(e: &Expr, env: &mut Env, truth: bool) {
        if let ExprKind::Binary(op, a, b) = &e.kind {
            let n = match (&a.kind, &b.kind) {
                (ExprKind::Name(n), ExprKind::Literal(Literal::Int(0)))
                | (ExprKind::Literal(Literal::Int(0)), ExprKind::Name(n)) => n,
                _ => return,
            };
            if let Some(binding) = env.bindings.get_mut(n) {
                match (op.as_str(), truth) {
                    ("!=", true) | ("==", false) => binding.nonzero = true,
                    ("==", true) | ("!=", false) => {
                        binding.integer = Some(0);
                        binding.nonzero = false;
                    }
                    _ => (),
                }
            }
        }
    }
    fn bind(&mut self, p: &Pattern, ty: &Type, env: &mut Env) {
        self.bind_mode(p, ty, env, Pass::Sink);
    }
    fn bind_mode(&mut self, p: &Pattern, ty: &Type, env: &mut Env, pass: Pass) {
        match &p.kind {
            PatternKind::Wild => {
                if pass == Pass::Sink && self.mode(ty) == Mode::Linear {
                    self.error(
                        "E0302",
                        "wildcard cannot discard a linear value",
                        p.span.clone(),
                    );
                }
            }
            PatternKind::Bind(n) => {
                if let Some(old) = env.bindings.get(n)
                    && old.pass == Pass::Sink
                    && self.mode(&old.ty) == Mode::Linear
                    && old.moved.is_empty()
                {
                    self.error(
                        "E0302",
                        format!("shadowing would discard linear value '{n}'"),
                        p.span.clone(),
                    );
                    self.related(old.span.clone(), "unconsumed linear binding being shadowed");
                    self.help(
                        "A new binding does not consume the linear value with the same name.",
                    );
                }
                env.bindings.insert(
                    n.clone(),
                    Binding {
                        ty: ty.clone(),
                        pass,
                        projections: vec![],
                        moved: vec![],
                        move_causes: vec![],
                        span: p.span.clone(),
                        integer: None,
                        nonzero: false,
                    },
                );
            }
            PatternKind::Literal(l) => {
                if let Literal::String(s) = l {
                    self.string_capacity = self.string_capacity.max(s.len());
                }
                self.expect_type(ty, &literal_type(l), p.span.clone());
            }
            PatternKind::Tuple(ps) => {
                if let Type::Tuple(ts) = ty {
                    if ps.len() != ts.len() {
                        self.error("E0211", "tuple pattern arity mismatch", p.span.clone());
                    }
                    for (p, t) in ps.iter().zip(ts) {
                        self.bind_mode(p, t, env, pass);
                    }
                } else {
                    self.error("E0211", "tuple pattern requires tuple type", p.span.clone());
                }
            }
            PatternKind::Constructor { name, fields, .. } => {
                let Some((a, v)) = self.world.constructor(name) else {
                    self.error(
                        "E0211",
                        format!("unknown pattern constructor '{name}'"),
                        p.span.clone(),
                    );
                    return;
                };
                let args = if let Type::Named(n, args) = ty {
                    if n != &a.name {
                        self.error(
                            "E0211",
                            format!("pattern '{name}' does not match {ty}"),
                            p.span.clone(),
                        );
                    }
                    args.clone()
                } else {
                    self.error(
                        "E0211",
                        "constructor pattern requires an algebraic data type",
                        p.span.clone(),
                    );
                    vec![]
                };
                let vars = a
                    .generics
                    .iter()
                    .zip(args)
                    .map(|(g, t)| (g.name.clone(), t))
                    .collect();
                let mut seen = BTreeSet::new();
                for (n, p) in fields {
                    if !seen.insert(n) {
                        self.error("E0211", "duplicate pattern field", p.span.clone());
                    }
                    if let Some(f) = v.fields.iter().find(|f| &f.name == n) {
                        self.bind_mode(p, &substitute(&f.ty, &vars), env, pass);
                    } else {
                        self.error("E0211", "unknown pattern field", p.span.clone());
                    }
                }
                if seen.len() != v.fields.len() {
                    self.error("E0211", "pattern must name every field", p.span.clone());
                }
            }
        }
    }
    fn irrefutable(&self, p: &Pattern, t: &Type) -> bool {
        matrix_covered(self.world, &[vec![p.clone()]], std::slice::from_ref(t), 0)
    }
    fn coverage(&mut self, arms: &[(Pattern, Expr)], t: &Type, span: Span) {
        let rows = arms
            .iter()
            .map(|(p, _)| vec![p.clone()])
            .collect::<Vec<_>>();
        if !matrix_covered(self.world, &rows, std::slice::from_ref(t), 0) {
            self.error(
                "E0212",
                "non-exhaustive match; cover every variant and payload or add a wildcard",
                span,
            );
        }
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "The inputs mirror the closure typing judgment"
    )]
    fn closure(
        &mut self,
        params: &[Param],
        mode: Option<Mode>,
        result: &Type,
        effects: &Effects,
        infer_effects: bool,
        body: &Expr,
        env: &mut Env,
        span: Span,
    ) -> Type {
        self.valid_effects(effects, &self.generics.clone(), span.clone());
        let mut free = free_names(body);
        for p in params {
            free.remove(&p.name);
        }
        let mut captured = Env::default();
        let mut inferred = Mode::Copy;
        for name in free {
            if let Some(b) = env.bindings.get(&name).cloned() {
                if b.pass != Pass::Sink {
                    self.error(
                        "E0310",
                        format!("closure cannot capture borrowed parameter '{name}'"),
                        span.clone(),
                    );
                }
                inferred = inferred.join(self.mode(&b.ty));
                self.place(
                    &Place {
                        root: name.clone(),
                        path: vec![],
                    },
                    env,
                    Usage::Own,
                    span.clone(),
                );
                captured.bindings.insert(name, b);
            }
        }
        let mode = mode.unwrap_or(inferred);
        if inferred > mode {
            self.error(
                "E0310",
                "closure mode cannot weaken its captured values",
                span.clone(),
            );
        }
        self.valid_type(result, &self.generics.clone(), span.clone());
        for p in params {
            self.valid_type(&p.ty, &self.generics.clone(), p.span.clone());
            if unresolved(&p.ty) {
                self.error(
                    "E0102",
                    "closure parameters need a type annotation when no expected type is available",
                    p.span.clone(),
                );
            }
            captured.bindings.insert(
                p.name.clone(),
                Binding {
                    ty: p.ty.clone(),
                    pass: p.pass,
                    projections: p.projections.clone(),
                    moved: vec![],
                    move_causes: vec![],
                    span: p.span.clone(),
                    integer: None,
                    nonzero: false,
                },
            );
        }
        let prefix = format!("c{}", span.start);
        let mut resolved_effects = effects.clone();
        for e in &mut resolved_effects.instances {
            e.label = self
                .active_effect(e)
                .and_then(|e| e.label)
                .or_else(|| Some(latent_capability(&prefix, e)));
        }
        let allowed = std::mem::replace(&mut self.allowed, effects.clone());
        let cap_base = self.caps.len();
        self.caps.extend(resolved_effects.instances.iter().cloned());
        self.effect_inference
            .push((infer_effects, cap_base, Effects::default(), prefix));
        let actual = self.expr_expected(body, &mut captured, Usage::Own, result);
        let (_, _, observed, _) = self.effect_inference.pop().unwrap();
        self.caps.truncate(cap_base);
        if infer_effects {
            resolved_effects = observed;
        } else if !effects_include(&resolved_effects, &observed) {
            self.error(
                "E0401",
                "closure body uses a capability absent from its declared effect row",
                body.span.clone(),
            );
        }
        self.allowed = allowed;
        self.expect_type(result, &actual, body.span.clone());
        self.exhaust(&captured, captured.bindings.keys().cloned().collect(), span);
        Type::Function {
            params: params.iter().map(|p| (p.pass, p.ty.clone())).collect(),
            result: Box::new(if *result == Type::Infer {
                crate::numeric::default_type(&actual)
            } else {
                refine_annotation(result, &actual)
            }),
            effects: resolved_effects,
            mode,
        }
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "The inputs mirror the stateful handler typing judgment"
    )]
    fn handle(
        &mut self,
        effect: &Effect,
        state: &Option<(Pattern, Box<Expr>)>,
        body: &Expr,
        ret: &Clause,
        ops: &[Clause],
        env: &mut Env,
        span: Span,
        expected: &Type,
    ) -> Type {
        self.valid_effects(
            &Effects {
                instances: vec![effect.clone()],
                tail: None,
            },
            &self.generics.clone(),
            span.clone(),
        );
        if crate::capability_effects::is_capability_effect(&effect.name) {
            self.diagnostics
                .push(crate::capability_effects::handler_rejected(effect, span));
            return Type::Infer;
        }
        let Ok(region) = self.region_block(effect, ret, span.clone()) else {
            return Type::Infer;
        };
        let Some(decl) = self.world.effects.get(&effect.name) else {
            self.error("E0400", "unknown handled effect", span);
            return Type::Infer;
        };
        if effect.args.len() != decl.generics.len() {
            self.error(
                "E0400",
                format!(
                    "handler for '{}' requires {} type arguments",
                    effect.name,
                    decl.generics.len()
                ),
                span.clone(),
            );
        }
        let state_type = state
            .as_ref()
            .map(|(_, e)| self.expr(e, env, Usage::Own))
            .unwrap_or_else(Type::unit);
        let outer = env.clone();
        let mut cap = effect.clone();
        cap.label = Some(format!("$cap{}", span.start));
        let aliases = self.cap_aliases.clone();
        if let Some(label) = &effect.label {
            self.cap_aliases
                .insert(label.clone(), cap.label.clone().unwrap());
        }
        self.caps.push(cap.clone());
        let guarded = self.mode(&state_type) == Mode::Linear;
        if guarded {
            self.state_guards.push((
                self.caps.len() - 1,
                self.effect_inference.len(),
                state_type.clone(),
                state
                    .as_ref()
                    .map_or_else(|| span.clone(), |(_, e)| e.span.clone()),
            ));
        }
        self.start_observing();
        let body_type = self.expr(body, env, Usage::Own);
        let mut outgoing = self.finish_observing();
        outgoing.instances.retain(|e| e.label != cap.label);
        if guarded {
            self.state_guards.pop();
        }
        self.caps.pop();
        self.cap_aliases = aliases;
        if contains_capability(&body_type, &cap) {
            if region.is_some() {
                self.diagnostics.push(
                    Diagnostic::new(
                        "E0791",
                        format!(
                            "a reference into region '{}' escapes its block as {}",
                            effect.label.as_deref().unwrap_or("r"),
                            crate::regions::display(&body_type)
                        ),
                        span.clone(),
                    )
                    .with_help("[T-RegionScope] requires r not to occur in the block's result type; copy the payload out with get or take"),
                );
            } else {
                self.error(
                    "E0403",
                    "lexical effect capability escapes its handler",
                    span.clone(),
                );
            }
        }
        let mut clause_env = Env::default();
        let mut free = clause_free_names(ret);
        for op in ops {
            free.extend(clause_free_names(op));
        }
        for (name, b) in &outer.bindings {
            if free.contains(name) {
                if self.mode(&b.ty) != Mode::Copy || b.pass != Pass::Sink {
                    self.error("E0404",format!("handler clause capture '{name}' must be owned copy; thread unique resources through state"),span.clone());
                }
                clause_env.bindings.insert(name.clone(), b.clone());
            }
        }
        let mut ret_env = clause_env.clone();
        let return_ty = if state.is_some() {
            Type::Tuple(vec![body_type, state_type.clone()])
        } else {
            body_type
        };
        if !self.irrefutable(&ret.pats[0], &return_ty) {
            self.error(
                "E0210",
                "handler return pattern must be irrefutable",
                ret.pats[0].span.clone(),
            );
        }
        self.bind(&ret.pats[0], &return_ty, &mut ret_env);
        self.start_observing();
        let returned = self.expr_expected(&ret.body, &mut ret_env, Usage::Own, expected);
        union_effects(&mut outgoing, &self.finish_observing());
        let mut resume_effects = outgoing.clone();
        if let Some(previous) = self.world.handler_effects.get(&(span.start, span.end)) {
            union_effects(&mut resume_effects, previous);
        }
        let out = if returned.name() == "Never" && *expected != Type::Infer {
            expected.clone()
        } else {
            returned
        };
        self.exhaust(
            &ret_env,
            ret_env.bindings.keys().cloned().collect(),
            ret.body.span.clone(),
        );
        let vars = decl
            .generics
            .iter()
            .zip(&effect.args)
            .map(|(g, t)| (g.name.clone(), t.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut seen = BTreeSet::new();
        for op in ops {
            if !seen.insert(&op.name) {
                self.error("E0405", "duplicate handler clause", op.body.span.clone());
            }
            let Some(sig) = decl.ops.iter().find(|f| f.name == op.name) else {
                self.error(
                    "E0405",
                    format!("unknown operation '{}'", op.name),
                    op.body.span.clone(),
                );
                continue;
            };
            let mut local = clause_env.clone();
            let mut ts = sig
                .params
                .iter()
                .map(|p| substitute(&p.ty, &vars))
                .collect::<Vec<_>>();
            if state.is_some() {
                ts.push(state_type.clone());
            }
            let mut resume_params = vec![(Pass::Sink, substitute(&sig.result, &vars))];
            if state.is_some() {
                resume_params.push((Pass::Sink, state_type.clone()));
            }
            ts.push(Type::Function {
                params: resume_params,
                result: Box::new(out.clone()),
                effects: resume_effects.clone(),
                mode: if decl.linear {
                    Mode::Linear
                } else {
                    Mode::Affine
                },
            });
            if op.pats.len() != ts.len() {
                self.error("E0405",format!("operation '{}' clause expects {} patterns (arguments, optional state, continuation)",op.name,ts.len()),op.body.span.clone());
            }
            for (i, (p, t)) in op.pats.iter().zip(ts).enumerate() {
                let pass = sig.params.get(i).map_or(Pass::Sink, |p| p.pass);
                if !self.irrefutable(p, &t) {
                    self.error(
                        "E0210",
                        "handler operation patterns must be irrefutable",
                        p.span.clone(),
                    );
                }
                self.bind_mode(p, &t, &mut local, pass);
                if let Some(param) = sig.params.get(i) {
                    if let PatternKind::Bind(name) = &p.kind {
                        if let Some(b) = local.bindings.get_mut(name) {
                            b.projections = param.projections.clone();
                        }
                    } else if !param.projections.is_empty() {
                        self.error("E0300", "bind a projected operation argument before destructuring its accessible fields", p.span.clone());
                    }
                }
            }
            self.start_observing();
            let actual = self.expr_expected(&op.body, &mut local, Usage::Own, &out);
            union_effects(&mut outgoing, &self.finish_observing());
            self.expect_type(&out, &actual, op.body.span.clone());
            self.exhaust(
                &local,
                local.bindings.keys().cloned().collect(),
                op.body.span.clone(),
            );
        }
        for op in decl.ops.iter().filter(|_| region.is_none()) {
            if !seen.contains(&op.name) {
                self.error(
                    "E0405",
                    format!("missing handler clause '{}'", op.name),
                    span.clone(),
                );
            }
        }
        outgoing
            .instances
            .sort_by(|a, b| (&a.name, &a.label).cmp(&(&b.name, &b.label)));
        self.observe(&outgoing);
        self.handler_effects
            .insert((span.start, span.end), outgoing);
        out
    }
}
pub fn literal_type(l: &Literal) -> Type {
    match l {
        Literal::Int(n) => Type::IntLiteral(*n),
        Literal::Float(_) => Type::named("f64"),
        Literal::Bool(_) => Type::named("bool"),
        Literal::String(_) => Type::named("String"),
    }
}
pub fn function_type(f: &Function) -> Type {
    Type::Function {
        params: f.params.iter().map(|p| (p.pass, p.ty.clone())).collect(),
        result: Box::new(f.result.clone()),
        effects: f.effects.clone(),
        mode: Mode::Copy,
    }
}
pub fn integer(t: &Type) -> bool {
    matches!(t.name(), "u8" | "u32" | "u64" | "i64")
        || matches!(t, Type::IntLiteral(_) | Type::Infer)
}
pub fn numeric(t: &Type) -> bool {
    integer(t) || t.name() == "f64"
}
pub fn compatible(a: &Type, b: &Type) -> bool {
    if *a == Type::Infer || *b == Type::Infer || b.name() == "Never" {
        return true;
    }
    match (a, b) {
        (Type::IntLiteral(_), Type::IntLiteral(_)) => true,
        (Type::IntLiteral(n), t) | (t, Type::IntLiteral(n)) => {
            crate::numeric::IntegerKind::from_type(t).is_some_and(|k| k.fits(*n))
        }
        (Type::Named(a, aa), Type::Named(b, bb)) => {
            (a == b) && aa.len() == bb.len() && aa.iter().zip(bb).all(|(a, b)| compatible(a, b))
        }
        (Type::Tuple(a), Type::Tuple(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| compatible(a, b))
        }
        (Type::Named(n, _), Type::Tuple(t)) | (Type::Tuple(t), Type::Named(n, _)) => {
            n == "Unit" && t.is_empty()
        }
        // Capability arguments of region types (`RegionBox[r, T]`).
        (Type::Row(a), Type::Row(b)) => a == b,
        (
            Type::Function {
                params: a,
                result: ar,
                mode: am,
                effects: ae,
            },
            Type::Function {
                params: b,
                result: br,
                mode: bm,
                effects: be,
            },
        ) => {
            effects_include(ae, be)
                && bm <= am
                && a.len() == b.len()
                && a.iter()
                    .zip(b)
                    .all(|((ap, a), (bp, b))| ap == bp && compatible(b, a))
                && compatible(ar, br)
        }
        _ => false,
    }
}
fn infer(expected: &Type, actual: &Type, generics: &[Generic], vars: &mut BTreeMap<String, Type>) {
    match (expected, actual) {
        (Type::Named(n, a), _) if a.is_empty() && generics.iter().any(|g| &g.name == n) => {
            if !vars.contains_key(n) || vars[n] == Type::Infer {
                vars.insert(n.clone(), actual.clone());
            } else if let Type::IntLiteral(previous) = vars[n] {
                if let Type::IntLiteral(next) = actual {
                    let representative = if *next > i64::MAX as i128 || previous > i64::MAX as i128
                    {
                        previous.max(*next)
                    } else if previous < 0 || *next < 0 {
                        previous.min(*next)
                    } else {
                        previous.max(*next)
                    };
                    vars.insert(n.clone(), Type::IntLiteral(representative));
                } else if compatible(&vars[n], actual) {
                    vars.insert(n.clone(), actual.clone());
                }
            } else if compatible(&vars[n], actual) {
                vars.insert(n.clone(), refine_annotation(&vars[n], actual));
            }
        }
        (Type::Named(_, a), Type::Named(_, b)) | (Type::Tuple(a), Type::Tuple(b)) => {
            for (a, b) in a.iter().zip(b) {
                infer(a, b, generics, vars)
            }
        }
        (
            Type::Function {
                params: a,
                result: ar,
                effects: ae,
                ..
            },
            Type::Function {
                params: b,
                result: br,
                effects: be,
                ..
            },
        ) => {
            for ((_, a), (_, b)) in a.iter().zip(b) {
                infer(a, b, generics, vars);
            }
            infer(ar, br, generics, vars);
            if let Some(row) = &ae.tail {
                let remainder = Effects {
                    instances: be
                        .instances
                        .iter()
                        .filter(|e| {
                            !ae.instances
                                .iter()
                                .any(|x| x.name == e.name && x.label == e.label)
                        })
                        .cloned()
                        .collect(),
                    tail: be.tail.clone(),
                };
                if let Some(Type::Row(previous)) = vars.get_mut(row) {
                    for e in remainder.instances {
                        if !previous.instances.contains(&e) {
                            previous.instances.push(e);
                        }
                    }
                    if previous.tail.is_none() {
                        previous.tail = remainder.tail;
                    }
                } else {
                    vars.insert(row.clone(), Type::Row(remainder));
                }
            }
        }
        _ => {}
    }
}

pub fn free_names(e: &Expr) -> BTreeSet<String> {
    fn pat(p: &Pattern, out: &mut BTreeSet<String>) {
        match &p.kind {
            PatternKind::Bind(n) => {
                out.insert(n.clone());
            }
            PatternKind::Tuple(ps) => {
                for p in ps {
                    pat(p, out)
                }
            }
            PatternKind::Constructor { fields, .. } => {
                for (_, p) in fields {
                    pat(p, out)
                }
            }
            _ => {}
        }
    }
    fn walk(e: &Expr, out: &mut BTreeSet<String>) {
        match &e.kind {
            ExprKind::Name(n) => {
                out.insert(n.clone());
            }
            ExprKind::Tuple(es) => {
                for e in es {
                    walk(e, out)
                }
            }
            ExprKind::Struct { fields, .. } => {
                for (_, e) in fields {
                    walk(e, out)
                }
            }
            ExprKind::Block { statements, tail } => {
                let mut local = BTreeSet::new();
                for s in statements {
                    match s {
                        Statement::Let { pat: p, value, .. } => {
                            let mut used = BTreeSet::new();
                            walk(value, &mut used);
                            out.extend(used.difference(&local).cloned());
                            pat(p, &mut local)
                        }
                        Statement::Expr(e) => {
                            let mut used = BTreeSet::new();
                            walk(e, &mut used);
                            out.extend(used.difference(&local).cloned());
                        }
                    }
                }
                if let Some(e) = tail {
                    let mut used = BTreeSet::new();
                    walk(e, &mut used);
                    out.extend(used.difference(&local).cloned());
                }
            }
            ExprKind::Call { callee, args, .. } => {
                walk(callee, out);
                for a in args {
                    walk(&a.expr, out)
                }
            }
            ExprKind::Field(e, _) | ExprKind::Try(e) | ExprKind::Unary(_, e) => walk(e, out),
            ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Assign(_, a, b) => {
                walk(a, out);
                walk(b, out)
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                walk(condition, out);
                walk(then, out);
                if let Some(e) = otherwise {
                    walk(e, out)
                }
            }
            ExprKind::Match { value, arms } => {
                walk(value, out);
                for (p, e) in arms {
                    let mut used = BTreeSet::new();
                    walk(e, &mut used);
                    let mut bound = BTreeSet::new();
                    pat(p, &mut bound);
                    out.extend(used.difference(&bound).cloned());
                }
            }
            ExprKind::Closure { params, body, .. } => {
                let mut used = BTreeSet::new();
                walk(body, &mut used);
                for p in params {
                    used.remove(&p.name);
                }
                out.extend(used);
            }
            ExprKind::Handle {
                state,
                body,
                ret,
                ops,
                ..
            } => {
                if let Some((_, s)) = state {
                    walk(s, out);
                }
                walk(body, out);
                for c in std::iter::once(ret.as_ref()).chain(ops) {
                    let mut used = BTreeSet::new();
                    walk(&c.body, &mut used);
                    let mut bound = BTreeSet::new();
                    for p in &c.pats {
                        pat(p, &mut bound);
                    }
                    out.extend(used.difference(&bound).cloned());
                }
            }
            _ => {}
        }
    }
    let mut out = BTreeSet::new();
    walk(e, &mut out);
    out
}

fn pattern_names(p: &Pattern) -> BTreeSet<String> {
    match &p.kind {
        PatternKind::Bind(n) => BTreeSet::from([n.clone()]),
        PatternKind::Tuple(ps) => ps.iter().flat_map(pattern_names).collect(),
        PatternKind::Constructor { fields, .. } => {
            fields.iter().flat_map(|(_, p)| pattern_names(p)).collect()
        }
        _ => BTreeSet::new(),
    }
}
// Constructor-specialization coverage matrix, including nested enum payloads.
fn matrix_covered(world: &World, rows: &[Vec<Pattern>], types: &[Type], depth: usize) -> bool {
    if types.is_empty() {
        return !rows.is_empty();
    }
    if depth > 64 {
        return false;
    }
    let wildcard = |p: &Pattern| matches!(p.kind, PatternKind::Wild | PatternKind::Bind(_));
    if rows
        .iter()
        .any(|r| r.len() == types.len() && r.iter().all(wildcard))
    {
        return true;
    }
    type Constructors = Vec<(String, Vec<(String, Type)>)>;
    let constructors: Option<Constructors> = match &types[0] {
        Type::Named(n, _) if n == "bool" => {
            Some(vec![("true".into(), vec![]), ("false".into(), vec![])])
        }
        Type::Named(n, args) => world.adts.get(n).map(|a| {
            let vars = a
                .generics
                .iter()
                .zip(args)
                .map(|(g, t)| (g.name.clone(), t.clone()))
                .collect();
            a.variants
                .iter()
                .map(|v| {
                    (
                        v.name.clone(),
                        v.fields
                            .iter()
                            .map(|f| (f.name.clone(), substitute(&f.ty, &vars)))
                            .collect(),
                    )
                })
                .collect()
        }),
        Type::Tuple(ts) => Some(vec![(
            "tuple".into(),
            ts.iter()
                .enumerate()
                .map(|(i, t)| (i.to_string(), t.clone()))
                .collect(),
        )]),
        _ => None,
    };
    let Some(constructors) = constructors else {
        let rest = rows
            .iter()
            .filter(|r| r.first().is_some_and(wildcard))
            .map(|r| r[1..].to_vec())
            .collect::<Vec<_>>();
        return matrix_covered(world, &rest, &types[1..], depth + 1);
    };
    constructors.into_iter().all(|(tag, fields)| {
        let mut specialized = vec![];
        for row in rows {
            let Some(p) = row.first() else {
                continue;
            };
            let mut ps = if wildcard(p) {
                fields
                    .iter()
                    .map(|_| Pattern {
                        kind: PatternKind::Wild,
                        span: p.span.clone(),
                    })
                    .collect::<Vec<_>>()
            } else {
                match &p.kind {
                    PatternKind::Constructor {
                        name, fields: ps, ..
                    } if name.rsplit("::").next() == Some(tag.as_str()) => {
                        let mut result = vec![];
                        for (n, _) in &fields {
                            if let Some((_, p)) = ps.iter().find(|(f, _)| f == n) {
                                result.push(p.clone());
                            } else {
                                return false;
                            }
                        }
                        result
                    }
                    PatternKind::Tuple(ps) if tag == "tuple" && ps.len() == fields.len() => {
                        ps.clone()
                    }
                    PatternKind::Literal(Literal::Bool(b)) if tag == b.to_string() => vec![],
                    _ => continue,
                }
            };
            ps.extend_from_slice(&row[1..]);
            specialized.push(ps);
        }
        let mut ts = fields.into_iter().map(|(_, t)| t).collect::<Vec<_>>();
        ts.extend_from_slice(&types[1..]);
        matrix_covered(world, &specialized, &ts, depth + 1)
    })
}

fn unresolved(t: &Type) -> bool {
    match t {
        Type::Infer => true,
        Type::Named(_, ts) | Type::Tuple(ts) => ts.iter().any(unresolved),
        Type::Function { params, result, .. } => {
            params.iter().any(|(_, t)| unresolved(t)) || unresolved(result)
        }
        _ => false,
    }
}
fn union_effects(into: &mut Effects, from: &Effects) {
    for e in &from.instances {
        if !into.instances.contains(e) {
            into.instances.push(e.clone());
        }
    }
    if into.tail.is_none() {
        into.tail = from.tail.clone();
    }
}
fn bound_capabilities(t: &Type) -> BTreeSet<String> {
    fn collect(t: &Type, caps: &mut BTreeSet<String>) {
        match t {
            Type::Named(_, ts) | Type::Tuple(ts) => {
                for t in ts {
                    collect(t, caps);
                }
            }
            Type::Row(effects) => {
                for e in &effects.instances {
                    if let Some(label) = &e.label
                        && (label.starts_with("$cap") || label.starts_with("$fncap:"))
                    {
                        caps.insert(label.clone());
                    }
                    for t in &e.args {
                        collect(t, caps);
                    }
                }
            }
            Type::Function {
                params,
                result,
                effects,
                ..
            } => {
                for e in &effects.instances {
                    if let Some(label) = &e.label
                        && (label.starts_with("$cap") || label.starts_with("$fncap:"))
                    {
                        caps.insert(label.clone());
                    }
                    for t in &e.args {
                        collect(t, caps);
                    }
                }
                for (_, t) in params {
                    collect(t, caps);
                }
                collect(result, caps);
            }
            _ => {}
        }
    }
    let mut caps = BTreeSet::new();
    collect(t, &mut caps);
    caps
}
fn contains_capability(t: &Type, cap: &Effect) -> bool {
    match t {
        Type::Row(effects) => effects
            .instances
            .iter()
            .any(|e| e.label == cap.label || e.args.iter().any(|t| contains_capability(t, cap))),
        Type::Named(_, ts) | Type::Tuple(ts) => ts.iter().any(|t| contains_capability(t, cap)),
        Type::Function {
            params,
            result,
            effects,
            ..
        } => {
            effects.instances.iter().any(|e| e.label == cap.label)
                || params.iter().any(|(_, t)| contains_capability(t, cap))
                || contains_capability(result, cap)
        }
        _ => false,
    }
}

fn effects_include(expected: &Effects, actual: &Effects) -> bool {
    (actual.tail.is_none() || expected.tail.is_some())
        && actual.instances.iter().all(|e| {
            expected.tail.is_some()
                || expected.instances.iter().any(|x| {
                    x.name == e.name
                        && (x.label.is_none() || x.label == e.label)
                        && x.args.len() == e.args.len()
                        && x.args.iter().zip(&e.args).all(|(a, b)| compatible(a, b))
                })
        })
}
fn refine_annotation(expected: &Type, actual: &Type) -> Type {
    match (expected, actual) {
        (Type::Named(n, a), Type::Named(m, b)) if n == m => Type::Named(
            n.clone(),
            a.iter()
                .zip(b)
                .map(|(a, b)| refine_annotation(a, b))
                .collect(),
        ),
        (Type::Tuple(a), Type::Tuple(b)) => Type::Tuple(
            a.iter()
                .zip(b)
                .map(|(a, b)| refine_annotation(a, b))
                .collect(),
        ),
        (
            Type::Function {
                params,
                result,
                effects,
                mode,
            },
            Type::Function {
                result: ar,
                effects: ae,
                ..
            },
        ) => {
            let mut row = ae.clone();
            for e in &effects.instances {
                if !row.instances.iter().any(|x| x.name == e.name) {
                    row.instances.push(e.clone());
                }
            }
            if row.tail.is_none() {
                row.tail = effects.tail.clone();
            }
            Type::Function {
                params: params.clone(),
                result: Box::new(refine_annotation(result, ar)),
                effects: row,
                mode: *mode,
            }
        }
        _ => expected.clone(),
    }
}

pub fn clause_free_names(c: &Clause) -> BTreeSet<String> {
    let mut free = free_names(&c.body);
    for p in &c.pats {
        for n in pattern_names(p) {
            free.remove(&n);
        }
    }
    free
}

fn function_capability(name: &str, index: usize) -> String {
    format!("$fncap:{name}:{index}")
}
fn required_label(label: &Option<String>) -> Option<String> {
    label.as_ref().and_then(|s| {
        if s.starts_with("$latent:") {
            s.split(':')
                .nth(2)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        } else {
            Some(s.clone())
        }
    })
}
fn latent_capability(origin: &str, e: &Effect) -> String {
    format!(
        "$latent:{origin}:{}:{}",
        required_label(&e.label).unwrap_or_default(),
        e.name
    )
}
fn rename_capabilities(t: &Type, labels: &BTreeMap<String, Option<String>>) -> Type {
    match t {
        Type::Named(n, ts) => Type::Named(
            n.clone(),
            ts.iter().map(|t| rename_capabilities(t, labels)).collect(),
        ),
        Type::Tuple(ts) => Type::Tuple(ts.iter().map(|t| rename_capabilities(t, labels)).collect()),
        Type::Row(effects) => {
            let mut effects = effects.clone();
            for e in &mut effects.instances {
                if let Some(label) = e.label.as_ref().and_then(|l| labels.get(l)) {
                    e.label = label.clone();
                }
                e.args = e
                    .args
                    .iter()
                    .map(|t| rename_capabilities(t, labels))
                    .collect();
            }
            Type::Row(effects)
        }
        Type::Function {
            params,
            result,
            effects,
            mode,
        } => {
            let mut effects = effects.clone();
            for e in &mut effects.instances {
                if let Some(label) = e.label.as_ref().and_then(|l| labels.get(l)) {
                    e.label = label.clone();
                }
                e.args = e
                    .args
                    .iter()
                    .map(|t| rename_capabilities(t, labels))
                    .collect();
            }
            Type::Function {
                params: params
                    .iter()
                    .map(|(m, t)| (*m, rename_capabilities(t, labels)))
                    .collect(),
                result: Box::new(rename_capabilities(result, labels)),
                effects,
                mode: *mode,
            }
        }
        _ => t.clone(),
    }
}
fn bind_signature_capabilities(t: &Type, f: &Function) -> Type {
    fn collect(t: &Type, f: &Function) -> Type {
        match t {
            Type::Function {
                params,
                result,
                effects,
                mode,
            } => {
                let mut row = effects.clone();
                for e in &mut row.instances {
                    if let Some((i, _)) = f.effects.instances.iter().enumerate().find(|(_, x)| {
                        x.name == e.name
                            && x.args == e.args
                            && (e.label.is_none() || e.label == x.label)
                    }) {
                        e.label = Some(function_capability(&f.name, i));
                    }
                }
                Type::Function {
                    params: params.iter().map(|(m, t)| (*m, collect(t, f))).collect(),
                    result: Box::new(collect(result, f)),
                    effects: row,
                    mode: *mode,
                }
            }
            Type::Named(n, ts) => {
                Type::Named(n.clone(), ts.iter().map(|t| collect(t, f)).collect())
            }
            Type::Tuple(ts) => Type::Tuple(ts.iter().map(|t| collect(t, f)).collect()),
            _ => t.clone(),
        }
    }
    collect(t, f)
}
fn function_value_type(f: &Function, inferred: Option<&Type>) -> Type {
    let mut effects = f.effects.clone();
    let mut labels = BTreeMap::new();
    for (i, e) in effects.instances.iter_mut().enumerate() {
        let label = latent_capability(&format!("f{}_{}", f.name.replace(':', "_"), i), e);
        labels.insert(function_capability(&f.name, i), Some(label.clone()));
        e.label = Some(label);
    }
    Type::Function {
        params: f.params.iter().map(|p| (p.pass, p.ty.clone())).collect(),
        result: Box::new(rename_capabilities(inferred.unwrap_or(&f.result), &labels)),
        effects,
        mode: Mode::Copy,
    }
}
fn instantiate_latent_effects(actual: &Type, target: &Type) -> Type {
    let mut labels = BTreeMap::new();
    fn collect(a: &Type, b: &Type, labels: &mut BTreeMap<String, Option<String>>) {
        if let (
            Type::Function {
                params: a,
                result: ar,
                effects: ae,
                ..
            },
            Type::Function {
                params: b,
                result: br,
                effects: be,
                ..
            },
        ) = (a, b)
        {
            if be.tail.is_none() {
                for e in &ae.instances {
                    if let Some(label) = &e.label
                        && label.starts_with("$latent:")
                        && let Some(want) = be
                            .instances
                            .iter()
                            .find(|x| x.name == e.name && x.args == e.args)
                        && want.label.is_some()
                    {
                        labels.insert(label.clone(), want.label.clone());
                    }
                }
            }
            for ((_, a), (_, b)) in a.iter().zip(b) {
                collect(a, b, labels);
            }
            collect(ar, br, labels);
        }
    }
    collect(actual, target, &mut labels);
    rename_capabilities(actual, &labels)
}
