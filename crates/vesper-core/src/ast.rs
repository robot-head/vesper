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
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Module {
    pub items: Vec<Item>,
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
