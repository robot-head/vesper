//! Lowering of checked packages plus binding DTOs into a Pact `ProgramDto`.

use crate::binding::*;
use crate::error::{BridgeError, ErrorKind, Sink, SourceSpan};
use crate::identity::{DefKind, DefPath, PackageIdentity, valid_ident, valid_key};
use crate::modes::{derive_scheme, to_pact};
use crate::source::CheckedPackage;
use crate::tables::*;
use std::collections::{BTreeMap, BTreeSet};
use vesper::ast::{Adt, Mode, Module};
use vesper::check::World;
use vesper_pact::{
    AssocBound, AssocDecl, AssocIdx, Brand, ConformanceDecl, EffectDecl, EffectId, EffectLabel,
    EffectRow, GenericArg, Goal, ImplDecl, ImplId, ModeScheme, ModeTerm, ModelDecl, ModelId,
    PackageDecl, PackageId, ParamKind, ProgramDto, Projection, TraitDecl, TraitId, TraitRef, Ty,
    TypeCtorDecl, TypeCtorId, Var,
};

/// Largest tuple arity with a builtin constructor.
pub const MAX_TUPLE_ARITY: usize = 12;

const PRIMITIVES: [(&str, Mode); 8] = [
    ("u8", Mode::Copy),
    ("u32", Mode::Copy),
    ("u64", Mode::Copy),
    ("i64", Mode::Copy),
    ("f64", Mode::Copy),
    ("bool", Mode::Copy),
    ("Never", Mode::Copy),
    ("String", Mode::Affine),
];

/// The checker prelude, as collected by `World::collect` for every module.
pub(crate) fn prelude_world() -> World {
    World::collect(&Module::default()).0
}

/// Translation failure of one term.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Fail {
    Invalid(String),
    Unsupported(String),
}

pub(crate) type R<T> = Result<T, Fail>;

fn invalid<T>(s: impl Into<String>) -> R<T> {
    Err(Fail::Invalid(s.into()))
}
fn unsupported<T>(s: impl Into<String>) -> R<T> {
    Err(Fail::Unsupported(s.into()))
}

pub(crate) fn pact_kind(k: ParamKindDto) -> ParamKind {
    match k {
        ParamKindDto::Type => ParamKind::Type,
        ParamKindDto::Mode => ParamKind::Mode,
        ParamKindDto::Row => ParamKind::Row,
        ParamKindDto::Brand => ParamKind::Brand,
        ParamKindDto::Capability => ParamKind::Capability,
    }
}

/// One package's input.
pub(crate) struct Input<'a> {
    pub package: &'a CheckedPackage,
    pub bindings: &'a BindingsDto,
}

pub(crate) struct Lowered {
    pub dto: ProgramDto,
    pub tables: IdTables,
}

// ---------------------------------------------------------------------------
// Phase 1: identities and dense tables

struct PendingType<'a> {
    path: DefPath,
    origin: TypeOrigin,
    adt: Option<(&'a World, &'a Adt, Option<SourceSpan>)>,
    brands: Option<&'a BrandedTypeDto>,
}

