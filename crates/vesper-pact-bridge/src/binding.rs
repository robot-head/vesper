//! Versioned declaration-binding DTOs.
//!
//! Vesper source has no `trait`, `impl Trait for`, `model` or `via` syntax
//! yet. Until the source traits/models lane adds it, trait, impl and model
//! declarations (and queries) enter a session through these DTOs. Every type
//! they mention must resolve to a checked source declaration, a builtin, or a
//! parameter of the enclosing binding; the DTOs cannot declare new types.
//!
//! This boundary is temporary: the DTOs describe exactly what lowered surface
//! declarations will carry, without claiming that surface syntax exists.

use crate::identity::PackageIdentity;
use serde::{Deserialize, Serialize};
use vesper::ast::{Mode, Type};

/// Version of the binding schema. Accepted when the major version matches and
/// the minor version is not newer than [`BINDING_SCHEMA`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BindingSchema {
    pub major: u16,
    pub minor: u16,
}

pub const BINDING_SCHEMA: BindingSchema = BindingSchema { major: 0, minor: 1 };

impl BindingSchema {
    pub fn accepts(self, v: BindingSchema) -> bool {
        v.major == self.major && v.minor <= self.minor
    }
}

/// Declarations of one package that are not yet expressible in source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingsDto {
    pub schema: BindingSchema,
    /// Must equal the identity of the checked package it is attached to.
    pub package: PackageIdentity,
    #[serde(default)]
    pub branded_types: Vec<BrandedTypeDto>,
    #[serde(default)]
    pub traits: Vec<TraitDto>,
    #[serde(default)]
    pub impls: Vec<ImplDto>,
    #[serde(default)]
    pub models: Vec<ModelDto>,
}

impl BindingsDto {
    pub fn empty(package: PackageIdentity) -> Self {
        BindingsDto {
            schema: BINDING_SCHEMA,
            package,
            branded_types: vec![],
            traits: vec![],
            impls: vec![],
            models: vec![],
        }
    }
}

/// Appends model-identity brand parameters to a source ADT of this package
/// (`CacheBTree[K, V]` becomes `CacheBTree[K, V, M]`). Source occurrences use
/// the defaulted canonical brand; bindings may name a model explicitly.
/// Brands are zero-sized: they never change the type's mode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrandedTypeDto {
    pub ty: String,
    pub brands: Vec<BrandParamDto>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrandParamDto {
    pub name: String,
    /// The trait whose model the brand identifies (`Ord` in `Canonical[Ord, K]`).
    #[serde(rename = "trait")]
    pub trait_ref: DefRefDto,
}

/// Reference to a definition. `package: None` resolves in the use-site
/// package first and then in the builtin package.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefRefDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<PackageIdentity>,
    pub name: String,
}

