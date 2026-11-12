//! Versioned declaration DTOs, query environments and their validation.

use crate::diag::{DiagCode, Diagnostic, Subject};
use crate::ids::*;
use crate::term::*;
use serde::{Deserialize, Serialize};

/// Version of the Pact boundary schema. A consumer accepts a DTO whose major
/// version matches and whose minor version is not newer than its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SchemaVersion {
    pub major: u16,
    pub minor: u16,
}

pub const SCHEMA_VERSION: SchemaVersion = SchemaVersion { major: 0, minor: 1 };

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageDecl {
    pub id: PackageId,
}

/// Modal kind of a constructor application: `base ⊔ mode(args[i]) for i in joins`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeScheme {
    pub base: Mode,
    /// Indices of type parameters whose modes are joined into the result.
    pub joins: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeCtorDecl {
    pub id: TypeCtorId,
    pub package: PackageId,
    pub params: Vec<ParamKind>,
    pub mode: ModeScheme,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectDecl {
    pub id: EffectId,
    pub package: PackageId,
    pub params: Vec<ParamKind>,
}

/// A trait bound on an associated type; its self type is the projection.
/// Templates bind `^0 = Self` and `^(i+1) = trait parameter i`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssocBound {
    pub trait_id: TraitId,
    pub args: Vec<GenericArg>,
    pub brand: Brand,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssocDecl {
    /// Upper modal bound of the associated type (`type Item: mod linear`).
    pub mode_bound: ModeTerm,
    pub bounds: Vec<AssocBound>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraitDecl {
    pub id: TraitId,
    pub package: PackageId,
    /// Parameters after `Self` (types, modes, effect rows, brands).
    pub params: Vec<ParamKind>,
    pub assoc: Vec<AssocDecl>,
    /// Cycles consisting solely of goals of coinductive traits hold by
    /// coinduction (greatest fixed point); all other cycles are inductive.
    pub coinductive: bool,
}

/// Shared shape of canonical impls and named models. All terms are templates
/// over `params` (`Var::Bound`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConformanceDecl {
    pub package: PackageId,
    pub params: Vec<ParamKind>,
    pub trait_ref: TraitRef,
    pub where_clauses: Vec<Goal>,
    /// One value per associated type of the trait, in declaration order.
    pub assoc_values: Vec<Ty>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImplDecl {
    pub id: ImplId,
    #[serde(flatten)]
    pub decl: ConformanceDecl,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelDecl {
    pub id: ModelId,
    #[serde(flatten)]
    pub decl: ConformanceDecl,
}

/// The complete declaration set handed over by the compiler adapter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramDto {
    pub schema: SchemaVersion,
    pub packages: Vec<PackageDecl>,
    pub type_ctors: Vec<TypeCtorDecl>,
    pub effects: Vec<EffectDecl>,
    pub traits: Vec<TraitDecl>,
    pub impls: Vec<ImplDecl>,
    pub models: Vec<ModelDecl>,
}

impl ProgramDto {
    pub fn new() -> Self {
        ProgramDto {
            schema: SCHEMA_VERSION,
            packages: vec![],
            type_ctors: vec![],
            effects: vec![],
            traits: vec![],
            impls: vec![],
            models: vec![],
        }
    }
}

impl Default for ProgramDto {
    fn default() -> Self {
        Self::new()
    }
}

/// A validated program. Construct with [`Program::new`].
#[derive(Clone, Debug)]
pub struct Program {
    dto: ProgramDto,
    impls_by_trait: Vec<Vec<ImplId>>,
}

impl Program {
    /// Validates ids, arities, variable scoping and fragment restrictions.
    /// Diagnostics are sorted and deterministic.
    pub fn new(dto: ProgramDto) -> Result<Program, Vec<Diagnostic>> {
        let mut diags = Vec::new();
        Validator {
            dto: &dto,
            diags: &mut diags,
        }
        .run();
        if !diags.is_empty() {
            diags.sort();
            diags.dedup();
            return Err(diags);
        }
        let mut impls_by_trait = vec![Vec::new(); dto.traits.len()];
        for i in &dto.impls {
            impls_by_trait[i.decl.trait_ref.trait_id.index()].push(i.id);
        }
        Ok(Program {
            dto,
            impls_by_trait,
        })
    }
    pub fn dto(&self) -> &ProgramDto {
        &self.dto
    }
    pub fn trait_decl(&self, id: TraitId) -> &TraitDecl {
        &self.dto.traits[id.index()]
    }
    pub fn ctor(&self, id: TypeCtorId) -> &TypeCtorDecl {
        &self.dto.type_ctors[id.index()]
    }
    pub fn impl_decl(&self, id: ImplId) -> &ImplDecl {
        &self.dto.impls[id.index()]
    }
    pub fn model(&self, id: ModelId) -> &ModelDecl {
        &self.dto.models[id.index()]
    }
    pub fn impls_of(&self, t: TraitId) -> &[ImplId] {
        &self.impls_by_trait[t.index()]
    }
    pub fn model_count(&self) -> usize {
        self.dto.models.len()
    }
}

/// Rigid parameter of a query environment.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PlaceholderDecl {
    pub kind: ParamKind,
    /// Declared modal bound for type placeholders (`T: mod copy`). `None`
    /// means unconstrained, i.e. `linear`.
    pub mode_bound: Option<Mode>,
}

impl PlaceholderDecl {
    pub fn ty(mode_bound: Option<Mode>) -> Self {
        PlaceholderDecl {
            kind: ParamKind::Type,
            mode_bound,
        }
    }
    pub fn of(kind: ParamKind) -> Self {
        PlaceholderDecl {
            kind,
            mode_bound: None,
        }
    }
}

/// The parameter environment of a query: generic parameters in scope, their
/// where-clauses, and lexically bound named models.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Env {
    pub placeholders: Vec<PlaceholderDecl>,
    /// `Implements` and `ProjectionEq` goals assumed to hold.
    pub assumptions: Vec<Goal>,
    /// `use model M;` scopes, outermost first.
    pub model_scopes: Vec<Vec<ModelId>>,
}