pub(crate) fn lower(inputs: &[Input<'_>]) -> Result<Lowered, BridgeError> {
    let mut sink = Sink::default();
    let prelude = prelude_world();

    // Packages.
    let mut packages: Vec<PackageIdentity> = vec![PackageIdentity::builtin()];
    let mut seen_pkgs = BTreeSet::new();
    for i in inputs {
        let id = i.package.identity();
        if !seen_pkgs.insert(id.clone()) {
            sink.invalid(None, None, format!("package {id} is added twice"));
        }
        if !BINDING_SCHEMA.accepts(i.bindings.schema) {
            sink.invalid(
                None,
                None,
                format!(
                    "bindings for {id} use schema {}.{}; this bridge accepts {}.0..={}.{}",
                    i.bindings.schema.major,
                    i.bindings.schema.minor,
                    BINDING_SCHEMA.major,
                    BINDING_SCHEMA.major,
                    BINDING_SCHEMA.minor
                ),
            );
        }
        if &i.bindings.package != id {
            sink.invalid(
                None,
                None,
                format!(
                    "bindings name package {} but are attached to {id}",
                    i.bindings.package
                ),
            );
        }
        packages.push(id.clone());
    }
    if let Some(e) = sink.into_error() {
        return Err(e);
    }
    let mut sink = Sink::default();
    packages.sort();
    let package_index: BTreeMap<PackageIdentity, PackageId> = packages
        .iter()
        .enumerate()
        .map(|(i, p)| (p.clone(), PackageId(i as u32)))
        .collect();
    let builtin = PackageIdentity::builtin();

    // Types.
    let mut pending: Vec<PendingType<'_>> = vec![];
    for (name, _) in PRIMITIVES {
        pending.push(PendingType {
            path: DefPath::new(builtin.clone(), DefKind::Type, name),
            origin: TypeOrigin::Primitive,
            adt: None,
            brands: None,
        });
    }
    pending.push(PendingType {
        path: DefPath::new(builtin.clone(), DefKind::Type, "Unit"),
        origin: TypeOrigin::Unit,
        adt: None,
        brands: None,
    });
    for k in 1..=MAX_TUPLE_ARITY {
        pending.push(PendingType {
            // Not an identifier, so a tuple constructor cannot be named.
            path: DefPath::new(builtin.clone(), DefKind::Type, format!("(tuple/{k})")),
            origin: TypeOrigin::Tuple(k),
            adt: None,
            brands: None,
        });
    }
    let prelude_names: BTreeSet<String> = prelude.adts.keys().cloned().collect();
    for adt in prelude.adts.values() {
        pending.push(PendingType {
            path: DefPath::new(builtin.clone(), DefKind::Type, adt.name.clone()),
            origin: TypeOrigin::Adt,
            adt: Some((&prelude, adt, None)),
            brands: None,
        });
    }
    for i in inputs {
        let pkg = i.package.identity();
        let world = i.package.world();
        let mut branded: BTreeMap<&str, &BrandedTypeDto> = BTreeMap::new();
        for b in &i.bindings.branded_types {
            if branded.insert(b.ty.as_str(), b).is_some() {
                sink.invalid(
                    Some(DefPath::new(pkg.clone(), DefKind::Type, b.ty.clone())),
                    None,
                    "brand parameters are declared twice".into(),
                );
            }
        }
        for name in i.package.adt_names() {
            let Some(adt) = world.adts.get(name) else {
                continue;
            };
            let span = i
                .package
                .adt_span(name)
                .map(|span| SourceSpan::new(pkg.clone(), span));
            pending.push(PendingType {
                path: DefPath::new(pkg.clone(), DefKind::Type, name),
                origin: TypeOrigin::Adt,
                adt: Some((world, adt, span)),
                brands: branded.remove(name),
            });
        }
        for (name, _) in branded {
            sink.invalid(
                Some(DefPath::new(pkg.clone(), DefKind::Type, name)),
                None,
                format!("brand parameters name `{name}`, which is not a source ADT of {pkg}"),
            );
        }
        // Prelude ADTs are reserved; World::collect already rejects
        // redefinitions, so a successfully checked package cannot shadow them.
        debug_assert!(i.package.adt_names().all(|n| !prelude_names.contains(n)));
    }
    pending.sort_by(|a, b| a.path.cmp(&b.path));

    // Effects.
    let mut pending_effects: Vec<(DefPath, Vec<(String, ParamKind)>)> = vec![];
    for e in prelude.effects.values() {
        pending_effects.push((
            DefPath::new(builtin.clone(), DefKind::Effect, e.name.clone()),
            generic_kinds(&e.generics),
        ));
    }
    for i in inputs {
        let world = i.package.world();
        for name in i.package.effect_names() {
            if let Some(e) = world.effects.get(name) {
                pending_effects.push((
                    DefPath::new(i.package.identity().clone(), DefKind::Effect, name),
                    generic_kinds(&e.generics),
                ));
            }
        }
    }
    pending_effects.sort();

    // Traits, impls and models.
    let mut traits: Vec<(DefPath, &TraitDto)> = vec![];
    let mut impls: Vec<(DefPath, &ImplDto, &PackageIdentity)> = vec![];
    let mut models: Vec<(DefPath, &ModelDto, &PackageIdentity)> = vec![];
    for i in inputs {
        let pkg = i.package.identity();
        for t in &i.bindings.traits {
            let path = DefPath::new(pkg.clone(), DefKind::Trait, t.name.clone());
            if !valid_ident(&t.name) {
                sink.invalid(Some(path.clone()), None, "invalid trait name".into());
            }
            traits.push((path, t));
        }
        for im in &i.bindings.impls {
            let path = DefPath::new(pkg.clone(), DefKind::Impl, im.key.clone());
            if !valid_key(&im.key) {
                sink.invalid(Some(path.clone()), None, "invalid impl key".into());
            }
            impls.push((path, im, pkg));
        }
        for m in &i.bindings.models {
            let path = DefPath::new(pkg.clone(), DefKind::Model, m.name.clone());
            if !valid_ident(&m.name) {
                sink.invalid(Some(path.clone()), None, "invalid model name".into());
            }
            models.push((path, m, pkg));
        }
    }
    traits.sort_by(|a, b| a.0.cmp(&b.0));
    impls.sort_by(|a, b| a.0.cmp(&b.0));
    models.sort_by(|a, b| a.0.cmp(&b.0));
    for w in traits.windows(2) {
        if w[0].0 == w[1].0 {
            sink.invalid(Some(w[0].0.clone()), None, "duplicate trait".into());
        }
    }
    for w in impls.windows(2) {
        if w[0].0 == w[1].0 {
            sink.invalid(Some(w[0].0.clone()), None, "duplicate impl key".into());
        }
    }
    for w in models.windows(2) {
        if w[0].0 == w[1].0 {
            sink.invalid(Some(w[0].0.clone()), None, "duplicate model".into());
        }
    }
    if let Some(e) = sink.into_error() {
        return Err(e);
    }
    let mut sink = Sink::default();

    let mut tables = IdTables {
        packages: packages.clone(),
        package_index,
        ..IdTables::default()
    };
    for (i, (path, t)) in traits.iter().enumerate() {
        tables.trait_index.insert(path.clone(), TraitId(i as u32));
        tables.traits.push(TraitEntry {
            path: path.clone(),
            package: tables.package_index[&path.package],
            params: t
                .params
                .iter()
                .map(|p| (p.name.clone(), pact_kind(p.kind)))
                .collect(),
            assoc: t.assoc.iter().map(|a| a.name.clone()).collect(),
        });
    }
    for (i, (path, _, pkg)) in impls.iter().enumerate() {
        tables.impl_index.insert(path.clone(), ImplId(i as u32));
        tables.impls.push(ConformanceEntry {
            path: path.clone(),
            package: tables.package_index[*pkg],
            trait_id: None,
        });
    }
    for (i, (path, _, pkg)) in models.iter().enumerate() {
        tables.model_index.insert(path.clone(), ModelId(i as u32));
        tables.models.push(ConformanceEntry {
            path: path.clone(),
            package: tables.package_index[*pkg],
            trait_id: None,
        });
    }
    for (i, (path, params)) in pending_effects.iter().enumerate() {
        tables.effect_index.insert(path.clone(), EffectId(i as u32));
        tables.effects.push(EffectEntry {
            path: path.clone(),
            package: tables.package_index[&path.package],
            params: params.clone(),
        });
    }
    // Conformance traits are resolved now so brand checks can use them.
    for (idx, (_, dto, pkg)) in impls.iter().enumerate() {
        tables.impls[idx].trait_id = resolve_trait(&tables, pkg, &dto.decl.trait_ref).ok();
    }
    for (idx, (_, dto, pkg)) in models.iter().enumerate() {
        tables.models[idx].trait_id = resolve_trait(&tables, pkg, &dto.decl.trait_ref).ok();
    }

    for (i, p) in pending.iter().enumerate() {
        let id = TypeCtorId(i as u32);
        tables.type_index.insert(p.path.clone(), id);
        let package = tables.package_index[&p.path.package];
        let (params, mode) = match (&p.origin, &p.adt) {
            (TypeOrigin::Primitive, _) => {
                let m = PRIMITIVES
                    .iter()
                    .find(|(n, _)| *n == p.path.name)
                    .map(|(_, m)| *m)
                    .unwrap_or(Mode::Linear);
                (vec![], scheme(m, vec![]))
            }
            (TypeOrigin::Unit, _) => (vec![], scheme(Mode::Copy, vec![])),
            (TypeOrigin::Tuple(k), _) => (
                (0..*k)
                    .map(|j| SourceParam {
                        name: format!("{j}"),
                        kind: ParamKind::Type,
                        bound: Mode::Linear,
                    })
                    .collect(),
                // World::mode: a tuple is the join of its components.
                scheme(Mode::Copy, (0..*k as u32).collect()),
            ),
            (TypeOrigin::Adt, Some((world, adt, span))) => {
                let (world, adt): (&World, &Adt) = (world, adt);
                let params = adt
                    .generics
                    .iter()
                    .map(|g| SourceParam {
                        name: g.name.clone(),
                        kind: if g.row {
                            ParamKind::Row
                        } else {
                            ParamKind::Type
                        },
                        bound: g.bound,
                    })
                    .collect();
                let mode = match derive_scheme(world, adt) {
                    Ok(s) => s,
                    Err(msg) => {
                        sink.unsupported(Some(p.path.clone()), span.clone(), msg);
                        scheme(Mode::Linear, vec![])
                    }
                };
                (params, mode)
            }
            (TypeOrigin::Adt, None) => unreachable!("ADT entries carry their declaration"),
        };
        tables.types.push(TypeEntry {
            path: p.path.clone(),
            package,
            params,
            brands: vec![],
            mode,
            origin: p.origin.clone(),
        });
    }
    // Brand parameters (need trait ids).
    for (i, p) in pending.iter().enumerate() {
        let Some(b) = p.brands else { continue };
        let mut brands = vec![];
        let mut names: BTreeSet<&str> = tables.types[i]
            .params
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        for bp in &b.brands {
            if !valid_ident(&bp.name) || !names.insert(&bp.name) {
                sink.invalid(
                    Some(p.path.clone()),
                    None,
                    format!("brand parameter `{}` is invalid or duplicated", bp.name),
                );
                continue;
            }
            match resolve_trait(&tables, &p.path.package, &bp.trait_ref) {
                Ok(t) => brands.push((bp.name.clone(), t)),
                Err(f) => push_fail(&mut sink, Some(p.path.clone()), f),
            }
        }
        tables.types[i].brands = brands;
    }
    if let Some(e) = sink.into_error() {
        return Err(e);
    }
    let mut sink = Sink::default();

    // ------------------------------------------------------------------
    // Phase 2: declarations.
    let mut dto = ProgramDto::new();
    dto.packages = (0..tables.packages.len())
        .map(|i| PackageDecl {
            id: PackageId(i as u32),
        })
        .collect();
    dto.type_ctors = tables
        .types
        .iter()
        .enumerate()
        .map(|(i, t)| TypeCtorDecl {
            id: TypeCtorId(i as u32),
            package: t.package,
            params: t.pact_params(),
            mode: t.mode.clone(),
        })
        .collect();
    dto.effects = tables
        .effects
        .iter()
        .enumerate()
        .map(|(i, e)| EffectDecl {
            id: EffectId(i as u32),
            package: e.package,
            params: e.params.iter().map(|p| p.1).collect(),
        })
        .collect();

    for (i, (path, t)) in traits.iter().enumerate() {
        match lower_trait(&tables, &path.package, t) {
            Ok(mut d) => {
                d.id = TraitId(i as u32);
                dto.traits.push(d);
            }
            Err(f) => push_fail(&mut sink, Some(path.clone()), f),
        }
    }
    for (i, (path, im, pkg)) in impls.iter().enumerate() {
        match lower_conformance(&tables, pkg, &im.decl) {
            Ok(decl) => dto.impls.push(ImplDecl {
                id: ImplId(i as u32),
                decl,
            }),
            Err(f) => push_fail(&mut sink, Some(path.clone()), f),
        }
    }
    for (i, (path, m, pkg)) in models.iter().enumerate() {
        match lower_conformance(&tables, pkg, &m.decl) {
            Ok(decl) => dto.models.push(ModelDecl {
                id: ModelId(i as u32),
                decl,
            }),
            Err(f) => push_fail(&mut sink, Some(path.clone()), f),
        }
    }
    if let Some(e) = sink.into_error() {
        return Err(e);
    }
    Ok(Lowered { dto, tables })
}

fn scheme(base: Mode, joins: Vec<u32>) -> ModeScheme {
    ModeScheme {
        base: to_pact(base),
        joins,
    }
}

fn generic_kinds(g: &[vesper::ast::Generic]) -> Vec<(String, ParamKind)> {
    g.iter()
        .map(|g| {
            (
                g.name.clone(),
                if g.row {
                    ParamKind::Row
                } else {
                    ParamKind::Type
                },
            )
        })
        .collect()
}

pub(crate) fn push_fail(sink: &mut Sink, subject: Option<DefPath>, f: Fail) {
    match f {
        Fail::Invalid(m) => sink.invalid(subject, None, m),
        Fail::Unsupported(m) => sink.unsupported(subject, None, m),
    }
}

pub(crate) fn fail_error(
    kind_invalid: ErrorKind,
    subject: Option<DefPath>,
    f: Fail,
) -> BridgeError {
    match f {
        Fail::Invalid(m) => BridgeError::single(kind_invalid, subject, m),
        Fail::Unsupported(m) => BridgeError::single(ErrorKind::Unsupported, subject, m),
    }
}

// ---------------------------------------------------------------------------
// Name resolution

fn candidates(
    tables: &IdTables,
    site: &PackageIdentity,
    r: &DefRefDto,
    kind: DefKind,
) -> R<Vec<DefPath>> {
    match &r.package {
        Some(p) => {
            if !tables.package_index.contains_key(p) {
                return invalid(format!("unknown package {p} in reference to `{}`", r.name));
            }
            Ok(vec![DefPath::new(p.clone(), kind, r.name.clone())])
        }
        None => Ok(vec![
            DefPath::new(site.clone(), kind, r.name.clone()),
            DefPath::new(PackageIdentity::builtin(), kind, r.name.clone()),
        ]),
    }
}

fn kind_word(kind: DefKind) -> &'static str {
    match kind {
        DefKind::Type => "type",
        DefKind::Effect => "effect",
        DefKind::Trait => "trait",
        DefKind::Impl => "impl",
        DefKind::Model => "model",
    }
}