impl DefRefDto {
    pub fn local(name: impl Into<String>) -> Self {
        DefRefDto {
            package: None,
            name: name.into(),
        }
    }
    pub fn qualified(package: PackageIdentity, name: impl Into<String>) -> Self {
        DefRefDto {
            package: Some(package),
            name: name.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamKindDto {
    Type,
    Mode,
    Row,
    Brand,
    Capability,
}

/// A named parameter of a trait, impl, model, query environment or query.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParamDto {
    pub name: String,
    pub kind: ParamKindDto,
    /// Modal bound of a type parameter; absent means `linear`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound: Option<Mode>,
    /// The effect a capability parameter labels (environment parameters only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<EffectUseDto>,
}

impl ParamDto {
    pub fn ty(name: impl Into<String>, bound: Option<Mode>) -> Self {
        ParamDto {
            name: name.into(),
            kind: ParamKindDto::Type,
            bound,
            effect: None,
        }
    }
    pub fn of(name: impl Into<String>, kind: ParamKindDto) -> Self {
        ParamDto {
            name: name.into(),
            kind,
            bound: None,
            effect: None,
        }
    }
    pub fn capability(name: impl Into<String>, effect: EffectUseDto) -> Self {
        ParamDto {
            name: name.into(),
            kind: ParamKindDto::Capability,
            bound: None,
            effect: Some(effect),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectUseDto {
    pub effect: DefRefDto,
    #[serde(default)]
    pub args: Vec<ArgDto>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypeDto {
    /// A source ADT, builtin primitive or prelude type, applied to arguments.
    /// With `package: None` and no arguments, an in-scope parameter of the
    /// same name shadows a type, exactly as generic names do in source.
    Def {
        def: DefRefDto,
        #[serde(default)]
        args: Vec<ArgDto>,
    },
    Tuple(Vec<TypeDto>),
    Param(String),
    Projection(Box<ProjectionDto>),
}

impl TypeDto {
    pub fn named(name: impl Into<String>, args: Vec<TypeDto>) -> Self {
        TypeDto::Def {
            def: DefRefDto::local(name),
            args: args.into_iter().map(ArgDto::Type).collect(),
        }
    }
    pub fn param(name: impl Into<String>) -> Self {
        TypeDto::Param(name.into())
    }

    /// Converts a checked source type. Names stay unresolved references; the
    /// session resolves them in the use-site package. Function types,
    /// unresolved types and checker-internal capability labels are rejected.
    pub fn from_source(t: &Type) -> Result<TypeDto, String> {
        Ok(match t {
            Type::Named(n, args) => TypeDto::Def {
                def: DefRefDto::local(n.clone()),
                args: args
                    .iter()
                    .map(ArgDto::from_source)
                    .collect::<Result<_, _>>()?,
            },
            Type::Tuple(ts) => TypeDto::Tuple(
                ts.iter()
                    .map(TypeDto::from_source)
                    .collect::<Result<_, _>>()?,
            ),
            Type::Function { .. } => {
                return Err(format!(
                    "function type `{t}` has no Pact type constructor in this fragment"
                ));
            }
            Type::Row(_) => return Err("an effect row is not a type".into()),
            Type::IntLiteral(_) | Type::Infer => {
                return Err(format!("unresolved source type `{t}`"));
            }
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionDto {
    pub self_ty: TypeDto,
    #[serde(rename = "trait")]
    pub trait_ref: DefRefDto,
    #[serde(default)]
    pub args: Vec<ArgDto>,
    #[serde(default)]
    pub brand: Option<BrandDto>,
    pub assoc: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModeDto {
    Const(Mode),
    Param(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowDto {
    #[serde(default)]
    pub labels: Vec<LabelDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail: Option<String>,
}

/// `Effect[args]`, or `cap: Effect[args]` for a lexical capability label.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabelDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<String>,
    pub effect: DefRefDto,
    #[serde(default)]
    pub args: Vec<ArgDto>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrandDto {
    Canonical,
    Model(DefRefDto),
    Param(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgDto {
    Type(TypeDto),
    Mode(ModeDto),
    Row(RowDto),
    Brand(BrandDto),
    Capability(String),
}

impl ArgDto {
    pub fn from_source(t: &Type) -> Result<ArgDto, String> {
        match t {
            Type::Row(e) => {
                let mut labels = vec![];
                for inst in &e.instances {
                    if let Some(l) = &inst.label
                        && l.starts_with('$')
                    {
                        return Err(format!(
                            "checker-internal capability label {l:?} is not a stable source name"
                        ));
                    }
                    labels.push(LabelDto {
                        capability: inst.label.clone(),
                        effect: DefRefDto::local(inst.name.clone()),
                        args: inst
                            .args
                            .iter()
                            .map(ArgDto::from_source)
                            .collect::<Result<_, _>>()?,
                    });
                }
                Ok(ArgDto::Row(RowDto {
                    labels,
                    tail: e.tail.clone(),
                }))
            }
            other => TypeDto::from_source(other).map(ArgDto::Type),
        }
    }
}

/// `Self: Trait[args]` via `brand` (bounds of associated types and
/// where-clauses). An absent brand means the canonical model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraitBoundDto {
    #[serde(rename = "trait")]
    pub trait_ref: DefRefDto,
    #[serde(default)]
    pub args: Vec<ArgDto>,
    #[serde(default)]
    pub brand: Option<BrandDto>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalDto {
    /// In a query, an absent brand requests implicit resolution (`use model`
    /// scopes, then canonical impls). Elsewhere it means the canonical model.
    Implements {
        self_ty: TypeDto,
        #[serde(rename = "trait")]
        trait_ref: DefRefDto,
        #[serde(default)]
        args: Vec<ArgDto>,
        #[serde(default)]
        brand: Option<BrandDto>,
    },
    ProjectionEq {
        projection: ProjectionDto,
        ty: TypeDto,
    },
    ModeBound {
        ty: TypeDto,
        bound: ModeDto,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssocDto {
    pub name: String,
    /// Upper modal bound; may mention the trait's mode parameters.
    pub mode_bound: ModeDto,
    #[serde(default)]
    pub bounds: Vec<TraitBoundDto>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraitDto {
    pub name: String,
    /// Parameters after `Self`.
    #[serde(default)]
    pub params: Vec<ParamDto>,
    #[serde(default)]
    pub assoc: Vec<AssocDto>,
    #[serde(default)]
    pub coinductive: bool,
}

/// Shared shape of canonical impls and named models.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConformanceDto {
    #[serde(default)]
    pub params: Vec<ParamDto>,
    #[serde(rename = "trait")]
    pub trait_ref: DefRefDto,
    pub self_ty: TypeDto,
    #[serde(default)]
    pub args: Vec<ArgDto>,
    #[serde(default)]
    pub where_clauses: Vec<GoalDto>,
    /// Associated values by name; every associated type of the trait must be
    /// given exactly once.
    #[serde(default)]
    pub assoc: Vec<(String, TypeDto)>,
}

/// An anonymous canonical impl. `key` is its stable external identity within
/// the package (anonymous impls have no source name).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImplDto {
    pub key: String,
    #[serde(flatten)]
    pub decl: ConformanceDto,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelDto {
    pub name: String,
    #[serde(flatten)]
    pub decl: ConformanceDto,
}

/// The parameter environment of a query: rigid generic parameters, assumed
/// where-clauses and lexical `use model` scopes (outermost first).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvDto {
    #[serde(default)]
    pub params: Vec<ParamDto>,
    #[serde(default)]
    pub assumptions: Vec<GoalDto>,
    #[serde(default)]
    pub use_models: Vec<Vec<DefRefDto>>,
}

/// A trait query at a use site in `package`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryBindingDto {
    pub schema: BindingSchema,
    pub package: PackageIdentity,
    #[serde(default)]
    pub env: EnvDto,
    /// Existential inference variables (checker metavariables).
    #[serde(default)]
    pub infer: Vec<ParamDto>,
    pub goal: GoalDto,
}

impl QueryBindingDto {
    pub fn new(package: PackageIdentity, env: EnvDto, infer: Vec<ParamDto>, goal: GoalDto) -> Self {
        QueryBindingDto {
            schema: BINDING_SCHEMA,
            package,
            env,
            infer,
            goal,
        }
    }
}
