//! What a session knows of the checked program: the ADTs, the effect
//! declarations and the entry signatures `vesper sim-host` reports when it
//! loads the source (`src/sim_host.rs` in the root package). Types keep the
//! checker's `Display`, so scenario signatures and error texts read as they
//! did when this crate linked the compiler.
use serde::Deserialize;
use std::collections::BTreeMap;

/// The type name the checker gives `dyn Trait` (`ast::DYN_TYPE`).
const DYN_TYPE: &str = "dyn";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Pass {
    #[default]
    In,
    Inout,
    Sink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Copy,
    Affine,
    Linear,
}

/// A checked type, as the host sends it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Type {
    Named(String, Vec<Type>),
    Tuple(Vec<Type>),
    Function {
        params: Vec<(Pass, Type)>,
        result: Box<Type>,
    },
    /// A literal, row or inferred type, by its `Display` text. No host value
    /// has one.
    Other(String),
}

impl Type {
    pub fn named(name: &str) -> Self {
        Self::Named(name.into(), vec![])
    }
    pub fn unit() -> Self {
        Self::Tuple(vec![])
    }
    pub fn name(&self) -> &str {
        match self {
            Self::Named(n, _) => n,
            _ => "",
        }
    }
}

fn join(items: impl Iterator<Item = String>) -> String {
    items.collect::<Vec<_>>().join(", ")
}

impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Named(n, a) if n == DYN_TYPE && a.len() == 1 => write!(f, "dyn {}", a[0]),
            Self::Named(n, a) => {
                write!(f, "{n}")?;
                if !a.is_empty() {
                    write!(f, "[{}]", join(a.iter().map(ToString::to_string)))?;
                }
                Ok(())
            }
            Self::Tuple(a) => write!(f, "({})", join(a.iter().map(ToString::to_string))),
            Self::Function { params, result } => write!(
                f,
                "fn({}) -> {result}",
                join(params.iter().map(|(_, t)| t.to_string()))
            ),
            Self::Other(text) => f.write_str(text),
        }
    }
}

/// Replace each generic parameter named in `vars` (`ast::substitute`).
pub fn substitute(ty: &Type, vars: &BTreeMap<String, Type>) -> Type {
    match ty {
        Type::Named(n, a) if a.is_empty() && vars.contains_key(n) => vars[n].clone(),
        Type::Named(n, a) => {
            Type::Named(n.clone(), a.iter().map(|t| substitute(t, vars)).collect())
        }
        Type::Tuple(a) => Type::Tuple(a.iter().map(|t| substitute(t, vars)).collect()),
        Type::Function { params, result } => Type::Function {
            params: params
                .iter()
                .map(|(p, t)| (*p, substitute(t, vars)))
                .collect(),
            result: Box::new(substitute(result, vars)),
        },
        Type::Other(_) => ty.clone(),
    }
}

impl From<String> for Generic {
    fn from(name: String) -> Self {
        Self { name }
    }
}

/// A generic parameter, sent as its name.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(from = "String")]
pub struct Generic {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Variant {
    pub name: String,
    pub named: bool,
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Adt {
    #[serde(skip)]
    pub name: String,
    pub mode: Mode,
    pub generics: Vec<Generic>,
    pub is_enum: bool,
    pub variants: Vec<Variant>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Param {
    pub name: String,
    pub pass: Pass,
    pub ty: Type,
}

/// An effect operation's signature.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub name: String,
    pub params: Vec<Param>,
    pub result: Type,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectDecl {
    #[serde(skip)]
    pub name: String,
    pub linear: bool,
    pub generics: Vec<Generic>,
    pub ops: Vec<Operation>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Effect {
    pub label: Option<String>,
    pub name: String,
    pub args: Vec<Type>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Effects {
    pub instances: Vec<Effect>,
    pub tail: Option<String>,
}

/// A function with a body; `result` is its checked result type.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Function {
    pub generics: Vec<Generic>,
    pub params: Vec<Param>,
    pub result: Type,
    pub effects: Effects,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct World {
    pub adts: BTreeMap<String, Adt>,
    pub effects: BTreeMap<String, EffectDecl>,
    pub functions: BTreeMap<String, Function>,
}

impl World {
    /// Read the facts of a `loaded` reply.
    pub(crate) fn from_facts(facts: serde_json::Value) -> Result<Self, String> {
        let mut world: Self =
            serde_json::from_value(facts).map_err(|e| format!("sim-host world facts: {e}"))?;
        for (name, adt) in &mut world.adts {
            adt.name.clone_from(name);
        }
        for (name, effect) in &mut world.effects {
            effect.name.clone_from(name);
        }
        Ok(world)
    }
}