fn resolve<T: Copy>(
    index: &BTreeMap<DefPath, T>,
    tables: &IdTables,
    site: &PackageIdentity,
    r: &DefRefDto,
    kind: DefKind,
) -> R<T> {
    for c in candidates(tables, site, r, kind)? {
        if let Some(id) = index.get(&c) {
            return Ok(*id);
        }
    }
    invalid(match &r.package {
        Some(p) => format!("unknown {} `{}` in {p}", kind_word(kind), r.name),
        None => format!("unknown {} `{}` in {site}", kind_word(kind), r.name),
    })
}

pub(crate) fn resolve_trait(
    tables: &IdTables,
    site: &PackageIdentity,
    r: &DefRefDto,
) -> R<TraitId> {
    resolve(&tables.trait_index, tables, site, r, DefKind::Trait)
}
pub(crate) fn resolve_model(
    tables: &IdTables,
    site: &PackageIdentity,
    r: &DefRefDto,
) -> R<ModelId> {
    resolve(&tables.model_index, tables, site, r, DefKind::Model)
}
fn resolve_type(tables: &IdTables, site: &PackageIdentity, r: &DefRefDto) -> R<TypeCtorId> {
    if !valid_ident(&r.name) {
        // Tuple constructors are internal and reachable only as tuples.
        return invalid(format!("`{}` is not a type name", r.name));
    }
    if r.package.is_none() && r.name == "Unit" {
        return Ok(
            tables.type_index[&DefPath::new(PackageIdentity::builtin(), DefKind::Type, "Unit")]
        );
    }
    resolve(&tables.type_index, tables, site, r, DefKind::Type)
}
fn resolve_effect(tables: &IdTables, site: &PackageIdentity, r: &DefRefDto) -> R<EffectId> {
    resolve(&tables.effect_index, tables, site, r, DefKind::Effect)
}

