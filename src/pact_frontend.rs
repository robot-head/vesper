//! Source trait/model frontend: static evidence elaboration (design §16.2–16.3).
//!
//! Accepted surface (see `docs/parallel-results/source-traits-models.md`):
//!
//! * `trait Name[params] { type A: mod m; fn method(in self, ...) -> T eff [r]; }`
//! * canonical `impl[params] Trait[args] for Type where T: Trait { type A = T; fn ... }`
//! * named `model Name[params]: Trait[args] for Type { ... }`
//! * lexical `use model Name;` in a block (or at item level)
//! * projections `<T as Trait[args]>::A` and shorthand `T::A` / `Self::A`
//! * receiver calls `x.method(...)`, trait paths `Trait::method(x, ...)` and
//!   explicitly branded model paths `Model::method(x, ...)`
//!
//! Declarations are lowered to the root-free `vesper-pact-binding` DTOs,
//! validated and coherence-checked by Pact. Every trait call is resolved by
//! a Pact query under the call's lexical `use model` scopes; only `Proven`
//! evidence that the session re-checks and consumes selects a declaration.
//! Its method becomes a private generated function and the call becomes an
//! ordinary call of that function. A second, ordinary checking pass over the
//! fully elaborated module produces the World backends consume, so the
//! existing `Type`/`ExprKind`/Place-IR schemas are unchanged and unresolved
//! trait operations never reach a backend.
//!
//! Generic dictionary dispatch (trait-bounded function generics, calls
//! resolved only by a where-clause assumption), unresolved projections and
//! cross-package dispatch are rejected before a World is accepted.
use crate::ast::*;
use crate::check::{self, Checked, TraitCall, TraitCalls, World};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use vesper_pact::SolverConfig;
use vesper_pact_binding::{
    ArgDto, AssocDto, BINDING_SCHEMA, BindingsDto, BridgeError, BridgeOutcome, Certificate,
    CoherentSession, ConformanceDto, DefKind, DefPath, DefRefDto, EnvDto, ErrorKind, GoalDto,
    ImplDto, ModeDto, ModelDto, PackageIdentity, ParamDto, ParamKindDto, ProjectionDto,
    QueryBindingDto, Revision, Selection, SessionBuilder, SourcePackage, SourceWorld, TraitDto,
    TypeDto,
};

/// Package identity used by [`check::check`], which has no package context.
pub const DEFAULT_PACKAGE: &str = "@local/main";
pub const DEFAULT_VERSION: &str = "0.0.0";

impl SourceWorld for World {
    fn adts(&self) -> &BTreeMap<String, Adt> {
        &self.adts
    }
    fn effects(&self) -> &BTreeMap<String, EffectDecl> {
        &self.effects
    }
    fn mode(&self, ty: &Type, bounds: &BTreeMap<String, Mode>) -> Mode {
        World::mode(self, ty, bounds)
    }
}

/// The checker prelude (builtin ADTs and effects) as a World.
pub fn prelude_world() -> World {
    World::collect(&Module::default()).0
}

/// An already-written dependency package whose trait, impl and model
/// declarations join the coherence check. Its source is checked on its own.
/// The dependent may name its traits as `@scope/name::Trait`; calls that
/// would dispatch into a dependency's implementation are unsupported (no
/// cross-package linking exists).
#[derive(Clone, Debug)]
pub struct Dependency {
    pub identity: PackageIdentity,
    pub module: Module,
}

#[derive(Clone, Debug, Default)]
pub struct FrontendOptions {
    pub solver: Option<SolverConfig>,
    pub dependencies: Vec<Dependency>,
}

/// One trait call resolved by consumed evidence.
#[derive(Clone, Debug)]
pub struct ResolvedCall {
    pub span: Span,
    /// The generated function the call now invokes.
    pub function: String,
    /// The selected impl or model, rendered with its package-qualified path.
    pub selection: String,
    /// Serializable evidence, valid only under [`FrontendChecked::revision`].
    pub certificate: Certificate,
}

/// The result of checking one source package.
pub struct FrontendChecked {
    /// The checked, fully elaborated core World (or diagnostics).
    pub checked: Checked,
    /// The package's source declarations lowered to binding DTOs.
    pub bindings: BindingsDto,
    /// The coherent declaration session (absent without trait declarations
    /// or when lowering/coherence failed).
    pub session: Option<CoherentSession>,
    pub revision: Option<Revision>,
    pub evidence: Vec<ResolvedCall>,
}

/// [`check::check`]: elaborate under [`DEFAULT_PACKAGE`].
pub fn check_module(module: &Module) -> Checked {
    if !has_frontend_syntax(module) {
        return check::check_core(module, None).0;
    }
    check_package(module, &default_identity()).checked
}

pub fn default_identity() -> PackageIdentity {
    PackageIdentity::new(DEFAULT_PACKAGE, DEFAULT_VERSION)
}

pub fn check_package(module: &Module, identity: &PackageIdentity) -> FrontendChecked {
    check_package_with(module, identity, &FrontendOptions::default())
}

pub fn check_package_with(
    module: &Module,
    identity: &PackageIdentity,
    options: &FrontendOptions,
) -> FrontendChecked {
    let empty = BindingsDto::empty(identity.clone());
    if !has_frontend_syntax(module) && options.dependencies.is_empty() {
        let checked = check::check_core(module, None).0;
        return FrontendChecked {
            checked,
            bindings: empty,
            session: None,
            revision: None,
            evidence: vec![],
        };
    }
    let fail = |diagnostics: Vec<Diagnostic>, bindings: BindingsDto| FrontendChecked {
        checked: Checked {
            world: World::default(),
            diagnostics,
            expression_types: TypeMap::new(),
        },
        bindings,
        session: None,
        revision: None,
        evidence: vec![],
    };
    if let Err(e) = identity.validate() {
        return fail(vec![Diagnostic::new("E0600", e, 0..0)], empty);
    }
    let mut fe = match Frontend::new(module, identity, options) {
        Ok(fe) => fe,
        Err(d) => return fail(d, empty),
    };
    let bindings = fe.bindings.clone();
    if !fe.diagnostics.is_empty() {
        return fail(std::mem::take(&mut fe.diagnostics), bindings);
    }
    let session = match fe.build_session() {
        Ok(s) => s,
        Err(d) => return fail(d, bindings),
    };
    let revision = session.revision().clone();
    let fe = Elaborator {
        fe,
        session,
        evidence: RefCell::new(BTreeMap::new()),
        normalizing: RefCell::new(vec![]),
    };
    // Pass 1: generated functions plus normalized source, trait calls
    // resolved through Pact.
    let pass1 = match fe.pass1_module() {
        Ok(m) => m,
        Err(d) => {
            let bindings = fe.fe.bindings.clone();
            return fail(d, bindings);
        }
    };
    let (first, resolved) = check::check_core(&pass1, Some(&fe));
    if !first.diagnostics.is_empty() {
        let Elaborator { fe, session, .. } = fe;
        return FrontendChecked {
            checked: first,
            bindings: fe.bindings,
            session: Some(session),
            revision: Some(revision),
            evidence: vec![],
        };
    }
    // Pass 2: the elaborated module contains only ordinary calls.
    let pass2 = rewrite_calls(pass1, &resolved);
    let (mut checked, _) = check::check_core(&pass2, None);
    let mut extra = vec![];
    for f in checked.world.functions.values() {
        let mut residue = false;
        let mut probe = f.clone();
        walk_function(&mut probe, &mut |t| residue |= contains_projection(t));
        if residue {
            extra.push(Diagnostic::new(
                "E0612",
                format!("unresolved projection remains in '{}'", f.name),
                f.span.clone(),
            ));
        }
    }
    checked.diagnostics.extend(extra);
    if checked.diagnostics.is_empty() {
        checked.world.elaboration = fe.metadata(&revision);
    }
    let Elaborator {
        fe,
        session,
        evidence,
        ..
    } = fe;
    let evidence = evidence
        .into_inner()
        .into_iter()
        .filter(|(k, _)| resolved.contains_key(k))
        .map(|(_, v)| v)
        .collect();
    FrontendChecked {
        checked,
        bindings: fe.bindings,
        session: Some(session),
        revision: Some(revision),
        evidence,
    }
}

/// Whether the module uses any frontend-only trait syntax.
pub fn has_frontend_syntax(module: &Module) -> bool {
    if !module.model_uses.is_empty() || !module.trait_bounds.is_empty() {
        return true;
    }
    let mut found = false;
    for item in &module.items {
        match item {
            Item::Trait(_) | Item::Conformance(_) => return true,
            Item::Function(f) => {
                let mut f = (**f).clone();
                walk_function(&mut f, &mut |t| found |= contains_projection(t));
            }
            Item::Impl { methods, .. } => {
                for f in methods {
                    let mut f = f.clone();
                    walk_function(&mut f, &mut |t| found |= contains_projection(t));
                }
            }
            Item::Adt(a) => {
                for f in a.variants.iter().flat_map(|v| &v.fields) {
                    found |= contains_projection(&f.ty);
                }
            }
            Item::Effect(e) => {
                for f in &e.ops {
                    let mut f = f.clone();
                    walk_function(&mut f, &mut |t| found |= contains_projection(t));
                }
            }
        }
        if found {
            return true;
        }
    }
    false
}

