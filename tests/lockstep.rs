//! Stage 0b differential lockstep (§42.1 pillar 1, issue #453): programs of
//! the core calculus run through the Rust seed compiler (`vesper check`, the
//! tree evaluator and the Cranelift JIT) and through the executable Lean
//! reference checker and evaluator in `lean/ReferenceEval.lean`.
//!
//! Programs come from two sources: the `.ves` fixtures (translated from the
//! parsed Rust AST, so they need no second parser) and a seeded generator
//! that writes each program twice, as Vesper source for the Rust side and in
//! the reference's S-expression wire format for the Lean side. Generated
//! source is also parsed back and translated, which cross-checks the parser
//! against the generator.
//!
//! Compared per program: the static verdict (accept, or the code of the first
//! diagnostic), the main result of the evaluator, the JIT and the reference,
//! and, for accepted programs, that the reference run neither got stuck nor
//! left a live linear continuation. Without `VESPER_LEAN` the Rust side is
//! compared against verdicts recorded from the reference
//! (`tests/fixtures/lockstep/*.golden`); with it, the reference is run and
//! must reproduce the recorded verdicts. `VESPER_LOCKSTEP_BLESS=1` rewrites
//! them. The nightly-sized run is `VESPER_LOCKSTEP_PROGRAMS=10000`.
//!
//! Agreement on these programs is evidence about these programs only: it is
//! not a proof that the Rust compiler implements the reference.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use vesper::ast::{self as ast, ExprKind, Item, Literal, PatternKind, Statement};
use vesper::{check, native, parser, runtime::Runtime};

// ---------------------------------------------------------------------------
// The reference fragment, as written to the wire
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Ty {
    I64,
    U64,
    Bool,
    Adt(String),
}

#[derive(Clone, Debug, PartialEq)]
enum Eff {
    Fail(String),
    User(String),
}

#[derive(Clone, Debug, PartialEq)]
enum Pat {
    Wild,
    Bind(String),
    Ctor(String, Vec<Pat>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

const ARITH: [Op; 5] = [Op::Add, Op::Sub, Op::Mul, Op::Div, Op::Rem];
const ORDER: [Op; 6] = [Op::Eq, Op::Ne, Op::Lt, Op::Le, Op::Gt, Op::Ge];

impl Op {
    fn symbol(self) -> &'static str {
        match self {
            Op::Add => "+",
            Op::Sub => "-",
            Op::Mul => "*",
            Op::Div => "/",
            Op::Rem => "%",
            Op::Eq => "==",
            Op::Ne => "!=",
            Op::Lt => "<",
            Op::Le => "<=",
            Op::Gt => ">",
            Op::Ge => ">=",
            Op::And => "&&",
            Op::Or => "||",
        }
    }
    fn wire(self) -> &'static str {
        match self {
            Op::Add => "add",
            Op::Sub => "sub",
            Op::Mul => "mul",
            Op::Div => "div",
            Op::Rem => "rem",
            Op::Eq => "eq",
            Op::Ne => "ne",
            Op::Lt => "lt",
            Op::Le => "le",
            Op::Gt => "gt",
            Op::Ge => "ge",
            Op::And => "and",
            Op::Or => "or",
        }
    }
    fn from_symbol(s: &str) -> Option<Self> {
        ARITH
            .iter()
            .chain(&ORDER)
            .chain(&[Op::And, Op::Or])
            .copied()
            .find(|o| o.symbol() == s)
    }
}

#[derive(Clone, Debug, PartialEq)]
enum E {
    Int(i128),
    Bool(bool),
    Var(String),
    Neg(Box<E>),
    Not(Box<E>),
    Bin(Op, Box<E>, Box<E>),
    /// `wrapping_add` / `wrapping_sub` / `wrapping_mul` (Add, Sub, Mul).
    Wrap(Op, Box<E>, Box<E>),
    Let(String, Option<Ty>, Box<E>, Box<E>),
    Seq(Box<E>, Box<E>),
    If(Box<E>, Box<E>, Box<E>),
    Call(String, Vec<E>),
    Struct(String, Vec<(String, E)>),
    Field(Box<E>, String),
    Ctor(String, Vec<E>),
    Match(Box<E>, Vec<(Pat, E)>),
    Handle {
        eff: Eff,
        body: Box<E>,
        rx: String,
        ret: Box<E>,
        params: Vec<String>,
        k: String,
        op_body: Box<E>,
    },
    Perform(String, String, Vec<E>),
    Resume(String, Box<E>),
}

#[derive(Clone, Debug, PartialEq)]
struct EffDecl {
    name: String,
    op: String,
    params: Vec<Ty>,
    result: Ty,
}

#[derive(Clone, Debug, PartialEq)]
struct FnDecl {
    name: String,
    params: Vec<(String, Ty)>,
    result: Ty,
    effs: Vec<Eff>,
    body: E,
}

/// An enum declaration: its name and its variants with their payload types.
type EnumDecl = (String, Vec<(String, Vec<Ty>)>);

#[derive(Clone, Debug, Default, PartialEq)]
struct Prog {
    structs: Vec<(String, Vec<(String, Ty)>)>,
    enums: Vec<EnumDecl>,
    effects: Vec<EffDecl>,
    fns: Vec<FnDecl>,
}

// ---------------------------------------------------------------------------
// Rendering: wire format and Vesper source
// ---------------------------------------------------------------------------

fn wire_ty(t: &Ty) -> String {
    match t {
        Ty::I64 => "(int i64)".into(),
        Ty::U64 => "(int u64)".into(),
        Ty::Bool => "bool".into(),
        Ty::Adt(n) => format!("(adt {n})"),
    }
}

fn wire_eff(e: &Eff) -> String {
    match e {
        Eff::Fail(p) => format!("(fail {p})"),
        Eff::User(n) => format!("(user {n})"),
    }
}

fn wire_pat(p: &Pat) -> String {
    match p {
        Pat::Wild => "_".into(),
        Pat::Bind(x) => format!("(bind {x})"),
        Pat::Ctor(c, subs) => {
            let mut s = format!("(ctor {c}");
            for p in subs {
                s.push(' ');
                s.push_str(&wire_pat(p));
            }
            s + ")"
        }
    }
}

fn wire_list(head: &str, items: &[E]) -> String {
    let mut s = format!("({head}");
    for e in items {
        s.push(' ');
        s.push_str(&wire(e));
    }
    s + ")"
}

fn wire(e: &E) -> String {
    match e {
        E::Int(n) => format!("(lit {n})"),
        E::Bool(b) => b.to_string(),
        E::Var(x) => format!("(var {x})"),
        E::Neg(a) => format!("(neg {})", wire(a)),
        E::Not(a) => format!("(not {})", wire(a)),
        E::Bin(op, a, b) => format!("(bin {} {} {})", op.wire(), wire(a), wire(b)),
        E::Wrap(op, a, b) => format!("(wrap {} {} {})", op.wire(), wire(a), wire(b)),
        E::Let(x, t, v, b) => format!(
            "(let {x} {} {} {})",
            t.as_ref().map_or("_".into(), wire_ty),
            wire(v),
            wire(b)
        ),
        E::Seq(a, b) => format!("(seq {} {})", wire(a), wire(b)),
        E::If(c, t, f) => format!("(if {} {} {})", wire(c), wire(t), wire(f)),
        E::Call(f, args) => wire_list(&format!("call {f}"), args),
        E::Ctor(c, args) => wire_list(&format!("ctor {c}"), args),
        E::Perform(eff, op, args) => wire_list(&format!("perform {eff} {op}"), args),
        E::Struct(n, fields) => {
            let mut s = format!("(struct {n}");
            for (f, e) in fields {
                let _ = write!(s, " ({f} {})", wire(e));
            }
            s + ")"
        }
        E::Field(b, f) => format!("(field {} {f})", wire(b)),
        E::Match(s, arms) => {
            let mut out = format!("(match {}", wire(s));
            for (p, e) in arms {
                let _ = write!(out, " (arm {} {})", wire_pat(p), wire(e));
            }
            out + ")"
        }
        E::Handle {
            eff,
            body,
            rx,
            ret,
            params,
            k,
            op_body,
        } => format!(
            "(handle {} {} {rx} {} ({}) {k} {})",
            wire_eff(eff),
            wire(body),
            wire(ret),
            params.join(" "),
            wire(op_body)
        ),
        E::Resume(k, a) => format!("(resume {k} {})", wire(a)),
    }
}

