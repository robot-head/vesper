//! Per-function semantic models for proof lenses (§21.1.5, §21.3 Layer 5).
//!
//! The Lean extraction writes every source function as a `partial def`, so
//! the Lean kernel sees only opaque constants and can prove nothing about
//! their behaviour. Lens therefore builds its own small model of each
//! *scalar* source function (fixed-width integers and booleans; `let`, `if`,
//! checked and wrapping arithmetic, comparisons, short-circuit logic and calls
//! to other scalar functions, which are inlined) and states one theorem about
//! it:
//!
//! > for every input in range, evaluating the function under
//! > `vesper.arith.checked/1` raises no arithmetic fault: no
//! > `Fail[ArithOverflow]` and no `Fail[RuntimeError]::DivisionByZero`.
//!
//! Two Lean renderings come from the same model term:
//! - an exact-integer rendering whose theorem is proved by `omega` and is
//!   replayed by the kernel auditor (`vesper proof-audit`);
//! - a bit-vector rendering checked by `bv_decide`, used only when `omega`
//!   fails, to find a concrete counterexample. A `bv_decide` success relies
//!   on native evaluation (`Lean.ofReduceBool`) that the kernel auditor
//!   cannot replay, so it is reported separately and never as "proved".
//!
//! These are theorems about this model of the source function. They are not
//! theorems about the Rust compiler, the Lean extraction or native code.
use crate::{ast::*, check::Checked};
use std::collections::BTreeMap;

