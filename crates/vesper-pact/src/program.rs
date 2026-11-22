//! Versioned declaration DTOs, query environments and their validation.

use crate::diag::Diagnostic;
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
    /// The program's input words for the Vesper port (`crate::port`).
    #[cfg(vesper_port)]
    words: Vec<u64>,
}

impl Program {
    /// Validates ids, arities, variable scoping and fragment restrictions.
    /// Diagnostics are sorted and deterministic.
    pub fn new(dto: ProgramDto) -> Result<Program, Vec<Diagnostic>> {
        #[cfg(vesper_port)]
        let words = {
            let words = crate::port::program_words(&dto);
            let diags = crate::port::validate_program(&words);
            if !diags.is_empty() {
                return Err(diags);
            }
            words
        };
        #[cfg(not(vesper_port))]
        crate::engine::validate::validate_program(&dto)?;
        let mut impls_by_trait = vec![Vec::new(); dto.traits.len()];
        for i in &dto.impls {
            impls_by_trait[i.decl.trait_ref.trait_id.index()].push(i.id);
        }
        Ok(Program {
            dto,
            impls_by_trait,
            #[cfg(vesper_port)]
            words,
        })
    }
    #[cfg(vesper_port)]
    pub(crate) fn port_words(&self) -> &[u64] {
        &self.words
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

/// Validates a query against a program.
pub fn validate_query(program: &Program, q: &QueryDto) -> Result<(), Vec<Diagnostic>> {
    #[cfg(vesper_port)]
    return crate::port::validate_query(program, q);
    #[cfg(not(vesper_port))]
    crate::engine::validate::validate_query(program, q)
}