// ---------------------------------------------------------------------------
// Term translation

#[derive(Clone, Debug)]
pub(crate) struct ParamInfo {
    pub name: String,
    pub kind: ParamKind,
    pub var: Var,
    pub bound: Option<Mode>,
    pub effect: Option<(EffectId, Vec<GenericArg>)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Position {
    /// Impl/model/trait template: absent brands are canonical.
    Template,
    /// Query environment: absent brands are canonical.
    Env,
    /// Query goal: absent brands request implicit resolution.
    Goal,
}

pub(crate) struct Cx<'t> {
    pub tables: &'t IdTables,
    pub site: PackageIdentity,
    pub params: Vec<ParamInfo>,
    pub position: Position,
    /// Kinds of query binders (named infer params, then implicit brands).
    pub binders: Vec<ParamKind>,
}

impl<'t> Cx<'t> {
    pub fn new(tables: &'t IdTables, site: PackageIdentity, position: Position) -> Self {
        Cx {
            tables,
            site,
            params: vec![],
            position,
            binders: vec![],
        }
    }

    pub fn declare(&mut self, p: &ParamDto, var: Var) -> R<()> {
        if !valid_ident(&p.name) {
            return invalid(format!("invalid parameter name {:?}", p.name));
        }
        if p.name == "Self" {
            return invalid("`Self` is reserved for a trait's self type");
        }
        if self.params.iter().any(|q| q.name == p.name) {
            return invalid(format!("parameter `{}` is declared twice", p.name));
        }
        let kind = pact_kind(p.kind);
        if p.bound.is_some() && kind != ParamKind::Type {
            return invalid(format!("modal bound on non-type parameter `{}`", p.name));
        }
        let effect = match (&p.effect, kind) {
            (None, ParamKind::Capability) => {
                return invalid(format!(
                    "capability parameter `{}` must name its effect",
                    p.name
                ));
            }
            (Some(_), k) if k != ParamKind::Capability => {
                return invalid(format!("effect on non-capability parameter `{}`", p.name));
            }
            (Some(e), _) => {
                let id = resolve_effect(self.tables, &self.site, &e.effect)?;
                Some((id, self.effect_args(id, &e.args)?))
            }
            (None, _) => None,
        };
        self.params.push(ParamInfo {
            name: p.name.clone(),
            kind,
            var,
            bound: p.bound,
            effect,
        });
        Ok(())
    }