fn contains_projection(t: &Type) -> bool {
    match t {
        Type::Named(n, args) => n == PROJECTION_TYPE || args.iter().any(contains_projection),
        Type::Tuple(ts) => ts.iter().any(contains_projection),
        Type::Function {
            params,
            result,
            effects,
            ..
        } => {
            params.iter().any(|(_, t)| contains_projection(t))
                || contains_projection(result)
                || effects_contain_projection(effects)
        }
        Type::Row(e) => effects_contain_projection(e),
        _ => false,
    }
}
fn effects_contain_projection(e: &Effects) -> bool {
    e.instances
        .iter()
        .any(|i| i.args.iter().any(contains_projection))
}
fn has_infer(t: &Type) -> bool {
    match t {
        Type::Infer => true,
        Type::Named(_, args) | Type::Tuple(args) => args.iter().any(has_infer),
        _ => false,
    }
}
fn mentions(t: &Type, name: &str) -> bool {
    match t {
        Type::Named(n, args) => n == name || args.iter().any(|t| mentions(t, name)),
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
                    .any(|i| i.args.iter().any(|t| mentions(t, name)))
        }
        Type::Row(e) => e
            .instances
            .iter()
            .any(|i| i.args.iter().any(|t| mentions(t, name))),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Declarations

/// A trait visible to the package being checked.
#[derive(Clone)]
struct TraitInfo {
    decl: TraitDecl,
    package: PackageIdentity,
    local: bool,
}

/// A local or dependency conformance.
#[derive(Clone)]
struct ConfInfo {
    decl: Conformance,
    path: DefPath,
    /// Key of the conformed trait in `Frontend::traits`.
    trait_key: TraitKey,
    local: bool,
    /// Generated function name per method (local conformances only).
    generated: BTreeMap<String, String>,
}

/// `(package, trait name)`.
type TraitKey = (PackageIdentity, String);

struct Frontend {
    identity: PackageIdentity,
    module: Module,
    /// Ordinary items (frontend items removed).
    base: Module,
    base_world: World,
    traits: BTreeMap<TraitKey, TraitInfo>,
    confs: Vec<ConfInfo>,
    /// Dependency source packages and their bindings.
    deps: Vec<(SourcePackage, BindingsDto)>,
    dep_names: BTreeMap<String, PackageIdentity>,
    bindings: BindingsDto,
    solver: Option<SolverConfig>,
    diagnostics: Vec<Diagnostic>,
    /// Declaration spans by stable path, for coherence diagnostics.
    spans: BTreeMap<DefPath, Span>,
}

fn d(code: &str, msg: impl Into<String>, span: &Span) -> Diagnostic {
    Diagnostic::new(code, msg, span.clone())
}

/// Injective encoding of a type into impl-key segments (`.`/`-`-separated
/// identifiers or digits): declaration identities never use spans.
fn key_segment(t: &Type) -> String {
    match t {
        Type::Named(n, args) if args.is_empty() => n.clone(),
        Type::Named(n, args) => format!(
            "{n}-0-{}-2",
            args.iter().map(key_segment).collect::<Vec<_>>().join("-1-")
        ),
        Type::Tuple(ts) if ts.is_empty() => "Unit".into(),
        Type::Tuple(ts) => format!(
            "3-0-{}-2",
            ts.iter().map(key_segment).collect::<Vec<_>>().join("-1-")
        ),
        Type::Row(e) => format!(
            "4-0-{}-2",
            e.instances
                .iter()
                .map(|i| key_segment(&Type::Named(i.name.clone(), i.args.clone())))
                .chain(e.tail.clone())
                .collect::<Vec<_>>()
                .join("-1-")
        ),
        _ => "5".into(),
    }
}

fn impl_key(c: &Conformance) -> String {
    let mut trait_part = c.trait_ref.name.clone();
    if !c.trait_ref.args.is_empty() {
        trait_part = key_segment(&Type::Named(trait_part, c.trait_ref.args.clone()));
    }
    format!("{trait_part}.{}", key_segment(&c.self_ty))
}

fn conformance_path(package: &PackageIdentity, c: &Conformance) -> DefPath {
    match &c.kind {
        ConformanceKind::Canonical => DefPath::new(package.clone(), DefKind::Impl, impl_key(c)),
        ConformanceKind::Model { name, .. } => {
            DefPath::new(package.clone(), DefKind::Model, name.clone())
        }
    }
}

fn generated_name(c: &Conformance, method: &str) -> String {
    match &c.kind {
        ConformanceKind::Canonical => format!("impl${}${method}", impl_key(c)),
        ConformanceKind::Model { name, .. } => format!("model${name}${method}"),
    }
}

fn base_module(module: &Module) -> Module {
    Module {
        items: module
            .items
            .iter()
            .filter(|i| !matches!(i, Item::Trait(_) | Item::Conformance(_)))
            .cloned()
            .collect(),
        ..Module::default()
    }
}

fn param_dto(g: &Generic) -> ParamDto {
    if g.row {
        ParamDto::of(g.name.clone(), ParamKindDto::Row)
    } else {
        ParamDto::ty(g.name.clone(), Some(g.bound))
    }
}

impl Frontend {
    fn new(
        module: &Module,
        identity: &PackageIdentity,
        options: &FrontendOptions,
    ) -> Result<Self, Vec<Diagnostic>> {
        let base = base_module(module);
        let (base_world, _) = World::collect(&base);
        let mut fe = Frontend {
            identity: identity.clone(),
            module: module.clone(),
            base,
            base_world,
            traits: BTreeMap::new(),
            confs: vec![],
            deps: vec![],
            dep_names: BTreeMap::new(),
            bindings: BindingsDto::empty(identity.clone()),
            solver: options.solver,
            diagnostics: vec![],
            spans: BTreeMap::new(),
        };
        for dep in &options.dependencies {
            fe.add_dependency(dep)?;
        }
        fe.collect_local();
        fe.bindings = fe.lower_bindings(&fe.identity.clone(), true);
        Ok(fe)
    }

    fn add_dependency(&mut self, dep: &Dependency) -> Result<(), Vec<Diagnostic>> {
        let span = 0..0;
        if dep.identity == self.identity || self.dep_names.contains_key(&dep.identity.name) {
            return Err(vec![d(
                "E0600",
                format!("dependency {} is declared twice", dep.identity),
                &span,
            )]);
        }
        let checked = check_package(&dep.module, &dep.identity);
        if !checked.checked.diagnostics.is_empty() {
            return Err(checked
                .checked
                .diagnostics
                .into_iter()
                .map(|x| {
                    Diagnostic::new(
                        &x.code,
                        format!("in dependency {}: {}", dep.identity, x.message),
                        0..0,
                    )
                })
                .collect());
        }
        let base = base_module(&dep.module);
        let (world, _) = World::collect(&base);
        let source = SourcePackage::new(dep.identity.clone(), Arc::new(world), &base)
            .map_err(|e| bridge_diagnostics(&e, &BTreeMap::new()))?
            .with_fingerprint(declaration_fingerprint(&dep.module));
        for item in &dep.module.items {
            match item {
                Item::Trait(t) => {
                    self.traits.insert(
                        (dep.identity.clone(), t.name.clone()),
                        TraitInfo {
                            decl: t.clone(),
                            package: dep.identity.clone(),
                            local: false,
                        },
                    );
                }
                Item::Conformance(c) => {
                    let trait_key = (
                        c.trait_ref
                            .package
                            .as_ref()
                            .and_then(|p| self.dep_names.get(p).cloned())
                            .unwrap_or_else(|| dep.identity.clone()),
                        c.trait_ref.name.clone(),
                    );
                    self.confs.push(ConfInfo {
                        decl: c.clone(),
                        path: conformance_path(&dep.identity, c),
                        trait_key,
                        local: false,
                        generated: BTreeMap::new(),
                    });
                }
                _ => {}
            }
        }
        self.deps.push((source, checked.bindings));
        self.dep_names
            .insert(dep.identity.name.clone(), dep.identity.clone());
        Ok(())
    }

    /// Collects and validates local trait and conformance declarations.
    fn collect_local(&mut self) {
        let module = self.module.clone();
        let mut names: BTreeMap<String, &'static str> = BTreeMap::new();
        for n in self.base_world.adts.keys() {
            names.insert(n.clone(), "type");
        }
        for n in self.base_world.effects.keys() {
            names.insert(n.clone(), "effect");
        }
        for item in &module.items {
            if let Item::Trait(t) = item {
                if let Some(kind) = names.insert(t.name.clone(), "trait") {
                    self.diagnostics.push(d(
                        "E0600",
                        format!(
                            "trait '{}' conflicts with a {kind} of the same name",
                            t.name
                        ),
                        &t.span,
                    ));
                    continue;
                }
                self.validate_trait(t);
                self.spans.insert(
                    DefPath::new(self.identity.clone(), DefKind::Trait, t.name.clone()),
                    t.span.clone(),
                );
                self.traits.insert(
                    (self.identity.clone(), t.name.clone()),
                    TraitInfo {
                        decl: t.clone(),
                        package: self.identity.clone(),
                        local: true,
                    },
                );
            }
        }
        for item in &module.items {
            let Item::Conformance(c) = item else { continue };
            if let ConformanceKind::Model { name, .. } = &c.kind
                && let Some(kind) = names.insert(name.clone(), "model")
            {
                self.diagnostics.push(d(
                    "E0600",
                    format!("model '{name}' conflicts with a {kind} of the same name"),
                    &c.span,
                ));
                continue;
            }
            let Some(trait_key) = self.trait_key(&c.trait_ref) else {
                continue;
            };
            let path = conformance_path(&self.identity, c);
            if self.confs.iter().any(|x| x.local && x.path == path) {
                self.diagnostics.push(d(
                    "E0605",
                    format!(
                        "{path} overlaps an earlier canonical impl with the same head; declare one as a named `model`"
                    ),
                    &c.span,
                ));
                continue;
            }
            self.spans.insert(path.clone(), c.span.clone());
            let generated = c
                .methods
                .iter()
                .map(|m| (m.name.clone(), generated_name(c, &m.name)))
                .collect();
            let info = ConfInfo {
                decl: c.clone(),
                path,
                trait_key,
                local: true,
                generated,
            };
            self.validate_conformance(&info);
            self.confs.push(info);
        }
        for u in &module.model_uses {
            if !self
                .confs
                .iter()
                .any(|c| c.local && matches!(&c.decl.kind, ConformanceKind::Model { name, .. } if *name == u.model))
            {
                self.diagnostics.push(d(
                    "E0613",
                    format!("'{}' is not a model declared in this package", u.model),
                    &u.span,
                ));
            }
        }
        for b in &module.trait_bounds {
            self.diagnostics.push(d(
                "E0607",
                format!(
                    "unsupported generic dictionary dispatch: '{}: {}' on a function needs a runtime dictionary; only concrete (static) trait calls are elaborated",
                    b.param, b.trait_ref.name
                ),
                &b.span,
            ));
        }
        for item in &module.items {
            match item {
                Item::Adt(a)
                    if a.variants
                        .iter()
                        .flat_map(|v| &v.fields)
                        .any(|f| contains_projection(&f.ty)) =>
                {
                    self.diagnostics.push(d(
                        "E0607",
                        format!(
                            "projections in the fields of '{}' are not supported",
                            a.name
                        ),
                        &a.span,
                    ));
                }
                Item::Effect(e) => {
                    for op in &e.ops {
                        let mut f = op.clone();
                        let mut found = false;
                        walk_function(&mut f, &mut |t| found |= contains_projection(t));
                        if found {
                            self.diagnostics.push(d(
                                "E0607",
                                "projections in effect operation signatures are not supported",
                                &op.span,
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn trait_key(&mut self, r: &TraitRef) -> Option<TraitKey> {
        let package = match &r.package {
            Some(p) => match self.dep_names.get(p) {
                Some(id) => id.clone(),
                None => {
                    self.diagnostics.push(d(
                        "E0601",
                        format!("unknown dependency package '{p}'"),
                        &r.span,
                    ));
                    return None;
                }
            },
            None => self.identity.clone(),
        };
        let key = (package, r.name.clone());
        if self.traits.contains_key(&key) {
            Some(key)
        } else {
            self.diagnostics
                .push(d("E0601", format!("unknown trait '{}'", r.name), &r.span));
            None
        }
    }

    fn validate_trait(&mut self, t: &TraitDecl) {
        let mut seen = BTreeSet::new();
        for g in &t.generics {
            if g.name == "Self" || !seen.insert(g.name.clone()) {
                self.diagnostics.push(d(
                    "E0600",
                    format!("duplicate or reserved trait parameter '{}'", g.name),
                    &t.span,
                ));
            }
        }
        for a in &t.assoc {
            if !seen.insert(a.name.clone()) {
                self.diagnostics.push(d(
                    "E0600",
                    format!("duplicate associated type '{}'", a.name),
                    &a.span,
                ));
            }
        }
        let mut methods = BTreeSet::new();
        for m in &t.methods {
            if !methods.insert(m.name.clone()) {
                self.diagnostics.push(d(
                    "E0600",
                    format!("duplicate trait method '{}'", m.name),
                    &m.span,
                ));
            }
            if !m.generics.is_empty() {
                self.diagnostics.push(d(
                    "E0607",
                    "generic trait methods are not supported in the static fragment",
                    &m.span,
                ));
            }
            match m.params.first() {
                Some(p) if p.name == "self" => {
                    if p.ty != Type::Infer && p.ty != Type::named("Self") {
                        self.diagnostics.push(d(
                            "E0600",
                            "a trait method's 'self' has type Self",
                            &p.span,
                        ));
                    }
                }
                _ => self.diagnostics.push(d(
                    "E0607",
                    format!(
                        "trait method '{}' needs a 'self' receiver; static trait functions are not supported",
                        m.name
                    ),
                    &m.span,
                )),
            }
            for p in m.params.iter().skip(1) {
                if p.name == "self" {
                    self.diagnostics.push(d(
                        "E0600",
                        "'self' must be the first parameter",
                        &p.span,
                    ));
                }
                if p.ty == Type::Infer {
                    self.diagnostics.push(d(
                        "E0102",
                        "trait method parameters require a type",
                        &p.span,
                    ));
                }
            }
        }
    }

    /// Substitution of the trait's `Self`, parameters and own associated
    /// types for one conformance (projections of other types stay).
    fn trait_substitution(&self, c: &ConfInfo) -> BTreeMap<String, Type> {
        let t = &self.traits[&c.trait_key].decl;
        let mut vars = BTreeMap::new();
        vars.insert("Self".to_owned(), c.decl.self_ty.clone());
        for (g, a) in t.generics.iter().zip(&c.decl.trait_ref.args) {
            vars.insert(g.name.clone(), if g.row { row_arg(a) } else { a.clone() });
        }
        vars
    }

    /// Replaces `Self`, trait parameters and `Self::A`/`<Self as T>::A` of
    /// conformance `c` by their declared values.
    fn conform_type(&self, t: &Type, c: &ConfInfo, vars: &BTreeMap<String, Type>) -> Type {
        let tr = &self.traits[&c.trait_key].decl;
        let assoc = |name: &str| {
            c.decl
                .assoc
                .iter()
                .find(|a| a.name == name)
                .map(|a| a.ty.clone())
        };
        fn go(
            t: &Type,
            vars: &BTreeMap<String, Type>,
            tr: &TraitDecl,
            assoc: &dyn Fn(&str) -> Option<Type>,
            stack: &mut Vec<String>,
        ) -> Type {
            // `stack` holds the associated types being expanded on this
            // path; a cyclic binding (rejected with E0601 by
            // `validate_conformance`) is left unexpanded instead of
            // recursing forever.
            if let Some(p) = as_projection(t)
                && *p.self_ty == Type::named("Self")
                && p.trait_name.is_none_or(|n| n == tr.name)
                && tr.assoc.iter().any(|a| a.name == p.assoc)
                && !stack.iter().any(|s| s == p.assoc)
                && let Some(v) = assoc(p.assoc)
            {
                stack.push(p.assoc.to_owned());
                let out = go(&v, vars, tr, assoc, stack);
                stack.pop();
                return out;
            }
            match t {
                Type::Named(n, args) if args.is_empty() && vars.contains_key(n) => vars[n].clone(),
                Type::Named(n, args) => Type::Named(
                    n.clone(),
                    args.iter().map(|a| go(a, vars, tr, assoc, stack)).collect(),
                ),
                Type::Tuple(ts) => {
                    Type::Tuple(ts.iter().map(|a| go(a, vars, tr, assoc, stack)).collect())
                }
                Type::Function {
                    params,
                    result,
                    effects,
                    mode,
                } => Type::Function {
                    params: params
                        .iter()
                        .map(|(p, t)| (*p, go(t, vars, tr, assoc, stack)))
                        .collect(),
                    result: Box::new(go(result, vars, tr, assoc, stack)),
                    effects: go_effects(effects, vars, tr, assoc, stack),
                    mode: *mode,
                },
                Type::Row(e) => Type::Row(go_effects(e, vars, tr, assoc, stack)),
                _ => t.clone(),
            }
        }
        fn go_effects(
            e: &Effects,
            vars: &BTreeMap<String, Type>,
            tr: &TraitDecl,
            assoc: &dyn Fn(&str) -> Option<Type>,
            stack: &mut Vec<String>,
        ) -> Effects {
            let mapped = Effects {
                instances: e
                    .instances
                    .iter()
                    .map(|i| Effect {
                        label: i.label.clone(),
                        name: i.name.clone(),
                        args: i
                            .args
                            .iter()
                            .map(|a| go(a, vars, tr, assoc, stack))
                            .collect(),
                    })
                    .collect(),
                tail: e.tail.clone(),
            };
            substitute_effects(&mapped, vars)
        }
        go(t, vars, tr, &assoc, &mut vec![])
    }

    fn conform_effects(&self, e: &Effects, c: &ConfInfo, vars: &BTreeMap<String, Type>) -> Effects {
        match self.conform_type(&Type::Row(e.clone()), c, vars) {
            Type::Row(e) => e,
            _ => unreachable!("rows map to rows"),
        }
    }

    fn validate_conformance(&mut self, c: &ConfInfo) {
        let Some(info) = self.traits.get(&c.trait_key).cloned() else {
            return;
        };
        let t = &info.decl;
        let span = &c.decl.span;
        if c.decl.trait_ref.args.len() != t.generics.len() {
            self.diagnostics.push(d(
                "E0601",
                format!(
                    "trait '{}' takes {} arguments, found {}",
                    t.name,
                    t.generics.len(),
                    c.decl.trait_ref.args.len()
                ),
                &c.decl.trait_ref.span,
            ));
            return;
        }
        let bounds: BTreeMap<String, Mode> = c
            .decl
            .generics
            .iter()
            .filter(|g| !g.row)
            .map(|g| (g.name.clone(), g.bound))
            .collect();
        for (g, a) in t.generics.iter().zip(&c.decl.trait_ref.args) {
            if !g.row && contains_projection(a) {
                continue;
            }
            if !g.row && self.base_world.mode(a, &bounds) > g.bound {
                self.diagnostics.push(d(
                    "E0606",
                    format!(
                        "trait argument {a} for '{}' exceeds its modal bound mod {:?}",
                        g.name, g.bound
                    ),
                    &c.decl.trait_ref.span,
                ));
            }
        }
        // Associated types: exactly the trait's, each within its bound.
        let mut given = BTreeSet::new();
        for a in &c.decl.assoc {
            match t.assoc.iter().find(|x| x.name == a.name) {
                None => self.diagnostics.push(d(
                    "E0601",
                    format!("'{}' is not an associated type of '{}'", a.name, t.name),
                    &a.span,
                )),
                Some(decl) => {
                    if !given.insert(a.name.clone()) {
                        self.diagnostics.push(d(
                            "E0601",
                            format!("associated type '{}' is given twice", a.name),
                            &a.span,
                        ));
                    }
                    if !contains_projection(&a.ty)
                        && !mentions(&a.ty, "Self")
                        && self.base_world.mode(&a.ty, &bounds) > decl.bound
                    {
                        self.diagnostics.push(d(
                            "E0606",
                            format!(
                                "associated type {} = {} exceeds its modal bound mod {:?}",
                                a.name, a.ty, decl.bound
                            ),
                            &a.span,
                        ));
                    }
                }
            }
        }
        for a in &t.assoc {
            if !given.contains(&a.name) {
                self.diagnostics.push(d(
                    "E0601",
                    format!("missing associated type '{}'", a.name),
                    span,
                ));
            }
        }
        if let Some(cycle) = assoc_cycle(t, c) {
            self.diagnostics.push(d(
                "E0601",
                format!("associated type binding cycle: {}", cycle.join(" -> ")),
                span,
            ));
        }
        // Methods: exactly the trait's, with matching signatures.
        let vars = self.trait_substitution(c);
        let mut seen = BTreeSet::new();
        for m in &c.decl.methods {
            if !seen.insert(m.name.clone()) {
                self.diagnostics.push(d(
                    "E0601",
                    format!("method '{}' is defined twice", m.name),
                    &m.span,
                ));
                continue;
            }
            let Some(sig) = t.methods.iter().find(|x| x.name == m.name) else {
                self.diagnostics.push(d(
                    "E0601",
                    format!("'{}' is not a method of trait '{}'", m.name, t.name),
                    &m.span,
                ));
                continue;
            };
            self.compare_signature(c, sig, m, &vars);
        }
        for sig in &t.methods {
            if !seen.contains(&sig.name) {
                self.diagnostics.push(d(
                    "E0601",
                    format!("missing method '{}' of trait '{}'", sig.name, t.name),
                    span,
                ));
            }
        }
        if !c.local {
            return;
        }
        // Cross-package trait signatures must only mention builtin types.
        if !info.local {
            for sig in &t.methods {
                let mut f = sig.clone();
                let mut foreign = false;
                let dep_world = &self
                    .deps
                    .iter()
                    .find(|(p, _)| *p.identity() == info.package)
                    .map(|(p, _)| p.adt_names().map(str::to_owned).collect::<BTreeSet<_>>())
                    .unwrap_or_default();
                walk_function(&mut f, &mut |ty| {
                    foreign |= dep_world.iter().any(|n| mentions(ty, n));
                });
                if foreign {
                    self.diagnostics.push(d(
                        "E0607",
                        format!(
                            "method '{}' of dependency trait '{}' mentions dependency types; cross-package source types are not supported",
                            sig.name, t.name
                        ),
                        span,
                    ));
                }
            }
        }
    }

    fn compare_signature(
        &mut self,
        c: &ConfInfo,
        sig: &Function,
        m: &Function,
        vars: &BTreeMap<String, Type>,
    ) {
        if !m.generics.is_empty() {
            self.diagnostics.push(d(
                "E0607",
                "generic methods in impls and models are not supported in the static fragment",
                &m.span,
            ));
            return;
        }
        if sig.params.len() != m.params.len() {
            self.diagnostics.push(d(
                "E0602",
                format!(
                    "method '{}' takes {} parameters in the trait, found {}",
                    m.name,
                    sig.params.len(),
                    m.params.len()
                ),
                &m.span,
            ));
            return;
        }
        let self_ty = &c.decl.self_ty;
        let own: BTreeMap<String, Type> = [("Self".to_owned(), self_ty.clone())].into();
        for (i, (expected, actual)) in sig.params.iter().zip(&m.params).enumerate() {
            if i == 0 && actual.name != "self" {
                self.diagnostics.push(d(
                    "E0602",
                    format!("method '{}' must take 'self' first", m.name),
                    &actual.span,
                ));
                continue;
            }
            if expected.pass != actual.pass {
                self.diagnostics.push(d(
                    "E0603",
                    format!(
                        "parameter '{}' of '{}' is passed {:?} but the trait declares {:?}",
                        actual.name, m.name, actual.pass, expected.pass
                    ),
                    &actual.span,
                ));
            }
            if expected.projections != actual.projections {
                self.diagnostics.push(d(
                    "E0603",
                    format!(
                        "parameter '{}' of '{}' changes the trait's place projections",
                        actual.name, m.name
                    ),
                    &actual.span,
                ));
            }
            let want = if i == 0 {
                self_ty.clone()
            } else {
                self.conform_type(&expected.ty, c, vars)
            };
            let have = if i == 0 && actual.ty == Type::Infer {
                self_ty.clone()
            } else {
                self.conform_type(&actual.ty, c, &own)
            };
            if !same_type(&want, &have) {
                self.diagnostics.push(d(
                    "E0602",
                    format!(
                        "parameter '{}' of '{}' has type {have}, but the trait requires {want}",
                        actual.name, m.name
                    ),
                    &actual.span,
                ));
            }
        }
        let want = self.conform_type(&sig.result, c, vars);
        let have = self.conform_type(&m.result, c, &own);
        if !same_type(&want, &have) {
            self.diagnostics.push(d(
                "E0602",
                format!("'{}' returns {have}, but the trait requires {want}", m.name),
                &m.span,
            ));
        }
        let want = self.conform_effects(&sig.effects, c, vars);
        let have = self.conform_effects(&m.effects, c, &own);
        for e in &have.instances {
            if !want
                .instances
                .iter()
                .any(|w| w.name == e.name && same_args(&w.args, &e.args))
            {
                self.diagnostics.push(d(
                    "E0604",
                    format!(
                        "'{}' performs {}{} but the trait signature does not permit it",
                        m.name,
                        e.name,
                        if e.args.is_empty() {
                            String::new()
                        } else {
                            format!(
                                "[{}]",
                                e.args
                                    .iter()
                                    .map(ToString::to_string)
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            )
                        }
                    ),
                    &m.span,
                ));
            }
            if e.label.is_some() {
                self.diagnostics.push(d(
                    "E0607",
                    "labelled capabilities in trait method signatures are not supported",
                    &m.span,
                ));
            }
        }
        if have.tail.is_some() && have.tail != want.tail {
            self.diagnostics.push(d(
                "E0604",
                format!(
                    "'{}' has an open effect row the trait does not declare",
                    m.name
                ),
                &m.span,
            ));
        }
    }

    // -- Binding DTOs -------------------------------------------------------

    fn trait_def_ref(&self, key: &TraitKey, site: &PackageIdentity) -> DefRefDto {
        if key.0 == *site {
            DefRefDto::local(key.1.clone())
        } else {
            DefRefDto::qualified(key.0.clone(), key.1.clone())
        }
    }

    fn lower_bindings(&mut self, site: &PackageIdentity, local: bool) -> BindingsDto {
        let mut b = BindingsDto::empty(site.clone());
        let traits: Vec<TraitInfo> = self
            .traits
            .values()
            .filter(|t| t.local == local && t.package == *site)
            .cloned()
            .collect();
        for t in traits {
            b.traits.push(TraitDto {
                name: t.decl.name.clone(),
                // Pact trait parameters carry kinds only; their modal bounds
                // are checked by `validate_conformance` (E0606).
                params: t
                    .decl
                    .generics
                    .iter()
                    .map(|g| {
                        if g.row {
                            param_dto(g)
                        } else {
                            ParamDto::ty(g.name.clone(), None)
                        }
                    })
                    .collect(),
                assoc: t
                    .decl
                    .assoc
                    .iter()
                    .map(|a| AssocDto {
                        name: a.name.clone(),
                        mode_bound: ModeDto::Const(a.bound),
                        bounds: vec![],
                    })
                    .collect(),
                coinductive: false,
            });
        }
        let confs: Vec<ConfInfo> = self
            .confs
            .iter()
            .filter(|c| c.local == local)
            .cloned()
            .collect();
        for c in confs {
            let Some(info) = self.traits.get(&c.trait_key).cloned() else {
                continue;
            };
            let vars: BTreeMap<String, Type> = [("Self".to_owned(), c.decl.self_ty.clone())].into();
            let lower = || -> Result<ConformanceDto, String> {
                let self_ty = self.dto(&c.decl.self_ty, &c.decl.generics)?;
                let args = info
                    .decl
                    .generics
                    .iter()
                    .zip(&c.decl.trait_ref.args)
                    .map(|(g, a)| {
                        if g.row {
                            ArgDto::from_source(&row_arg(a))
                        } else {
                            self.dto(a, &c.decl.generics).map(ArgDto::Type)
                        }
                    })
                    .collect::<Result<_, _>>()?;
                let mut where_clauses = vec![];
                for w in &c.decl.where_clauses {
                    let Some(key) = self.where_key(&w.trait_ref) else {
                        return Err(format!("unknown trait '{}'", w.trait_ref.name));
                    };
                    where_clauses.push(GoalDto::Implements {
                        self_ty: self.dto(&w.ty, &c.decl.generics)?,
                        trait_ref: self.trait_def_ref(&key, site),
                        args: self.where_args(&key, &w.trait_ref.args, &c.decl.generics)?,
                        brand: None,
                    });
                }
                let mut assoc = vec![];
                for a in &c.decl.assoc {
                    let ty = self.conform_type(&a.ty, &c, &vars);
                    assoc.push((a.name.clone(), self.dto(&ty, &c.decl.generics)?));
                }
                Ok(ConformanceDto {
                    params: c.decl.generics.iter().map(param_dto).collect(),
                    trait_ref: self.trait_def_ref(&c.trait_key, site),
                    self_ty,
                    args,
                    where_clauses,
                    assoc,
                })
            };
            match lower() {
                Ok(decl) => match &c.decl.kind {
                    ConformanceKind::Canonical => b.impls.push(ImplDto {
                        key: c.path.name.clone(),
                        decl,
                    }),
                    ConformanceKind::Model { name, .. } => b.models.push(ModelDto {
                        name: name.clone(),
                        decl,
                    }),
                },
                Err(msg) => self.diagnostics.push(d(
                    "E0607",
                    format!("cannot lower {}: {msg}", c.path),
                    &c.decl.span,
                )),
            }
        }
        b
    }

    /// Lowers the arguments of a where-clause trait reference by the
    /// referenced trait's parameter kinds (effect rows as rows, like the
    /// arguments of a conformance head).
    fn where_args(
        &self,
        key: &TraitKey,
        args: &[Type],
        generics: &[Generic],
    ) -> Result<Vec<ArgDto>, String> {
        let params = &self.traits[key].decl.generics;
        if params.len() != args.len() {
            return Err(format!(
                "trait '{}' takes {} arguments, found {}",
                key.1,
                params.len(),
                args.len()
            ));
        }
        params
            .iter()
            .zip(args)
            .map(|(g, a)| {
                if g.row {
                    ArgDto::from_source(&row_arg(a))
                } else {
                    self.dto(a, generics).map(ArgDto::Type)
                }
            })
            .collect()
    }

    fn where_key(&self, r: &TraitRef) -> Option<TraitKey> {
        let package = match &r.package {
            Some(p) => self.dep_names.get(p)?.clone(),
            None => self.identity.clone(),
        };
        let key = (package, r.name.clone());
        self.traits.contains_key(&key).then_some(key)
    }

    /// A source type as a binding DTO (generic names become parameters).
    fn dto(&self, t: &Type, generics: &[Generic]) -> Result<TypeDto, String> {
        if let Some(p) = as_projection(t) {
            let trait_name = match p.trait_name {
                Some(n) => n.to_owned(),
                None => self.unique_assoc_trait(p.assoc)?.1,
            };
            let key = (self.identity.clone(), trait_name.clone());
            let trait_ref = if self.traits.contains_key(&key) {
                DefRefDto::local(trait_name)
            } else {
                return Err(format!("unknown trait '{trait_name}' in a projection"));
            };
            return Ok(TypeDto::Projection(Box::new(ProjectionDto {
                self_ty: self.dto(p.self_ty, generics)?,
                trait_ref,
                args: p
                    .trait_args
                    .iter()
                    .map(|a| self.dto(a, generics).map(ArgDto::Type))
                    .collect::<Result<_, _>>()?,
                brand: None,
                assoc: p.assoc.to_owned(),
            })));
        }
        match t {
            Type::Named(n, args) if args.is_empty() && generics.iter().any(|g| g.name == *n) => {
                Ok(TypeDto::Param(n.clone()))
            }
            Type::Named(n, args) => Ok(TypeDto::Def {
                def: DefRefDto::local(n.clone()),
                args: args
                    .iter()
                    .map(|a| match a {
                        Type::Row(_) => ArgDto::from_source(a),
                        _ => self.dto(a, generics).map(ArgDto::Type),
                    })
                    .collect::<Result<_, _>>()?,
            }),
            Type::Tuple(ts) => Ok(TypeDto::Tuple(
                ts.iter()
                    .map(|a| self.dto(a, generics))
                    .collect::<Result<_, _>>()?,
            )),
            other => TypeDto::from_source(other),
        }
    }

    /// The only trait (local or dependency) declaring associated type `name`.
    fn unique_assoc_trait(&self, name: &str) -> Result<TraitKey, String> {
        let found: Vec<&TraitKey> = self
            .traits
            .iter()
            .filter(|(_, t)| t.decl.assoc.iter().any(|a| a.name == name))
            .map(|(k, _)| k)
            .collect();
        match found.as_slice() {
            [k] => Ok((*k).clone()),
            [] => Err(format!("no trait declares an associated type '{name}'")),
            _ => Err(format!(
                "associated type '{name}' is declared by several traits; write <T as Trait>::{name}"
            )),
        }
    }

    fn build_session(&mut self) -> Result<CoherentSession, Vec<Diagnostic>> {
        let source = SourcePackage::new(
            self.identity.clone(),
            Arc::new(self.base_world.clone()),
            &self.base,
        )
        .map_err(|e| bridge_diagnostics(&e, &self.spans))?
        .with_fingerprint(declaration_fingerprint(&self.module));
        let mut builder = SessionBuilder::new(Arc::new(prelude_world()));
        for (p, b) in &self.deps {
            builder = builder.package(p.clone(), b.clone());
        }
        builder = builder.package(source, self.bindings.clone());
        if let Some(config) = self.solver {
            builder = builder.solver_config(config);
        }
        builder
            .build()
            .map_err(|e| bridge_diagnostics(&e, &self.spans))
    }
}

/// The first cycle among conformance `c`'s associated type bindings through
/// `Self::A` / `<Self as Trait>::A`, as the path of names that closes it.
fn assoc_cycle(tr: &TraitDecl, c: &ConfInfo) -> Option<Vec<String>> {
    fn refs(t: &Type, tr: &TraitDecl, out: &mut Vec<String>) {
        if let Some(p) = as_projection(t)
            && *p.self_ty == Type::named("Self")
            && p.trait_name.is_none_or(|n| n == tr.name)
            && tr.assoc.iter().any(|a| a.name == p.assoc)
        {
            out.push(p.assoc.to_owned());
            return;
        }
        let row = |e: &Effects, out: &mut Vec<String>| {
            for a in e.instances.iter().flat_map(|i| &i.args) {
                refs(a, tr, out);
            }
        };
        match t {
            Type::Named(_, ts) | Type::Tuple(ts) => ts.iter().for_each(|a| refs(a, tr, out)),
            Type::Function {
                params,
                result,
                effects,
                ..
            } => {
                params.iter().for_each(|(_, a)| refs(a, tr, out));
                refs(result, tr, out);
                row(effects, out);
            }
            Type::Row(e) => row(e, out),
            _ => {}
        }
    }
    fn visit(
        name: &str,
        tr: &TraitDecl,
        c: &ConfInfo,
        path: &mut Vec<String>,
        done: &mut BTreeSet<String>,
    ) -> Option<Vec<String>> {
        if let Some(i) = path.iter().position(|p| p == name) {
            let mut cycle = path[i..].to_vec();
            cycle.push(name.to_owned());
            return Some(cycle);
        }
        if !done.insert(name.to_owned()) {
            return None;
        }
        let binding = c.decl.assoc.iter().find(|a| a.name == name)?;
        let mut next = vec![];
        refs(&binding.ty, tr, &mut next);
        path.push(name.to_owned());
        for n in next {
            if let Some(cycle) = visit(&n, tr, c, path, done) {
                return Some(cycle);
            }
        }
        path.pop();
        None
    }
    let mut done = BTreeSet::new();
    c.decl
        .assoc
        .iter()
        .find_map(|a| visit(&a.name, tr, c, &mut vec![], &mut done))
}

fn row_arg(a: &Type) -> Type {
    match a {
        Type::Row(_) => a.clone(),
        Type::Named(n, args) => Type::Row(Effects {
            instances: vec![Effect {
                label: None,
                name: n.clone(),
                args: args.clone(),
            }],
            tail: None,
        }),
        Type::Tuple(ts) if ts.is_empty() => Type::Row(Effects::default()),
        other => other.clone(),
    }
}

fn same_type(a: &Type, b: &Type) -> bool {
    check::compatible(a, b) && check::compatible(b, a)
}
fn same_args(a: &[Type], b: &[Type]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| same_type(x, y))
}

fn bridge_diagnostics(e: &BridgeError, spans: &BTreeMap<DefPath, Span>) -> Vec<Diagnostic> {
    let code = match e.kind {
        ErrorKind::Incoherent => "E0605",
        ErrorKind::Unsupported => "E0607",
        ErrorKind::StaleEvidence => "E0614",
        _ => "E0601",
    };
    e.diagnostics
        .iter()
        .map(|x| {
            let span = x
                .subject
                .as_ref()
                .and_then(|p| spans.get(p))
                .cloned()
                .unwrap_or(0..0);
            let subject = x
                .subject
                .as_ref()
                .map(|p| format!(" ({p})"))
                .unwrap_or_default();
            Diagnostic::new(code, format!("{}: {}{subject}", x.code, x.message), span)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Elaboration

struct Elaborator {
    fe: Frontend,
    session: CoherentSession,
    evidence: RefCell<BTreeMap<(usize, usize), ResolvedCall>>,
    /// Projections being normalized on the current path (cycle guard).
    normalizing: RefCell<Vec<String>>,
}

/// Lexical model scopes in effect at `pos`, outermost first.
fn scopes_at(module: &Module, pos: usize) -> Vec<Vec<DefRefDto>> {
    let mut uses: Vec<&ModelUse> = module
        .model_uses
        .iter()
        .filter(|u| u.scope.start <= pos && pos < u.scope.end && u.span.end <= pos)
        .collect();
    uses.sort_by_key(|u| (u.scope.start, std::cmp::Reverse(u.scope.end), u.span.start));
    uses.into_iter()
        .map(|u| vec![DefRefDto::local(u.model.clone())])
        .collect()
}

/// Query translation context: rigid generic parameters and assumptions.
struct QueryCx {
    params: Vec<ParamDto>,
    assumptions: Vec<GoalDto>,
    infer: Vec<ParamDto>,
}

impl Elaborator {
    fn identity(&self) -> &PackageIdentity {
        &self.fe.identity
    }

    fn query_cx(&self, generics: &[Generic], function: &str) -> QueryCx {
        let mut cx = QueryCx {
            params: generics.iter().map(param_dto).collect(),
            assumptions: vec![],
            infer: vec![],
        };
        if let Some(c) = self
            .fe
            .confs
            .iter()
            .find(|c| c.local && c.generated.values().any(|g| g == function))
        {
            for w in &c.decl.where_clauses {
                if let (Some(key), Ok(self_ty)) = (
                    self.fe.where_key(&w.trait_ref),
                    self.fe.dto(&w.ty, &c.decl.generics),
                ) {
                    let Ok(args) = self
                        .fe
                        .where_args(&key, &w.trait_ref.args, &c.decl.generics)
                    else {
                        continue;
                    };
                    cx.assumptions.push(GoalDto::Implements {
                        self_ty,
                        trait_ref: self.fe.trait_def_ref(&key, self.identity()),
                        args,
                        brand: None,
                    });
                }
            }
        }
        cx
    }

    /// A checker type as a query DTO; unknown parts become inference vars.
    fn query_type(&self, t: &Type, cx: &mut QueryCx) -> Result<TypeDto, String> {
        Ok(match t {
            Type::Infer => {
                let name = format!("_infer{}", cx.infer.len());
                cx.infer.push(ParamDto::ty(name.clone(), None));
                TypeDto::Param(name)
            }
            Type::IntLiteral(_) => TypeDto::named("i64", vec![]),
            Type::Named(n, args) if args.is_empty() && cx.params.iter().any(|p| p.name == *n) => {
                TypeDto::Param(n.clone())
            }
            Type::Named(n, args) => TypeDto::Def {
                def: DefRefDto::local(n.clone()),
                args: args
                    .iter()
                    .map(|a| match a {
                        Type::Row(_) => ArgDto::from_source(a),
                        _ => self.query_type(a, cx).map(ArgDto::Type),
                    })
                    .collect::<Result<_, _>>()?,
            },
            Type::Tuple(ts) => TypeDto::Tuple(
                ts.iter()
                    .map(|a| self.query_type(a, cx))
                    .collect::<Result<_, _>>()?,
            ),
            Type::Function { .. } => {
                return Err(format!(
                    "a function value of type {t} cannot implement a trait in this fragment"
                ));
            }
            Type::Row(_) => return Err("an effect row is not a type".into()),
        })
    }

    fn query(&self, cx: QueryCx, goal: GoalDto, at: usize) -> QueryBindingDto {
        QueryBindingDto::new(
            self.identity().clone(),
            EnvDto {
                params: cx.params,
                assumptions: cx.assumptions,
                use_models: scopes_at(&self.fe.module, at),
            },
            cx.infer,
            goal,
        )
    }

    /// Solves `receiver: trait` (optionally branded) and returns the
    /// selected conformance index together with its evidence.
    #[expect(
        clippy::too_many_arguments,
        reason = "The inputs mirror the selection goal and its query context"
    )]
    fn select(
        &self,
        key: &TraitKey,
        receiver: &Type,
        brand: Option<&str>,
        trait_args: &[Type],
        generics: &[Generic],
        function: &str,
        span: &Span,
    ) -> Result<(usize, Certificate, String), (u8, String)> {
        let info = &self.fe.traits[key];
        let mut cx = self.query_cx(generics, function);
        let self_ty = self.query_type(receiver, &mut cx).map_err(|m| (0, m))?;
        let mut args = vec![];
        for (i, g) in info.decl.generics.iter().enumerate() {
            let name = format!("_arg{i}");
            // Arguments the source states constrain the goal; the rest are
            // inferred.
            if let Some(a) = trait_args.get(i) {
                args.push(if g.row {
                    ArgDto::from_source(&row_arg(a)).map_err(|m| (0, m))?
                } else {
                    ArgDto::Type(self.query_type(a, &mut cx).map_err(|m| (0, m))?)
                });
            } else if g.row {
                cx.infer.push(ParamDto::of(name.clone(), ParamKindDto::Row));
                args.push(ArgDto::Row(vesper_pact_binding::RowDto {
                    labels: vec![],
                    tail: Some(name),
                }));
            } else {
                cx.infer.push(ParamDto::ty(name.clone(), None));
                args.push(ArgDto::Type(TypeDto::Param(name)));
            }
        }
        let goal = GoalDto::Implements {
            self_ty,
            trait_ref: self.fe.trait_def_ref(key, self.identity()),
            args,
            brand: brand.map(|m| vesper_pact_binding::BrandDto::Model(DefRefDto::local(m))),
        };
        let q = self.query(cx, goal, span.start);
        let outcome = self.session.solve(&q).map_err(|e| (0, e.to_string()))?;
        self.accept(outcome)
    }

    /// Accepts only re-checked, consumed `Proven` evidence that selects an
    /// impl or model. Error tags: 0 unsupported, 1 no solution, 2
    /// ambiguous, 3 overflow, 4 dictionary (assumption/rigid).
    fn accept(&self, outcome: BridgeOutcome) -> Result<(usize, Certificate, String), (u8, String)> {
        match outcome {
            BridgeOutcome::Proven(e) => {
                let consumed = self.session.consume(&e).map_err(|e| (0, e.to_string()))?;
                let path = match &consumed.selection {
                    Selection::Impl { path, .. } | Selection::Model { path, .. } => path.clone(),
                    other => {
                        return Err((
                            4,
                            format!(
                                "the obligation is discharged by {other}, which needs generic dictionary dispatch"
                            ),
                        ));
                    }
                };
                let index = self
                    .fe
                    .confs
                    .iter()
                    .position(|c| c.path == path)
                    .ok_or((0, format!("no declaration for {path}")))?;
                Ok((index, e.certificate(), consumed.selection.to_string()))
            }
            BridgeOutcome::NoSolution { explanation, .. } => Err((1, explanation)),
            BridgeOutcome::Ambiguous { explanation, .. } => Err((2, explanation)),
            BridgeOutcome::Overflow { kind } => Err((3, format!("{kind:?}"))),
            BridgeOutcome::Unsupported { explanation, .. } => Err((0, explanation)),
        }
    }

    fn conformance_function(
        &self,
        index: usize,
        method: &str,
        span: &Span,
    ) -> Result<String, Diagnostic> {
        let c = &self.fe.confs[index];
        if !c.local {
            return Err(d(
                "E0607",
                format!(
                    "the call selects {}, an implementation in another package; cross-package dispatch is not supported",
                    c.path
                ),
                span,
            ));
        }
        c.generated.get(method).cloned().ok_or_else(|| {
            d(
                "E0610",
                format!("{} has no method '{method}'", c.path),
                span,
            )
        })
    }

    fn record(&self, span: &Span, function: &str, certificate: Certificate, selection: String) {
        self.evidence.borrow_mut().insert(
            (span.start, span.end),
            ResolvedCall {
                span: span.clone(),
                function: function.to_owned(),
                selection,
                certificate,
            },
        );
    }

    fn failure(
        &self,
        what: &str,
        receiver: &Type,
        (tag, msg): (u8, String),
        span: &Span,
    ) -> Diagnostic {
        match tag {
            1 => d(
                "E0608",
                format!("no implementation of {what} for {receiver}: {msg}"),
                span,
            ),
            2 => d(
                "E0609",
                format!(
                    "ambiguous implementation of {what} for {receiver}: {msg}{}",
                    if has_infer(receiver) {
                        "; annotate the receiver or bind it to a typed local"
                    } else {
                        ""
                    }
                ),
                span,
            ),
            3 => d(
                "E0611",
                format!("trait solving overflowed for {what} on {receiver} ({msg})"),
                span,
            ),
            4 => d(
                "E0607",
                format!("unsupported generic dictionary dispatch for {what} on {receiver}: {msg}"),
                span,
            ),
            _ => d(
                "E0607",
                format!("unsupported trait obligation {what} on {receiver}: {msg}"),
                span,
            ),
        }
    }

    // -- Projections --------------------------------------------------------

    /// Normalizes every projection in `t` (after `Self` substitution in a
    /// conformance context) to an ordinary type.
    fn normalize(
        &self,
        t: &Type,
        generics: &[Generic],
        function: &str,
        at: usize,
        span: &Span,
    ) -> Result<Type, Diagnostic> {
        if let Some(p) = as_projection(t) {
            let self_ty = self.normalize(p.self_ty, generics, function, at, span)?;
            let key = match p.trait_name {
                Some(n) => {
                    let key = (self.identity().clone(), n.to_owned());
                    if !self.fe.traits.contains_key(&key) {
                        return Err(d(
                            "E0612",
                            format!("unknown trait '{n}' in a projection"),
                            span,
                        ));
                    }
                    key
                }
                None => self
                    .fe
                    .unique_assoc_trait(p.assoc)
                    .map_err(|m| d("E0612", m, span))?,
            };
            let info = &self.fe.traits[&key];
            if !info.decl.assoc.iter().any(|a| a.name == p.assoc) {
                return Err(d(
                    "E0612",
                    format!("trait '{}' has no associated type '{}'", key.1, p.assoc),
                    span,
                ));
            }
            if !p.trait_args.is_empty() && p.trait_args.len() != info.decl.generics.len() {
                return Err(d(
                    "E0612",
                    "wrong number of trait arguments in a projection",
                    span,
                ));
            }
            let trait_args = p
                .trait_args
                .iter()
                .map(|a| self.normalize(a, generics, function, at, span))
                .collect::<Result<Vec<_>, _>>()?;
            let what = if trait_args.is_empty() {
                format!("<{self_ty} as {}>::{}", key.1, p.assoc)
            } else {
                let args: Vec<String> = trait_args.iter().map(ToString::to_string).collect();
                format!("<{self_ty} as {}[{}]>::{}", key.1, args.join(", "), p.assoc)
            };
            if self.normalizing.borrow().contains(&what) {
                return Err(d(
                    "E0612",
                    format!("projection {what} is defined in terms of itself"),
                    span,
                ));
            }
            let (index, certificate, selection) = self
                .select(&key, &self_ty, None, &trait_args, generics, function, span)
                .map_err(|(tag, m)| match tag {
                    1 | 4 => d(
                        "E0612",
                        format!("unresolved projection {what} escapes static elaboration: {m}"),
                        span,
                    ),
                    _ => self.failure(&what, &self_ty, (tag, m), span),
                })?;
            let c = &self.fe.confs[index];
            if !c.local {
                return Err(d(
                    "E0607",
                    format!(
                        "{what} is defined in another package; cross-package projections are not supported"
                    ),
                    span,
                ));
            }
            let binding = c
                .decl
                .assoc
                .iter()
                .find(|a| a.name == p.assoc)
                .map(|a| a.ty.clone())
                .ok_or_else(|| d("E0612", format!("{} binds no {}", c.path, p.assoc), span))?;
            // Instantiate the conformance's parameters from the self type.
            let mut vars = BTreeMap::new();
            if !match_type(&c.decl.self_ty, &self_ty, &c.decl.generics, &mut vars) {
                return Err(d(
                    "E0612",
                    format!("cannot instantiate {} for {self_ty}", c.path),
                    span,
                ));
            }
            vars.insert("Self".into(), self_ty.clone());
            let value = substitute(&self.fe.conform_type(&binding, c, &vars), &vars);
            self.record(span, &what, certificate, selection);
            self.normalizing.borrow_mut().push(what);
            let out = self.normalize(&value, generics, function, at, span);
            self.normalizing.borrow_mut().pop();
            return out;
        }
        Ok(match t {
            Type::Named(n, args) => Type::Named(
                n.clone(),
                args.iter()
                    .map(|a| self.normalize(a, generics, function, at, span))
                    .collect::<Result<_, _>>()?,
            ),
            Type::Tuple(ts) => Type::Tuple(
                ts.iter()
                    .map(|a| self.normalize(a, generics, function, at, span))
                    .collect::<Result<_, _>>()?,
            ),
            Type::Function {
                params,
                result,
                effects,
                mode,
            } => Type::Function {
                params: params
                    .iter()
                    .map(|(p, t)| Ok((*p, self.normalize(t, generics, function, at, span)?)))
                    .collect::<Result<_, Diagnostic>>()?,
                result: Box::new(self.normalize(result, generics, function, at, span)?),
                effects: self.normalize_effects(effects, generics, function, at, span)?,
                mode: *mode,
            },
            Type::Row(e) => Type::Row(self.normalize_effects(e, generics, function, at, span)?),
            other => other.clone(),
        })
    }

    fn normalize_effects(
        &self,
        e: &Effects,
        generics: &[Generic],
        function: &str,
        at: usize,
        span: &Span,
    ) -> Result<Effects, Diagnostic> {
        Ok(Effects {
            instances: e
                .instances
                .iter()
                .map(|i| {
                    Ok(Effect {
                        label: i.label.clone(),
                        name: i.name.clone(),
                        args: i
                            .args
                            .iter()
                            .map(|a| self.normalize(a, generics, function, at, span))
                            .collect::<Result<_, Diagnostic>>()?,
                    })
                })
                .collect::<Result<_, Diagnostic>>()?,
            tail: e.tail.clone(),
        })
    }

    /// Normalizes all types of a function in place.
    /// `outer` holds the generics of an enclosing inherent `impl`, which
    /// scope over the method but are only added to it by `World::collect`.
    fn normalize_function(
        &self,
        f: &mut Function,
        outer: &[Generic],
        errors: &mut Vec<Diagnostic>,
    ) {
        let generics: Vec<Generic> = outer.iter().chain(&f.generics).cloned().collect();
        let name = f.name.clone();
        let span = f.span.clone();
        walk_function_at(f, &span, &mut |t, at| {
            if contains_projection(t) {
                match self.normalize(t, &generics, &name, at.start, at) {
                    Ok(n) => *t = n,
                    Err(e) => errors.push(e),
                }
            }
        });
    }

    /// Generated function for method `m` of local conformance `c`.
    fn generate(&self, c: &ConfInfo, m: &Function) -> Function {
        // Only `Self` and the trait's own projections are conformance-bound
        // in implementation signatures and bodies; trait parameter names do
        // not scope there.
        let vars: BTreeMap<String, Type> = [("Self".to_owned(), c.decl.self_ty.clone())].into();
        let mut f = m.clone();
        f.name = c.generated[&m.name].clone();
        f.generics = c.decl.generics.clone();
        if let Some(p) = f.params.first_mut()
            && p.name == "self"
            && p.ty == Type::Infer
        {
            p.ty = c.decl.self_ty.clone();
        }
        walk_function(&mut f, &mut |ty| *ty = self.fe.conform_type(ty, c, &vars));
        f
    }

    fn pass1_module(&self) -> Result<Module, Vec<Diagnostic>> {
        let mut errors = vec![];
        let mut module = self.fe.base.clone();
        for item in &mut module.items {
            match item {
                Item::Function(f) => self.normalize_function(f, &[], &mut errors),
                Item::Impl {
                    methods, generics, ..
                } => {
                    for f in methods {
                        self.normalize_function(f, generics, &mut errors);
                    }
                }
                _ => {}
            }
        }
        for c in self.fe.confs.iter().filter(|c| c.local) {
            for m in &c.decl.methods {
                let mut f = self.generate(c, m);
                self.normalize_function(&mut f, &[], &mut errors);
                module.items.push(Item::Function(Box::new(f)));
            }
        }
        if errors.is_empty() {
            Ok(module)
        } else {
            Err(errors)
        }
    }

    fn metadata(&self, revision: &Revision) -> Elaboration {
        let mut e = Elaboration {
            package: Some(self.identity().to_string()),
            revision: Some(revision.to_string()),
            ..Elaboration::default()
        };
        for t in self.fe.traits.values().filter(|t| t.local) {
            e.traits.push(t.decl.name.clone());
        }
        for c in self.fe.confs.iter().filter(|c| c.local) {
            let (model, public) = match &c.decl.kind {
                ConformanceKind::Canonical => (None, true),
                ConformanceKind::Model { name, public } => (Some(name.clone()), *public),
            };
            e.conformances.push(ConformanceInfo {
                path: c.path.to_string(),
                trait_name: c.trait_key.1.clone(),
                self_ty: c.decl.self_ty.to_string(),
                model,
                public,
            });
            for (method, name) in &c.generated {
                e.generated.insert(
                    name.clone(),
                    GeneratedFunction {
                        conformance: c.path.to_string(),
                        trait_name: c.trait_key.1.clone(),
                        method: method.clone(),
                        public: false,
                    },
                );
            }
        }
        e
    }
}

impl TraitCalls for Elaborator {
    fn resolve(&self, call: &TraitCall<'_>) -> Option<Result<String, Diagnostic>> {
        let span = &call.span;
        let receiver = crate::numeric::default_type(&call.receiver);
        let (keys, method, brand): (Vec<TraitKey>, &str, Option<&str>) = if call.path {
            let (head, method) = call.callee.rsplit_once("::")?;
            let key = (self.identity().clone(), head.to_owned());
            if self.fe.traits.contains_key(&key) {
                (vec![key], method, None)
            } else {
                let c = self.fe.confs.iter().find(|c| {
                    c.local
                        && matches!(&c.decl.kind, ConformanceKind::Model { name, .. } if name == head)
                })?;
                (vec![c.trait_key.clone()], method, Some(head))
            }
        } else {
            let keys: Vec<TraitKey> = self
                .fe
                .traits
                .iter()
                .filter(|(_, t)| t.decl.methods.iter().any(|m| m.name == call.callee))
                .map(|(k, _)| k.clone())
                .collect();
            if keys.is_empty() {
                return None;
            }
            (keys, call.callee, None)
        };
        if !keys.iter().any(|k| {
            self.fe.traits[k]
                .decl
                .methods
                .iter()
                .any(|m| m.name == method)
        }) {
            return Some(Err(d(
                "E0608",
                format!("trait '{}' has no method '{method}'", keys[0].1),
                span,
            )));
        }
        let mut found = vec![];
        let mut failures = vec![];
        for key in &keys {
            match self.select(
                key,
                &receiver,
                brand,
                &[],
                call.generics,
                call.function,
                span,
            ) {
                Ok(ok) => found.push((key.clone(), ok)),
                Err(e) => failures.push((key.clone(), e)),
            }
        }
        match found.len() {
            1 => {
                let (_, (index, certificate, selection)) = found.remove(0);
                Some(
                    self.conformance_function(index, method, span)
                        .inspect(|f| self.record(span, f, certificate, selection)),
                )
            }
            0 => {
                // Report the most informative failure.
                failures.sort_by_key(|(_, (tag, _))| std::cmp::Reverse(*tag));
                let (key, err) = failures.remove(0);
                if brand.is_some() && err.0 == 1 {
                    return Some(Err(d(
                        "E0613",
                        format!(
                            "model '{}' is not a model of {} for {receiver} (wrong model brand): {}",
                            brand.unwrap_or_default(),
                            key.1,
                            err.1
                        ),
                        span,
                    )));
                }
                if err.0 == 1 && call.generics.iter().any(|g| mentions(&receiver, &g.name)) {
                    return Some(Err(d(
                        "E0607",
                        format!(
                            "unsupported generic dictionary dispatch: no static implementation of {} for generic {receiver}: {}",
                            key.1, err.1
                        ),
                        span,
                    )));
                }
                Some(Err(self.failure(&key.1, &receiver, err, span)))
            }
            _ => Some(Err(d(
                "E0609",
                format!(
                    "ambiguous method '{method}' on {receiver}: implemented through traits {}; call it as Trait::{method}(...)",
                    found
                        .iter()
                        .map(|(k, _)| k.1.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                span,
            ))),
        }
    }
}

/// First-order matching of a conformance self type against a concrete type.
fn match_type(
    pattern: &Type,
    actual: &Type,
    generics: &[Generic],
    vars: &mut BTreeMap<String, Type>,
) -> bool {
    match (pattern, actual) {
        (Type::Named(n, a), _) if a.is_empty() && generics.iter().any(|g| g.name == *n) => {
            match vars.get(n) {
                Some(bound) => same_type(bound, actual),
                None => {
                    vars.insert(n.clone(), actual.clone());
                    true
                }
            }
        }
        (Type::Named(n, a), Type::Named(m, b)) => {
            n == m
                && a.len() == b.len()
                && a.iter()
                    .zip(b)
                    .all(|(x, y)| match_type(x, y, generics, vars))
        }
        (Type::Tuple(a), Type::Tuple(b)) => {
            a.len() == b.len()
                && a.iter()
                    .zip(b)
                    .all(|(x, y)| match_type(x, y, generics, vars))
        }
        (Type::Row(_), Type::Row(_)) => true,
        _ => same_type(pattern, actual),
    }
}

// ---------------------------------------------------------------------------
// AST walkers

/// Applies `g` to every top-level type occurrence in a function.
pub(crate) fn walk_function(f: &mut Function, g: &mut dyn FnMut(&mut Type)) {
    let span = f.span.clone();
    walk_function_at(f, &span, &mut |t, _| g(t));
}

fn walk_function_at(f: &mut Function, span: &Span, g: &mut dyn FnMut(&mut Type, &Span)) {
    for p in &mut f.params {
        let s = p.span.clone();
        g(&mut p.ty, &s);
    }
    g(&mut f.result, span);
    walk_effects(&mut f.effects, span, g);
    if let Some(b) = &mut f.body {
        walk_expr(b, g);
    }
}

fn walk_effects(e: &mut Effects, span: &Span, g: &mut dyn FnMut(&mut Type, &Span)) {
    for i in &mut e.instances {
        for a in &mut i.args {
            g(a, span);
        }
    }
}

fn walk_expr(e: &mut Expr, g: &mut dyn FnMut(&mut Type, &Span)) {
    let span = e.span.clone();
    match &mut e.kind {
        ExprKind::Literal(_) | ExprKind::Name(_) => {}
        ExprKind::Tuple(es) => es.iter_mut().for_each(|e| walk_expr(e, g)),
        ExprKind::Struct { fields, .. } => fields.iter_mut().for_each(|(_, e)| walk_expr(e, g)),
        ExprKind::Block { statements, tail } => {
            for s in statements {
                match s {
                    Statement::Let { ty, value, .. } => {
                        let at = value.span.clone();
                        g(ty, &at);
                        walk_expr(value, g);
                    }
                    Statement::Expr(e) => walk_expr(e, g),
                }
            }
            if let Some(t) = tail {
                walk_expr(t, g);
            }
        }
        ExprKind::Call {
            callee,
            types,
            args,
        } => {
            walk_expr(callee, g);
            for t in types {
                g(t, &span);
            }
            for a in args {
                walk_expr(&mut a.expr, g);
            }
        }
        ExprKind::Field(b, _) | ExprKind::Try(b) | ExprKind::Unary(_, b) => walk_expr(b, g),
        ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Assign(_, a, b) => {
            walk_expr(a, g);
            walk_expr(b, g);
        }
        ExprKind::If {
            condition,
            then,
            otherwise,
        } => {
            walk_expr(condition, g);
            walk_expr(then, g);
            if let Some(o) = otherwise {
                walk_expr(o, g);
            }
        }
        ExprKind::Match { value, arms } => {
            walk_expr(value, g);
            for (_, a) in arms {
                walk_expr(a, g);
            }
        }
        ExprKind::Closure {
            params,
            result,
            effects,
            body,
            ..
        } => {
            for p in params {
                g(&mut p.ty, &span);
            }
            g(result, &span);
            walk_effects(effects, &span, g);
            walk_expr(body, g);
        }
        ExprKind::Handle {
            effect,
            state,
            body,
            ret,
            ops,
        } => {
            for a in &mut effect.args {
                g(a, &span);
            }
            if let Some((_, s)) = state {
                walk_expr(s, g);
            }
            walk_expr(body, g);
            walk_expr(&mut ret.body, g);
            for o in ops {
                walk_expr(&mut o.body, g);
            }
        }
    }
}

/// Replaces every resolved trait call by an ordinary call of its generated
/// function (receivers become the first argument).
fn rewrite_calls(mut module: Module, resolved: &BTreeMap<(usize, usize), String>) -> Module {
    let params: BTreeMap<String, Vec<Pass>> = module
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Function(f) => Some((f.name.clone(), f.params.iter().map(|p| p.pass).collect())),
            _ => None,
        })
        .collect();
    for item in &mut module.items {
        match item {
            Item::Function(f) => {
                if let Some(b) = &mut f.body {
                    rewrite_expr(b, resolved, &params);
                }
            }
            Item::Impl { methods, .. } => {
                for f in methods {
                    if let Some(b) = &mut f.body {
                        rewrite_expr(b, resolved, &params);
                    }
                }
            }
            _ => {}
        }
    }
    module.model_uses.clear();
    module.trait_bounds.clear();
    module
}

fn rewrite_expr(
    e: &mut Expr,
    resolved: &BTreeMap<(usize, usize), String>,
    params: &BTreeMap<String, Vec<Pass>>,
) {
    let key = (e.span.start, e.span.end);
    match &mut e.kind {
        ExprKind::Literal(_) | ExprKind::Name(_) => {}
        ExprKind::Tuple(es) => es
            .iter_mut()
            .for_each(|e| rewrite_expr(e, resolved, params)),
        ExprKind::Struct { fields, .. } => fields
            .iter_mut()
            .for_each(|(_, e)| rewrite_expr(e, resolved, params)),
        ExprKind::Block { statements, tail } => {
            for s in statements {
                match s {
                    Statement::Let { value, .. } => rewrite_expr(value, resolved, params),
                    Statement::Expr(e) => rewrite_expr(e, resolved, params),
                }
            }
            if let Some(t) = tail {
                rewrite_expr(t, resolved, params);
            }
        }
        ExprKind::Call { callee, args, .. } => {
            for a in args.iter_mut() {
                rewrite_expr(&mut a.expr, resolved, params);
            }
            match &mut callee.kind {
                ExprKind::Field(base, _) => rewrite_expr(base, resolved, params),
                _ => rewrite_expr(callee, resolved, params),
            }
            if let Some(name) = resolved.get(&key) {
                let callee_span = callee.span.clone();
                if let ExprKind::Field(base, _) = &callee.kind {
                    let pass = params
                        .get(name)
                        .and_then(|p| p.first().copied())
                        .unwrap_or_default();
                    args.insert(
                        0,
                        Arg {
                            pass,
                            explicit: false,
                            expr: (**base).clone(),
                        },
                    );
                }
                **callee = Expr {
                    kind: ExprKind::Name(name.clone()),
                    span: callee_span,
                };
            }
        }
        ExprKind::Field(b, _) | ExprKind::Try(b) | ExprKind::Unary(_, b) => {
            rewrite_expr(b, resolved, params)
        }
        ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Assign(_, a, b) => {
            rewrite_expr(a, resolved, params);
            rewrite_expr(b, resolved, params);
        }
        ExprKind::If {
            condition,
            then,
            otherwise,
        } => {
            rewrite_expr(condition, resolved, params);
            rewrite_expr(then, resolved, params);
            if let Some(o) = otherwise {
                rewrite_expr(o, resolved, params);
            }
        }
        ExprKind::Match { value, arms } => {
            rewrite_expr(value, resolved, params);
            for (_, a) in arms {
                rewrite_expr(a, resolved, params);
            }
        }
        ExprKind::Closure { body, .. } => rewrite_expr(body, resolved, params),
        ExprKind::Handle {
            state,
            body,
            ret,
            ops,
            ..
        } => {
            if let Some((_, s)) = state {
                rewrite_expr(s, resolved, params);
            }
            rewrite_expr(body, resolved, params);
            rewrite_expr(&mut ret.body, resolved, params);
            for o in ops {
                rewrite_expr(&mut o.body, resolved, params);
            }
        }
    }
}

/// BLAKE3 fingerprint of a module's declarations (types, effects, traits,
/// conformances and function signatures), independent of spans, comments,
/// whitespace and declaration order. Function and method bodies are not
/// part of it: evidence selects declarations, not their behaviour.
pub fn declaration_fingerprint(module: &Module) -> String {
    fn sig(f: &Function) -> serde_json::Value {
        serde_json::json!({
            "name": f.name,
            "generics": f.generics,
            "params": f.params.iter().map(|p| (&p.name, p.pass, &p.projections, &p.ty)).collect::<Vec<_>>(),
            "result": f.result,
            "effects": f.effects,
        })
    }
    let mut items: Vec<String> = module
        .items
        .iter()
        .map(|item| {
            let v = match item {
                Item::Adt(a) => {
                    serde_json::json!(["adt", a.name, a.mode, a.generics, a.variants, a.is_enum])
                }
                Item::Effect(e) => serde_json::json!([
                    "effect",
                    e.name,
                    e.linear,
                    e.generics,
                    e.ops.iter().map(sig).collect::<Vec<_>>()
                ]),
                Item::Function(f) => serde_json::json!(["fn", sig(f)]),
                Item::Impl {
                    ty,
                    generics,
                    methods,
                } => serde_json::json!([
                    "inherent",
                    ty,
                    generics,
                    methods.iter().map(sig).collect::<Vec<_>>()
                ]),
                Item::Trait(t) => serde_json::json!([
                    "trait",
                    t.name,
                    t.public,
                    t.generics,
                    t.assoc
                        .iter()
                        .map(|a| (&a.name, a.bound))
                        .collect::<Vec<_>>(),
                    t.methods.iter().map(sig).collect::<Vec<_>>()
                ]),
                Item::Conformance(c) => serde_json::json!([
                    "conformance",
                    c.kind,
                    c.generics,
                    (&c.trait_ref.package, &c.trait_ref.name, &c.trait_ref.args),
                    c.self_ty,
                    c.where_clauses
                        .iter()
                        .map(|w| (
                            &w.ty,
                            &w.trait_ref.package,
                            &w.trait_ref.name,
                            &w.trait_ref.args
                        ))
                        .collect::<Vec<_>>(),
                    c.assoc.iter().map(|a| (&a.name, &a.ty)).collect::<Vec<_>>(),
                    c.methods.iter().map(sig).collect::<Vec<_>>()
                ]),
            };
            v.to_string()
        })
        .collect();
    items.sort();
    let mut uses: Vec<&str> = module.model_uses.iter().map(|u| u.model.as_str()).collect();
    uses.sort();
    let mut h = blake3::Hasher::new();
    h.update(b"vesper.source-declarations/v1\0");
    for i in &items {
        h.update(i.as_bytes());
        h.update(b"\0");
    }
    h.update(
        serde_json::to_string(&uses)
            .expect("names serialize")
            .as_bytes(),
    );
    h.finalize().to_hex().to_string()
}

/// Re-validates serialized evidence against a session: a certificate from a
/// different declaration revision (any source or binding change) is stale.
pub fn revalidate(session: &CoherentSession, certificate: &Certificate) -> Result<(), Diagnostic> {
    session
        .accept_certificate(certificate)
        .and_then(|e| session.consume(&e))
        .map(|_| ())
        .map_err(|e| {
            let code = if e.kind == ErrorKind::StaleEvidence {
                "E0614"
            } else {
                "E0601"
            };
            Diagnostic::new(code, e.to_string(), 0..0)
        })
}

/// Schema version of the binding DTOs this frontend emits.
pub const fn binding_schema() -> vesper_pact_binding::BindingSchema {
    BINDING_SCHEMA
}
