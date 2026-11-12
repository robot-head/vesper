//! Surface HIR. Every node retains its source range for diagnostics and tooling.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, ops::Range};

pub type Span = Range<usize>;
pub type TypeMap = std::collections::BTreeMap<(usize, usize), Type>;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: String,
    pub message: String,
    pub span: Span,
}
impl Diagnostic {
    pub fn new(code: &str, message: impl Into<String>, span: Span) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            span,
        }
    }
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Mode {
    Copy,
    #[default]
    Affine,
    Linear,
}
impl Mode {
    pub fn join(self, other: Self) -> Self {
        self.max(other)
    }
    pub fn meet(self, other: Self) -> Self {
        self.min(other)
    }
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pass {
    #[default]
    In,
    Inout,
    Sink,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Type {
    /// A numeric literal awaiting its contextual integer type.
    IntLiteral(i128),
    Named(String, Vec<Type>),
    Tuple(Vec<Type>),
    Function {
        params: Vec<(Pass, Type)>,
        result: Box<Type>,
        effects: Effects,
        mode: Mode,
    },
    Row(Effects),
    Infer,
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
impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IntLiteral(n) => write!(f, "integer literal {n}"),
            Self::Named(n, a) => {
                write!(f, "{n}")?;
                if !a.is_empty() {
                    write!(
                        f,
                        "[{}]",
                        a.iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )?;
                }
                Ok(())
            }
            Self::Tuple(a) => write!(
                f,
                "({})",
                a.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Function { params, result, .. } => write!(
                f,
                "fn({}) -> {result}",
                params
                    .iter()
                    .map(|(_, t)| t.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Row(e) => write!(f, "eff {:?}", e),
            Self::Infer => write!(f, "_"),
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Effects {
    pub instances: Vec<Effect>,
    pub tail: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Effect {
    pub label: Option<String>,
    pub name: String,
    pub args: Vec<Type>,
}
impl Effects {
    pub fn names(&self) -> BTreeSet<String> {
        self.instances.iter().map(|e| e.name.clone()).collect()
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Generic {
    pub name: String,
    pub bound: Mode,
    pub row: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Field {
    pub name: String,
    pub ty: Type,
    pub modality: Mode,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Variant {
    pub name: String,
    pub fields: Vec<Field>,
    pub named: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Adt {
    pub name: String,
    pub mode: Mode,
    pub generics: Vec<Generic>,
    pub variants: Vec<Variant>,
    pub is_enum: bool,
    pub span: Span,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Param {
    pub name: String,
    pub pass: Pass,
    pub projections: Vec<Vec<String>>,
    pub ty: Type,
    pub span: Span,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Function {
    pub name: String,
    pub generics: Vec<Generic>,
    pub params: Vec<Param>,
    pub result: Type,
    pub effects: Effects,
    pub body: Option<Expr>,
    pub span: Span,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EffectDecl {
    pub name: String,
    pub linear: bool,
    pub generics: Vec<Generic>,
    pub ops: Vec<Function>,
    pub span: Span,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Item {
    Adt(Adt),
    Function(Box<Function>),
    Effect(EffectDecl),
    Impl {
        ty: Type,
        generics: Vec<Generic>,
        methods: Vec<Function>,
    },
    /// Frontend-only `trait` declaration (§16.2). Elaborated away before a
    /// checked `World` reaches any backend.
    Trait(TraitDecl),
    /// Frontend-only canonical `impl Trait for Type` or named `model`
    /// declaration (§16.3). Its methods become private generated functions.
    Conformance(Conformance),
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Module {
    pub items: Vec<Item>,
    /// Frontend-only lexical `use model M;` declarations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_uses: Vec<ModelUse>,
    /// Frontend-only trait bounds written on function generics or
    /// `where` clauses outside conformances (generic dictionary dispatch,
    /// which the static elaboration rejects).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trait_bounds: Vec<TraitBoundSyntax>,
}

// ---------------------------------------------------------------------------
// Frontend trait/model declaration metadata (design §16.2-16.3).
//
// These shapes are produced by the parser and consumed only by the source
// frontend (`vesper::pact_frontend`). They never appear in the core
// `ExprKind`/`Type` schema of an accepted `World`: trait method calls are
// elaborated into ordinary calls of generated functions, and associated
// type projections are normalized to ordinary types, or the module is
// rejected.

/// `trait Name[params] { type A: mod m; fn method(in self, ...) -> T eff [r]; }`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TraitDecl {
    pub name: String,
    pub public: bool,
    /// Parameters after `Self`: type parameters (with modal bounds) and
    /// effect-row parameters.
    pub generics: Vec<Generic>,
    pub assoc: Vec<AssocTypeDecl>,
    /// Method signatures (bodies are `None`).
    pub methods: Vec<Function>,
    pub span: Span,
}
/// `type Name: mod m;` inside a trait.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AssocTypeDecl {
    pub name: String,
    pub bound: Mode,
    pub span: Span,
}
/// `Trait[args]`, optionally package-qualified (`@scope/name::Trait`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TraitRef {
    pub package: Option<String>,
    pub name: String,
    pub args: Vec<Type>,
    pub span: Span,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ConformanceKind {
    /// Anonymous canonical `impl` (global model uniqueness).
    Canonical,
    /// Type-branded named `model Name: Trait for Type`.
    Model { name: String, public: bool },
}
/// `type Name = T;` inside an impl or model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AssocTypeBinding {
    pub name: String,
    pub ty: Type,
    pub span: Span,
}
/// `where T: Trait[args]` on an impl or model (or `[T: Trait]`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WhereClause {
    pub ty: Type,
    pub trait_ref: TraitRef,
    pub span: Span,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Conformance {
    pub kind: ConformanceKind,
    pub generics: Vec<Generic>,
    pub trait_ref: TraitRef,
    pub self_ty: Type,
    pub where_clauses: Vec<WhereClause>,
    pub assoc: Vec<AssocTypeBinding>,
    pub methods: Vec<Function>,
    pub span: Span,
}
/// `use model M;`. It applies to code after `span` inside `scope` (the
/// enclosing block, or the whole module for an item-level use).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelUse {
    pub model: String,
    pub span: Span,
    pub scope: Span,
}
/// A trait bound outside a conformance head, e.g. `fn f[T: Show]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TraitBoundSyntax {
    pub param: String,
    pub trait_ref: TraitRef,
    pub span: Span,
}

/// Reserved frontend-only encoding of an associated-type projection
/// `<T as Trait[args]>::Assoc` (or the shorthand `T::Assoc`). No source
/// identifier can spell it, and the frontend normalizes every occurrence to
/// an ordinary type or rejects the module; it never reaches a checked World.
pub const PROJECTION_TYPE: &str = "$projection";
/// Placeholder trait of a shorthand projection whose trait is inferred.
pub const PROJECTION_ANY_TRAIT: &str = "$any";

/// A projection decoded from its reserved [`PROJECTION_TYPE`] encoding.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectionSyntax<'a> {
    pub self_ty: &'a Type,
    /// `None` for shorthand `T::Assoc`.
    pub trait_name: Option<&'a str>,
    pub trait_args: &'a [Type],
    pub assoc: &'a str,
}
pub fn projection_type(
    self_ty: Type,
    trait_ref: Option<(String, Vec<Type>)>,
    assoc: impl Into<String>,
) -> Type {
    let (name, args) = trait_ref.unwrap_or_else(|| (PROJECTION_ANY_TRAIT.into(), vec![]));
    Type::Named(
        PROJECTION_TYPE.into(),
        vec![
            self_ty,
            Type::Named(name, args),
            Type::Named(assoc.into(), vec![]),
        ],
    )
}
pub fn as_projection(t: &Type) -> Option<ProjectionSyntax<'_>> {
    match t {
        Type::Named(n, args) if n == PROJECTION_TYPE => match args.as_slice() {
            [self_ty, Type::Named(tr, targs), Type::Named(assoc, a)] if a.is_empty() => {
                Some(ProjectionSyntax {
                    self_ty,
                    trait_name: (tr != PROJECTION_ANY_TRAIT).then_some(tr.as_str()),
                    trait_args: targs,
                    assoc,
                })
            }
            _ => None,
        },
        _ => None,
    }
}

/// Elaboration metadata a checked World carries about source trait
/// declarations. Generated implementation functions are private: interface
/// export omits them and reports the trait/model contracts as unsupported.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Elaboration {
    /// Package identity the declarations were lowered under.
    pub package: Option<String>,
    /// Pact declaration revision the elaborated calls were resolved under.
    pub revision: Option<String>,
    /// Declared trait names.
    pub traits: Vec<String>,
    /// Declared conformances, by their package-qualified declaration path.
    pub conformances: Vec<ConformanceInfo>,
    /// Generated function name to its origin.
    pub generated: std::collections::BTreeMap<String, GeneratedFunction>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConformanceInfo {
    pub path: String,
    pub trait_name: String,
    pub self_ty: String,
    pub model: Option<String>,
    pub public: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedFunction {
    /// Package-qualified declaration path of the impl or model.
    pub conformance: String,
    pub trait_name: String,
    pub method: String,
    /// Always false: generated bodies are never public source exports.
    pub public: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Literal {
    Int(i128),
    Float(f64),
    Bool(bool),
    String(String),
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Arg {
    pub pass: Pass,
    pub explicit: bool,
    pub expr: Expr,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Statement {
    Let { pat: Pattern, ty: Type, value: Expr },
    Expr(Expr),
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Clause {
    pub name: String,
    pub pats: Vec<Pattern>,
    pub body: Expr,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ExprKind {
    Literal(Literal),
    Name(String),
    Tuple(Vec<Expr>),
    Struct {
        name: String,
        fields: Vec<(String, Expr)>,
    },
    Block {
        statements: Vec<Statement>,
        tail: Option<Box<Expr>>,
    },
    Call {
        callee: Box<Expr>,
        types: Vec<Type>,
        args: Vec<Arg>,
    },
    Field(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    Try(Box<Expr>),
    Unary(String, Box<Expr>),
    Binary(String, Box<Expr>, Box<Expr>),
    Assign(String, Box<Expr>, Box<Expr>),
    If {
        condition: Box<Expr>,
        then: Box<Expr>,
        otherwise: Option<Box<Expr>>,
    },
    Match {
        value: Box<Expr>,
        arms: Vec<(Pattern, Expr)>,
    },
    Closure {
        params: Vec<Param>,
        mode: Option<Mode>,
        result: Type,
        effects: Effects,
        infer_effects: bool,
        body: Box<Expr>,
    },
    Handle {
        effect: Effect,
        state: Option<(Pattern, Box<Expr>)>,
        body: Box<Expr>,
        ret: Box<Clause>,
        ops: Vec<Clause>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Pattern {
    pub kind: PatternKind,
    pub span: Span,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum PatternKind {
    Wild,
    Bind(String),
    Literal(Literal),
    Tuple(Vec<Pattern>),
    Constructor {
        name: String,
        fields: Vec<(String, Pattern)>,
        named: bool,
    },
}

pub fn substitute(ty: &Type, vars: &std::collections::BTreeMap<String, Type>) -> Type {
    match ty {
        Type::Named(n, a) if a.is_empty() && vars.contains_key(n) => vars[n].clone(),
        Type::Named(n, a) => {
            Type::Named(n.clone(), a.iter().map(|t| substitute(t, vars)).collect())
        }
        Type::Tuple(a) => Type::Tuple(a.iter().map(|t| substitute(t, vars)).collect()),
        Type::Function {
            params,
            result,
            effects,
            mode,
        } => Type::Function {
            params: params
                .iter()
                .map(|(p, t)| (*p, substitute(t, vars)))
                .collect(),
            result: Box::new(substitute(result, vars)),
            effects: substitute_effects(effects, vars),
            mode: *mode,
        },
        _ => ty.clone(),
    }
}

pub fn substitute_effects(e: &Effects, vars: &std::collections::BTreeMap<String, Type>) -> Effects {
    let mut out = Effects {
        instances: e
            .instances
            .iter()
            .map(|e| Effect {
                label: e.label.clone(),
                name: e.name.clone(),
                args: e.args.iter().map(|t| substitute(t, vars)).collect(),
            })
            .collect(),
        tail: e.tail.clone(),
    };
    if let Some(tail) = &e.tail
        && let Some(Type::Row(row)) = vars.get(tail)
    {
        out.instances.extend(row.instances.clone());
        out.tail = row.tail.clone();
    }
    let mut seen = std::collections::BTreeSet::new();
    out.instances.retain(|e| {
        seen.insert((
            e.label.clone(),
            e.name.clone(),
            e.args.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ))
    });
    out
}