    fn lookup(&self, name: &str) -> Option<&ParamInfo> {
        self.params.iter().find(|p| p.name == name)
    }

    fn param_var(&self, name: &str, kind: ParamKind) -> R<Var> {
        match self.lookup(name) {
            Some(p) if p.kind == kind => Ok(p.var),
            Some(p) => invalid(format!(
                "parameter `{name}` has kind {:?}, used as {kind:?}",
                p.kind
            )),
            None => invalid(format!("unknown {kind:?} parameter `{name}`")),
        }
    }

    pub fn ty(&mut self, t: &TypeDto) -> R<Ty> {
        match t {
            TypeDto::Param(n) => Ok(Ty::Var(self.param_var(n, ParamKind::Type)?)),
            TypeDto::Def { def, args } => {
                if def.package.is_none() && args.is_empty() && self.lookup(&def.name).is_some() {
                    return Ok(Ty::Var(self.param_var(&def.name, ParamKind::Type)?));
                }
                let id = resolve_type(self.tables, &self.site, def)?;
                self.ctor(id, args)
            }
            TypeDto::Tuple(ts) => {
                if ts.is_empty() {
                    let id = resolve_type(self.tables, &self.site, &DefRefDto::local("Unit"))?;
                    return Ok(Ty::Ctor {
                        ctor: id,
                        args: vec![],
                    });
                }
                if ts.len() > MAX_TUPLE_ARITY {
                    return unsupported(format!(
                        "tuple arity {} exceeds the bridge's {MAX_TUPLE_ARITY}",
                        ts.len()
                    ));
                }
                let path = DefPath::new(
                    PackageIdentity::builtin(),
                    DefKind::Type,
                    format!("(tuple/{})", ts.len()),
                );
                let id = self.tables.type_index[&path];
                let args = ts
                    .iter()
                    .map(|t| Ok(GenericArg::Ty(self.ty(t)?)))
                    .collect::<R<Vec<_>>>()?;
                Ok(Ty::Ctor { ctor: id, args })
            }
            TypeDto::Projection(p) => Ok(Ty::Projection(Box::new(self.projection(p)?))),
        }
    }

    fn ctor(&mut self, id: TypeCtorId, args: &[ArgDto]) -> R<Ty> {
        let entry = &self.tables.types[id.index()];
        let n_src = entry.params.len();
        let n_all = n_src + entry.brands.len();
        if args.len() != n_src && args.len() != n_all {
            return invalid(format!(
                "`{}` expects {n_src} argument(s){}, got {}",
                entry.path,
                if entry.brands.is_empty() {
                    String::new()
                } else {
                    format!(" (or {n_all} with brands)")
                },
                args.len()
            ));
        }
        let mut out = vec![];
        for (i, a) in args.iter().enumerate() {
            if i < n_src {
                let p = &entry.params[i];
                let arg = self.arg(a, p.kind)?;
                out.push(arg);
            } else {
                let (bname, trait_id) = entry.brands[i - n_src].clone();
                let ArgDto::Brand(b) = a else {
                    return invalid(format!(
                        "brand `{bname}` of `{}` needs a brand argument",
                        entry.path
                    ));
                };
                let brand = self.brand(b)?;
                self.check_brand_trait(&brand, trait_id, &bname)?;
                out.push(GenericArg::Brand(brand));
            }
        }
        for _ in out.len()..n_all {
            // Source occurrences default to Canonical[Trait, K].
            out.push(GenericArg::Brand(Brand::Canonical));
        }
        let ty = Ty::Ctor {
            ctor: id,
            args: out,
        };
        self.check_bounds(&ty)?;
        Ok(ty)
    }

    fn check_brand_trait(&self, b: &Brand, trait_id: TraitId, bname: &str) -> R<()> {
        if let Brand::Named(m) = b {
            let entry = &self.tables.models[m.index()];
            if entry.trait_id != Some(trait_id) {
                return invalid(format!(
                    "brand `{bname}` identifies a model of {}, but {} models another trait",
                    self.tables.traits[trait_id.index()].path,
                    entry.path
                ));
            }
        }
        Ok(())
    }