fn wire_prog(p: &Prog) -> String {
    let mut s = String::from("(program");
    for (n, fields) in &p.structs {
        let _ = write!(s, " (struct {n}");
        for (f, t) in fields {
            let _ = write!(s, " ({f} {})", wire_ty(t));
        }
        s.push(')');
    }
    for (n, variants) in &p.enums {
        let _ = write!(s, " (enum {n}");
        for (v, ts) in variants {
            let _ = write!(s, " ({v}");
            for t in ts {
                let _ = write!(s, " {}", wire_ty(t));
            }
            s.push(')');
        }
        s.push(')');
    }
    for e in &p.effects {
        let params: Vec<String> = e.params.iter().map(wire_ty).collect();
        let _ = write!(
            s,
            " (effect {} {} ({}) {})",
            e.name,
            e.op,
            params.join(" "),
            wire_ty(&e.result)
        );
    }
    for f in &p.fns {
        let params: Vec<String> = f
            .params
            .iter()
            .map(|(x, t)| format!("({x} {})", wire_ty(t)))
            .collect();
        let effs: Vec<String> = f.effs.iter().map(wire_eff).collect();
        let _ = write!(
            s,
            " (fn {} ({}) {} ({}) {})",
            f.name,
            params.join(" "),
            wire_ty(&f.result),
            effs.join(" "),
            wire(&f.body)
        );
    }
    s + ")"
}

fn src_ty(t: &Ty) -> String {
    match t {
        Ty::I64 => "i64".into(),
        Ty::U64 => "u64".into(),
        Ty::Bool => "bool".into(),
        Ty::Adt(n) => n.clone(),
    }
}

fn src_eff(e: &Eff) -> String {
    match e {
        Eff::Fail(p) => format!("Fail[{p}]"),
        Eff::User(n) => n.clone(),
    }
}

