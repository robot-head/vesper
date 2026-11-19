//! The port build's side of the boundary (`cfg(vesper_port)`): the flat
//! encoding of syntax trees that the Vesper printer and reference walks read
//! (`vesper/src/lib.ves`), and the error messages of its error codes.
//!
//! A tree is one prefix-order word stream over a text pool. A text (an
//! identifier, a literal) is its pool offset and length. A name is its
//! rooted flag, its component count and its components. Every list is its
//! length followed by its items, and every optional item is 0, or 1 followed
//! by the item. The tags of each node kind are in the `encode_*` functions.

pub use crate::port_binding::*;
use crate::*;

/// The Rust message of each error code of the port (`LeanError`).
pub fn error(e: LeanError) -> Error {
    Error(
        match e {
            LeanError::OneTuple => "Lean has no one-element tuple syntax",
            LeanError::NoBinders => "lambda/forall requires at least one binder",
            LeanError::ShortProduct => "product type needs at least two elements",
            LeanError::EmptyMatch => "empty match syntax is unsupported",
            LeanError::EmptyUpdate => "record update needs at least one field",
            LeanError::MutablePattern => "mutable do bindings require one local identifier",
            LeanError::EmptyStatementMatch => "empty statement match is unsupported; use nomatch",
            LeanError::OptionType => "option value has the wrong fixed type",
            LeanError::EmptyMutual => "empty mutual group",
            LeanError::RootedNamespace => "root-qualified namespace commands are unsupported",
        }
        .into(),
    )
}

/// The error codes of the Vesper printer, as the binding's Rust type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeanError {
    OneTuple,
    NoBinders,
    ShortProduct,
    EmptyMatch,
    EmptyUpdate,
    MutablePattern,
    EmptyStatementMatch,
    OptionType,
    EmptyMutual,
    RootedNamespace,
}

#[derive(Default)]
pub struct Enc {
    pub words: Vec<u64>,
    pub pool: String,
}