    /// Rejects instantiations whose argument mode definitely exceeds the
    /// parameter's declared bound. Unknown modes (inference variables) are
    /// re-checked on the solution; elsewhere they fail closed.
    fn check_bounds(&self, ty: &Ty) -> R<()> {
        let Ty::Ctor { ctor, args } = ty else {
            return Ok(());
        };
        let entry = &self.tables.types[ctor.index()];
        for (p, a) in entry.params.iter().zip(args) {
            if p.kind != ParamKind::Type || p.bound == Mode::Linear {
                continue;
            }
            let GenericArg::Ty(t) = a else { continue };
            match upper_mode(self.tables, t, &|v| self.var_bound(v)) {
                ModeKnowledge::Known(m) if m > p.bound => {
                    return invalid(format!(
                        "argument of mode {m:?} for parameter `{}: mod {:?}` of `{}`",
                        p.name, p.bound, entry.path
                    ));
                }
                ModeKnowledge::Known(_) => {}
                ModeKnowledge::Unknown if self.position == Position::Goal => {}
                ModeKnowledge::Unknown => {
                    return unsupported(format!(
                        "cannot establish the bound `{}: mod {:?}` of `{}` for a projection argument",
                        p.name, p.bound, entry.path
                    ));
                }
            }
        }
        Ok(())
    }

    fn var_bound(&self, v: Var) -> Option<Mode> {
        if let Var::Infer(_) = v {
            return None;
        }
        Some(
            self.params
                .iter()
                .find(|p| p.var == v && p.kind == ParamKind::Type)
                .and_then(|p| p.bound)
                .unwrap_or(Mode::Linear),
        )
    }

    pub fn arg(&mut self, a: &ArgDto, kind: ParamKind) -> R<GenericArg> {
        let got = match a {
            ArgDto::Type(t) => {
                // A bare name may denote a row/brand/mode parameter.
                if let TypeDto::Def { def, args } = t
                    && def.package.is_none()
                    && args.is_empty()
                    && kind != ParamKind::Type
                    && let Some(p) = self.lookup(&def.name)
                    && p.kind == kind
                {
                    return Ok(GenericArg::var(kind, p.var));
                }
                GenericArg::Ty(self.ty(t)?)
            }
            ArgDto::Mode(m) => GenericArg::Mode(self.mode(m)?),
            ArgDto::Row(r) => GenericArg::Row(self.row(r)?),
            ArgDto::Brand(b) => GenericArg::Brand(self.brand(b)?),
            ArgDto::Capability(c) => {
                GenericArg::Capability(self.param_var(c, ParamKind::Capability)?)
            }
        };
        if got.kind() != kind {
            return invalid(format!(
                "expected a {kind:?} argument, got {:?}",
                got.kind()
            ));
        }
        Ok(got)
    }

    fn args_for(&mut self, kinds: &[ParamKind], args: &[ArgDto], what: &str) -> R<Vec<GenericArg>> {
        if kinds.len() != args.len() {
            return invalid(format!(
                "{what} expects {} argument(s), got {}",
                kinds.len(),
                args.len()
            ));
        }
        kinds
            .iter()
            .zip(args)
            .map(|(k, a)| self.arg(a, *k))
            .collect()
    }

    fn effect_args(&mut self, id: EffectId, args: &[ArgDto]) -> R<Vec<GenericArg>> {
        let entry = &self.tables.effects[id.index()];
        let kinds: Vec<ParamKind> = entry.params.iter().map(|p| p.1).collect();
        let what = format!("effect `{}`", entry.path);
        self.args_for(&kinds, args, &what)
    }

    pub fn mode(&mut self, m: &ModeDto) -> R<ModeTerm> {
        Ok(match m {
            ModeDto::Const(c) => ModeTerm::Const(to_pact(*c)),
            ModeDto::Param(n) => ModeTerm::Var(self.param_var(n, ParamKind::Mode)?),
        })
    }

    pub fn row(&mut self, r: &RowDto) -> R<EffectRow> {
        let mut labels = vec![];
        for l in &r.labels {
            let effect = resolve_effect(self.tables, &self.site, &l.effect)?;
            let args = self.effect_args(effect, &l.args)?;
            match &l.capability {
                None => labels.push(EffectLabel::Effect { effect, args }),
                Some(c) => {
                    let var = self.param_var(c, ParamKind::Capability)?;
                    let declared = self
                        .params
                        .iter()
                        .find(|p| p.var == var)
                        .and_then(|p| p.effect.clone());
                    if declared != Some((effect, args)) {
                        return invalid(format!(
                            "capability `{c}` is used with a different effect than it labels"
                        ));
                    }
                    labels.push(EffectLabel::Capability(var));
                }
            }
        }
        let tail = match &r.tail {
            Some(t) => Some(self.param_var(t, ParamKind::Row)?),
            None => None,
        };
        let row = EffectRow { labels, tail };
        let mut sorted = row.clone();
        sorted.labels.sort_by_key(|a| a.key());
        if let Some(k) = sorted.duplicate_key() {
            let what = match k {
                vesper_pact::LabelKey::Effect(e) => self.tables.effects[e.index()].path.to_string(),
                vesper_pact::LabelKey::Capability(_) => "a capability label".into(),
            };
            return unsupported(format!(
                "effect row repeats {what}; duplicate labels are outside Pact's fragment"
            ));
        }
        Ok(match sorted.tail {
            Some(t) => EffectRow::open(sorted.labels, t),
            None => EffectRow::closed(sorted.labels),
        })
    }