fn src_pat(p: &Pat) -> String {
    match p {
        Pat::Wild => "_".into(),
        Pat::Bind(x) => x.clone(),
        Pat::Ctor(c, subs) if subs.is_empty() => c.clone(),
        Pat::Ctor(c, subs) => format!(
            "{c}({})",
            subs.iter().map(src_pat).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// The statements and tail of a block whose value is `e`.
fn src_block(e: &E) -> String {
    let mut out = String::from("{ ");
    let mut cur = e;
    loop {
        match cur {
            E::Let(x, t, v, b) => {
                let ann = t.as_ref().map(|t| format!(": {}", src_ty(t)));
                let _ = write!(out, "let {x}{} = {}; ", ann.unwrap_or_default(), src(v));
                cur = b;
            }
            E::Seq(a, b) => {
                let _ = write!(out, "{}; ", src(a));
                cur = b;
            }
            _ => break,
        }
    }
    out + &src(cur) + " }"
}

fn src_args(args: &[E]) -> String {
    args.iter().map(src).collect::<Vec<_>>().join(", ")
}

fn src(e: &E) -> String {
    match e {
        E::Int(n) if *n < 0 => format!("({n})"),
        E::Int(n) => n.to_string(),
        E::Bool(b) => b.to_string(),
        E::Var(x) => x.clone(),
        E::Neg(a) => format!("(-{})", src(a)),
        E::Not(a) => format!("(!{})", src(a)),
        E::Bin(op, a, b) => format!("({} {} {})", src(a), op.symbol(), src(b)),
        E::Wrap(op, a, b) => {
            let name = match op {
                Op::Add => "wrapping_add",
                Op::Sub => "wrapping_sub",
                _ => "wrapping_mul",
            };
            format!("{name}({}, {})", src(a), src(b))
        }
        // Block-like expressions are parenthesized so that they can stand
        // as operands; the parser drops the parentheses.
        E::Let(..) | E::Seq(..) => format!("({})", src_block(e)),
        E::If(c, t, f) => format!("(if {} {} else {})", src(c), src_block(t), src_block(f)),
        E::Call(f, args) => format!("{f}({})", src_args(args)),
        E::Ctor(c, args) if args.is_empty() => c.clone(),
        E::Ctor(c, args) => format!("{c}({})", src_args(args)),
        E::Perform(eff, op, args) => format!("{eff}::{op}({})", src_args(args)),
        // Parenthesized: a struct literal cannot open a match scrutinee.
        E::Struct(n, fields) => format!(
            "({n} {{ {} }})",
            fields
                .iter()
                .map(|(f, e)| format!("{f}: {}", src(e)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        E::Field(b, f) => format!("{}.{f}", src(b)),
        E::Match(s, arms) => format!(
            "(match {} {{ {} }})",
            src(s),
            arms.iter()
                .map(|(p, e)| format!("{} => {},", src_pat(p), src(e)))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        E::Handle {
            eff,
            body,
            rx,
            ret,
            params,
            k,
            op_body,
        } => {
            let op = match eff {
                Eff::Fail(_) => "raise".to_string(),
                Eff::User(n) => n.to_lowercase(),
            };
            let mut pats = params.clone();
            pats.push(k.clone());
            format!(
                "(handle {} in {} with {{ return {rx} => {}, {op}({}) => {}, }})",
                src_eff(eff),
                src_block(body),
                src(ret),
                pats.join(", "),
                src(op_body)
            )
        }
        E::Resume(k, a) => format!("{k}({})", src(a)),
    }
}

fn src_prog(p: &Prog) -> String {
    let mut s = String::new();
    for (n, fields) in &p.structs {
        let fs: Vec<String> = fields
            .iter()
            .map(|(f, t)| format!("{f}: {}", src_ty(t)))
            .collect();
        let _ = writeln!(s, "copy struct {n} {{ {} }}", fs.join(", "));
    }
    for (n, variants) in &p.enums {
        let vs: Vec<String> = variants
            .iter()
            .map(|(v, ts)| {
                if ts.is_empty() {
                    v.clone()
                } else {
                    format!(
                        "{v}({})",
                        ts.iter().map(src_ty).collect::<Vec<_>>().join(", ")
                    )
                }
            })
            .collect();
        let _ = writeln!(s, "copy enum {n} {{ {} }}", vs.join(", "));
    }
    for e in &p.effects {
        let ps: Vec<String> = e
            .params
            .iter()
            .enumerate()
            .map(|(i, t)| format!("a{i}: {}", src_ty(t)))
            .collect();
        let _ = writeln!(
            s,
            "linear effect {} {{ fn {}({}) -> {}; }}",
            e.name,
            e.op,
            ps.join(", "),
            src_ty(&e.result)
        );
    }
    for f in &p.fns {
        let ps: Vec<String> = f
            .params
            .iter()
            .map(|(x, t)| format!("{x}: {}", src_ty(t)))
            .collect();
        let effs = if f.effs.is_empty() {
            String::new()
        } else {
            format!(
                " eff [{}]",
                f.effs.iter().map(src_eff).collect::<Vec<_>>().join(", ")
            )
        };
        let _ = writeln!(
            s,
            "fn {}({}) -> {}{effs} {}",
            f.name,
            ps.join(", "),
            src_ty(&f.result),
            src_block(&f.body)
        );
    }
    s
}

// ---------------------------------------------------------------------------
// Translation of the parsed Rust AST into the fragment
// ---------------------------------------------------------------------------

struct Translator {
    enums: Vec<String>,
    /// Every variant name, which the fragment uses unqualified.
    variants: Vec<String>,
    effects: Vec<String>,
    /// Continuation binders currently in scope.
    konts: Vec<String>,
}

type Tr<T> = Result<T, String>;

impl Translator {
    fn ty(&self, t: &ast::Type) -> Tr<Ty> {
        match t {
            ast::Type::Named(n, args) if args.is_empty() => Ok(match n.as_str() {
                "i64" => Ty::I64,
                "u64" => Ty::U64,
                "bool" => Ty::Bool,
                _ => Ty::Adt(n.clone()),
            }),
            other => Err(format!("type {other} is outside the fragment")),
        }
    }
    fn eff(&self, e: &ast::Effect) -> Tr<Eff> {
        if e.label.is_some() {
            return Err("labelled effects are outside the fragment".into());
        }
        match (e.name.as_str(), e.args.as_slice()) {
            ("Fail", [ast::Type::Named(p, a)]) if a.is_empty() => Ok(Eff::Fail(p.clone())),
            (n, []) => Ok(Eff::User(n.into())),
            _ => Err(format!("effect {} is outside the fragment", e.name)),
        }
    }
    /// A constructor name, unqualified (`RuntimeError::DivisionByZero`).
    fn ctor_name(&self, n: &str) -> Option<String> {
        if self.variants.iter().any(|v| v == n) {
            return Some(n.to_string());
        }
        let (e, v) = n.split_once("::")?;
        self.enums.iter().any(|x| x == e).then(|| v.to_string())
    }
    fn pat(&self, p: &ast::Pattern) -> Tr<Pat> {
        match &p.kind {
            PatternKind::Wild => Ok(Pat::Wild),
            PatternKind::Bind(x) => Ok(Pat::Bind(x.clone())),
            PatternKind::Constructor {
                name,
                fields,
                named: false,
            } => {
                let name = self.ctor_name(name).unwrap_or_else(|| name.clone());
                Ok(Pat::Ctor(
                    name,
                    fields.iter().map(|(_, p)| self.pat(p)).collect::<Tr<_>>()?,
                ))
            }
            _ => Err("pattern outside the fragment".into()),
        }
    }
    fn block(&mut self, statements: &[Statement], tail: Option<&ast::Expr>) -> Tr<E> {
        let Some((first, rest)) = statements.split_first() else {
            return match tail {
                Some(t) => self.expr(t),
                None => Err("blocks without a tail are outside the fragment".into()),
            };
        };
        match first {
            Statement::Let { pat, ty, value } => {
                let PatternKind::Bind(x) = &pat.kind else {
                    return Err("destructuring let is outside the fragment".into());
                };
                let ann = match ty {
                    ast::Type::Infer => None,
                    t => Some(self.ty(t)?),
                };
                let v = self.expr(value)?;
                let b = self.block(rest, tail)?;
                Ok(E::Let(x.clone(), ann, Box::new(v), Box::new(b)))
            }
            Statement::Expr(e) => {
                let a = self.expr(e)?;
                let b = self.block(rest, tail)?;
                Ok(E::Seq(Box::new(a), Box::new(b)))
            }
        }
    }
    fn args(&mut self, args: &[ast::Arg]) -> Tr<Vec<E>> {
        args.iter()
            .map(|a| {
                if a.explicit {
                    Err("explicit argument modes are outside the fragment".into())
                } else {
                    self.expr(&a.expr)
                }
            })
            .collect()
    }
    fn expr(&mut self, e: &ast::Expr) -> Tr<E> {
        Ok(match &e.kind {
            ExprKind::Literal(Literal::Int(n)) => E::Int(*n),
            ExprKind::Literal(Literal::Bool(b)) => E::Bool(*b),
            ExprKind::Literal(_) => return Err("literal outside the fragment".into()),
            ExprKind::Name(n) => match self.ctor_name(n) {
                Some(c) => E::Ctor(c, vec![]),
                None => E::Var(n.clone()),
            },
            ExprKind::Unary(op, a) if op == "-" => E::Neg(Box::new(self.expr(a)?)),
            ExprKind::Unary(op, a) if op == "!" => E::Not(Box::new(self.expr(a)?)),
            ExprKind::Binary(op, a, b) => {
                let op = Op::from_symbol(op).ok_or_else(|| format!("operator {op}"))?;
                E::Bin(op, Box::new(self.expr(a)?), Box::new(self.expr(b)?))
            }
            ExprKind::Block { statements, tail } => self.block(statements, tail.as_deref())?,
            ExprKind::If {
                condition,
                then,
                otherwise: Some(otherwise),
            } => E::If(
                Box::new(self.expr(condition)?),
                Box::new(self.expr(then)?),
                Box::new(self.expr(otherwise)?),
            ),
            ExprKind::Call {
                callee,
                types,
                args,
            } if types.is_empty() => {
                let ExprKind::Name(name) = &callee.kind else {
                    return Err("method calls are outside the fragment".into());
                };
                let wrap = match name.as_str() {
                    "wrapping_add" => Some(Op::Add),
                    "wrapping_sub" => Some(Op::Sub),
                    "wrapping_mul" => Some(Op::Mul),
                    _ => None,
                };
                let mut args = self.args(args)?;
                if let Some(op) = wrap {
                    if args.len() != 2 {
                        return Err("wrapping arity".into());
                    }
                    let b = args.pop().unwrap();
                    let a = args.pop().unwrap();
                    E::Wrap(op, Box::new(a), Box::new(b))
                } else if self.konts.contains(name) {
                    if args.len() != 1 {
                        return Err("resume arity".into());
                    }
                    E::Resume(name.clone(), Box::new(args.pop().unwrap()))
                } else if let Some(c) = self.ctor_name(name) {
                    E::Ctor(c, args)
                } else if let Some((eff, op)) = name.split_once("::") {
                    if !self.effects.iter().any(|e| e == eff) {
                        return Err(format!("path {name} is outside the fragment"));
                    }
                    E::Perform(eff.into(), op.into(), args)
                } else {
                    E::Call(name.clone(), args)
                }
            }
            ExprKind::Struct { name, fields } => E::Struct(
                name.clone(),
                fields
                    .iter()
                    .map(|(f, e)| Ok((f.clone(), self.expr(e)?)))
                    .collect::<Tr<_>>()?,
            ),
            ExprKind::Field(b, f) => E::Field(Box::new(self.expr(b)?), f.clone()),
            ExprKind::Match { value, arms } => E::Match(
                Box::new(self.expr(value)?),
                arms.iter()
                    .map(|(p, e)| Ok((self.pat(p)?, self.expr(e)?)))
                    .collect::<Tr<_>>()?,
            ),
            ExprKind::Handle {
                effect,
                state: None,
                body,
                ret,
                ops,
            } => {
                let eff = self.eff(effect)?;
                let [op] = ops.as_slice() else {
                    return Err("handlers with several clauses are outside the fragment".into());
                };
                let [
                    ast::Pattern {
                        kind: PatternKind::Bind(rx),
                        ..
                    },
                ] = ret.pats.as_slice()
                else {
                    return Err("return clause pattern".into());
                };
                let mut names = vec![];
                for p in &op.pats {
                    match &p.kind {
                        PatternKind::Bind(x) => names.push(x.clone()),
                        PatternKind::Wild => names.push("_".into()),
                        _ => return Err("operation clause pattern".into()),
                    }
                }
                let k = names.pop().ok_or("operation clause without continuation")?;
                let body = self.expr(body)?;
                let ret_e = self.expr(&ret.body)?;
                self.konts.push(k.clone());
                let op_body = self.expr(&op.body);
                self.konts.pop();
                E::Handle {
                    eff,
                    body: Box::new(body),
                    rx: rx.clone(),
                    ret: Box::new(ret_e),
                    params: names,
                    k,
                    op_body: Box::new(op_body?),
                }
            }
            ExprKind::Tuple(items) if items.is_empty() => {
                return Err("unit values are outside the fragment".into());
            }
            _ => return Err("expression outside the fragment".into()),
        })
    }
}

fn translate(module: &ast::Module) -> Tr<Prog> {
    let mut tr = Translator {
        enums: vec!["RuntimeError".into()],
        variants: vec!["DivisionByZero".into(), "IndexOutOfBounds".into()],
        effects: vec![],
        konts: vec![],
    };
    for item in &module.items {
        match item {
            Item::Adt(a) if a.is_enum => {
                tr.enums.push(a.name.clone());
                tr.variants
                    .extend(a.variants.iter().map(|v| v.name.clone()));
            }
            Item::Effect(e) => tr.effects.push(e.name.clone()),
            _ => {}
        }
    }
    if !module.model_uses.is_empty() || !module.trait_bounds.is_empty() {
        return Err("traits are outside the fragment".into());
    }
    let mut p = Prog::default();
    for item in &module.items {
        match item {
            Item::Adt(a) => {
                if a.mode != ast::Mode::Copy || !a.generics.is_empty() {
                    return Err(format!("type {} is outside the fragment", a.name));
                }
                if a.is_enum {
                    let mut vs = vec![];
                    for v in &a.variants {
                        if v.named {
                            return Err("named enum payloads are outside the fragment".into());
                        }
                        vs.push((
                            v.name.clone(),
                            v.fields
                                .iter()
                                .map(|f| tr.ty(&f.ty))
                                .collect::<Tr<Vec<_>>>()?,
                        ));
                    }
                    p.enums.push((a.name.clone(), vs));
                } else {
                    let [v] = a.variants.as_slice() else {
                        return Err("struct shape".into());
                    };
                    p.structs.push((
                        a.name.clone(),
                        v.fields
                            .iter()
                            .map(|f| Ok((f.name.clone(), tr.ty(&f.ty)?)))
                            .collect::<Tr<Vec<_>>>()?,
                    ));
                }
            }
            Item::Effect(e) => {
                let [op] = e.ops.as_slice() else {
                    return Err("effects with several operations are outside the fragment".into());
                };
                if !e.linear || !e.generics.is_empty() {
                    return Err(format!("effect {} is outside the fragment", e.name));
                }
                p.effects.push(EffDecl {
                    name: e.name.clone(),
                    op: op.name.clone(),
                    params: op.params.iter().map(|q| tr.ty(&q.ty)).collect::<Tr<_>>()?,
                    result: tr.ty(&op.result)?,
                });
            }
            Item::Function(f) => {
                if !f.generics.is_empty() || f.effects.tail.is_some() {
                    return Err(format!("function {} is outside the fragment", f.name));
                }
                let mut params = vec![];
                for q in &f.params {
                    if q.pass != ast::Pass::In || !q.projections.is_empty() {
                        return Err("parameter modes are outside the fragment".into());
                    }
                    params.push((q.name.clone(), tr.ty(&q.ty)?));
                }
                let body = f.body.as_ref().ok_or("function without body")?;
                p.fns.push(FnDecl {
                    name: f.name.clone(),
                    params,
                    result: tr.ty(&f.result)?,
                    effs: f
                        .effects
                        .instances
                        .iter()
                        .map(|e| tr.eff(e))
                        .collect::<Tr<_>>()?,
                    body: tr.expr(body)?,
                });
            }
            _ => return Err("item outside the fragment".into()),
        }
    }
    Ok(p)
}

// ---------------------------------------------------------------------------
// The seeded generator
// ---------------------------------------------------------------------------

/// SplitMix64: a deterministic generator, identical on every platform.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

#[derive(Clone)]
struct Scope {
    vars: Vec<(String, Ty)>,
    effs: Vec<Eff>,
    /// In an `if` condition or `match` scrutinee, where the parser reads
    /// `Name {` as the start of the block even inside parentheses.
    restricted: bool,
}

struct Gen<'p> {
    rng: Rng,
    prog: &'p Prog,
    fresh: usize,
    /// Callable helpers (index < the function being generated).
    callable: usize,
}

const LITERALS: [i128; 12] = [
    0,
    1,
    2,
    3,
    7,
    10,
    100,
    255,
    65_536,
    3_037_000_499,
    4_611_686_018_427_387_904,
    9_223_372_036_854_775_807,
];

fn restrict(sc: &Scope) -> Scope {
    Scope {
        restricted: true,
        ..sc.clone()
    }
}

/// Negation; the parser folds a negated literal into a negative literal.
fn neg(e: E) -> E {
    match e {
        E::Int(n) => E::Int(-n),
        e => E::Neg(Box::new(e)),
    }
}

fn fail(p: &str) -> Eff {
    Eff::Fail(p.into())
}

impl Gen<'_> {
    fn name(&mut self, base: &str) -> String {
        self.fresh += 1;
        format!("{base}{}", self.fresh)
    }
    fn lit(&mut self) -> E {
        if self.rng.chance(70) {
            E::Int(*self.rng.pick(&LITERALS[..8]))
        } else {
            E::Int(*self.rng.pick(&LITERALS))
        }
    }
    fn var_of(&mut self, sc: &Scope, t: &Ty) -> Option<E> {
        let vs: Vec<&String> = sc
            .vars
            .iter()
            .filter(|(_, vt)| vt == t)
            .map(|(x, _)| x)
            .collect();
        (!vs.is_empty()).then(|| E::Var((*self.rng.pick(&vs)).clone()))
    }
    fn leaf(&mut self, sc: &Scope, t: &Ty) -> E {
        if (sc.restricted && self.is_struct(t) || self.rng.chance(50))
            && let Some(v) = self.var_of(sc, t)
        {
            return v;
        }
        match t {
            Ty::I64 | Ty::U64 => self.lit(),
            Ty::Bool => E::Bool(self.rng.chance(50)),
            Ty::Adt(n) => self.construct(sc, n, 0),
        }
    }
    fn construct(&mut self, sc: &Scope, n: &str, depth: usize) -> E {
        if let Some((_, fields)) = self.prog.structs.iter().find(|(s, _)| s == n) {
            let fields = fields.clone();
            let inner = Scope {
                restricted: false,
                ..sc.clone()
            };
            let mut fs: Vec<(String, E)> = fields
                .iter()
                .map(|(f, t)| (f.clone(), self.expr(&inner, t, depth + 1)))
                .collect();
            if self.rng.chance(30) {
                fs.reverse();
            }
            let lit = E::Struct(n.into(), fs);
            if sc.restricted {
                // bind it in a block statement instead
                let t = self.name("s");
                return E::Let(t.clone(), None, Box::new(lit), Box::new(E::Var(t)));
            }
            return lit;
        }
        let (_, variants) = self
            .prog
            .enums
            .iter()
            .find(|(e, _)| e == n)
            .expect("generated ADT")
            .clone();
        let (v, ts) = self.rng.pick(&variants).clone();
        E::Ctor(v, ts.iter().map(|t| self.expr(sc, t, depth + 1)).collect())
    }
    fn is_struct(&self, t: &Ty) -> bool {
        matches!(t, Ty::Adt(n) if self.prog.structs.iter().any(|(s, _)| s == n))
    }
    fn has(&self, sc: &Scope, e: &Eff) -> bool {
        sc.effs.contains(e)
    }
    fn expr(&mut self, sc: &Scope, t: &Ty, depth: usize) -> E {
        if depth > 4 || self.rng.chance(15 + depth as u64 * 12) {
            return self.leaf(sc, t);
        }
        match self.rng.below(10) {
            0 => {
                // let
                let vt = self.any_ty(sc.restricted);
                let x = self.name("x");
                let v = self.expr(sc, &vt, depth + 1);
                let mut inner = sc.clone();
                inner.vars.push((x.clone(), vt.clone()));
                let ann = (vt == Ty::U64 || self.rng.chance(20)).then(|| vt.clone());
                E::Let(
                    x,
                    ann,
                    Box::new(v),
                    Box::new(self.expr(&inner, t, depth + 1)),
                )
            }
            1 => {
                let c = self.cond(&restrict(sc), depth + 1);
                E::If(
                    Box::new(c),
                    Box::new(self.expr(sc, t, depth + 1)),
                    Box::new(self.expr(sc, t, depth + 1)),
                )
            }
            2 if !self.prog.enums.is_empty() => self.match_expr(sc, t, depth),
            3 if self.callable > 0 => {
                let fs: Vec<&FnDecl> = self.prog.fns[..self.callable]
                    .iter()
                    .filter(|f| &f.result == t && f.effs.iter().all(|e| self.has(sc, e)))
                    // no struct literal can be written in a restricted context
                    .filter(|f| {
                        !sc.restricted
                            || f.params.iter().all(|(_, pt)| {
                                !self.is_struct(pt) || sc.vars.iter().any(|(_, vt)| vt == pt)
                            })
                    })
                    .collect();
                if fs.is_empty() {
                    return self.leaf(sc, t);
                }
                let f = (*self.rng.pick(&fs)).clone();
                E::Call(
                    f.name.clone(),
                    f.params
                        .iter()
                        .map(|(_, pt)| self.expr(sc, pt, depth + 1))
                        .collect(),
                )
            }
            4 if !self.prog.structs.is_empty() => {
                // a field projection of a struct-typed variable
                let candidates: Vec<(String, String, Ty)> = sc
                    .vars
                    .iter()
                    .filter_map(|(x, vt)| match vt {
                        Ty::Adt(n) => self
                            .prog
                            .structs
                            .iter()
                            .find(|(s, _)| s == n)
                            .and_then(|(_, fs)| fs.iter().find(|(_, ft)| ft == t))
                            .map(|(f, ft)| (x.clone(), f.clone(), ft.clone())),
                        _ => None,
                    })
                    .collect();
                if candidates.is_empty() {
                    return self.leaf(sc, t);
                }
                let (x, f, _) = self.rng.pick(&candidates).clone();
                E::Field(Box::new(E::Var(x)), f)
            }
            5 if self
                .prog
                .effects
                .first()
                .is_some_and(|d| &d.result == t && self.has(sc, &Eff::User(d.name.clone()))) =>
            {
                let d = self.prog.effects[0].clone();
                E::Perform(
                    d.name.clone(),
                    d.op.clone(),
                    d.params
                        .iter()
                        .map(|pt| self.expr(sc, pt, depth + 1))
                        .collect(),
                )
            }
            6 if matches!(t, Ty::I64 | Ty::U64) && self.rng.chance(50) => {
                let ops = [Op::Add, Op::Sub, Op::Mul];
                E::Wrap(
                    *self.rng.pick(&ops),
                    Box::new(self.expr(sc, t, depth + 1)),
                    Box::new(self.expr(sc, t, depth + 1)),
                )
            }
            6..=8 if matches!(t, Ty::I64 | Ty::U64) => {
                let op = *self.rng.pick(&ARITH);
                let a = self.expr(sc, t, depth + 1);
                let b = if matches!(op, Op::Div | Op::Rem) && self.rng.chance(60) {
                    E::Int(*self.rng.pick(&[1, 2, 3, 7, 10]))
                } else {
                    self.expr(sc, t, depth + 1)
                };
                E::Bin(op, Box::new(a), Box::new(b))
            }
            9 if *t == Ty::I64 => neg(self.expr(sc, t, depth + 1)),
            _ if *t == Ty::Bool => self.cond(sc, depth),
            _ => self.leaf(sc, t),
        }
    }
    fn cond(&mut self, sc: &Scope, depth: usize) -> E {
        match self.rng.below(5) {
            0 | 1 => {
                let it = if self.rng.chance(85) {
                    Ty::I64
                } else {
                    Ty::U64
                };
                let op = *self.rng.pick(&ORDER);
                if self.rng.chance(30)
                    && let Some(v) = self.var_of(sc, &it)
                {
                    // the `x == 0` / `x != 0` refinement shapes
                    let op = if self.rng.chance(50) { Op::Eq } else { Op::Ne };
                    return E::Bin(op, Box::new(v), Box::new(E::Int(0)));
                }
                E::Bin(
                    op,
                    Box::new(self.expr(sc, &it, depth + 1)),
                    Box::new(self.expr(sc, &it, depth + 1)),
                )
            }
            2 => E::Bin(
                *self.rng.pick(&[Op::And, Op::Or]),
                Box::new(self.expr(sc, &Ty::Bool, depth + 1)),
                Box::new(self.expr(sc, &Ty::Bool, depth + 1)),
            ),
            3 => E::Not(Box::new(self.expr(sc, &Ty::Bool, depth + 1))),
            _ => self.leaf(sc, &Ty::Bool),
        }
    }
    fn match_expr(&mut self, sc: &Scope, t: &Ty, depth: usize) -> E {
        let (en, variants) = self.rng.pick(&self.prog.enums).clone();
        let scrut = self.expr(&restrict(sc), &Ty::Adt(en.clone()), depth + 1);
        let mut arms = vec![];
        let wildcard_at = if self.rng.chance(25) {
            Some(self.rng.below(variants.len()))
        } else {
            None
        };
        for (i, (v, ts)) in variants.iter().enumerate() {
            if Some(i) == wildcard_at {
                arms.push((Pat::Wild, self.expr(sc, t, depth + 1)));
                break;
            }
            let mut inner = sc.clone();
            let subs = ts
                .iter()
                .map(|pt| {
                    if self.rng.chance(25) {
                        Pat::Wild
                    } else {
                        let x = self.name("p");
                        inner.vars.push((x.clone(), pt.clone()));
                        Pat::Bind(x)
                    }
                })
                .collect();
            arms.push((Pat::Ctor(v.clone(), subs), self.expr(&inner, t, depth + 1)));
        }
        E::Match(Box::new(scrut), arms)
    }
    fn any_ty(&mut self, restricted: bool) -> Ty {
        let mut tys = vec![Ty::I64, Ty::I64, Ty::I64, Ty::Bool, Ty::U64];
        for (n, _) in self.prog.structs.iter().filter(|_| !restricted) {
            tys.push(Ty::Adt(n.clone()));
        }
        for (n, _) in &self.prog.enums {
            tys.push(Ty::Adt(n.clone()));
        }
        self.rng.pick(&tys).clone()
    }
    /// An operation clause that resumes `k` exactly once.
    fn op_clause(&mut self, sc: &Scope, k: &str, arg: &Ty, out: &Ty) -> E {
        let v = self.name("r");
        let resume_arg = self.leaf(sc, arg);
        match self.rng.below(3) {
            0 => E::Resume(k.into(), Box::new(resume_arg)),
            1 => {
                // resume, then compute with the answer (no live continuation)
                let mut after = sc.clone();
                after.vars.push((v.clone(), out.clone()));
                let rest = self.expr(&after, out, 2);
                E::Let(
                    v,
                    None,
                    Box::new(E::Resume(k.into(), Box::new(resume_arg))),
                    Box::new(rest),
                )
            }
            _ => {
                let c = self.cond(&restrict(sc), 3);
                E::If(
                    Box::new(c),
                    Box::new(E::Resume(k.into(), Box::new(resume_arg.clone()))),
                    Box::new(E::Resume(k.into(), Box::new(self.leaf(sc, arg)))),
                )
            }
        }
    }
}

fn gen_decls(rng: &mut Rng) -> Prog {
    let mut p = Prog::default();
    if rng.chance(60) {
        let mut fields = vec![("a".to_string(), Ty::I64)];
        if rng.chance(60) {
            fields.push(("b".into(), if rng.chance(50) { Ty::Bool } else { Ty::I64 }));
        }
        if rng.chance(30) {
            fields.push(("c".into(), Ty::I64));
        }
        p.structs.push(("Pt".into(), fields));
    }
    if rng.chance(60) {
        let mut variants = vec![("Nil".to_string(), vec![]), ("One".into(), vec![Ty::I64])];
        if rng.chance(50) {
            variants.push(("Two".into(), vec![Ty::I64, Ty::Bool]));
        }
        p.enums.push(("Sh".into(), variants));
    }
    if rng.chance(50) {
        let params = if rng.chance(50) {
            vec![]
        } else {
            vec![Ty::I64]
        };
        p.effects.push(EffDecl {
            name: "Ask".into(),
            op: "ask".into(),
            params,
            result: Ty::I64,
        });
    }
    p
}

/// Generate one program; `mutate` asks for one deliberate defect.
fn generate(seed: u64, mutate: bool) -> Prog {
    let mut rng = Rng(seed);
    let mut prog = gen_decls(&mut rng);
    let helpers = rng.below(4);
    let has_ask = !prog.effects.is_empty();
    for i in 0..=helpers {
        let is_main = i == helpers;
        let mut effs = vec![];
        if !is_main {
            if rng.chance(60) {
                effs.push(fail("ArithOverflow"));
            }
            if rng.chance(35) {
                effs.push(fail("RuntimeError"));
            }
            if has_ask && rng.chance(30) {
                effs.push(Eff::User("Ask".into()));
            }
        }
        let result = if is_main || rng.chance(75) {
            if rng.chance(85) { Ty::I64 } else { Ty::Bool }
        } else {
            Ty::Adt(
                prog.structs
                    .first()
                    .map(|s| s.0.clone())
                    .unwrap_or_else(|| "Unused".into()),
            )
        };
        let result = if matches!(&result, Ty::Adt(n) if n == "Unused") {
            Ty::I64
        } else {
            result
        };
        let mut params = vec![];
        if !is_main {
            for j in 0..rng.below(3) {
                let t = match rng.below(5) {
                    0 => Ty::Bool,
                    1 if !prog.structs.is_empty() => Ty::Adt(prog.structs[0].0.clone()),
                    2 if !prog.enums.is_empty() => Ty::Adt(prog.enums[0].0.clone()),
                    _ => Ty::I64,
                };
                params.push((format!("v{j}"), t));
            }
        }
        let name = if is_main {
            "main".to_string()
        } else {
            format!("f{i}")
        };
        let body = {
            let mut g = Gen {
                rng: Rng(rng.next()),
                prog: &prog,
                fresh: 0,
                callable: i,
            };
            if is_main {
                main_body(&mut g, &result)
            } else {
                let sc = Scope {
                    vars: params.clone(),
                    effs: effs.clone(),
                    restricted: false,
                };
                g.expr(&sc, &result, 0)
            }
        };
        prog.fns.push(FnDecl {
            name,
            params,
            result,
            effs,
            body,
        });
    }
    if mutate {
        let mut rng = Rng(seed ^ 0xdead_beef);
        mutate_prog(&mut prog, &mut rng);
    }
    prog
}

/// `main` installs the handlers: Fail[ArithOverflow] and Fail[RuntimeError]
/// (each usually), and the user effect's one-shot handler when declared.
fn main_body(g: &mut Gen<'_>, result: &Ty) -> E {
    let mut effs = vec![];
    let ao = g.rng.chance(85);
    let re = g.rng.chance(75);
    if ao {
        effs.push(fail("ArithOverflow"));
    }
    if re {
        effs.push(fail("RuntimeError"));
    }
    let ask = g.prog.effects.first().cloned();
    let use_ask = ask.is_some() && g.rng.chance(80);
    if use_ask {
        effs.push(Eff::User("Ask".into()));
    }
    let sc = Scope {
        vars: vec![],
        effs: effs.clone(),
        restricted: false,
    };
    let mut body = g.expr(&sc, result, 0);
    // inner handler first: the user effect, then the failures around it
    if use_ask {
        let d = ask.unwrap();
        let k = "k".to_string();
        let params: Vec<String> = (0..d.params.len()).map(|i| format!("q{i}")).collect();
        // clauses run outside the user effect's handler, under the failures
        let mut clause_sc = Scope {
            vars: params
                .iter()
                .cloned()
                .zip(d.params.iter().cloned())
                .collect(),
            effs: effs
                .iter()
                .filter(|e| matches!(e, Eff::Fail(_)))
                .cloned()
                .collect(),
            restricted: false,
        };
        // a live continuation forbids aborts until it is resumed: keep
        // failure-raising arithmetic out of resumption arguments
        clause_sc.effs.clear();
        let op_body = g.op_clause(&clause_sc, &k, &d.result, result);
        let rx = "v".to_string();
        let ret = if g.rng.chance(60) {
            E::Var(rx.clone())
        } else {
            E::Wrap(Op::Add, Box::new(E::Var(rx.clone())), Box::new(g.lit()))
        };
        body = E::Handle {
            eff: Eff::User(d.name.clone()),
            body: Box::new(body),
            rx,
            ret: Box::new(ret),
            params,
            k,
            op_body: Box::new(op_body),
        };
    }
    for (on, payload, value) in [(re, "RuntimeError", -2), (ao, "ArithOverflow", -1)] {
        if on {
            let failure_value = if *result == Ty::Bool {
                E::Bool(value == -1)
            } else {
                E::Int(value)
            };
            body = E::Handle {
                eff: fail(payload),
                body: Box::new(body),
                rx: "v".into(),
                ret: Box::new(E::Var("v".into())),
                params: vec!["e".into()],
                k: "_".into(),
                op_body: Box::new(failure_value),
            };
        }
    }
    body
}

/// Apply one deliberate defect, retrying kinds and functions until one
/// applies.
fn mutate_prog(p: &mut Prog, rng: &mut Rng) {
    for _ in 0..32 {
        let before = p.clone();
        mutate_once(p, rng);
        if *p != before {
            return;
        }
    }
}

fn mutate_once(p: &mut Prog, rng: &mut Rng) {
    let f = rng.below(p.fns.len());
    let kind = rng.below(9);
    let fd = &mut p.fns[f];
    match kind {
        0 => {
            // an unbound name somewhere
            replace_first(&mut fd.body, rng, &|e| matches!(e, E::Var(_)), &|_| {
                E::Var("zz_unbound".into())
            });
        }
        1 => {
            // a bool where an integer is expected
            replace_first(&mut fd.body, rng, &|e| matches!(e, E::Int(_)), &|_| {
                E::Bool(true)
            });
        }
        2 => {
            // an integer where a bool is expected
            replace_first(&mut fd.body, rng, &|e| matches!(e, E::Bool(_)), &|_| {
                E::Int(1)
            });
        }
        3 => {
            // drop the outermost handler of main
            if let E::Handle { body, .. } = &fd.body {
                fd.body = (**body).clone();
            }
        }
        4 => {
            // wrong arity
            replace_first(&mut fd.body, rng, &|e| matches!(e, E::Call(..)), &|e| {
                let E::Call(f, mut args) = e.clone() else {
                    unreachable!()
                };
                args.push(E::Int(0));
                E::Call(f, args)
            });
        }
        5 => {
            // a non-exhaustive match
            replace_first(
                &mut fd.body,
                rng,
                &|e| matches!(e, E::Match(_, arms) if arms.len() > 1),
                &|e| {
                    let E::Match(s, mut arms) = e.clone() else {
                        unreachable!()
                    };
                    arms.pop();
                    E::Match(s, arms)
                },
            );
        }
        6 => {
            // resume twice, or never
            replace_first(
                &mut fd.body,
                rng,
                &|e| {
                    matches!(
                        e,
                        E::Handle {
                            eff: Eff::User(_),
                            ..
                        }
                    )
                },
                &|e| {
                    let E::Handle {
                        eff,
                        body,
                        rx,
                        ret,
                        params,
                        k,
                        op_body,
                    } = e.clone()
                    else {
                        unreachable!()
                    };
                    let op_body = if matches!(*op_body, E::Resume(..)) {
                        E::Wrap(
                            Op::Add,
                            Box::new((*op_body).clone()),
                            Box::new((*op_body).clone()),
                        )
                    } else {
                        E::Int(0)
                    };
                    E::Handle {
                        eff,
                        body,
                        rx,
                        ret,
                        params,
                        k,
                        op_body: Box::new(op_body),
                    }
                },
            );
        }
        7 => {
            // an unknown field
            replace_first(&mut fd.body, rng, &|e| matches!(e, E::Field(..)), &|e| {
                let E::Field(b, _) = e.clone() else {
                    unreachable!()
                };
                E::Field(b, "zz".into())
            });
        }
        _ => {
            // checked arithmetic while the continuation is live
            replace_first(&mut fd.body, rng, &|e| matches!(e, E::Resume(..)), &|e| {
                let E::Resume(k, a) = e.clone() else {
                    unreachable!()
                };
                E::Resume(
                    k,
                    Box::new(E::Bin(Op::Add, a, Box::new(E::Var("v".into())))),
                )
            });
        }
    }
}

fn children(e: &mut E) -> Vec<&mut E> {
    match e {
        E::Int(_) | E::Bool(_) | E::Var(_) => vec![],
        E::Neg(a) | E::Not(a) | E::Field(a, _) | E::Resume(_, a) => vec![a],
        E::Bin(_, a, b) | E::Wrap(_, a, b) | E::Seq(a, b) | E::Let(_, _, a, b) => vec![a, b],
        E::If(a, b, c) => vec![a, b, c],
        E::Call(_, xs) | E::Ctor(_, xs) | E::Perform(_, _, xs) => xs.iter_mut().collect(),
        E::Struct(_, fs) => fs.iter_mut().map(|(_, e)| e).collect(),
        E::Match(s, arms) => {
            let mut v: Vec<&mut E> = vec![s];
            v.extend(arms.iter_mut().map(|(_, e)| e));
            v
        }
        E::Handle {
            body, ret, op_body, ..
        } => vec![body, ret, op_body],
    }
}

fn count(e: &mut E, pred: &dyn Fn(&E) -> bool) -> usize {
    usize::from(pred(e))
        + children(e)
            .into_iter()
            .map(|c| count(c, pred))
            .sum::<usize>()
}

fn replace_nth(e: &mut E, n: &mut usize, pred: &dyn Fn(&E) -> bool, f: &dyn Fn(&E) -> E) {
    if pred(e) {
        if *n == 0 {
            *e = f(e);
            *n = usize::MAX;
            return;
        }
        *n -= 1;
    }
    for c in children(e) {
        if *n == usize::MAX {
            return;
        }
        replace_nth(c, n, pred, f);
    }
}

fn replace_first(e: &mut E, rng: &mut Rng, pred: &dyn Fn(&E) -> bool, f: &dyn Fn(&E) -> E) {
    let total = count(e, pred);
    if total > 0 {
        let mut n = rng.below(total);
        replace_nth(e, &mut n, pred, f);
    }
}

// ---------------------------------------------------------------------------
// Running both sides
// ---------------------------------------------------------------------------

/// The Rust seed's observable behaviour on one program.
#[derive(Debug)]
enum RustRun {
    Parse(String),
    Reject(String),
    Accept {
        eval: Result<String, String>,
        jit: Result<String, String>,
    },
}

fn run_rust(source: &str) -> (RustRun, Option<ast::Module>) {
    let p = parser::parse(source);
    if let Some(d) = p.diagnostics.first() {
        return (RustRun::Parse(format!("{}: {}", d.code, d.message)), None);
    }
    let c = check::check(&p.module);
    if let Some(d) = c.diagnostics.first() {
        return (RustRun::Reject(d.code.clone()), Some(p.module));
    }
    let eval = Runtime::new(&c.world)
        .run("main", vec![])
        .map(|v| v.to_string());
    let jit = native::jit(&c.world, "main")
        .map(|v| v.to_string())
        .map_err(|e| e.to_string());
    (RustRun::Accept { eval, jit }, Some(p.module))
}

/// One parsed reference verdict line.
#[derive(Debug, PartialEq)]
enum LeanVerdict {
    Reject(String),
    Accept {
        ty: String,
        outcome: String,
        live_linear: usize,
    },
    Other(String),
}

fn parse_verdict(line: &str) -> LeanVerdict {
    if let Some(code) = line.strip_prefix("reject ") {
        return LeanVerdict::Reject(code.trim().to_string());
    }
    let Some(rest) = line.strip_prefix("accept ") else {
        return LeanVerdict::Other(line.to_string());
    };
    let Some((ty, rest)) = rest.split_once(' ') else {
        return LeanVerdict::Other(line.to_string());
    };
    let (outcome, stats) = rest.split_once(" ; ").unwrap_or((rest, ""));
    let live_linear = stats
        .split_whitespace()
        .find_map(|kv| kv.strip_prefix("live_linear="))
        .and_then(|n| n.parse().ok())
        .unwrap_or(usize::MAX);
    LeanVerdict::Accept {
        ty: ty.to_string(),
        outcome: outcome.to_string(),
        live_linear,
    }
}

/// Resolve an elan proxy to the pinned toolchain's binary (as the other Lean
/// tests do).
fn resolve_lean(lean: String) -> String {
    let Ok(output) = std::process::Command::new(&lean)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .arg("--print-prefix")
        .output()
    else {
        return lean;
    };
    let prefix = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let binary = PathBuf::from(prefix)
        .join("bin")
        .join(format!("lean{}", std::env::consts::EXE_SUFFIX));
    if output.status.success() && binary.is_file() {
        binary.to_string_lossy().into_owned()
    } else {
        lean
    }
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "vesper-lockstep-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir(&dir).expect("private temporary directory");
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn reference_source() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lean/ReferenceEval.lean")
}

/// Run the Lean reference over the wire programs, one verdict per program.
fn run_lean(lean: &str, programs: &[String]) -> Vec<String> {
    let dir = TempDir::new("reference");
    // Interpret a private regular-file copy: Bazel runfiles are symlinks.
    let model = dir.0.join("ReferenceEval.lean");
    std::fs::copy(reference_source(), &model).expect("copy the reference");
    let input = dir.0.join("programs.txt");
    std::fs::write(&input, programs.join("\n") + "\n").expect("write programs");
    let out = std::process::Command::new(lean)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .arg("--run")
        .arg(&model)
        .arg(&input)
        .output()
        .expect("spawn Lean");
    let stdout = String::from_utf8(out.stdout).expect("UTF-8 verdicts");
    assert!(
        out.status.success(),
        "Lean reference failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<String> = stdout.lines().map(str::to_string).collect();
    assert_eq!(lines.len(), programs.len(), "one verdict per program");
    lines
}

// ---------------------------------------------------------------------------
// Comparison
// ---------------------------------------------------------------------------

struct Case {
    name: String,
    source: String,
    wire: String,
}

#[derive(Default)]
struct Tally {
    programs: usize,
    accepted: usize,
    rejected: usize,
    jit_unsupported: usize,
    codes: BTreeMap<String, usize>,
    outcomes: BTreeMap<String, usize>,
    disagreements: Vec<String>,
}

impl Tally {
    fn summary(&self) -> String {
        format!(
            "{} programs: {} accepted, {} rejected, {} JIT-unsupported; codes {:?}; outcomes {:?}; {} disagreements",
            self.programs,
            self.accepted,
            self.rejected,
            self.jit_unsupported,
            self.codes,
            self.outcomes,
            self.disagreements.len()
        )
    }
}

fn compare(case: &Case, rust: &RustRun, lean: &str, tally: &mut Tally) {
    tally.programs += 1;
    let mut disagree = |why: String| {
        tally.disagreements.push(format!(
            "{}: {why}\n  lean: {lean}\n  rust: {rust:?}\n  source:\n{}\n  wire: {}",
            case.name, case.source, case.wire
        ));
    };
    match (rust, parse_verdict(lean)) {
        (RustRun::Parse(e), _) => disagree(format!("generated source does not parse: {e}")),
        (RustRun::Reject(code), LeanVerdict::Reject(lc)) => {
            tally.rejected += 1;
            *tally.codes.entry(code.clone()).or_default() += 1;
            if *code != lc {
                disagree(format!(
                    "first diagnostic {code} (Rust) vs {lc} (reference)"
                ));
            }
        }
        (
            RustRun::Accept { eval, jit },
            LeanVerdict::Accept {
                outcome,
                live_linear,
                ..
            },
        ) => {
            tally.accepted += 1;
            let kind = outcome.split_whitespace().next().unwrap_or("").to_string();
            *tally.outcomes.entry(kind).or_default() += 1;
            let lean_value = outcome.strip_prefix("value ");
            match (eval, lean_value) {
                (Ok(v), Some(lv)) if v == lv => {}
                (e, _) => disagree(format!("evaluator {e:?} vs reference {outcome}")),
            }
            match jit {
                Ok(j) if Ok(j) == eval.as_ref() => {}
                Ok(j) => disagree(format!("JIT {j} vs evaluator {eval:?}")),
                Err(e) if e.contains("unsupported") => tally.jit_unsupported += 1,
                Err(e) => disagree(format!("JIT error {e}")),
            }
            if live_linear != 0 {
                disagree(format!(
                    "{live_linear} linear continuations left live by an accepted program"
                ));
            }
        }
        (r, l) => disagree(format!("verdicts differ: {r:?} vs {l:?}")),
    }
}

// ---------------------------------------------------------------------------
// Corpora
// ---------------------------------------------------------------------------

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lockstep")
}

/// The `.ves` fixtures of the fragment: `tests/fixtures/lockstep/*.ves` and
/// the examples that translate. Untranslatable examples are listed, not run.
fn fixture_cases() -> (Vec<Case>, Vec<(String, String)>) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files: Vec<PathBuf> = vec![];
    for dir in [root.join("examples"), fixture_dir()] {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
            .expect("fixture directory")
            .map(|e| e.expect("entry").path())
            .filter(|p| p.extension().is_some_and(|x| x == "ves"))
            .collect();
        entries.sort();
        files.extend(entries);
    }
    let mut cases = vec![];
    let mut skipped = vec![];
    for path in files {
        let name = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let source = std::fs::read_to_string(&path).expect("fixture source");
        let parsed = parser::parse(&source);
        if !parsed.diagnostics.is_empty() {
            skipped.push((name, "does not parse".into()));
            continue;
        }
        match translate(&parsed.module) {
            Ok(p) => cases.push(Case {
                name,
                source,
                wire: wire_prog(&p),
            }),
            Err(why) => skipped.push((name, why)),
        }
    }
    (cases, skipped)
}

/// Bracket nesting of the source; the parser's default depth bound is 128.
fn nesting(source: &str) -> usize {
    let (mut depth, mut max) = (0usize, 0usize);
    for c in source.chars() {
        match c {
            '(' | '{' | '[' => {
                depth += 1;
                max = max.max(depth);
            }
            ')' | '}' | ']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    max
}

/// `n` generated programs from consecutive seeds, a quarter of them
/// mutated; seeds whose source would exceed the parser's nesting bound are
/// skipped.
fn generated_cases(first_seed: u64, n: usize) -> Vec<Case> {
    let mut cases = vec![];
    let mut seed = first_seed;
    while cases.len() < n {
        let prog = generate(seed, cases.len() % 4 == 3);
        let source = src_prog(&prog);
        // stay inside the parser's default nesting bound
        let bounded = nesting(&source) <= 64
            && !parser::parse(&source)
                .diagnostics
                .iter()
                .any(|d| d.code == "E0003");
        if bounded {
            cases.push(Case {
                name: format!("seed {seed}"),
                source,
                wire: wire_prog(&prog),
            });
        }
        seed = seed.wrapping_add(1);
    }
    cases
}

fn golden_path(name: &str) -> PathBuf {
    fixture_dir().join(format!("{name}.golden"))
}

/// The reference verdicts: computed with `VESPER_LEAN` (and checked against,
/// or written to, the recorded file), or read from the recorded file.
fn reference_verdicts(name: &str, cases: &[Case]) -> Vec<String> {
    let path = golden_path(name);
    let wires: Vec<String> = cases.iter().map(|c| c.wire.clone()).collect();
    let recorded: Option<Vec<String>> = std::fs::read_to_string(&path)
        .ok()
        .map(|s| s.lines().map(str::to_string).collect());
    match std::env::var("VESPER_LEAN") {
        Ok(lean) => {
            let lean = resolve_lean(lean);
            let verdicts = run_lean(&lean, &wires);
            let bless = std::env::var("VESPER_LOCKSTEP_BLESS").is_ok_and(|v| v == "1");
            if bless {
                std::fs::write(&path, verdicts.join("\n") + "\n").expect("write golden");
            } else {
                let recorded = recorded.unwrap_or_else(|| {
                    panic!(
                        "{} is missing; run with VESPER_LOCKSTEP_BLESS=1",
                        path.display()
                    )
                });
                for (i, (got, want)) in verdicts.iter().zip(&recorded).enumerate() {
                    assert_eq!(
                        got, want,
                        "reference verdict {i} ({}) changed; rerun with VESPER_LOCKSTEP_BLESS=1 if intended",
                        cases[i].name
                    );
                }
                assert_eq!(verdicts.len(), recorded.len(), "recorded verdict count");
            }
            verdicts
        }
        Err(_) => {
            eprintln!("VESPER_LEAN unset; comparing against recorded reference verdicts");
            let recorded = recorded.unwrap_or_else(|| panic!("{} is missing", path.display()));
            assert_eq!(recorded.len(), cases.len(), "recorded verdict count");
            recorded
        }
    }
}

fn lockstep(name: &str, cases: &[Case], verdicts: &[String]) -> Tally {
    let mut tally = Tally::default();
    for (case, lean) in cases.iter().zip(verdicts) {
        let (rust, module) = run_rust(&case.source);
        // Generated programs: the parser's reading must translate back to
        // exactly the program the generator wrote.
        if name.starts_with("generated")
            && let Some(module) = &module
        {
            match translate(module) {
                Ok(p) if wire_prog(&p) == case.wire => {}
                Ok(p) => tally.disagreements.push(format!(
                    "{}: parser reading differs from the generated program\n  parsed: {}\n  wrote:  {}",
                    case.name,
                    wire_prog(&p),
                    case.wire
                )),
                Err(e) => tally
                    .disagreements
                    .push(format!("{}: parsed program does not translate: {e}", case.name)),
            }
        }
        compare(case, &rust, lean, &mut tally);
    }
    tally
}

fn report(name: &str, tally: &Tally) {
    eprintln!("lockstep {name}: {}", tally.summary());
    assert!(
        tally.disagreements.is_empty(),
        "lockstep {name}: {} disagreements:\n{}",
        tally.disagreements.len(),
        tally.disagreements.join("\n\n")
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Seeds of the CI corpus: a few hundred programs, a quarter mutated.
const CI_SEED: u64 = 0x5eed_0453;
const CI_PROGRAMS: usize = 400;

#[test]
fn fixtures_agree_with_the_reference() {
    let (cases, skipped) = fixture_cases();
    for (name, why) in &skipped {
        eprintln!("not in the fragment: {name}: {why}");
    }
    assert!(cases.len() >= 20, "only {} fixtures translate", cases.len());
    let verdicts = reference_verdicts("fixtures", &cases);
    let tally = lockstep("fixtures", &cases, &verdicts);
    report("fixtures", &tally);
    assert!(
        tally.accepted >= 10 && tally.rejected >= 8,
        "{}",
        tally.summary()
    );
}

#[test]
fn generated_programs_agree_with_the_reference() {
    let cases = generated_cases(CI_SEED, CI_PROGRAMS);
    let verdicts = reference_verdicts("generated", &cases);
    let tally = lockstep("generated", &cases, &verdicts);
    report("generated", &tally);
    // The corpus exercises both verdicts and several diagnostics.
    assert!(tally.accepted >= CI_PROGRAMS / 3, "{}", tally.summary());
    assert!(tally.rejected >= CI_PROGRAMS / 10, "{}", tally.summary());
    assert!(tally.codes.len() >= 5, "{}", tally.summary());
}

/// The nightly-sized campaign: `VESPER_LOCKSTEP_PROGRAMS` programs from
/// `VESPER_LOCKSTEP_SEED`, against the live reference (needs `VESPER_LEAN`).
#[test]
fn nightly_campaign() {
    let Ok(n) = std::env::var("VESPER_LOCKSTEP_PROGRAMS") else {
        eprintln!("VESPER_LOCKSTEP_PROGRAMS unset; nightly campaign skipped");
        return;
    };
    let n: usize = n.parse().expect("program count");
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        panic!("the nightly campaign needs VESPER_LEAN");
    };
    let seed = std::env::var("VESPER_LOCKSTEP_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x0b5e_55ed);
    let lean = resolve_lean(lean);
    let cases = generated_cases(seed, n);
    let mut verdicts = vec![];
    for chunk in cases.chunks(2000) {
        let wires: Vec<String> = chunk.iter().map(|c| c.wire.clone()).collect();
        verdicts.extend(run_lean(&lean, &wires));
    }
    let tally = lockstep("generated-nightly", &cases, &verdicts);
    report("nightly", &tally);
}

#[test]
fn reference_is_executable_lean_without_escape_hatches() {
    let source = std::fs::read_to_string(reference_source()).expect("read the reference");
    for banned in [
        "sorry",
        "admit",
        "axiom ",
        "unsafe",
        "partial ",
        "native_decide",
        "@[implemented_by",
        "@[extern",
        "opaque ",
        "import ",
    ] {
        assert!(
            !source.contains(banned),
            "ReferenceEval.lean contains {banned:?}"
        );
    }
    for required in ["def checkExpr", "def evalSteps", "def main"] {
        assert!(source.contains(required), "{required} missing");
    }
}

/// The soundness proof (lean/ReferenceEval/Soundness.lean) against Appendix A's
/// model: compiled in a private directory from regular-file copies of the
/// reference and of lean/MetatheoryFull.lean, which it imports read-only.
/// Every theorem may rest only on Lean's standard axioms.
#[test]
fn soundness_proof_checks_with_standard_axioms_only() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let proof = root.join("lean/ReferenceEval/Soundness.lean");
    let text = std::fs::read_to_string(&proof).expect("read the soundness proof");
    for banned in [
        "sorry",
        "admit",
        "axiom ",
        "native_decide",
        "@[implemented_by",
        "@[extern",
        "unsafe",
    ] {
        assert!(!text.contains(banned), "Soundness.lean contains {banned:?}");
    }
    let theorems = [
        "ok_typed",
        "shape_ok",
        "reference_checker_sound",
        "reference_checker_sound_closed",
    ];
    for name in theorems {
        assert!(
            text.contains(&format!(
                "#print axioms Vesper.ReferenceEval.Soundness.{name}\n"
            )) || text.ends_with(&format!(
                "#print axioms Vesper.ReferenceEval.Soundness.{name}"
            )),
            "no axiom audit for {name}"
        );
    }
    let Ok(lean) = std::env::var("VESPER_LEAN") else {
        eprintln!("VESPER_LEAN unset; soundness proof not checked");
        return;
    };
    let lean = resolve_lean(lean);
    let prefix = PathBuf::from(&lean)
        .parent()
        .and_then(Path::parent)
        .expect("Lean prefix")
        .to_path_buf();
    let dir = TempDir::new("soundness");
    let lean_path =
        std::env::join_paths([dir.0.clone(), prefix.join("lib").join("lean")]).expect("LEAN_PATH");
    let run = |args: &[&std::ffi::OsStr]| {
        let out = std::process::Command::new(&lean)
            .current_dir(&dir.0)
            .env("LEAN_PATH", &lean_path)
            .args(args)
            .output()
            .expect("spawn Lean");
        let text = String::from_utf8_lossy(&out.stdout).into_owned()
            + &String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "Lean failed on {args:?}:\n{text}");
        text
    };
    for module in ["MetatheoryFull", "ReferenceEval"] {
        let source = dir.0.join(format!("{module}.lean"));
        std::fs::copy(root.join(format!("lean/{module}.lean")), &source).expect("copy module");
        let olean = dir.0.join(format!("{module}.olean"));
        run(&["-o".as_ref(), olean.as_os_str(), source.as_os_str()]);
    }
    let local = dir.0.join("Soundness.lean");
    std::fs::copy(&proof, &local).expect("copy the proof");
    let output = run(&[local.as_os_str()]);
    assert!(!output.contains("error"), "{output}");
    assert!(!output.contains("sorryAx"), "{output}");
    let flat = output.split_whitespace().collect::<Vec<_>>().join(" ");
    for name in theorems {
        let head = format!("'Vesper.ReferenceEval.Soundness.{name}' depends on axioms: [");
        let start = flat
            .find(&head)
            .unwrap_or_else(|| panic!("no axiom report for {name}:\n{output}"))
            + head.len();
        let list = &flat[start..start + flat[start..].find(']').expect("axiom list")];
        for axiom in list.split(',').map(str::trim) {
            assert!(
                ["propext", "Classical.choice", "Quot.sound"].contains(&axiom),
                "{name} depends on {axiom}"
            );
        }
    }
}

#[test]
fn generator_is_deterministic_and_round_trips() {
    for seed in 0..50 {
        let a = generate(seed, seed % 3 == 0);
        assert_eq!(a, generate(seed, seed % 3 == 0));
        let p = parser::parse(&src_prog(&a));
        assert!(
            p.diagnostics.is_empty(),
            "{:?}\n{}",
            p.diagnostics,
            src_prog(&a)
        );
        assert_eq!(
            wire_prog(&translate(&p.module).expect("translate")),
            wire_prog(&a)
        );
    }
    assert!(Path::new(&reference_source()).is_file());
}
