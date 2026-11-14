//! Structured Lean syntax, not a Lean type checker or compiler-correctness proof.
//!
//! Names/literals are validated or escaped, and all syntax is printed centrally.
//! There is no raw-source node. Call Lean to elaborate/check the resulting module.
//! This library deliberately leaves checked/wrapping arithmetic to its caller.
#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub String);
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}

/// One Lean name component. Dots remain part of this component, not namespace separators.
/// The printer always quotes identifiers, including keywords and generated `$` names.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ident(String);
impl Ident {
    pub fn new(value: impl Into<String>) -> Result<Self, Error> {
        let value = value.into();
        if value.is_empty()
            || value == "_root_"
            || value.chars().any(|c| {
                c.is_control()
                    || matches!(c,
            '«' | '»' | '\u{2028}' | '\u{2029}' | '\u{202a}'..='\u{202e}' |
            '\u{2066}'..='\u{2069}' | '\u{feff}')
            })
        {
            return Err(Error(
                "identifier is empty, reserved _root_, or contains an unsupported delimiter/control".into(),
            ));
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Name {
    components: Vec<Ident>,
    rooted: bool,
}
impl Name {
    pub fn single(value: impl Into<String>) -> Result<Self, Error> {
        Ok(Self {
            components: vec![Ident::new(value)?],
            rooted: false,
        })
    }
    pub fn qualified(components: impl IntoIterator<Item = Ident>) -> Result<Self, Error> {
        let components = components.into_iter().collect::<Vec<_>>();
        if components.is_empty() {
            return Err(Error("a Lean name needs at least one component".into()));
        }
        Ok(Self {
            components,
            rooted: false,
        })
    }
    /// Explicit global lookup, useful inside a namespace with shadowing names.
    pub fn rooted(mut self) -> Self {
        self.rooted = true;
        self
    }
    pub fn components(&self) -> &[Ident] {
        &self.components
    }
}

/// Restricted ordinary ASCII module path; never a filesystem path or arbitrary import text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleName(Vec<String>);
impl ModuleName {
    pub fn new(value: &str) -> Result<Self, Error> {
        let components = value.split('.').map(str::to_owned).collect::<Vec<_>>();
        if components.iter().any(|part| {
            part.is_empty()
                || !part.as_bytes()[0].is_ascii_alphabetic()
                || !part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        }) {
            return Err(Error("invalid ASCII Lean module path".into()));
        }
        Ok(Self(components))
    }
}

/// Arbitrary-size canonical nonnegative decimal syntax. No fixed-width truncation is performed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NatLiteral(String);
impl NatLiteral {
    pub fn decimal(value: &str) -> Result<Self, Error> {
        if value.is_empty()
            || !value.bytes().all(|b| b.is_ascii_digit())
            || (value.len() > 1 && value.starts_with('0'))
        {
            return Err(Error(
                "expected a canonical nonnegative decimal literal".into(),
            ));
        }
        Ok(Self(value.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl From<u128> for NatLiteral {
    fn from(value: u128) -> Self {
        Self(value.to_string())
    }
}
impl From<u64> for NatLiteral {
    fn from(value: u64) -> Self {
        Self(value.to_string())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sort {
    Prop,
    Type(u32),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinderKind {
    Explicit,
    Implicit,
    Instance,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binder {
    pub name: Option<Ident>,
    pub ty: Box<Term>,
    pub kind: BinderKind,
}
impl Binder {
    pub fn explicit(name: Ident, ty: Term) -> Self {
        Self {
            name: Some(name),
            ty: Box::new(ty),
            kind: BinderKind::Explicit,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Argument {
    Positional(Term),
    Named { name: Ident, value: Term },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    BoolEq,
    BoolNe,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    BoolAnd,
    BoolOr,
    Iff,
}
impl BinaryOp {
    fn text(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::Mod => "%",
            Self::Eq => "=",
            Self::Ne => "≠",
            Self::BoolEq => "==",
            Self::BoolNe => "!=",
            Self::Lt => "<",
            Self::Le => "≤",
            Self::Gt => ">",
            Self::Ge => "≥",
            Self::And => "∧",
            Self::Or => "∨",
            Self::BoolAnd => "&&",
            Self::BoolOr => "||",
            Self::Iff => "↔",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
    BoolNot,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Projection {
    Field(Ident),
    Tuple(std::num::NonZeroU32),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pattern {
    Wildcard,
    Bind(Ident),
    Constructor { name: Name, arguments: Vec<Pattern> },
    Tuple(Vec<Pattern>),
    Nat(NatLiteral),
    Bool(bool),
    String(String),
    Char(char),
    Int(i128),
    Record { fields: Vec<(Ident, Pattern)> },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub body: Term,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LetBinding {
    pub pattern: Pattern,
    pub ty: Option<Box<Term>>,
    pub value: Term,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DoStatement {
    Let {
        binding: LetBinding,
        mutable: bool,
    },
    Bind {
        pattern: Pattern,
        action: Term,
    },
    Assign {
        local: Ident,
        value: Term,
    },
    Expr(Term),
    BindTyped {
        pattern: Pattern,
        ty: Term,
        action: Term,
    },
    LetInstance {
        name: Option<Ident>,
        ty: Term,
        value: Term,
    },
    If {
        condition: Term,
        yes: Vec<DoStatement>,
        no: Option<Vec<DoStatement>>,
    },
    Match {
        scrutinee: Term,
        arms: Vec<(Pattern, Vec<DoStatement>)>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Term {
    /// Explicit Lean elaboration placeholder; unresolved metavariables still fail Lean checking.
    Infer,
    Ref(Name),
    Sort(Sort),
    Nat(NatLiteral),
    Bool(bool),
    String(String),
    Char(char),
    App {
        function: Box<Term>,
        arguments: Vec<Argument>,
    },
    Lambda {
        binders: Vec<Binder>,
        body: Box<Term>,
    },
    Forall {
        binders: Vec<Binder>,
        body: Box<Term>,
    },
    Arrow(Box<Term>, Box<Term>),
    ProductType(Vec<Term>),
    Tuple(Vec<Term>),
    List(Vec<Term>),
    Let {
        binding: Box<LetBinding>,
        body: Box<Term>,
    },
    If {
        condition: Box<Term>,
        yes: Box<Term>,
        no: Box<Term>,
    },
    Match {
        scrutinee: Box<Term>,
        arms: Vec<MatchArm>,
    },
    Project {
        base: Box<Term>,
        projection: Projection,
    },
    Ascribe {
        value: Box<Term>,
        ty: Box<Term>,
    },
    Unary {
        op: UnaryOp,
        value: Box<Term>,
    },
    Binary {
        op: BinaryOp,
        left: Box<Term>,
        right: Box<Term>,
    },
    Record {
        fields: Vec<(Ident, Term)>,
    },
    Update {
        base: Box<Term>,
        fields: Vec<(Ident, Term)>,
    },
    Do {
        statements: Vec<DoStatement>,
        result: Box<Term>,
    },
    /// Fixed Lean syntax, not a caller-provided tactic string.
    Rfl,
    ByDecide,
    Eliminate {
        value: Box<Term>,
    },
}
impl Term {
    pub fn app(function: Term, arguments: impl IntoIterator<Item = Term>) -> Self {
        Self::App {
            function: Box::new(function),
            arguments: arguments.into_iter().map(Argument::Positional).collect(),
        }
    }
    pub fn binary(op: BinaryOp, left: Term, right: Term) -> Self {
        Self::Binary {
            op,
            left: Box::new(left),
            right: Box::new(right),
        }
    }
    pub fn render(&self) -> Result<String, Error> {
        Printer::term(self)
    }
}

/// Free names of a definition: names its binder types and body mention that
/// are not bound by its parameters or by a binder, pattern, `let` or `do`
/// binding in scope. A bound name shadows only single, unrooted components.
impl Term {
    pub fn free_references(parameters: &[Binder], body: &Term) -> std::collections::BTreeSet<Name> {
        let mut out = std::collections::BTreeSet::new();
        let mut scope = vec![];
        for b in parameters {
            b.ty.free(&mut scope, &mut out);
            scope.extend(b.name.clone());
        }
        body.free(&mut scope, &mut out);
        out
    }
    fn free(&self, scope: &mut Vec<Ident>, out: &mut std::collections::BTreeSet<Name>) {
        fn bound(p: &Pattern, scope: &mut Vec<Ident>) {
            match p {
                Pattern::Bind(i) => scope.push(i.clone()),
                Pattern::Constructor { arguments, .. } | Pattern::Tuple(arguments) => {
                    for a in arguments {
                        bound(a, scope);
                    }
                }
                Pattern::Record { fields } => {
                    for (_, a) in fields {
                        bound(a, scope);
                    }
                }
                _ => {}
            }
        }
        fn statements(
            ss: &[DoStatement],
            result: Option<&Term>,
            scope: &mut Vec<Ident>,
            out: &mut std::collections::BTreeSet<Name>,
        ) {
            let depth = scope.len();
            for s in ss {
                match s {
                    DoStatement::Let { binding, .. } => {
                        if let Some(t) = &binding.ty {
                            t.free(scope, out);
                        }
                        binding.value.free(scope, out);
                        bound(&binding.pattern, scope);
                    }
                    DoStatement::Bind { pattern, action } => {
                        action.free(scope, out);
                        bound(pattern, scope);
                    }
                    DoStatement::BindTyped {
                        pattern,
                        ty,
                        action,
                    } => {
                        ty.free(scope, out);
                        action.free(scope, out);
                        bound(pattern, scope);
                    }
                    DoStatement::Assign { value, .. } => value.free(scope, out),
                    DoStatement::Expr(t) => t.free(scope, out),
                    DoStatement::LetInstance { name, ty, value } => {
                        ty.free(scope, out);
                        value.free(scope, out);
                        scope.extend(name.clone());
                    }
                    DoStatement::If { condition, yes, no } => {
                        condition.free(scope, out);
                        statements(yes, None, scope, out);
                        if let Some(no) = no {
                            statements(no, None, scope, out);
                        }
                    }
                    DoStatement::Match { scrutinee, arms } => {
                        scrutinee.free(scope, out);
                        for (p, body) in arms {
                            let depth = scope.len();
                            bound(p, scope);
                            statements(body, None, scope, out);
                            scope.truncate(depth);
                        }
                    }
                }
            }
            if let Some(r) = result {
                r.free(scope, out);
            }
            scope.truncate(depth);
        }
        match self {
            Term::Ref(n) => {
                let local =
                    matches!(n.components(), [single] if !n.rooted && scope.contains(single));
                if !local {
                    out.insert(n.clone());
                }
            }
            Term::Lambda { binders, body } | Term::Forall { binders, body } => {
                let depth = scope.len();
                for b in binders {
                    b.ty.free(scope, out);
                    scope.extend(b.name.clone());
                }
                body.free(scope, out);
                scope.truncate(depth);
            }
            Term::Let { binding, body } => {
                if let Some(t) = &binding.ty {
                    t.free(scope, out);
                }
                binding.value.free(scope, out);
                let depth = scope.len();
                bound(&binding.pattern, scope);
                body.free(scope, out);
                scope.truncate(depth);
            }
            Term::Match { scrutinee, arms } => {
                scrutinee.free(scope, out);
                for arm in arms {
                    let depth = scope.len();
                    bound(&arm.pattern, scope);
                    arm.body.free(scope, out);
                    scope.truncate(depth);
                }
            }
            Term::Do {
                statements: ss,
                result,
            } => statements(ss, Some(result), scope, out),
            Term::App {
                function,
                arguments,
            } => {
                function.free(scope, out);
                for a in arguments {
                    match a {
                        Argument::Positional(t) | Argument::Named { value: t, .. } => {
                            t.free(scope, out)
                        }
                    }
                }
            }
            Term::Arrow(a, b) => {
                a.free(scope, out);
                b.free(scope, out);
            }
            Term::ProductType(ts) | Term::Tuple(ts) | Term::List(ts) => {
                for t in ts {
                    t.free(scope, out);
                }
            }
            Term::If { condition, yes, no } => {
                condition.free(scope, out);
                yes.free(scope, out);
                no.free(scope, out);
            }
            Term::Project { base, .. } => base.free(scope, out),
            Term::Ascribe { value, ty } => {
                value.free(scope, out);
                ty.free(scope, out);
            }
            Term::Unary { value, .. } | Term::Eliminate { value } => value.free(scope, out),
            Term::Binary { left, right, .. } => {
                left.free(scope, out);
                right.free(scope, out);
            }
            Term::Record { fields } => {
                for (_, t) in fields {
                    t.free(scope, out);
                }
            }
            Term::Update { base, fields } => {
                base.free(scope, out);
                for (_, t) in fields {
                    t.free(scope, out);
                }
            }
            Term::Infer
            | Term::Sort(_)
            | Term::Nat(_)
            | Term::Bool(_)
            | Term::String(_)
            | Term::Char(_)
            | Term::Rfl
            | Term::ByDecide => {}
        }
    }
}

/// Every name a term mentions, in any position (heads, arguments, binder
/// types, patterns' constructor names excluded). Local binders are not
/// removed, so callers compare against names they know to be global.
impl Term {
    pub fn references(&self) -> std::collections::BTreeSet<Name> {
        let mut out = std::collections::BTreeSet::new();
        self.collect_references(&mut out);
        out
    }
    fn collect_references(&self, out: &mut std::collections::BTreeSet<Name>) {
        fn binders(bs: &[Binder], out: &mut std::collections::BTreeSet<Name>) {
            for b in bs {
                b.ty.collect_references(out);
            }
        }
        fn statements(ss: &[DoStatement], out: &mut std::collections::BTreeSet<Name>) {
            for s in ss {
                match s {
                    DoStatement::Let { binding, .. } => {
                        if let Some(t) = &binding.ty {
                            t.collect_references(out);
                        }
                        binding.value.collect_references(out);
                    }
                    DoStatement::Bind { action, .. } => action.collect_references(out),
                    DoStatement::Assign { value, .. } => value.collect_references(out),
                    DoStatement::Expr(t) => t.collect_references(out),
                    DoStatement::BindTyped { ty, action, .. } => {
                        ty.collect_references(out);
                        action.collect_references(out);
                    }
                    DoStatement::LetInstance { ty, value, .. } => {
                        ty.collect_references(out);
                        value.collect_references(out);
                    }
                    DoStatement::If { condition, yes, no } => {
                        condition.collect_references(out);
                        statements(yes, out);
                        if let Some(no) = no {
                            statements(no, out);
                        }
                    }
                    DoStatement::Match { scrutinee, arms } => {
                        scrutinee.collect_references(out);
                        for (_, body) in arms {
                            statements(body, out);
                        }
                    }
                }
            }
        }
        match self {
            Term::Infer
            | Term::Sort(_)
            | Term::Nat(_)
            | Term::Bool(_)
            | Term::String(_)
            | Term::Char(_)
            | Term::Rfl
            | Term::ByDecide => {}
            Term::Ref(n) => {
                out.insert(n.clone());
            }
            Term::App {
                function,
                arguments,
            } => {
                function.collect_references(out);
                for a in arguments {
                    match a {
                        Argument::Positional(t) | Argument::Named { value: t, .. } => {
                            t.collect_references(out)
                        }
                    }
                }
            }
            Term::Lambda { binders: bs, body } | Term::Forall { binders: bs, body } => {
                binders(bs, out);
                body.collect_references(out);
            }
            Term::Arrow(a, b) => {
                a.collect_references(out);
                b.collect_references(out);
            }
            Term::ProductType(ts) | Term::Tuple(ts) | Term::List(ts) => {
                for t in ts {
                    t.collect_references(out);
                }
            }
            Term::Let { binding, body } => {
                if let Some(t) = &binding.ty {
                    t.collect_references(out);
                }
                binding.value.collect_references(out);
                body.collect_references(out);
            }
            Term::If { condition, yes, no } => {
                condition.collect_references(out);
                yes.collect_references(out);
                no.collect_references(out);
            }
            Term::Match { scrutinee, arms } => {
                scrutinee.collect_references(out);
                for arm in arms {
                    arm.body.collect_references(out);
                }
            }
            Term::Project { base, .. } => base.collect_references(out),
            Term::Ascribe { value, ty } => {
                value.collect_references(out);
                ty.collect_references(out);
            }
            Term::Unary { value, .. } => value.collect_references(out),
            Term::Binary { left, right, .. } => {
                left.collect_references(out);
                right.collect_references(out);
            }
            Term::Record { fields } => {
                for (_, t) in fields {
                    t.collect_references(out);
                }
            }
            Term::Update { base, fields } => {
                base.collect_references(out);
                for (_, t) in fields {
                    t.collect_references(out);
                }
            }
            Term::Do {
                statements: ss,
                result,
            } => {
                statements(ss, out);
                result.collect_references(out);
            }
            Term::Eliminate { value } => value.collect_references(out),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefinitionKind {
    Def,
    NoncomputableDef,
    PartialDef,
    Abbrev,
    /// `@[simp] def`: unfolded by `simp` without being named.
    SimpDef,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Definition {
    pub name: Name,
    pub binders: Vec<Binder>,
    pub result: Term,
    pub body: Term,
    pub kind: DefinitionKind,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Constructor {
    pub name: Ident,
    pub binders: Vec<Binder>,
    pub result: Option<Term>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub name: Ident,
    pub ty: Term,
    pub default: Option<Term>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeDeclaration {
    Class {
        name: Name,
        parameters: Vec<Binder>,
        fields: Vec<Field>,
    },
    Inductive {
        name: Name,
        parameters: Vec<Binder>,
        sort: Term,
        constructors: Vec<Constructor>,
        deriving: Vec<Name>,
    },
    Structure {
        name: Name,
        parameters: Vec<Binder>,
        fields: Vec<Field>,
        deriving: Vec<Name>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instance {
    pub name: Option<Name>,
    pub binders: Vec<Binder>,
    pub ty: Term,
    pub body: Term,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeanOption {
    LinterUnusedVariables,
    MaxRecDepth,
    MaxHeartbeats,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptionValue {
    Bool(bool),
    Nat(u64),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Declaration {
    Instance(Instance),
    SetOption {
        option: LeanOption,
        value: OptionValue,
    },
    Definition(Definition),
    Type(TypeDeclaration),
    MutualDefinitions(Vec<Definition>),
    MutualTypes(Vec<TypeDeclaration>),
    Namespace {
        name: Name,
        declarations: Vec<Declaration>,
    },
    Theorem {
        name: Name,
        binders: Vec<Binder>,
        proposition: Term,
        proof: Term,
    },
    Example {
        binders: Vec<Binder>,
        proposition: Term,
        proof: Term,
    },
    Eval(Term),
    Check(Term),
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Module {
    pub imports: Vec<ModuleName>,
    pub declarations: Vec<Declaration>,
}
impl Module {
    pub fn render(&self) -> Result<String, Error> {
        let mut out = String::new();
        for import in &self.imports {
            out.push_str("import ");
            out.push_str(
                &import
                    .0
                    .iter()
                    .map(|part| format!("«{part}»"))
                    .collect::<Vec<_>>()
                    .join("."),
            );
            out.push('\n');
        }
        if !self.imports.is_empty() {
            out.push('\n');
        }
        for declaration in &self.declarations {
            Printer::declaration(declaration, 0, &mut out)?;
            out.push('\n');
        }
        Ok(out)
    }
}

struct Printer;
impl Printer {
    fn ident(value: &Ident) -> String {
        format!("«{}»", value.0)
    }
    fn name(value: &Name) -> String {
        format!(
            "{}{}",
            if value.rooted { "_root_." } else { "" },
            value
                .components
                .iter()
                .map(Self::ident)
                .collect::<Vec<_>>()
                .join(".")
        )
    }
    fn quoted(value: &str, delimiter: char) -> String {
        let mut out = String::new();
        out.push(delimiter);
        for c in value.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if c == delimiter => {
                    out.push('\\');
                    out.push(c);
                }
                c if c.is_control() && (c as u32) <= 255 => {
                    out.push_str(&format!("\\x{:02x}", c as u32))
                }
                '\u{2028}' | '\u{2029}' => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out.push(delimiter);
        out
    }
    fn binder(value: &Binder) -> Result<String, Error> {
        let ty = Self::term(&value.ty)?;
        let body = match &value.name {
            Some(name) => format!("{} : {ty}", Self::ident(name)),
            None if value.kind == BinderKind::Instance => ty,
            None => format!("_ : {ty}"),
        };
        let (left, right) = match value.kind {
            BinderKind::Explicit => ('(', ')'),
            BinderKind::Implicit => ('{', '}'),
            BinderKind::Instance => ('[', ']'),
        };
        Ok(format!("{left}{body}{right}"))
    }
    fn binders(values: &[Binder]) -> Result<String, Error> {
        values
            .iter()
            .map(Self::binder)
            .collect::<Result<Vec<_>, _>>()
            .map(|values| values.join(" "))
    }
    fn pattern(value: &Pattern) -> Result<String, Error> {
        Ok(match value {
            Pattern::Wildcard => "_".into(),
            Pattern::Bind(name) => Self::ident(name),
            Pattern::Constructor { name, arguments } => {
                if arguments.is_empty() {
                    Self::name(name)
                } else {
                    format!(
                        "({} {})",
                        Self::name(name),
                        arguments
                            .iter()
                            .map(Self::pattern)
                            .collect::<Result<Vec<_>, _>>()?
                            .join(" ")
                    )
                }
            }
            Pattern::Tuple(values) => {
                Self::tuple_length(values.len())?;
                format!(
                    "({})",
                    values
                        .iter()
                        .map(Self::pattern)
                        .collect::<Result<Vec<_>, _>>()?
                        .join(", ")
                )
            }
            Pattern::Nat(value) => value.0.clone(),
            Pattern::Bool(value) => value.to_string(),
            Pattern::String(value) => Self::quoted(value, '"'),
            Pattern::Char(value) => Self::quoted(&value.to_string(), '\''),
            Pattern::Int(value) => value.to_string(),
            Pattern::Record { fields } => format!(
                "{{ {} }}",
                fields
                    .iter()
                    .map(|(name, pattern)| Ok(format!(
                        "{} := {}",
                        Self::ident(name),
                        Self::pattern(pattern)?
                    )))
                    .collect::<Result<Vec<_>, Error>>()?
                    .join(", ")
            ),
        })
    }
    fn tuple_length(length: usize) -> Result<(), Error> {
        if length == 1 {
            Err(Error("Lean has no one-element tuple syntax".into()))
        } else {
            Ok(())
        }
    }
    fn binding(value: &LetBinding) -> Result<String, Error> {
        Ok(format!(
            "{}{} := {}",
            Self::pattern(&value.pattern)?,
            value
                .ty
                .as_ref()
                .map(|ty| Self::term(ty).map(|ty| format!(" : {ty}")))
                .transpose()?
                .unwrap_or_default(),
            Self::term(&value.value)?
        ))
    }
    fn fields(values: &[(Ident, Term)]) -> Result<String, Error> {
        values
            .iter()
            .map(|(name, value)| Ok(format!("{} := {}", Self::ident(name), Self::term(value)?)))
            .collect::<Result<Vec<_>, _>>()
            .map(|parts| parts.join(", "))
    }
    fn term(value: &Term) -> Result<String, Error> {
        Ok(match value {
            Term::Infer => "_".into(),
            Term::Ref(name) => Self::name(name),
            Term::Sort(Sort::Prop) => "Prop".into(),
            Term::Sort(Sort::Type(0)) => "Type".into(),
            Term::Sort(Sort::Type(level)) => format!("(Type {level})"),
            Term::Nat(value) => value.0.clone(),
            Term::Bool(value) => value.to_string(),
            Term::String(value) => Self::quoted(value, '"'),
            Term::Char(value) => Self::quoted(&value.to_string(), '\''),
            Term::App {
                function,
                arguments,
            } => {
                if arguments.is_empty() {
                    Self::term(function)?
                } else {
                    let arguments = arguments
                        .iter()
                        .map(|arg| match arg {
                            Argument::Positional(term) => Self::term(term),
                            Argument::Named { name, value } => {
                                Ok(format!("({} := {})", Self::ident(name), Self::term(value)?))
                            }
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    format!("({} {})", Self::term(function)?, arguments.join(" "))
                }
            }
            Term::Lambda { binders, body } | Term::Forall { binders, body } => {
                if binders.is_empty() {
                    return Err(Error("lambda/forall requires at least one binder".into()));
                }
                let (keyword, separator) = if matches!(value, Term::Lambda { .. }) {
                    ("fun", "=>")
                } else {
                    ("∀", ",")
                };
                format!(
                    "({keyword} {} {separator} {})",
                    Self::binders(binders)?,
                    Self::term(body)?
                )
            }
            Term::Arrow(left, right) => format!("({} → {})", Self::term(left)?, Self::term(right)?),
            Term::ProductType(values) => {
                if values.len() < 2 {
                    return Err(Error("product type needs at least two elements".into()));
                }
                format!(
                    "({})",
                    values
                        .iter()
                        .map(Self::term)
                        .collect::<Result<Vec<_>, _>>()?
                        .join(" × ")
                )
            }
            Term::List(values) => format!(
                "[{}]",
                values
                    .iter()
                    .map(Self::term)
                    .collect::<Result<Vec<_>, _>>()?
                    .join(", ")
            ),
            Term::Tuple(values) => {
                Self::tuple_length(values.len())?;
                format!(
                    "({})",
                    values
                        .iter()
                        .map(Self::term)
                        .collect::<Result<Vec<_>, _>>()?
                        .join(", ")
                )
            }
            Term::Let { binding, body } => {
                format!("(let {}; {})", Self::binding(binding)?, Self::term(body)?)
            }
            Term::If { condition, yes, no } => format!(
                "(if {} then {} else {})",
                Self::term(condition)?,
                Self::term(yes)?,
                Self::term(no)?
            ),
            Term::Match { scrutinee, arms } => {
                if arms.is_empty() {
                    return Err(Error("empty match syntax is unsupported".into()));
                }
                format!(
                    "(match {} with {})",
                    Self::term(scrutinee)?,
                    arms.iter()
                        .map(|arm| Ok(format!(
                            "| {} => {}",
                            Self::pattern(&arm.pattern)?,
                            Self::term(&arm.body)?
                        )))
                        .collect::<Result<Vec<_>, Error>>()?
                        .join(" ")
                )
            }
            Term::Project { base, projection } => format!(
                "({}).{}",
                Self::term(base)?,
                match projection {
                    Projection::Field(name) => Self::ident(name),
                    Projection::Tuple(index) => index.to_string(),
                }
            ),
            Term::Ascribe { value, ty } => {
                format!("({} : {})", Self::term(value)?, Self::term(ty)?)
            }
            Term::Unary { op, value } => format!(
                "({}{})",
                match op {
                    UnaryOp::Neg => "-",
                    UnaryOp::Not => "¬ ",
                    UnaryOp::BoolNot => "!",
                },
                Self::term(value)?
            ),
            Term::Binary { op, left, right } => format!(
                "({} {} {})",
                Self::term(left)?,
                op.text(),
                Self::term(right)?
            ),
            Term::Record { fields } => format!("{{ {} }}", Self::fields(fields)?),
            Term::Update { base, fields } => {
                if fields.is_empty() {
                    return Err(Error("record update needs at least one field".into()));
                }
                format!("{{ {} with {} }}", Self::term(base)?, Self::fields(fields)?)
            }
            Term::Do { statements, result } => {
                format!("(do {})", Self::do_body(statements, result)?)
            }
            Term::Rfl => "rfl".into(),
            Term::ByDecide => "(by decide)".into(),
            Term::Eliminate { value } => format!("(nomatch {})", Self::term(value)?),
        })
    }
    // Lean's do elaborator threads surrounding mutable locals through doIf/doMatch.
    // Parenthesizing those actions as ordinary terms changes their elaboration context.
    fn do_action(value: &Term) -> Result<String, Error> {
        Ok(match value {
            Term::If { condition, yes, no } => format!(
                "if {} then {} else {}",
                Self::term(condition)?,
                Self::do_branch(yes)?,
                Self::do_branch(no)?
            ),
            Term::Match { scrutinee, arms } => {
                if arms.is_empty() {
                    return Err(Error("empty match syntax is unsupported".into()));
                }
                format!(
                    "match {} with {}",
                    Self::term(scrutinee)?,
                    arms.iter()
                        .map(|arm| {
                            Ok(format!(
                                "| {} => {}",
                                Self::pattern(&arm.pattern)?,
                                Self::do_branch(&arm.body)?
                            ))
                        })
                        .collect::<Result<Vec<_>, Error>>()?
                        .join(" ")
                )
            }
            Term::Do { statements, result } => format!("do {}", Self::do_body(statements, result)?),
            _ => Self::term(value)?,
        })
    }
    fn do_branch(value: &Term) -> Result<String, Error> {
        match value {
            Term::Do { statements, result } => Self::do_body(statements, result),
            _ => Ok(format!("{{ {} }}", Self::do_action(value)?)),
        }
    }
    fn do_body(statements: &[DoStatement], result: &Term) -> Result<String, Error> {
        let mut parts = statements
            .iter()
            .map(Self::do_statement)
            .collect::<Result<Vec<_>, _>>()?;
        parts.push(Self::do_action(result)?);
        Ok(format!("{{ {} }}", parts.join("; ")))
    }
    fn do_sequence(statements: &[DoStatement]) -> Result<String, Error> {
        let parts = statements
            .iter()
            .map(Self::do_statement)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(format!(
            "{{ {} }}",
            if parts.is_empty() {
                "pure ()".into()
            } else {
                parts.join("; ")
            }
        ))
    }
    fn do_statement(statement: &DoStatement) -> Result<String, Error> {
        Ok(match statement {
            DoStatement::Let { binding, mutable } => {
                if *mutable && !matches!(binding.pattern, Pattern::Bind(_)) {
                    return Err(Error(
                        "mutable do bindings require one local identifier".into(),
                    ));
                }
                format!(
                    "let {}{}",
                    if *mutable { "mut " } else { "" },
                    Self::binding(binding)?
                )
            }
            DoStatement::Bind { pattern, action } => {
                format!(
                    "let {} ← {}",
                    Self::pattern(pattern)?,
                    Self::do_action(action)?
                )
            }
            DoStatement::BindTyped {
                pattern,
                ty,
                action,
            } => format!(
                "let ({} : {}) ← {}",
                Self::pattern(pattern)?,
                Self::term(ty)?,
                Self::do_action(action)?
            ),
            DoStatement::LetInstance { name, ty, value } => format!(
                "letI {}: {} := {}",
                name.as_ref()
                    .map(|name| format!("{} ", Self::ident(name)))
                    .unwrap_or_default(),
                Self::term(ty)?,
                Self::term(value)?
            ),
            DoStatement::Assign { local, value } => {
                format!("{} := {}", Self::ident(local), Self::term(value)?)
            }
            DoStatement::Expr(term) => Self::do_action(term)?,
            DoStatement::If { condition, yes, no } => format!(
                "if {} then {}{}",
                Self::term(condition)?,
                Self::do_sequence(yes)?,
                no.as_ref()
                    .map(|no| Self::do_sequence(no).map(|body| format!(" else {body}")))
                    .transpose()?
                    .unwrap_or_default()
            ),
            DoStatement::Match { scrutinee, arms } => {
                if arms.is_empty() {
                    return Err(Error(
                        "empty statement match is unsupported; use nomatch".into(),
                    ));
                }
                format!(
                    "match {} with {}",
                    Self::term(scrutinee)?,
                    arms.iter()
                        .map(|(pattern, body)| Ok(format!(
                            "| {} => {}",
                            Self::pattern(pattern)?,
                            Self::do_sequence(body)?
                        )))
                        .collect::<Result<Vec<_>, Error>>()?
                        .join(" ")
                )
            }
        })
    }
    fn line(out: &mut String, indent: usize, value: &str) {
        out.push_str(&" ".repeat(indent));
        out.push_str(value);
        out.push('\n');
    }
    fn definition(value: &Definition, indent: usize, out: &mut String) -> Result<(), Error> {
        Self::line(
            out,
            indent,
            &format!(
                "{} {} {} : {} := {}",
                match value.kind {
                    DefinitionKind::Def => "def",
                    DefinitionKind::NoncomputableDef => "noncomputable def",
                    DefinitionKind::PartialDef => "partial def",
                    DefinitionKind::Abbrev => "abbrev",
                    DefinitionKind::SimpDef => "@[simp] def",
                },
                Self::name(&value.name),
                Self::binders(&value.binders)?,
                Self::term(&value.result)?,
                Self::term(&value.body)?
            ),
        );
        Ok(())
    }
    fn type_declaration(
        value: &TypeDeclaration,
        indent: usize,
        out: &mut String,
    ) -> Result<(), Error> {
        let deriving: &[Name] = match value {
            TypeDeclaration::Class {
                name,
                parameters,
                fields,
            } => {
                Self::line(
                    out,
                    indent,
                    &format!(
                        "class {} {} where",
                        Self::name(name),
                        Self::binders(parameters)?
                    ),
                );
                for field in fields {
                    Self::line(
                        out,
                        indent + 2,
                        &format!(
                            "{} : {}{}",
                            Self::ident(&field.name),
                            Self::term(&field.ty)?,
                            field
                                .default
                                .as_ref()
                                .map(|value| Self::term(value).map(|value| format!(" := {value}")))
                                .transpose()?
                                .unwrap_or_default()
                        ),
                    );
                }
                &[]
            }
            TypeDeclaration::Inductive {
                name,
                parameters,
                sort,
                constructors,
                deriving,
            } => {
                Self::line(
                    out,
                    indent,
                    &format!(
                        "inductive {} {} : {} where",
                        Self::name(name),
                        Self::binders(parameters)?,
                        Self::term(sort)?
                    ),
                );
                for constructor in constructors {
                    Self::line(
                        out,
                        indent + 2,
                        &format!(
                            "| {} {}{}",
                            Self::ident(&constructor.name),
                            Self::binders(&constructor.binders)?,
                            constructor
                                .result
                                .as_ref()
                                .map(|result| Self::term(result)
                                    .map(|result| format!(" : {result}")))
                                .transpose()?
                                .unwrap_or_default()
                        ),
                    );
                }
                deriving
            }
            TypeDeclaration::Structure {
                name,
                parameters,
                fields,
                deriving,
            } => {
                Self::line(
                    out,
                    indent,
                    &format!(
                        "structure {} {} where",
                        Self::name(name),
                        Self::binders(parameters)?
                    ),
                );
                for field in fields {
                    Self::line(
                        out,
                        indent + 2,
                        &format!(
                            "{} : {}{}",
                            Self::ident(&field.name),
                            Self::term(&field.ty)?,
                            field
                                .default
                                .as_ref()
                                .map(|value| Self::term(value).map(|value| format!(" := {value}")))
                                .transpose()?
                                .unwrap_or_default()
                        ),
                    );
                }
                deriving
            }
        };
        if !deriving.is_empty() {
            Self::line(
                out,
                indent + 2,
                &format!(
                    "deriving {}",
                    deriving
                        .iter()
                        .map(Self::name)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
        }
        Ok(())
    }
    fn declaration(value: &Declaration, indent: usize, out: &mut String) -> Result<(), Error> {
        match value {
            Declaration::Instance(instance) => Self::line(
                out,
                indent,
                &format!(
                    "instance {} {} : {} := {}",
                    instance.name.as_ref().map(Self::name).unwrap_or_default(),
                    Self::binders(&instance.binders)?,
                    Self::term(&instance.ty)?,
                    Self::term(&instance.body)?
                ),
            ),
            Declaration::SetOption { option, value } => {
                let (name, text) = match (option, value) {
                    (LeanOption::LinterUnusedVariables, OptionValue::Bool(value)) => {
                        ("linter.unusedVariables", value.to_string())
                    }
                    (LeanOption::MaxRecDepth, OptionValue::Nat(value)) => {
                        ("maxRecDepth", value.to_string())
                    }
                    (LeanOption::MaxHeartbeats, OptionValue::Nat(value)) => {
                        ("maxHeartbeats", value.to_string())
                    }
                    _ => return Err(Error("option value has the wrong fixed type".into())),
                };
                Self::line(out, indent, &format!("set_option {name} {text}"));
            }
            Declaration::Definition(value) => Self::definition(value, indent, out)?,
            Declaration::Type(value) => Self::type_declaration(value, indent, out)?,
            Declaration::MutualDefinitions(values) => {
                if values.is_empty() {
                    return Err(Error("empty mutual group".into()));
                }
                Self::line(out, indent, "mutual");
                for value in values {
                    Self::definition(value, indent + 2, out)?;
                }
                Self::line(out, indent, "end");
            }
            Declaration::MutualTypes(values) => {
                if values.is_empty() {
                    return Err(Error("empty mutual group".into()));
                }
                Self::line(out, indent, "mutual");
                for value in values {
                    Self::type_declaration(value, indent + 2, out)?;
                }
                Self::line(out, indent, "end");
            }
            Declaration::Namespace { name, declarations } => {
                if name.rooted {
                    return Err(Error(
                        "root-qualified namespace commands are unsupported".into(),
                    ));
                }
                Self::line(out, indent, &format!("namespace {}", Self::name(name)));
                for value in declarations {
                    Self::declaration(value, indent, out)?;
                }
                Self::line(out, indent, &format!("end {}", Self::name(name)));
            }
            Declaration::Theorem {
                name,
                binders,
                proposition,
                proof,
            } => Self::line(
                out,
                indent,
                &format!(
                    "theorem {} {} : {} := {}",
                    Self::name(name),
                    Self::binders(binders)?,
                    Self::term(proposition)?,
                    Self::term(proof)?
                ),
            ),
            Declaration::Example {
                binders,
                proposition,
                proof,
            } => Self::line(
                out,
                indent,
                &format!(
                    "example {} : {} := {}",
                    Self::binders(binders)?,
                    Self::term(proposition)?,
                    Self::term(proof)?
                ),
            ),
            Declaration::Eval(value) => {
                Self::line(out, indent, &format!("#eval {}", Self::term(value)?))
            }
            Declaration::Check(value) => {
                Self::line(out, indent, &format!("#check {}", Self::term(value)?))
            }
        }
        Ok(())
    }
}