    pub fn brand(&mut self, b: &BrandDto) -> R<Brand> {
        Ok(match b {
            BrandDto::Canonical => Brand::Canonical,
            BrandDto::Model(m) => Brand::Named(resolve_model(self.tables, &self.site, m)?),
            BrandDto::Param(n) => Brand::Var(self.param_var(n, ParamKind::Brand)?),
        })
    }

    fn opt_brand(&mut self, b: &Option<BrandDto>) -> R<Brand> {
        match b {
            Some(b) => self.brand(b),
            None if self.position == Position::Goal => {
                let i = self.binders.len() as u32;
                self.binders.push(ParamKind::Brand);
                Ok(Brand::Var(Var::Infer(i)))
            }
            None => Ok(Brand::Canonical),
        }
    }

    pub fn trait_ref(&mut self, self_ty: &TypeDto, t: &DefRefDto, args: &[ArgDto]) -> R<TraitRef> {
        let trait_id = resolve_trait(self.tables, &self.site, t)?;
        let self_ty = self.ty(self_ty)?;
        let entry = &self.tables.traits[trait_id.index()];
        let kinds: Vec<ParamKind> = entry.params.iter().map(|p| p.1).collect();
        let what = format!("{}", entry.path);
        let args = self.args_for(&kinds, args, &what)?;
        Ok(TraitRef {
            trait_id,
            self_ty,
            args,
        })
    }

    fn projection(&mut self, p: &ProjectionDto) -> R<Projection> {
        let trait_ref = self.trait_ref(&p.self_ty, &p.trait_ref, &p.args)?;
        let brand = self.opt_brand(&p.brand)?;
        let assoc = self.assoc_idx(trait_ref.trait_id, &p.assoc)?;
        Ok(Projection {
            trait_ref,
            brand,
            assoc,
        })
    }

    fn assoc_idx(&self, t: TraitId, name: &str) -> R<AssocIdx> {
        let entry = &self.tables.traits[t.index()];
        match entry.assoc.iter().position(|a| a == name) {
            Some(i) => Ok(AssocIdx(i as u32)),
            None => invalid(format!("{} has no associated type `{name}`", entry.path)),
        }
    }

    pub fn goal(&mut self, g: &GoalDto) -> R<Goal> {
        Ok(match g {
            GoalDto::Implements {
                self_ty,
                trait_ref,
                args,
                brand,
            } => {
                let trait_ref = self.trait_ref(self_ty, trait_ref, args)?;
                let brand = self.opt_brand(brand)?;
                Goal::Implements { trait_ref, brand }
            }
            GoalDto::ProjectionEq { projection, ty } => Goal::ProjectionEq {
                projection: self.projection(projection)?,
                ty: self.ty(ty)?,
            },
            GoalDto::ModeBound { ty, bound } => Goal::ModeBound {
                ty: self.ty(ty)?,
                bound: self.mode(bound)?,
            },
        })
    }
}

// ---------------------------------------------------------------------------
// Modal upper bounds of translated types

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModeKnowledge {
    Known(Mode),
    Unknown,
}

/// Mode of `ty` from constructor schemes and variable bounds; projections and
/// inference variables are unknown. Out-of-range ids are unknown (callers
/// validate certificates before trusting this).
pub(crate) fn upper_mode(
    tables: &IdTables,
    ty: &Ty,
    var: &dyn Fn(Var) -> Option<Mode>,
) -> ModeKnowledge {
    match ty {
        Ty::Ctor { ctor, args } => {
            let Some(e) = tables.types.get(ctor.index()) else {
                return ModeKnowledge::Unknown;
            };
            let mut m = crate::modes::from_pact(e.mode.base);
            for &j in &e.mode.joins {
                match args.get(j as usize) {
                    Some(GenericArg::Ty(t)) => match upper_mode(tables, t, var) {
                        ModeKnowledge::Known(x) => m = m.join(x),
                        ModeKnowledge::Unknown => return ModeKnowledge::Unknown,
                    },
                    _ => return ModeKnowledge::Unknown,
                }
            }
            ModeKnowledge::Known(m)
        }
        Ty::Var(v) => match var(*v) {
            Some(m) => ModeKnowledge::Known(m),
            None => ModeKnowledge::Unknown,
        },
        Ty::Projection(_) => ModeKnowledge::Unknown,
    }
}

/// Checks every constructor application in `goal` against declared bounds.
/// Unknown modes count as violations (used on solutions and certificates).
pub(crate) fn well_formed_goal(
    tables: &IdTables,
    goal: &Goal,
    var: &dyn Fn(Var) -> Option<Mode>,
) -> Result<(), String> {
    let mut err = None;
    let mut check = |t: &Ty| {
        if err.is_some() {
            return;
        }
        if let Err(e) = well_formed_ty(tables, t, var) {
            err = Some(e);
        }
    };
    match goal {
        Goal::Implements { trait_ref, .. } => {
            check(&trait_ref.self_ty);
            for a in &trait_ref.args {
                if let GenericArg::Ty(t) = a {
                    check(t);
                }
            }
        }
        Goal::ProjectionEq { projection, ty } => {
            check(&projection.trait_ref.self_ty);
            check(ty);
        }
        Goal::ModeBound { ty, .. } => check(ty),
    }
    err.map_or(Ok(()), Err)
}