/// A trait query from the compiler: existential binders and one goal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryDto {
    pub schema: SchemaVersion,
    pub env: Env,
    pub binders: Vec<ParamKind>,
    pub goal: Goal,
}

impl QueryDto {
    pub fn new(env: Env, binders: Vec<ParamKind>, goal: Goal) -> Self {
        QueryDto {
            schema: SCHEMA_VERSION,
            env,
            binders,
            goal,
        }
    }
}

pub(crate) fn check_schema(v: SchemaVersion) -> Option<Diagnostic> {
    if v.major != SCHEMA_VERSION.major || v.minor > SCHEMA_VERSION.minor {
        Some(Diagnostic::new(
            DiagCode::SchemaVersion,
            Subject::Program,
            format!(
                "unsupported schema {}.{} (this solver accepts {}.0..={}.{})",
                v.major, v.minor, SCHEMA_VERSION.major, SCHEMA_VERSION.major, SCHEMA_VERSION.minor
            ),
        ))
    } else {
        None
    }
}

/// Which variable forms a term may contain.
#[derive(Clone, Copy)]
pub(crate) enum Scope<'a> {
    Template(&'a [ParamKind]),
    Query {
        placeholders: &'a [PlaceholderDecl],
        binders: &'a [ParamKind],
    },
}

pub(crate) struct Validator<'a> {
    pub dto: &'a ProgramDto,
    pub diags: &'a mut Vec<Diagnostic>,
}