impl Enc {
    fn w(&mut self, x: u64) {
        self.words.push(x);
    }
    fn text(&mut self, s: &str) {
        self.words.push(self.pool.len() as u64);
        self.words.push(s.len() as u64);
        self.pool.push_str(s);
    }
    fn len(&mut self, n: usize) {
        self.words.push(n as u64);
    }
    fn ident(&mut self, i: &Ident) {
        self.text(&i.0);
    }
    pub fn name(&mut self, n: &Name) {
        self.w(n.rooted as u64);
        self.len(n.components.len());
        for c in &n.components {
            self.ident(c);
        }
    }
    fn char(&mut self, c: char) {
        let mut buf = [0u8; 4];
        self.text(c.encode_utf8(&mut buf));
    }
    pub fn binders(&mut self, bs: &[Binder]) {
        self.len(bs.len());
        for b in bs {
            self.w(match b.kind {
                BinderKind::Explicit => 0,
                BinderKind::Implicit => 1,
                BinderKind::Instance => 2,
            });
            match &b.name {
                Some(n) => {
                    self.w(1);
                    self.ident(n);
                }
                None => self.w(0),
            }
            self.term(&b.ty);
        }
    }
    /// Patterns: 0 `_`, 1 bind, 2 constructor, 3 tuple, 4 nat, 5 bool,
    /// 6 string, 7 char, 8 int (low and high words), 9 record.
    fn pattern(&mut self, p: &Pattern) {
        match p {
            Pattern::Wildcard => self.w(0),
            Pattern::Bind(i) => {
                self.w(1);
                self.ident(i);
            }
            Pattern::Constructor { name, arguments } => {
                self.w(2);
                self.name(name);
                self.len(arguments.len());
                for a in arguments {
                    self.pattern(a);
                }
            }
            Pattern::Tuple(ps) => {
                self.w(3);
                self.len(ps.len());
                for a in ps {
                    self.pattern(a);
                }
            }
            Pattern::Nat(n) => {
                self.w(4);
                self.text(&n.0);
            }
            Pattern::Bool(b) => {
                self.w(5);
                self.w(*b as u64);
            }
            Pattern::String(s) => {
                self.w(6);
                self.text(s);
            }
            Pattern::Char(c) => {
                self.w(7);
                self.char(*c);
            }
            Pattern::Int(v) => {
                self.w(8);
                self.w(*v as u128 as u64);
                self.w((*v as u128 >> 64) as u64);
            }
            Pattern::Record { fields } => {
                self.w(9);
                self.len(fields.len());
                for (i, p) in fields {
                    self.ident(i);
                    self.pattern(p);
                }
            }
        }
    }
    fn binding(&mut self, b: &LetBinding) {
        self.pattern(&b.pattern);
        match &b.ty {
            Some(t) => {
                self.w(1);
                self.term(t);
            }
            None => self.w(0),
        }
        self.term(&b.value);
    }
    fn fields(&mut self, fs: &[(Ident, Term)]) {
        for (i, t) in fs {
            self.ident(i);
            self.term(t);
        }
    }
    /// Terms: 0 `_`, 1 ref, 2 Prop, 3 Type (level), 4 nat, 5 bool, 6 string,
    /// 7 char, 8 app (argument count, the function's word count, function,
    /// arguments), 9 fun, 10 forall, 11 arrow, 12 product, 13 tuple, 14 list,
    /// 15 let, 16 if, 17 match (arm count, scrutinee, arms), 18 projection,
    /// 19 ascription, 20 unary (op), 21 binary (op), 22 record, 23 update
    /// (field count, base, fields), 24 do, 25 rfl, 26 by decide, 27 nomatch.
    pub fn term(&mut self, t: &Term) {
        match t {
            Term::Infer => self.w(0),
            Term::Ref(n) => {
                self.w(1);
                self.name(n);
            }
            Term::Sort(Sort::Prop) => self.w(2),
            Term::Sort(Sort::Type(l)) => {
                self.w(3);
                self.w(u64::from(*l));
            }
            Term::Nat(n) => {
                self.w(4);
                self.text(&n.0);
            }
            Term::Bool(b) => {
                self.w(5);
                self.w(*b as u64);
            }
            Term::String(s) => {
                self.w(6);
                self.text(s);
            }
            Term::Char(c) => {
                self.w(7);
                self.char(*c);
            }
            Term::App {
                function,
                arguments,
            } => {
                self.w(8);
                self.len(arguments.len());
                let size = self.words.len();
                self.w(0);
                self.term(function);
                self.words[size] = (self.words.len() - size - 1) as u64;
                for a in arguments {
                    match a {
                        Argument::Positional(t) => {
                            self.w(0);
                            self.term(t);
                        }
                        Argument::Named { name, value } => {
                            self.w(1);
                            self.ident(name);
                            self.term(value);
                        }
                    }
                }
            }
            Term::Lambda { binders, body } | Term::Forall { binders, body } => {
                self.w(if matches!(t, Term::Lambda { .. }) {
                    9
                } else {
                    10
                });
                self.binders(binders);
                self.term(body);
            }
            Term::Arrow(a, b) => {
                self.w(11);
                self.term(a);
                self.term(b);
            }
            Term::ProductType(ts) | Term::Tuple(ts) | Term::List(ts) => {
                self.w(match t {
                    Term::ProductType(_) => 12,
                    Term::Tuple(_) => 13,
                    _ => 14,
                });
                self.len(ts.len());
                for t in ts {
                    self.term(t);
                }
            }
            Term::Let { binding, body } => {
                self.w(15);
                self.binding(binding);
                self.term(body);
            }
            Term::If { condition, yes, no } => {
                self.w(16);
                self.term(condition);
                self.term(yes);
                self.term(no);
            }
            Term::Match { scrutinee, arms } => {
                self.w(17);
                self.len(arms.len());
                self.term(scrutinee);
                for arm in arms {
                    self.pattern(&arm.pattern);
                    self.term(&arm.body);
                }
            }
            Term::Project { base, projection } => {
                self.w(18);
                self.term(base);
                match projection {
                    Projection::Field(i) => {
                        self.w(0);
                        self.ident(i);
                    }
                    Projection::Tuple(n) => {
                        self.w(1);
                        self.w(u64::from(n.get()));
                    }
                }
            }
            Term::Ascribe { value, ty } => {
                self.w(19);
                self.term(value);
                self.term(ty);
            }
            Term::Unary { op, value } => {
                self.w(20);
                self.w(match op {
                    UnaryOp::Neg => 0,
                    UnaryOp::Not => 1,
                    UnaryOp::BoolNot => 2,
                });
                self.term(value);
            }
            Term::Binary { op, left, right } => {
                self.w(21);
                self.w(*op as u64);
                self.term(left);
                self.term(right);
            }
            Term::Record { fields } => {
                self.w(22);
                self.len(fields.len());
                self.fields(fields);
            }
            Term::Update { base, fields } => {
                self.w(23);
                self.len(fields.len());
                self.term(base);
                self.fields(fields);
            }
            Term::Do { statements, result } => {
                self.w(24);
                self.statements(statements);
                self.term(result);
            }
            Term::Rfl => self.w(25),
            Term::ByDecide => self.w(26),
            Term::Eliminate { value } => {
                self.w(27);
                self.term(value);
            }
        }
    }
    /// Statements: 0 let (mutable), 1 bind, 2 assign, 3 expression, 4 typed
    /// bind, 5 letI, 6 if, 7 match (arm count, scrutinee, arms).
    fn statements(&mut self, ss: &[DoStatement]) {
        self.len(ss.len());
        for s in ss {
            match s {
                DoStatement::Let { binding, mutable } => {
                    self.w(0);
                    self.w(*mutable as u64);
                    self.binding(binding);
                }
                DoStatement::Bind { pattern, action } => {
                    self.w(1);
                    self.pattern(pattern);
                    self.term(action);
                }
                DoStatement::Assign { local, value } => {
                    self.w(2);
                    self.ident(local);
                    self.term(value);
                }
                DoStatement::Expr(t) => {
                    self.w(3);
                    self.term(t);
                }
                DoStatement::BindTyped {
                    pattern,
                    ty,
                    action,
                } => {
                    self.w(4);
                    self.pattern(pattern);
                    self.term(ty);
                    self.term(action);
                }
                DoStatement::LetInstance { name, ty, value } => {
                    self.w(5);
                    match name {
                        Some(n) => {
                            self.w(1);
                            self.ident(n);
                        }
                        None => self.w(0),
                    }
                    self.term(ty);
                    self.term(value);
                }
                DoStatement::If { condition, yes, no } => {
                    self.w(6);
                    self.term(condition);
                    self.statements(yes);
                    match no {
                        Some(no) => {
                            self.w(1);
                            self.statements(no);
                        }
                        None => self.w(0),
                    }
                }
                DoStatement::Match { scrutinee, arms } => {
                    self.w(7);
                    self.len(arms.len());
                    self.term(scrutinee);
                    for (p, body) in arms {
                        self.pattern(p);
                        self.statements(body);
                    }
                }
            }
        }
    }
    fn definition(&mut self, d: &Definition) {
        self.w(match d.kind {
            DefinitionKind::Def => 0,
            DefinitionKind::NoncomputableDef => 1,
            DefinitionKind::PartialDef => 2,
            DefinitionKind::Abbrev => 3,
            DefinitionKind::SimpDef => 4,
        });
        self.name(&d.name);
        self.binders(&d.binders);
        self.term(&d.result);
        self.term(&d.body);
    }
    fn field_list(&mut self, fields: &[Field]) {
        self.len(fields.len());
        for f in fields {
            self.ident(&f.name);
            self.term(&f.ty);
            match &f.default {
                Some(t) => {
                    self.w(1);
                    self.term(t);
                }
                None => self.w(0),
            }
        }
    }
    fn names(&mut self, ns: &[Name]) {
        self.len(ns.len());
        for n in ns {
            self.name(n);
        }
    }
    /// Type declarations: 0 class, 1 inductive, 2 structure.
    fn type_declaration(&mut self, t: &TypeDeclaration) {
        match t {
            TypeDeclaration::Class {
                name,
                parameters,
                fields,
            } => {
                self.w(0);
                self.name(name);
                self.binders(parameters);
                self.field_list(fields);
            }
            TypeDeclaration::Inductive {
                name,
                parameters,
                sort,
                constructors,
                deriving,
            } => {
                self.w(1);
                self.name(name);
                self.binders(parameters);
                self.term(sort);
                self.len(constructors.len());
                for c in constructors {
                    self.ident(&c.name);
                    self.binders(&c.binders);
                    match &c.result {
                        Some(t) => {
                            self.w(1);
                            self.term(t);
                        }
                        None => self.w(0),
                    }
                }
                self.names(deriving);
            }
            TypeDeclaration::Structure {
                name,
                parameters,
                fields,
                deriving,
            } => {
                self.w(2);
                self.name(name);
                self.binders(parameters);
                self.field_list(fields);
                self.names(deriving);
            }
        }
    }
    /// Declarations: 0 instance, 1 set_option (option, value kind, value),
    /// 2 def, 3 type, 4 mutual defs, 5 mutual types, 6 namespace, 7 theorem,
    /// 8 example, 9 #eval, 10 #check.
    fn declaration(&mut self, d: &Declaration) {
        match d {
            Declaration::Instance(i) => {
                self.w(0);
                match &i.name {
                    Some(n) => {
                        self.w(1);
                        self.name(n);
                    }
                    None => self.w(0),
                }
                self.binders(&i.binders);
                self.term(&i.ty);
                self.term(&i.body);
            }
            Declaration::SetOption { option, value } => {
                self.w(1);
                self.w(match option {
                    LeanOption::LinterUnusedVariables => 0,
                    LeanOption::MaxRecDepth => 1,
                    LeanOption::MaxHeartbeats => 2,
                });
                match value {
                    OptionValue::Bool(b) => {
                        self.w(0);
                        self.w(*b as u64);
                    }
                    OptionValue::Nat(n) => {
                        self.w(1);
                        self.w(*n);
                    }
                }
            }
            Declaration::Definition(d) => {
                self.w(2);
                self.definition(d);
            }
            Declaration::Type(t) => {
                self.w(3);
                self.type_declaration(t);
            }
            Declaration::MutualDefinitions(ds) => {
                self.w(4);
                self.len(ds.len());
                for d in ds {
                    self.definition(d);
                }
            }
            Declaration::MutualTypes(ts) => {
                self.w(5);
                self.len(ts.len());
                for t in ts {
                    self.type_declaration(t);
                }
            }
            Declaration::Namespace { name, declarations } => {
                self.w(6);
                self.name(name);
                self.len(declarations.len());
                for d in declarations {
                    self.declaration(d);
                }
            }
            Declaration::Theorem {
                name,
                binders,
                proposition,
                proof,
            } => {
                self.w(7);
                self.name(name);
                self.binders(binders);
                self.term(proposition);
                self.term(proof);
            }
            Declaration::Example {
                binders,
                proposition,
                proof,
            } => {
                self.w(8);
                self.binders(binders);
                self.term(proposition);
                self.term(proof);
            }
            Declaration::Eval(t) => {
                self.w(9);
                self.term(t);
            }
            Declaration::Check(t) => {
                self.w(10);
                self.term(t);
            }
        }
    }
    pub fn module(&mut self, m: &Module) {
        self.len(m.imports.len());
        for i in &m.imports {
            self.len(i.0.len());
            for part in &i.0 {
                self.text(part);
            }
        }
        self.len(m.declarations.len());
        for d in &m.declarations {
            self.declaration(d);
        }
    }

    /// The name encoded at word `at`.
    pub fn decode_name(&self, at: usize) -> Name {
        let rooted = self.words[at] == 1;
        let n = self.words[at + 1] as usize;
        let components = (0..n)
            .map(|i| {
                let off = self.words[at + 2 + 2 * i] as usize;
                let len = self.words[at + 3 + 2 * i] as usize;
                Ident(self.pool[off..off + len].to_owned())
            })
            .collect();
        Name { components, rooted }
    }
}