fn well_formed_ty(
    tables: &IdTables,
    ty: &Ty,
    var: &dyn Fn(Var) -> Option<Mode>,
) -> Result<(), String> {
    match ty {
        Ty::Ctor { ctor, args } => {
            let Some(e) = tables.types.get(ctor.index()) else {
                return Err(format!("unknown {ctor}"));
            };
            for (i, a) in args.iter().enumerate() {
                if let GenericArg::Ty(t) = a {
                    well_formed_ty(tables, t, var)?;
                    if let Some(p) = e.params.get(i)
                        && p.bound != Mode::Linear
                    {
                        match upper_mode(tables, t, var) {
                            ModeKnowledge::Known(m) if m <= p.bound => {}
                            k => {
                                return Err(format!(
                                    "argument {} for parameter `{}: mod {:?}` of `{}`",
                                    match k {
                                        ModeKnowledge::Known(m) => format!("of mode {m:?}"),
                                        ModeKnowledge::Unknown => "of unknown mode".into(),
                                    },
                                    p.name,
                                    p.bound,
                                    e.path
                                ));
                            }
                        }
                    }
                }
            }
            Ok(())
        }
        Ty::Var(_) => Ok(()),
        Ty::Projection(p) => well_formed_ty(tables, &p.trait_ref.self_ty, var),
    }
}

// ---------------------------------------------------------------------------
// Declarations

fn lower_trait(tables: &IdTables, site: &PackageIdentity, t: &TraitDto) -> R<TraitDecl> {
    let mut cx = Cx::new(tables, site.clone(), Position::Template);
    cx.params.push(ParamInfo {
        name: "Self".into(),
        kind: ParamKind::Type,
        var: Var::Bound(0),
        bound: None,
        effect: None,
    });
    for (i, p) in t.params.iter().enumerate() {
        if p.bound.is_some() {
            return unsupported(format!(
                "bound on trait parameter `{}`: Pact trait parameters carry kinds only",
                p.name
            ));
        }
        if p.kind == ParamKindDto::Capability {
            return unsupported("capability parameters on traits are outside Pact's fragment");
        }
        cx.declare(p, Var::Bound(i as u32 + 1))?;
    }
    let mut names = BTreeSet::new();
    let mut assoc = vec![];
    for a in &t.assoc {
        if !valid_ident(&a.name) || !names.insert(&a.name) {
            return invalid(format!(
                "associated type `{}` is invalid or duplicated",
                a.name
            ));
        }
        let mode_bound = cx.mode(&a.mode_bound)?;
        let mut bounds = vec![];
        for b in &a.bounds {
            let trait_id = resolve_trait(tables, site, &b.trait_ref)?;
            let entry = &tables.traits[trait_id.index()];
            let kinds: Vec<ParamKind> = entry.params.iter().map(|p| p.1).collect();
            let what = format!("{}", entry.path);
            let args = cx.args_for(&kinds, &b.args, &what)?;
            let brand = cx.opt_brand(&b.brand)?;
            bounds.push(AssocBound {
                trait_id,
                args,
                brand,
            });
        }
        assoc.push(AssocDecl { mode_bound, bounds });
    }
    Ok(TraitDecl {
        id: TraitId(0),
        package: tables.package_index[site],
        params: t.params.iter().map(|p| pact_kind(p.kind)).collect(),
        assoc,
        coinductive: t.coinductive,
    })
}

fn lower_conformance(
    tables: &IdTables,
    site: &PackageIdentity,
    c: &ConformanceDto,
) -> R<ConformanceDecl> {
    let mut cx = Cx::new(tables, site.clone(), Position::Template);
    let mut where_clauses = vec![];
    for (i, p) in c.params.iter().enumerate() {
        if p.kind == ParamKindDto::Capability {
            return unsupported(
                "capability parameters on impls/models are outside Pact's fragment",
            );
        }
        cx.declare(p, Var::Bound(i as u32))?;
        if let Some(b) = p.bound
            && b != Mode::Linear
        {
            // Pact assumes exactly these mode where-clauses for well-formedness.
            where_clauses.push(Goal::ModeBound {
                ty: Ty::Var(Var::Bound(i as u32)),
                bound: ModeTerm::Const(to_pact(b)),
            });
        }
    }
    let trait_ref = cx.trait_ref(&c.self_ty, &c.trait_ref, &c.args)?;
    for w in &c.where_clauses {
        where_clauses.push(cx.goal(w)?);
    }
    let entry = &tables.traits[trait_ref.trait_id.index()];
    let mut values: Vec<Option<Ty>> = vec![None; entry.assoc.len()];
    for (name, ty) in &c.assoc {
        let Some(i) = entry.assoc.iter().position(|a| a == name) else {
            return invalid(format!("{} has no associated type `{name}`", entry.path));
        };
        if values[i].is_some() {
            return invalid(format!("associated type `{name}` is given twice"));
        }
        values[i] = Some(cx.ty(ty)?);
    }
    let mut assoc_values = vec![];
    for (i, v) in values.into_iter().enumerate() {
        match v {
            Some(v) => assoc_values.push(v),
            None => {
                return invalid(format!("missing associated type `{}`", entry.assoc[i]));
            }
        }
    }
    Ok(ConformanceDecl {
        package: tables.package_index[site],
        params: c.params.iter().map(|p| pact_kind(p.kind)).collect(),
        trait_ref,
        where_clauses,
        assoc_values,
    })
}