const MAX_NODES: usize = 4096;
const MAX_INLINE_DEPTH: usize = 8;
const MAX_PARAMS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    U8,
    U32,
    U64,
    I64,
    Bool,
    Unit,
}
impl Kind {
    fn of(ty: &Type) -> Option<Self> {
        match ty {
            Type::Named(name, args) if args.is_empty() => match name.as_str() {
                "u8" => Some(Self::U8),
                "u32" => Some(Self::U32),
                "u64" => Some(Self::U64),
                "i64" => Some(Self::I64),
                "bool" | "Bool" => Some(Self::Bool),
                "Unit" => Some(Self::Unit),
                _ => None,
            },
            Type::Tuple(items) if items.is_empty() => Some(Self::Unit),
            _ => None,
        }
    }
    fn integer(self) -> bool {
        matches!(self, Self::U8 | Self::U32 | Self::U64 | Self::I64)
    }
    fn signed(self) -> bool {
        self == Self::I64
    }
    fn width(self) -> u32 {
        match self {
            Self::U8 => 8,
            Self::U32 => 32,
            Self::U64 | Self::I64 => 64,
            Self::Bool => 1,
            Self::Unit => 0,
        }
    }
    fn range(self) -> (i128, i128) {
        match self {
            Self::U8 => (0, u8::MAX as i128),
            Self::U32 => (0, u32::MAX as i128),
            Self::U64 => (0, u64::MAX as i128),
            Self::I64 => (i64::MIN as i128, i64::MAX as i128),
            Self::Bool => (0, 1),
            Self::Unit => (0, 0),
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::U8 => "u8",
            Self::U32 => "u32",
            Self::U64 => "u64",
            Self::I64 => "i64",
            Self::Bool => "bool",
            Self::Unit => "Unit",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arith {
    Add,
    Sub,
    Mul,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cmp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// The model term. Variables are indices into the model's variable table.
#[derive(Clone, Debug)]
enum Term {
    Var(usize),
    Int(i128, Kind),
    Bool(bool),
    Unit,
    Arith {
        op: Arith,
        a: Box<Term>,
        b: Box<Term>,
        kind: Kind,
        checked: bool,
    },
    Div {
        a: Box<Term>,
        b: Box<Term>,
        kind: Kind,
        rem: bool,
    },
    Neg(Box<Term>, Kind),
    Not(Box<Term>),
    Cmp {
        op: Cmp,
        a: Box<Term>,
        b: Box<Term>,
        kind: Kind,
    },
    And(Box<Term>, Box<Term>),
    Or(Box<Term>, Box<Term>),
    Ite {
        c: Box<Term>,
        t: Box<Term>,
        e: Box<Term>,
        kind: Kind,
    },
    /// Evaluate `value` into variable `var`, then `body`.
    Let {
        var: usize,
        value: Box<Term>,
        body: Box<Term>,
    },
}

/// A Lean-ready semantic model of one source function.
#[derive(Clone, Debug)]
pub(crate) struct Model {
    pub function: String,
    /// Exact-integer module (theorem proved by `omega`), without its header.
    pub exact: String,
    /// Bit-vector module (`bv_decide` counterexample search), without header.
    pub bitvec: String,
    /// blake3 of both renderings: identical models share proof outcomes.
    pub key: [u8; 32],
    /// Parameter names and kinds, for rendering counterexamples.
    pub params: Vec<(String, Kind)>,
}
impl Model {
    pub(crate) fn module_name(&self) -> String {
        format!("VesperLensModel_{}", &hex(&self.key)[..24])
    }
    pub(crate) fn theorem(&self) -> String {
        format!("{}.no_arith_fault", self.module_name())
    }
    pub(crate) fn exact_module(&self) -> String {
        format!(
            "namespace {}\n{}end {}\n",
            self.module_name(),
            self.exact,
            self.module_name()
        )
    }
    pub(crate) fn bitvec_module(&self) -> String {
        format!(
            "import Std.Tactic.BVDecide\nnamespace {}\n{}end {}\n",
            self.module_name(),
            self.bitvec,
            self.module_name()
        )
    }
    /// Render a `bv_decide` assignment (`x = 0x..#64`) as source values.
    pub(crate) fn counterexample(&self, lean_output: &str) -> Option<String> {
        let start = lean_output.find("found a counterexample")?;
        let mut values = Vec::new();
        for line in lean_output[start..].lines().skip(1) {
            let Some((name, value)) = line.split_once(" = ") else {
                if values.is_empty() {
                    continue;
                }
                break;
            };
            let name = name.trim();
            let Some((_, kind)) = self.params.iter().find(|(p, _)| p == name) else {
                continue;
            };
            values.push(format!("{name} = {}", render_value(value.trim(), *kind)));
            if values.len() >= MAX_PARAMS {
                break;
            }
        }
        (!values.is_empty()).then(|| values.join(", "))
    }
}

fn render_value(text: &str, kind: Kind) -> String {
    if kind == Kind::Bool {
        return text.to_owned();
    }
    let digits = text.split('#').next().unwrap_or(text);
    let parsed = digits
        .strip_prefix("0x")
        .map(|hex| u128::from_str_radix(hex, 16))
        .unwrap_or_else(|| digits.parse::<u128>());
    match parsed {
        Ok(bits) if kind.signed() => (bits as u64 as i64).to_string(),
        Ok(bits) => bits.to_string(),
        Err(_) => text.to_owned(),
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Build the semantic model of `name`, or the reason there is none.
pub(crate) fn model(checked: &Checked, name: &str) -> Result<Model, String> {
    let world = &checked.world;
    let f = world
        .functions
        .get(name)
        .ok_or_else(|| format!("function '{name}' is not checked"))?;
    let mut builder = Builder {
        checked,
        vars: Vec::new(),
        nodes: 0,
        stack: vec![name.to_owned()],
    };
    let (params, result, body) = builder.signature(f)?;
    let mut scope = Scope::default();
    let mut names = Vec::new();
    for (param, kind) in f.params.iter().zip(&params) {
        let var = builder.var(&param.name, *kind, true);
        scope.bind(&param.name, var);
        names.push((sanitize(&param.name), *kind));
    }
    let term = builder.expr(body, &mut scope, Some(result))?;
    let (exact, bitvec) = (
        Exact::render(&builder.vars, &term),
        BitVecs::render(&builder.vars, &term, &names),
    );
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"vesper.lens.model/1\0");
    hasher.update(exact.as_bytes());
    hasher.update(b"\0");
    hasher.update(bitvec.as_bytes());
    Ok(Model {
        function: name.to_owned(),
        exact,
        bitvec,
        key: *hasher.finalize().as_bytes(),
        params: names,
    })
}

#[derive(Clone, Debug)]
struct VarInfo {
    name: String,
    kind: Kind,
    param: bool,
}

#[derive(Default, Clone)]
struct Scope {
    frames: Vec<Vec<(String, usize)>>,
}
impl Scope {
    fn bind(&mut self, name: &str, var: usize) {
        if self.frames.is_empty() {
            self.frames.push(Vec::new());
        }
        self.frames
            .last_mut()
            .expect("scope frame")
            .push((name.to_owned(), var));
    }
    fn lookup(&self, name: &str) -> Option<(usize, usize)> {
        self.frames.iter().enumerate().rev().find_map(|(depth, f)| {
            f.iter()
                .rev()
                .find(|(n, _)| n == name)
                .map(|(_, v)| (*v, depth))
        })
    }
}

struct Builder<'a> {
    checked: &'a Checked,
    vars: Vec<VarInfo>,
    nodes: usize,
    stack: Vec<String>,
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

impl Builder<'_> {
    fn charge(&mut self) -> Result<(), String> {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err("the model exceeds the Lens node budget".into());
        }
        Ok(())
    }
    fn var(&mut self, name: &str, kind: Kind, param: bool) -> usize {
        self.vars.push(VarInfo {
            name: sanitize(name),
            kind,
            param,
        });
        self.vars.len() - 1
    }
    fn signature<'f>(&self, f: &'f Function) -> Result<(Vec<Kind>, Kind, &'f Expr), String> {
        if !f.generics.is_empty() {
            return Err("generic functions have no scalar model".into());
        }
        if f.params.len() > MAX_PARAMS {
            return Err("too many parameters for a scalar model".into());
        }
        for effect in &f.effects.instances {
            let allowed = effect.name == "Fail"
                && effect
                    .args
                    .iter()
                    .all(|a| matches!(a.name(), "ArithOverflow" | "RuntimeError"));
            if !allowed {
                return Err(format!(
                    "effect '{}' is outside the scalar model (only Fail[ArithOverflow] and Fail[RuntimeError])",
                    effect.name
                ));
            }
        }
        if f.effects.tail.is_some() {
            return Err("open effect rows are outside the scalar model".into());
        }
        let mut params = Vec::new();
        for p in &f.params {
            if p.pass != Pass::In || !p.projections.is_empty() {
                return Err(format!(
                    "parameter '{}' is not an in-mode scalar parameter",
                    p.name
                ));
            }
            match Kind::of(&p.ty) {
                Some(kind) if kind != Kind::Unit => params.push(kind),
                _ => {
                    return Err(format!(
                        "parameter '{}' has type {} outside the scalar model",
                        p.name, p.ty
                    ));
                }
            }
        }
        let result = if f.result == Type::Infer {
            Kind::Unit
        } else {
            Kind::of(&f.result)
                .ok_or_else(|| format!("result type {} is outside the scalar model", f.result))?
        };
        let body = f.body.as_ref().ok_or("the function has no body")?;
        Ok((params, result, body))
    }

    fn kind_at(&self, e: &Expr) -> Option<Kind> {
        self.checked
            .expression_types
            .get(&(e.span.start, e.span.end))
            .and_then(Kind::of)
    }

    fn expr(&mut self, e: &Expr, scope: &mut Scope, want: Option<Kind>) -> Result<Term, String> {
        self.charge()?;
        if crate::lsp::request_cancelled() {
            return Err("cancelled".into());
        }
        let unsupported = |what: &str| Err(format!("{what} is outside the scalar model"));
        match &e.kind {
            ExprKind::Literal(Literal::Int(n)) => {
                let kind = self
                    .kind_at(e)
                    .filter(|k| k.integer())
                    .or(want.filter(|k| k.integer()))
                    .unwrap_or(Kind::I64);
                let (lo, hi) = kind.range();
                if *n < lo || *n > hi {
                    return Err(format!("literal {n} does not fit {}", kind.name()));
                }
                Ok(Term::Int(*n, kind))
            }
            ExprKind::Literal(Literal::Bool(b)) => Ok(Term::Bool(*b)),
            ExprKind::Literal(_) => unsupported("a non-integer literal"),
            ExprKind::Tuple(items) if items.is_empty() => Ok(Term::Unit),
            ExprKind::Name(n) => match scope.lookup(n) {
                Some((var, _)) => Ok(Term::Var(var)),
                None => Err(format!("'{n}' is not a scalar local")),
            },
            ExprKind::Block { statements, tail } => {
                scope.frames.push(Vec::new());
                let result = self.block(statements, tail.as_deref(), scope, want);
                scope.frames.pop();
                result
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => {
                let c = self.expr(condition, scope, Some(Kind::Bool))?;
                let t = self.expr(then, &mut scope.clone(), want)?;
                let (e2, kind) = match otherwise {
                    Some(otherwise) => {
                        let kind = self.kind_of(&t).or(want);
                        let e2 = self.expr(otherwise, &mut scope.clone(), kind)?;
                        let kind = self.kind_of(&t).or(self.kind_of(&e2));
                        (e2, kind)
                    }
                    None => (Term::Unit, Some(Kind::Unit)),
                };
                Ok(Term::Ite {
                    c: Box::new(c),
                    t: Box::new(t),
                    e: Box::new(e2),
                    kind: kind.or(want).unwrap_or(Kind::Unit),
                })
            }
            ExprKind::Unary(op, x) => {
                let x = self.expr(x, scope, want)?;
                let kind = self.kind_of(&x).ok_or("untyped operand")?;
                match (op.as_str(), kind) {
                    ("!", Kind::Bool) => Ok(Term::Not(Box::new(x))),
                    ("-", k) if k.integer() => Ok(Term::Neg(Box::new(x), k)),
                    _ => unsupported(&format!("unary '{op}' on {}", kind.name())),
                }
            }
            ExprKind::Binary(op, a, b) => self.binary(op, a, b, scope),
            ExprKind::Call {
                callee,
                types,
                args,
            } => {
                let ExprKind::Name(name) = &callee.kind else {
                    return unsupported("a call through a function value");
                };
                if !types.is_empty() || scope.lookup(name).is_some() {
                    return unsupported("a generic or local function call");
                }
                if args.iter().any(|a| a.explicit && a.pass != Pass::In) {
                    return unsupported("an inout or sink argument");
                }
                if let Some(op) = name.strip_prefix("wrapping_") {
                    let op = match op {
                        "add" => Arith::Add,
                        "sub" => Arith::Sub,
                        "mul" => Arith::Mul,
                        _ => return unsupported(&format!("intrinsic '{name}'")),
                    };
                    let [x, y] = args.as_slice() else {
                        return unsupported("a wrapping call without two arguments");
                    };
                    let kind = self.kind_at(e).filter(|k| k.integer()).or(want);
                    let (x, y, kind) = self.operands(&x.expr, &y.expr, scope, kind)?;
                    return Ok(Term::Arith {
                        op,
                        a: Box::new(x),
                        b: Box::new(y),
                        kind,
                        checked: false,
                    });
                }
                self.inline(name, args, scope)
            }
            ExprKind::Assign(..) => unsupported("an assignment outside a block statement position"),
            ExprKind::Match { .. } => unsupported("a match expression"),
            ExprKind::Handle { .. } => unsupported("a handler"),
            ExprKind::Closure { .. } => unsupported("a closure"),
            ExprKind::Struct { .. } | ExprKind::Tuple(_) => unsupported("an aggregate value"),
            ExprKind::Field(..) | ExprKind::Index(..) => unsupported("a projection"),
            ExprKind::Try(_) => unsupported("'?'"),
        }
    }

    fn kind_of(&self, t: &Term) -> Option<Kind> {
        Some(match t {
            Term::Var(v) => self.vars[*v].kind,
            Term::Int(_, k) => *k,
            Term::Bool(_) | Term::Not(_) | Term::Cmp { .. } | Term::And(..) | Term::Or(..) => {
                Kind::Bool
            }
            Term::Unit => Kind::Unit,
            Term::Arith { kind, .. }
            | Term::Div { kind, .. }
            | Term::Neg(_, kind)
            | Term::Ite { kind, .. } => *kind,
            Term::Let { body, .. } => return self.kind_of(body),
        })
    }

    /// Translate two operands of one integer kind; a literal takes the kind
    /// of the other side.
    fn operands(
        &mut self,
        a: &Expr,
        b: &Expr,
        scope: &mut Scope,
        want: Option<Kind>,
    ) -> Result<(Term, Term, Kind), String> {
        let literal = |e: &Expr| matches!(e.kind, ExprKind::Literal(Literal::Int(_)));
        let hint = self
            .kind_at(a)
            .or(self.kind_at(b))
            .filter(|k| k.integer() || *k == Kind::Bool)
            .or(want);
        let (x, y) = if literal(a) && !literal(b) {
            let y = self.expr(b, scope, hint)?;
            let k = self.kind_of(&y);
            (self.expr(a, scope, k)?, y)
        } else {
            let x = self.expr(a, scope, hint)?;
            let k = self.kind_of(&x);
            (x, self.expr(b, scope, k)?)
        };
        let (kx, ky) = (self.kind_of(&x), self.kind_of(&y));
        match (kx, ky) {
            (Some(k), Some(k2)) if k == k2 => Ok((x, y, k)),
            _ => Err("operands of different kinds are outside the scalar model".into()),
        }
    }

    fn binary(&mut self, op: &str, a: &Expr, b: &Expr, scope: &mut Scope) -> Result<Term, String> {
        match op {
            "&&" | "||" => {
                let x = self.expr(a, scope, Some(Kind::Bool))?;
                let y = self.expr(b, scope, Some(Kind::Bool))?;
                if self.kind_of(&x) != Some(Kind::Bool) || self.kind_of(&y) != Some(Kind::Bool) {
                    return Err("logical operands must be bool".into());
                }
                Ok(if op == "&&" {
                    Term::And(Box::new(x), Box::new(y))
                } else {
                    Term::Or(Box::new(x), Box::new(y))
                })
            }
            "==" | "!=" | "<" | "<=" | ">" | ">=" => {
                let (x, y, kind) = self.operands(a, b, scope, None)?;
                let cmp = match op {
                    "==" => Cmp::Eq,
                    "!=" => Cmp::Ne,
                    "<" => Cmp::Lt,
                    "<=" => Cmp::Le,
                    ">" => Cmp::Gt,
                    _ => Cmp::Ge,
                };
                if kind == Kind::Unit || (kind == Kind::Bool && !matches!(cmp, Cmp::Eq | Cmp::Ne)) {
                    return Err(format!("'{op}' on {} is outside the model", kind.name()));
                }
                Ok(Term::Cmp {
                    op: cmp,
                    a: Box::new(x),
                    b: Box::new(y),
                    kind,
                })
            }
            "+" | "-" | "*" | "/" | "%" => {
                let (x, y, kind) = self.operands(a, b, scope, None)?;
                if !kind.integer() {
                    return Err(format!("'{op}' on {} is outside the model", kind.name()));
                }
                let (x, y) = (Box::new(x), Box::new(y));
                Ok(match op {
                    "+" | "-" | "*" => Term::Arith {
                        op: match op {
                            "+" => Arith::Add,
                            "-" => Arith::Sub,
                            _ => Arith::Mul,
                        },
                        a: x,
                        b: y,
                        kind,
                        checked: true,
                    },
                    _ => Term::Div {
                        a: x,
                        b: y,
                        kind,
                        rem: op == "%",
                    },
                })
            }
            _ => Err(format!("operator '{op}' is outside the scalar model")),
        }
    }

    fn block(
        &mut self,
        statements: &[Statement],
        tail: Option<&Expr>,
        scope: &mut Scope,
        want: Option<Kind>,
    ) -> Result<Term, String> {
        let Some((first, rest)) = statements.split_first() else {
            return match tail {
                Some(tail) => self.expr(tail, scope, want),
                None => Ok(Term::Unit),
            };
        };
        let depth = scope.frames.len() - 1;
        let (name, value) = match first {
            Statement::Let { pat, ty, value } => {
                let PatternKind::Bind(name) = &pat.kind else {
                    return Err("a destructuring let is outside the scalar model".into());
                };
                let declared = if *ty == Type::Infer {
                    None
                } else {
                    Some(Kind::of(ty).ok_or("a let of non-scalar type")?)
                };
                (Some(name.clone()), self.expr(value, scope, declared)?)
            }
            Statement::Expr(Expr {
                kind: ExprKind::Assign(op, target, value),
                ..
            }) => {
                let ExprKind::Name(name) = &target.kind else {
                    return Err("assignment to a projection is outside the model".into());
                };
                let Some((old, at)) = scope.lookup(name) else {
                    return Err(format!("'{name}' is not a scalar local"));
                };
                if at != depth || self.vars[old].param {
                    return Err(format!(
                        "assignment to '{name}' outside its own block is outside the model"
                    ));
                }
                let kind = self.vars[old].kind;
                let rhs = self.expr(value, scope, Some(kind))?;
                let value = match op.as_str() {
                    "=" => rhs,
                    "+=" | "-=" | "*=" => Term::Arith {
                        op: match op.as_str() {
                            "+=" => Arith::Add,
                            "-=" => Arith::Sub,
                            _ => Arith::Mul,
                        },
                        a: Box::new(Term::Var(old)),
                        b: Box::new(rhs),
                        kind,
                        checked: true,
                    },
                    "/=" | "%=" => Term::Div {
                        a: Box::new(Term::Var(old)),
                        b: Box::new(rhs),
                        kind,
                        rem: op == "%=",
                    },
                    _ => return Err(format!("assignment '{op}' is outside the model")),
                };
                (Some(name.clone()), value)
            }
            Statement::Expr(x) => (None, self.expr(x, scope, None)?),
        };
        let kind = self.kind_of(&value).unwrap_or(Kind::Unit);
        let var = self.var(name.as_deref().unwrap_or("_"), kind, false);
        if let Some(name) = &name {
            scope.bind(name, var);
        }
        let body = self.block(rest, tail, scope, want)?;
        Ok(Term::Let {
            var,
            value: Box::new(value),
            body: Box::new(body),
        })
    }

    fn inline(&mut self, name: &str, args: &[Arg], scope: &mut Scope) -> Result<Term, String> {
        if self.stack.iter().any(|f| f == name) {
            return Err(format!("recursive call to '{name}' is outside the model"));
        }
        if self.stack.len() > MAX_INLINE_DEPTH {
            return Err("calls nest too deeply for the model".into());
        }
        let checked = self.checked;
        let f = checked
            .world
            .functions
            .get(name)
            .ok_or_else(|| format!("call to '{name}' is outside the scalar model"))?;
        let (params, result, body) = self
            .signature(f)
            .map_err(|reason| format!("callee '{name}': {reason}"))?;
        if args.len() != params.len() {
            return Err(format!("call to '{name}' has the wrong arity"));
        }
        let mut values = Vec::new();
        for (arg, kind) in args.iter().zip(&params) {
            let value = self.expr(&arg.expr, scope, Some(*kind))?;
            if self.kind_of(&value) != Some(*kind) {
                return Err(format!("argument to '{name}' has the wrong kind"));
            }
            values.push(value);
        }
        // The callee sees only its parameters.
        let mut inner = Scope::default();
        let mut vars = Vec::new();
        for (p, kind) in f.params.iter().zip(&params) {
            let var = self.var(&format!("{}_{}", sanitize(name), p.name), *kind, false);
            inner.bind(&p.name, var);
            vars.push(var);
        }
        self.stack.push(name.to_owned());
        let body = self.expr(body, &mut inner, Some(result));
        self.stack.pop();
        let mut term = body?;
        for (var, value) in vars.into_iter().zip(values).rev() {
            term = Term::Let {
                var,
                value: Box::new(value),
                body: Box::new(term),
            };
        }
        Ok(term)
    }
}

/// Exact-integer rendering: every value is an `Int`; booleans are 0/1.
/// Each non-atomic value is named by a variable with a *total* defining
/// constraint, so the universally quantified theorem is equivalent to "no
/// fault for any input in range".
struct Exact<'a> {
    vars: &'a [VarInfo],
    temps: usize,
    binders: Vec<String>,
    defs: Vec<String>,
}
impl<'a> Exact<'a> {
    fn render(vars: &'a [VarInfo], term: &Term) -> String {
        let mut this = Self {
            vars,
            temps: 0,
            binders: Vec::new(),
            defs: Vec::new(),
        };
        let mut ranges = Vec::new();
        for (i, v) in vars.iter().enumerate() {
            if v.param {
                this.binders.push(this.name(i));
                ranges.push(range_prop(&this.name(i), v.kind));
            }
        }
        let (_, fault) = this.emit(term);
        let mut out = String::from("theorem no_arith_fault :");
        if !this.binders.is_empty() {
            // `∀,` with no binders is not Lean syntax.
            out.push_str(" ∀");
            for binder in &this.binders {
                out.push_str(&format!(" ({binder} : Int)"));
            }
            out.push(',');
        }
        out.push('\n');
        for hypothesis in ranges.iter().chain(&this.defs) {
            out.push_str(&format!("    ({hypothesis}) →\n"));
        }
        // `¬ False` has no arithmetic for omega; close it directly.
        let tactic = if fault == "False" { "simp" } else { "omega" };
        out.push_str(&format!("    ¬ ({fault}) := by\n  intros\n  {tactic}\n"));
        out
    }
    fn name(&self, var: usize) -> String {
        if self.vars[var].param {
            format!("«{}»", self.vars[var].name)
        } else {
            format!("«$v{var}»")
        }
    }
    fn temp(&mut self) -> String {
        self.temps += 1;
        let name = format!("«$t{}»", self.temps);
        self.binders.push(name.clone());
        name
    }
    fn cond(&mut self, value: &str) -> String {
        format!("{value} = 1")
    }
    /// (value term, fault proposition)
    fn emit(&mut self, term: &Term) -> (String, String) {
        match term {
            Term::Var(v) => (self.name(*v), "False".into()),
            Term::Int(n, _) => (format!("({n} : Int)"), "False".into()),
            Term::Bool(b) => ((if *b { "1" } else { "0" }).into(), "False".into()),
            Term::Unit => ("0".into(), "False".into()),
            Term::Arith {
                op,
                a,
                b,
                kind,
                checked,
            } => {
                let (x, fx) = self.emit(a);
                let (y, fy) = self.emit(b);
                let symbol = match op {
                    Arith::Add => "+",
                    Arith::Sub => "-",
                    Arith::Mul => "*",
                };
                let t = self.temp();
                let exact = format!("{x} {symbol} {y}");
                if *checked {
                    self.defs.push(format!("{t} = {exact}"));
                    let overflow = format!("¬ ({})", range_prop(&t, *kind));
                    (t, any([fx, fy, overflow]))
                } else {
                    let modulus = 1i128 << kind.width();
                    let wrapped = if kind.signed() {
                        let half = modulus / 2;
                        format!("(({exact}) + {half}) % {modulus} - {half}")
                    } else {
                        format!("({exact}) % {modulus}")
                    };
                    self.defs.push(format!("{t} = {wrapped}"));
                    (t, any([fx, fy]))
                }
            }
            Term::Div { a, b, kind, rem } => {
                let (x, fx) = self.emit(a);
                let (y, fy) = self.emit(b);
                let t = self.temp();
                let (lo, _) = kind.range();
                let mut faults = vec![fx, fy, format!("{y} = 0")];
                if kind.signed() && !*rem {
                    faults.push(format!("({x} = ({lo} : Int) ∧ {y} = -1)"));
                }
                // Truncated division only for a nonzero literal divisor;
                // otherwise the value stays an unconstrained variable and the
                // theorem is about faults only (omega cannot divide by a
                // variable; such goals usually fall back to bv_decide).
                if let Term::Int(c, _) = b.as_ref()
                    && *c != 0
                {
                    let m = c.abs();
                    let q = self.temp();
                    self.defs.push(if *c > 0 {
                        format!("({x} ≥ 0 ∧ {q} = {x} / {m}) ∨ ({x} < 0 ∧ {q} = -((-{x}) / {m}))")
                    } else {
                        format!("({x} ≥ 0 ∧ {q} = -({x} / {m})) ∨ ({x} < 0 ∧ {q} = (-{x}) / {m})")
                    });
                    if *rem {
                        self.defs.push(format!("{t} = {x} - ({c} : Int) * {q}"));
                    } else {
                        self.defs.push(format!("{t} = {q}"));
                    }
                }
                (t, any(faults))
            }
            Term::Neg(a, kind) => {
                let (x, fx) = self.emit(a);
                let t = self.temp();
                self.defs.push(format!("{t} = 0 - {x}"));
                let (lo, _) = kind.range();
                let fault = if kind.signed() {
                    format!("{x} = ({lo} : Int)")
                } else {
                    format!("{x} ≠ 0")
                };
                (t, any([fx, fault]))
            }
            Term::Not(a) => {
                let (x, fx) = self.emit(a);
                let t = self.temp();
                self.defs.push(format!("{t} = 1 - {x}"));
                (t, fx)
            }
            Term::Cmp { op, a, b, .. } => {
                let (x, fx) = self.emit(a);
                let (y, fy) = self.emit(b);
                let symbol = match op {
                    Cmp::Eq => "=",
                    Cmp::Ne => "≠",
                    Cmp::Lt => "<",
                    Cmp::Le => "≤",
                    Cmp::Gt => ">",
                    Cmp::Ge => "≥",
                };
                let t = self.temp();
                let p = format!("{x} {symbol} {y}");
                self.defs
                    .push(format!("(({p}) ∧ {t} = 1) ∨ (¬ ({p}) ∧ {t} = 0)"));
                (t, any([fx, fy]))
            }
            Term::And(a, b) | Term::Or(a, b) => {
                let and = matches!(term, Term::And(..));
                let (x, fx) = self.emit(a);
                let (y, fy) = self.emit(b);
                let t = self.temp();
                let (cx, cy) = (self.cond(&x), self.cond(&y));
                let p = if and {
                    format!("{cx} ∧ {cy}")
                } else {
                    format!("{cx} ∨ {cy}")
                };
                self.defs
                    .push(format!("(({p}) ∧ {t} = 1) ∨ (¬ ({p}) ∧ {t} = 0)"));
                // Short circuit: the right operand runs only when needed.
                let runs = if and { cx } else { format!("¬ {cx}") };
                (t, any([fx, both(&runs, fy)]))
            }
            Term::Ite { c, t, e, .. } => {
                let (x, fc) = self.emit(c);
                let (vt, ft) = self.emit(t);
                let (ve, fe) = self.emit(e);
                let v = self.temp();
                let cx = self.cond(&x);
                self.defs
                    .push(format!("({cx} ∧ {v} = {vt}) ∨ (¬ {cx} ∧ {v} = {ve})"));
                (v, any([fc, both(&cx, ft), both(&format!("¬ {cx}"), fe)]))
            }
            Term::Let { var, value, body } => {
                let (x, fx) = self.emit(value);
                let name = self.name(*var);
                self.binders.push(name.clone());
                self.defs.push(format!("{name} = {x}"));
                let (v, fb) = self.emit(body);
                (v, any([fx, fb]))
            }
        }
    }
}

/// A disjunction without `False` atoms (omega needs linear atoms only).
fn any(parts: impl IntoIterator<Item = String>) -> String {
    let parts: Vec<String> = parts.into_iter().filter(|p| p != "False").collect();
    match parts.len() {
        0 => "False".into(),
        1 => parts.into_iter().next().expect("one part"),
        _ => parts
            .iter()
            .map(|p| format!("({p})"))
            .collect::<Vec<_>>()
            .join(" ∨ "),
    }
}

/// `condition ∧ fault`, or `False` when the fault cannot happen.
fn both(condition: &str, fault: String) -> String {
    if fault == "False" {
        fault
    } else {
        format!("{condition} ∧ ({fault})")
    }
}

fn range_prop(term: &str, kind: Kind) -> String {
    let (lo, hi) = kind.range();
    format!("({lo} : Int) ≤ {term} ∧ {term} ≤ {hi}")
}

/// Bit-vector rendering for `bv_decide`: functional `let`/`bif` terms.
struct BitVecs<'a> {
    vars: &'a [VarInfo],
}
impl<'a> BitVecs<'a> {
    fn render(vars: &'a [VarInfo], term: &Term, params: &[(String, Kind)]) -> String {
        let this = Self { vars };
        let binders: String = params
            .iter()
            .map(|(name, kind)| format!(" («{name}» : {})", bv_type(*kind)))
            .collect();
        let names: Vec<String> = params.iter().map(|(n, _)| format!("«{n}»")).collect();
        let (quantifier, intro) = if names.is_empty() {
            (String::new(), String::new())
        } else {
            (
                format!("∀{binders}, "),
                format!("  intro {}\n", names.join(" ")),
            )
        };
        let fault = this.fault(term);
        // `simp only [fault]` already closes a literally fault-free body.
        let finish = if fault == "false" {
            ""
        } else {
            "  bv_decide\n"
        };
        format!(
            "def fault{binders} : Bool :=\n  {fault}\ntheorem no_arith_fault_bv : {quantifier}fault {} = false := by\n{intro}  simp only [fault]\n{finish}",
            names.join(" "),
        )
    }
    fn name(&self, var: usize) -> String {
        if self.vars[var].param {
            format!("«{}»", self.vars[var].name)
        } else {
            format!("«$v{var}»")
        }
    }
    fn value(&self, term: &Term) -> String {
        match term {
            Term::Var(v) => self.name(*v),
            Term::Int(n, kind) => bv_literal(*n, kind.width()),
            Term::Bool(b) => b.to_string(),
            Term::Unit => "()".into(),
            Term::Arith { op, a, b, .. } => {
                let symbol = match op {
                    Arith::Add => "+",
                    Arith::Sub => "-",
                    Arith::Mul => "*",
                };
                format!("({} {symbol} {})", self.value(a), self.value(b))
            }
            Term::Div { a, b, kind, rem } => {
                let f = match (kind.signed(), rem) {
                    (true, false) => "BitVec.sdiv",
                    (true, true) => "BitVec.srem",
                    (false, false) => "BitVec.udiv",
                    (false, true) => "BitVec.umod",
                };
                format!("({f} {} {})", self.value(a), self.value(b))
            }
            Term::Neg(a, _) => format!("(-{})", self.value(a)),
            Term::Not(a) => format!("(!{})", self.value(a)),
            Term::Cmp { op, a, b, kind } => {
                let (x, y) = (self.value(a), self.value(b));
                let s = kind.signed();
                match op {
                    Cmp::Eq => format!("({x} == {y})"),
                    Cmp::Ne => format!("({x} != {y})"),
                    Cmp::Lt if s => format!("(BitVec.slt {x} {y})"),
                    Cmp::Le if s => format!("(BitVec.sle {x} {y})"),
                    Cmp::Gt if s => format!("(BitVec.slt {y} {x})"),
                    Cmp::Ge if s => format!("(BitVec.sle {y} {x})"),
                    Cmp::Lt => format!("(BitVec.ult {x} {y})"),
                    Cmp::Le => format!("(BitVec.ule {x} {y})"),
                    Cmp::Gt => format!("(BitVec.ult {y} {x})"),
                    Cmp::Ge => format!("(BitVec.ule {y} {x})"),
                }
            }
            Term::And(a, b) => format!("({} && {})", self.value(a), self.value(b)),
            Term::Or(a, b) => format!("({} || {})", self.value(a), self.value(b)),
            Term::Ite { c, t, e, .. } => format!(
                "(bif {} then {} else {})",
                self.value(c),
                self.value(t),
                self.value(e)
            ),
            Term::Let { var, value, body } => format!(
                "(let {} := {}; {})",
                self.name(*var),
                self.value(value),
                self.value(body)
            ),
        }
    }
    fn fault(&self, term: &Term) -> String {
        match term {
            Term::Var(_) | Term::Int(..) | Term::Bool(_) | Term::Unit => "false".into(),
            Term::Arith {
                op,
                a,
                b,
                kind,
                checked,
            } => {
                let inner = format!("({} || {})", self.fault(a), self.fault(b));
                if !checked {
                    return inner;
                }
                let (x, y) = (self.value(a), self.value(b));
                let w = kind.width();
                let overflow = match (op, kind.signed()) {
                    (Arith::Add, true) => format!(
                        "((BitVec.signExtend {} {x} + BitVec.signExtend {} {y}) != BitVec.signExtend {} ({x} + {y}))",
                        w + 1,
                        w + 1,
                        w + 1
                    ),
                    (Arith::Sub, true) => format!(
                        "((BitVec.signExtend {} {x} - BitVec.signExtend {} {y}) != BitVec.signExtend {} ({x} - {y}))",
                        w + 1,
                        w + 1,
                        w + 1
                    ),
                    (Arith::Mul, true) => format!(
                        "((BitVec.signExtend {} {x} * BitVec.signExtend {} {y}) != BitVec.signExtend {} ({x} * {y}))",
                        2 * w,
                        2 * w,
                        2 * w
                    ),
                    (Arith::Add, false) => format!("(BitVec.ult ({x} + {y}) {x})"),
                    (Arith::Sub, false) => format!("(BitVec.ult {x} {y})"),
                    (Arith::Mul, false) => format!(
                        "((BitVec.zeroExtend {} {x} * BitVec.zeroExtend {} {y}) != BitVec.zeroExtend {} ({x} * {y}))",
                        2 * w,
                        2 * w,
                        2 * w
                    ),
                };
                format!("({inner} || {overflow})")
            }
            Term::Div { a, b, kind, rem } => {
                let (x, y) = (self.value(a), self.value(b));
                let w = kind.width();
                let mut f = format!("({} || {} || ({y} == 0#{w})", self.fault(a), self.fault(b));
                if kind.signed() && !rem {
                    f.push_str(&format!(
                        " || ({x} == {} && {y} == {})",
                        bv_literal(i64::MIN as i128, w),
                        bv_literal(-1, w)
                    ));
                }
                f.push(')');
                f
            }
            Term::Neg(a, kind) => {
                let x = self.value(a);
                let w = kind.width();
                let fault = if kind.signed() {
                    format!("({x} == {})", bv_literal(i64::MIN as i128, w))
                } else {
                    format!("({x} != 0#{w})")
                };
                format!("({} || {fault})", self.fault(a))
            }
            Term::Not(a) => self.fault(a),
            Term::Cmp { a, b, .. } => format!("({} || {})", self.fault(a), self.fault(b)),
            Term::And(a, b) => format!(
                "({} || ({} && {}))",
                self.fault(a),
                self.value(a),
                self.fault(b)
            ),
            Term::Or(a, b) => format!(
                "({} || (!{} && {}))",
                self.fault(a),
                self.value(a),
                self.fault(b)
            ),
            Term::Ite { c, t, e, .. } => format!(
                "({} || (bif {} then {} else {}))",
                self.fault(c),
                self.value(c),
                self.fault(t),
                self.fault(e)
            ),
            Term::Let { var, value, body } => format!(
                "({} || (let {} := {}; {}))",
                self.fault(value),
                self.name(*var),
                self.value(value),
                self.fault(body)
            ),
        }
    }
}

fn bv_literal(n: i128, width: u32) -> String {
    format!("({}#{width})", n.rem_euclid(1i128 << width))
}

fn bv_type(kind: Kind) -> String {
    match kind {
        Kind::Bool => "Bool".into(),
        Kind::Unit => "Unit".into(),
        k => format!("BitVec {}", k.width()),
    }
}

/// Every function with a body, mapped to its model or the reason it has none.
pub(crate) fn all(checked: &Checked) -> BTreeMap<String, Result<Model, String>> {
    checked
        .world
        .functions
        .iter()
        .filter(|(_, f)| f.body.is_some())
        .take(256)
        .map(|(name, _)| (name.clone(), model(checked, name)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tooling::frontend::{AnalysisLimits, check_source};

    fn models(source: &str) -> BTreeMap<String, Result<Model, String>> {
        let snapshot = check_source(source, AnalysisLimits::default()).expect("checked");
        all(snapshot.checked())
    }

    const SOURCE: &str = "fn inc(x: i64) -> i64 eff [Fail[ArithOverflow]] {
    if x < 100 {
        x + 1
    } else {
        0
    }
}

fn add(a: i64, b: i64) -> i64 eff [Fail[ArithOverflow]] {
    a + b
}

fn half(x: i64) -> i64 eff [Fail[ArithOverflow], Fail[RuntimeError]] {
    let q = x / 2;
    let r = x % 3;
    wrapping_add(q, r)
}

fn ratio(x: u64, y: u64) -> u64 eff [Fail[RuntimeError]] {
    x / y
}

fn both(a: bool, b: bool, x: u8) -> bool eff [Fail[ArithOverflow]] {
    a && (x < 200) && (wrapping_add(x, 100) > 3)
}

fn negate(x: i64) -> i64 eff [Fail[ArithOverflow]] {
    -x
}

fn counted(x: u32) -> u32 eff [Fail[ArithOverflow]] {
    let total = x;
    total += 1;
    if total > 10 {
        total - 10
    } else {
        total
    }
}

fn main() -> i64 eff [Fail[ArithOverflow], Fail[RuntimeError]] {
    add(inc(3), half(4))
}

fn answer() -> i64 {
    42
}
";

    #[test]
    fn scalar_functions_get_models_and_others_say_why() {
        let models = models(SOURCE);
        for name in [
            "inc", "add", "half", "ratio", "both", "negate", "counted", "main", "answer",
        ] {
            let model = models[name]
                .as_ref()
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(!model.exact_module().contains("∀,"), "{name}");
            assert!(model.exact_module().contains("theorem no_arith_fault"));
            assert!(model.bitvec_module().contains("theorem no_arith_fault_bv"));
            assert!(model.theorem().starts_with("VesperLensModel_"));
        }
        // Calls are inlined, so main's model mentions the callee's checks.
        assert!(models["main"].as_ref().unwrap().exact.contains("100"));
        let unsupported = super::tests::models(
            "linear struct Tx {\n    slot: u64\n}\n\nfn make(x: u64) -> Tx {\n    Tx {\n        slot: x\n    }\n}\n\nfn loop_forever(x: u64) -> u64 {\n    loop_forever(x)\n}\n\nfn pick[T](sink x: T) -> T {\n    x\n}\n",
        );
        let reason = |name: &str| unsupported[name].as_ref().unwrap_err().clone();
        assert!(
            reason("make").contains("outside the scalar model"),
            "{}",
            reason("make")
        );
        assert!(
            reason("loop_forever").contains("recursive"),
            "{}",
            reason("loop_forever")
        );
        assert!(reason("pick").contains("generic"), "{}", reason("pick"));
    }

    #[test]
    fn model_identity_follows_the_function_and_its_callees_only() {
        let before = models(SOURCE);
        let edited = models(&SOURCE.replace("add(inc(3), half(4))", "add(inc(5), half(4))"));
        for name in ["inc", "add", "half", "ratio"] {
            assert_eq!(
                before[name].as_ref().unwrap().key,
                edited[name].as_ref().unwrap().key,
                "{name}"
            );
        }
        assert_ne!(
            before["main"].as_ref().unwrap().key,
            edited["main"].as_ref().unwrap().key
        );
        // Changing a callee changes its callers' models.
        let callee = models(&SOURCE.replace("x < 100", "x < 99"));
        assert_ne!(
            before["main"].as_ref().unwrap().key,
            callee["main"].as_ref().unwrap().key
        );
    }

    #[test]
    fn counterexamples_render_as_source_values() {
        let models = models(SOURCE);
        let add = models["add"].as_ref().unwrap();
        let output = "error: The prover found a counterexample, consider the following assignment:\nb = 0xffffffffffffffff#64\na = 0x8000000000000000#64\n\nmore";
        assert_eq!(
            add.counterexample(output).unwrap(),
            "b = -1, a = -9223372036854775808"
        );
        let ratio = models["ratio"].as_ref().unwrap();
        assert_eq!(
            ratio
                .counterexample(
                    "found a counterexample, consider:\ny = 0x0#64\nx = 18446744073709551615#64\n"
                )
                .unwrap(),
            "y = 0, x = 18446744073709551615"
        );
        assert!(
            add.counterexample("error: omega could not prove the goal")
                .is_none()
        );
    }

    /// Both renderings compile under real Lean 4.24.0 with the expected
    /// outcome. Runs when `VESPER_LEAN` is set; skipped otherwise.
    #[test]
    fn renderings_check_under_real_lean() {
        let Ok(lean) = std::env::var("VESPER_LEAN") else {
            eprintln!("VESPER_LEAN unset; real model check skipped");
            return;
        };
        let prefix = std::process::Command::new(&lean)
            .arg("--print-prefix")
            .output()
            .unwrap();
        let prefix = std::path::PathBuf::from(String::from_utf8(prefix.stdout).unwrap().trim());
        let dir = std::env::temp_dir().join(format!("vesper-model-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let run = |name: &str, text: &str| {
            let file = dir.join(format!("{name}.lean"));
            std::fs::write(&file, text).unwrap();
            let out = std::process::Command::new(prefix.join("bin/lean"))
                .env("LEAN_PATH", prefix.join("lib/lean"))
                .arg(&file)
                .output()
                .unwrap();
            (
                out.status.success(),
                String::from_utf8_lossy(&out.stdout).into_owned(),
            )
        };
        let models = models(SOURCE);
        // (function, omega proves it, bv_decide refutes it)
        for (name, proved, refuted) in [
            ("inc", true, false),
            ("add", false, true),
            ("half", true, false),
            ("ratio", false, true),
            ("both", true, false),
            ("negate", false, true),
            ("counted", false, true),
            ("main", true, false),
            ("answer", true, false),
        ] {
            let model = models[name].as_ref().unwrap();
            let (ok, out) = run(&format!("{name}_exact"), &model.exact_module());
            assert_eq!(ok, proved, "{name} omega:\n{}\n{out}", model.exact_module());
            let (ok, out) = run(&format!("{name}_bv"), &model.bitvec_module());
            assert_eq!(
                !ok,
                refuted,
                "{name} bv_decide:\n{}\n{out}",
                model.bitvec_module()
            );
            if refuted {
                assert!(model.counterexample(&out).is_some(), "{name}: {out}");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