impl Validator<'_> {
    fn err(&mut self, code: DiagCode, subject: Subject, msg: String) {
        self.diags.push(Diagnostic::new(code, subject, msg));
    }

    fn run(&mut self) {
        let dto = self.dto;
        if let Some(d) = check_schema(dto.schema) {
            self.diags.push(d);
            return;
        }
        for (i, p) in dto.packages.iter().enumerate() {
            if p.id.index() != i {
                self.err(
                    DiagCode::IdNotDense,
                    Subject::Program,
                    format!("package at {i} has id {}", p.id),
                );
            }
        }
        for (i, c) in dto.type_ctors.iter().enumerate() {
            if c.id.index() != i {
                self.err(
                    DiagCode::IdNotDense,
                    Subject::Program,
                    format!("type ctor at {i} has id {}", c.id),
                );
            }
            self.package(c.package, Subject::TypeCtor(c.id));
            for &j in &c.mode.joins {
                if c.params.get(j as usize) != Some(&ParamKind::Type) {
                    self.err(
                        DiagCode::Malformed,
                        Subject::TypeCtor(c.id),
                        format!("mode join index {j} is not a type parameter"),
                    );
                }
            }
        }
        for (i, e) in dto.effects.iter().enumerate() {
            if e.id.index() != i {
                self.err(
                    DiagCode::IdNotDense,
                    Subject::Program,
                    format!("effect at {i} has id {}", e.id),
                );
            }
            self.package(e.package, Subject::Effect(e.id));
        }
        for (i, t) in dto.traits.iter().enumerate() {
            if t.id.index() != i {
                self.err(
                    DiagCode::IdNotDense,
                    Subject::Program,
                    format!("trait at {i} has id {}", t.id),
                );
            }
            self.package(t.package, Subject::Trait(t.id));
            let mut binder = vec![ParamKind::Type];
            binder.extend(&t.params);
            let scope = Scope::Template(&binder);
            for (ai, a) in t.assoc.iter().enumerate() {
                let subj = Subject::Trait(t.id);
                self.mode(&a.mode_bound, scope, subj);
                for b in &a.bounds {
                    if !self.trait_args(b.trait_id, &b.args, scope, subj) {
                        continue;
                    }
                    self.brand(&b.brand, scope, subj);
                    if matches!(b.brand, Brand::Var(_)) {
                        self.err(
                            DiagCode::Unsupported,
                            subj,
                            format!(
                                "bound on {} must name an explicit brand",
                                AssocIdx(ai as u32)
                            ),
                        );
                    }
                }
            }
        }
        for (i, im) in dto.impls.iter().enumerate() {
            if im.id.index() != i {
                self.err(
                    DiagCode::IdNotDense,
                    Subject::Program,
                    format!("impl at {i} has id {}", im.id),
                );
            }
            self.conformance(&im.decl, Subject::Impl(im.id));
        }
        for (i, m) in dto.models.iter().enumerate() {
            if m.id.index() != i {
                self.err(
                    DiagCode::IdNotDense,
                    Subject::Program,
                    format!("model at {i} has id {}", m.id),
                );
            }
            self.conformance(&m.decl, Subject::Model(m.id));
        }
    }

    fn package(&mut self, p: PackageId, subj: Subject) {
        if p.index() >= self.dto.packages.len() {
            self.err(DiagCode::UnknownId, subj, format!("unknown {p}"));
        }
    }

    fn conformance(&mut self, c: &ConformanceDecl, subj: Subject) {
        self.package(c.package, subj);
        let scope = Scope::Template(&c.params);
        if !self.trait_ref(&c.trait_ref, scope, subj) {
            return;
        }
        if trait_ref_has_projection(&c.trait_ref) {
            self.err(
                DiagCode::Unsupported,
                subj,
                "associated-type projections in an impl/model head are outside this fragment"
                    .into(),
            );
        }
        for w in &c.where_clauses {
            // Template scope rejects inference brands, so where-clauses always
            // name canonical, named or parameter brands explicitly: lexical
            // `use model` scopes of a use site never leak into impl bodies.
            self.goal(w, scope, subj);
        }
        let t = &self.dto.traits[c.trait_ref.trait_id.index()];
        if c.assoc_values.len() != t.assoc.len() {
            self.err(
                DiagCode::Arity,
                subj,
                format!(
                    "{} associated values supplied, {} declares {}",
                    c.assoc_values.len(),
                    t.id,
                    t.assoc.len()
                ),
            );
        }
        for v in &c.assoc_values {
            self.ty(v, scope, subj);
        }
        // Every parameter must be determined by the head (no unconstrained
        // impl parameters), otherwise a selected impl would leave free choices.
        let mut seen = vec![false; c.params.len()];
        let head = Goal::Implements {
            trait_ref: c.trait_ref.clone(),
            brand: Brand::Canonical,
        };
        visit_goal_vars(&head, |v, _| {
            if let Var::Bound(i) = v
                && let Some(s) = seen.get_mut(i as usize)
            {
                *s = true;
            }
        });
        for (i, s) in seen.iter().enumerate() {
            if !s {
                self.err(
                    DiagCode::UnconstrainedParam,
                    subj,
                    format!("parameter ^{i} does not occur in the head"),
                );
            }
        }
    }

    fn trait_ref(&mut self, t: &TraitRef, scope: Scope, subj: Subject) -> bool {
        self.ty(&t.self_ty, scope, subj);
        self.trait_args(t.trait_id, &t.args, scope, subj)
    }

    fn trait_args(
        &mut self,
        id: TraitId,
        args: &[GenericArg],
        scope: Scope,
        subj: Subject,
    ) -> bool {
        let Some(t) = self.dto.traits.get(id.index()) else {
            self.err(DiagCode::UnknownId, subj, format!("unknown {id}"));
            return false;
        };
        let params = t.params.clone();
        self.args(&params, args, scope, subj, &format!("{id}"))
    }

    fn args(
        &mut self,
        params: &[ParamKind],
        args: &[GenericArg],
        scope: Scope,
        subj: Subject,
        what: &str,
    ) -> bool {
        if params.len() != args.len() {
            self.err(
                DiagCode::Arity,
                subj,
                format!(
                    "{what} expects {} arguments, got {}",
                    params.len(),
                    args.len()
                ),
            );
            return false;
        }
        let mut ok = true;
        for (p, a) in params.iter().zip(args) {
            if a.kind() != *p {
                self.err(
                    DiagCode::KindMismatch,
                    subj,
                    format!("{what} expects a {p:?} argument, got {a}"),
                );
                ok = false;
            } else {
                self.arg(a, scope, subj);
            }
        }
        ok
    }

    pub(crate) fn goal(&mut self, g: &Goal, scope: Scope, subj: Subject) {
        match g {
            Goal::Implements { trait_ref, brand } => {
                self.trait_ref(trait_ref, scope, subj);
                self.brand(brand, scope, subj);
            }
            Goal::ProjectionEq { projection, ty } => {
                self.projection(projection, scope, subj);
                self.ty(ty, scope, subj);
            }
            Goal::ModeBound { ty, bound } => {
                self.ty(ty, scope, subj);
                self.mode(bound, scope, subj);
            }
        }
    }

    fn projection(&mut self, p: &Projection, scope: Scope, subj: Subject) {
        if self.trait_ref(&p.trait_ref, scope, subj) {
            let t = &self.dto.traits[p.trait_ref.trait_id.index()];
            if p.assoc.0 as usize >= t.assoc.len() {
                self.err(
                    DiagCode::UnknownId,
                    subj,
                    format!("{} has no {}", t.id, p.assoc),
                );
            }
        }
        self.brand(&p.brand, scope, subj);
    }

    fn ty(&mut self, t: &Ty, scope: Scope, subj: Subject) {
        match t {
            Ty::Ctor { ctor, args } => {
                let Some(c) = self.dto.type_ctors.get(ctor.index()) else {
                    self.err(DiagCode::UnknownId, subj, format!("unknown {ctor}"));
                    return;
                };
                let params = c.params.clone();
                self.args(&params, args, scope, subj, &format!("{ctor}"));
            }
            Ty::Var(v) => self.var(*v, ParamKind::Type, scope, subj),
            Ty::Projection(p) => self.projection(p, scope, subj),
        }
    }

    fn arg(&mut self, a: &GenericArg, scope: Scope, subj: Subject) {
        match a {
            GenericArg::Ty(t) => self.ty(t, scope, subj),
            GenericArg::Mode(m) => self.mode(m, scope, subj),
            GenericArg::Row(r) => self.row(r, scope, subj),
            GenericArg::Brand(b) => self.brand(b, scope, subj),
            GenericArg::Capability(v) => self.cap(*v, scope, subj),
        }
    }

    fn mode(&mut self, m: &ModeTerm, scope: Scope, subj: Subject) {
        if let ModeTerm::Var(v) = m {
            self.var(*v, ParamKind::Mode, scope, subj);
        }
    }

    fn brand(&mut self, b: &Brand, scope: Scope, subj: Subject) {
        match b {
            Brand::Var(v) => self.var(*v, ParamKind::Brand, scope, subj),
            Brand::Named(m) => {
                if m.index() >= self.dto.models.len() {
                    self.err(DiagCode::UnknownId, subj, format!("unknown {m}"));
                }
            }
            Brand::Canonical => {}
        }
    }

    fn cap(&mut self, v: Var, scope: Scope, subj: Subject) {
        if let Var::Infer(_) = v {
            self.err(
                DiagCode::Unsupported,
                subj,
                "capability inference variables are outside this fragment".into(),
            );
            return;
        }
        self.var(v, ParamKind::Capability, scope, subj);
    }

    fn row(&mut self, r: &EffectRow, scope: Scope, subj: Subject) {
        if let Some(k) = r.duplicate_key() {
            self.err(
                DiagCode::Unsupported,
                subj,
                format!(
                    "effect row repeats label {k:?}; duplicate labels are outside this fragment"
                ),
            );
        }
        let mut sorted = r.clone();
        sorted.sort();
        if sorted != *r {
            self.err(
                DiagCode::Malformed,
                subj,
                format!("effect row {r} is not sorted by label key"),
            );
        }
        for l in &r.labels {
            match l {
                EffectLabel::Effect { effect, args } => {
                    let Some(e) = self.dto.effects.get(effect.index()) else {
                        self.err(DiagCode::UnknownId, subj, format!("unknown {effect}"));
                        continue;
                    };
                    let params = e.params.clone();
                    self.args(&params, args, scope, subj, &format!("{effect}"));
                }
                EffectLabel::Capability(v) => self.cap(*v, scope, subj),
            }
        }
        if let Some(t) = r.tail {
            self.var(t, ParamKind::Row, scope, subj);
        }
    }

    fn var(&mut self, v: Var, kind: ParamKind, scope: Scope, subj: Subject) {
        let found = match (scope, v) {
            (Scope::Template(ps), Var::Bound(i)) => ps.get(i as usize).copied(),
            (Scope::Query { placeholders, .. }, Var::Placeholder(i)) => {
                placeholders.get(i as usize).map(|p| p.kind)
            }
            (Scope::Query { binders, .. }, Var::Infer(i)) => binders.get(i as usize).copied(),
            _ => {
                self.err(
                    DiagCode::VarScope,
                    subj,
                    format!("variable {v} is not legal here"),
                );
                return;
            }
        };
        match found {
            None => self.err(DiagCode::VarScope, subj, format!("variable {v} is unbound")),
            Some(k) if k != kind => self.err(
                DiagCode::KindMismatch,
                subj,
                format!("variable {v} has kind {k:?}, used as {kind:?}"),
            ),
            _ => {}
        }
    }
}

/// Validates a query against a program.
pub fn validate_query(program: &Program, q: &QueryDto) -> Result<(), Vec<Diagnostic>> {
    let mut diags = Vec::new();
    if let Some(d) = check_schema(q.schema) {
        return Err(vec![d]);
    }
    let mut v = Validator {
        dto: program.dto(),
        diags: &mut diags,
    };
    let env_scope = Scope::Query {
        placeholders: &q.env.placeholders,
        binders: &[],
    };
    for p in &q.env.placeholders {
        if p.mode_bound.is_some() && p.kind != ParamKind::Type {
            v.err(
                DiagCode::Malformed,
                Subject::Query,
                "mode bound on a non-type placeholder".into(),
            );
        }
    }
    for a in &q.env.assumptions {
        if matches!(a, Goal::ModeBound { .. }) {
            v.err(
                DiagCode::Unsupported,
                Subject::Query,
                "mode assumptions must be placeholder mode bounds".into(),
            );
            continue;
        }
        v.goal(a, env_scope, Subject::Query);
        let brand = match a {
            Goal::Implements { brand, .. } => brand,
            Goal::ProjectionEq { projection, .. } => &projection.brand,
            Goal::ModeBound { .. } => unreachable!(),
        };
        if let Brand::Var(Var::Infer(_)) = brand {
            v.err(
                DiagCode::VarScope,
                Subject::Query,
                "assumption brand must be explicit".into(),
            );
        }
    }
    for scope in &q.env.model_scopes {
        for m in scope {
            if m.index() >= program.model_count() {
                v.err(DiagCode::UnknownId, Subject::Query, format!("unknown {m}"));
            }
        }
    }
    v.goal(
        &q.goal,
        Scope::Query {
            placeholders: &q.env.placeholders,
            binders: &q.binders,
        },
        Subject::Query,
    );
    if diags.is_empty() {
        Ok(())
    } else {
        diags.sort();
        diags.dedup();
        Err(diags)
    }
}
