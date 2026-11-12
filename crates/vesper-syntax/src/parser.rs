//! Recovering recursive descent parser, with Pratt precedence and a lossless CST.
use crate::{
    ast::*,
    lexer::{self, Token, TokenKind as K},
};
use rowan::{GreenNode, GreenNodeBuilder, Language, SyntaxKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VesperLanguage {}
impl Language for VesperLanguage {
    type Kind = SyntaxKind;
    fn kind_from_raw(raw: SyntaxKind) -> SyntaxKind {
        raw
    }
    fn kind_to_raw(kind: SyntaxKind) -> SyntaxKind {
        kind
    }
}
pub type SyntaxNode = rowan::SyntaxNode<VesperLanguage>;
pub const MODULE: SyntaxKind = SyntaxKind(200);
pub const ITEM: SyntaxKind = SyntaxKind(201);
pub const EXPR: SyntaxKind = SyntaxKind(202);
pub const PATTERN: SyntaxKind = SyntaxKind(203);
pub const TYPE: SyntaxKind = SyntaxKind(204);

#[derive(Debug, Clone, PartialEq)]
pub struct Parse {
    pub module: Module,
    pub green: GreenNode,
    pub diagnostics: Vec<Diagnostic>,
}
impl Parse {
    pub fn syntax(&self) -> SyntaxNode {
        SyntaxNode::new_root(self.green.clone())
    }
}
pub fn parse(source: &str) -> Parse {
    let (tokens, diagnostics) = lexer::lex(source);
    let mut p = Parser {
        source,
        tokens,
        pos: 0,
        builder: GreenNodeBuilder::new(),
        diagnostics,
        no_struct: false,
        row_names: Default::default(),
        bound_values: Default::default(),
    };
    p.builder.start_node(MODULE);
    let mut module = Module::default();
    while !p.at(K::Eof) {
        let before = p.pos;
        p.builder.start_node(ITEM);
        if let Some(item) = p.item() {
            module.items.push(item);
        }
        p.builder.finish_node();
        if p.pos == before {
            p.error("expected a declaration");
            p.bump();
        }
    }
    p.trivia();
    p.builder.finish_node();
    Parse {
        module,
        green: p.builder.finish(),
        diagnostics: p.diagnostics,
    }
}
struct Parser<'a> {
    source: &'a str,
    tokens: Vec<Token>,
    pos: usize,
    builder: GreenNodeBuilder<'static>,
    diagnostics: Vec<Diagnostic>,
    no_struct: bool,
    row_names: std::collections::BTreeSet<String>,
    bound_values: std::collections::BTreeSet<String>,
}
impl Parser<'_> {
    fn index(&self, n: usize) -> usize {
        self.tokens
            .iter()
            .enumerate()
            .skip(self.pos)
            .filter(|(_, t)| !t.kind.trivia())
            .nth(n)
            .map(|(i, _)| i)
            .unwrap_or(self.tokens.len() - 1)
    }
    fn kind(&self) -> K {
        self.tokens[self.index(0)].kind
    }
    fn nth(&self, n: usize) -> K {
        self.tokens[self.index(n)].kind
    }
    fn at(&self, k: K) -> bool {
        self.kind() == k
    }
    fn span(&self) -> Span {
        self.tokens[self.index(0)].span.clone()
    }
    fn start(&self) -> usize {
        self.span().start
    }
    fn end(&self) -> usize {
        if self.pos == 0 {
            0
        } else {
            self.tokens[self.pos - 1].span.end
        }
    }
    fn trivia(&mut self) {
        while self.pos < self.tokens.len() && self.tokens[self.pos].kind.trivia() {
            let t = &self.tokens[self.pos];
            self.builder
                .token(SyntaxKind(t.kind as u16), &self.source[t.span.clone()]);
            self.pos += 1;
        }
    }
    fn bump(&mut self) -> String {
        self.trivia();
        let t = &self.tokens[self.pos];
        let s = self.source[t.span.clone()].to_owned();
        if t.kind != K::Eof {
            self.builder.token(SyntaxKind(t.kind as u16), &s);
            self.pos += 1;
        }
        s
    }
    fn eat(&mut self, k: K) -> bool {
        if self.at(k) {
            self.bump();
            true
        } else {
            false
        }
    }
    fn expect(&mut self, k: K) {
        if !self.eat(k) {
            self.error(format!("expected {k:?}, found {:?}", self.kind()));
        }
    }
    fn error(&mut self, msg: impl Into<String>) {
        self.diagnostics
            .push(Diagnostic::new("E0002", msg, self.span()));
    }
    fn ident(&mut self) -> String {
        if matches!(self.kind(), K::Lower | K::Upper | K::SelfKw) {
            self.bump()
        } else {
            self.error("expected identifier");
            if !self.at(K::Eof) {
                self.bump()
            } else {
                "<error>".into()
            }
        }
    }
    fn lower(&mut self) -> String {
        if !matches!(self.kind(), K::Lower | K::SelfKw) {
            self.error("expected a lower-case identifier");
        }
        self.ident()
    }
    fn upper(&mut self) -> String {
        if !self.at(K::Upper) {
            self.error("expected an upper-case identifier");
        }
        self.ident()
    }
    fn mode(&mut self) -> Option<Mode> {
        match self.kind() {
            K::Copy => {
                self.bump();
                Some(Mode::Copy)
            }
            K::Affine => {
                self.bump();
                Some(Mode::Affine)
            }
            K::Linear => {
                self.bump();
                Some(Mode::Linear)
            }
            _ => None,
        }
    }
    fn pass(&mut self) -> (Pass, bool) {
        match self.kind() {
            K::In => {
                self.bump();
                (Pass::In, true)
            }
            K::Inout => {
                self.bump();
                (Pass::Inout, true)
            }
            K::Sink => {
                self.bump();
                (Pass::Sink, true)
            }
            _ => (Pass::In, false),
        }
    }
    fn generics(&mut self) -> Vec<Generic> {
        let mut out = vec![];
        if !self.eat(K::LBracket) {
            return out;
        }
        while !self.at(K::RBracket) && !self.at(K::Eof) {
            let before = self.pos;
            let row = self.at(K::Lower);
            let name = self.ident();
            let mut bound = Mode::Linear;
            if self.eat(K::Colon) {
                if row {
                    self.expect(K::Eff);
                } else {
                    self.expect(K::Mod);
                    bound = self.mode().unwrap_or_else(|| {
                        self.error("expected modal bound");
                        Mode::Linear
                    });
                }
            } else if row {
                self.error("effect row parameters require ': eff'");
            }
            out.push(Generic { name, bound, row });
            if !self.eat(K::Comma) || self.pos == before {
                break;
            }
        }
        self.expect(K::RBracket);
        out
    }
    fn fields(&mut self) -> Vec<Field> {
        let mut out = vec![];
        self.expect(K::LBrace);
        while !self.at(K::RBrace) && !self.at(K::Eof) {
            let name = self.lower();
            self.expect(K::Colon);
            let ty = self.ty();
            let modality = if self.eat(K::AtAt) {
                self.mode().unwrap_or_else(|| {
                    self.error("expected field modality");
                    Mode::Linear
                })
            } else {
                Mode::Linear
            };
            out.push(Field { name, ty, modality });
            if !self.eat(K::Comma) {
                break;
            }
        }
        self.expect(K::RBrace);
        out
    }
    fn item(&mut self) -> Option<Item> {
        let start = self.start();
        let mode = self.mode().unwrap_or_default();
        match self.kind() {
            K::Struct => {
                self.bump();
                let name = self.upper();
                let generics = self.generics();
                let fields = self.fields();
                Some(Item::Adt(Adt {
                    name: name.clone(),
                    mode,
                    generics,
                    variants: vec![Variant {
                        name,
                        fields,
                        named: true,
                    }],
                    is_enum: false,
                    span: start..self.end(),
                }))
            }
            K::Enum => {
                self.bump();
                let name = self.upper();
                let generics = self.generics();
                self.expect(K::LBrace);
                let mut variants = vec![];
                while !self.at(K::RBrace) && !self.at(K::Eof) {
                    let name = self.upper();
                    let named = self.at(K::LBrace);
                    let fields = if named {
                        self.fields()
                    } else if self.eat(K::LParen) {
                        let mut fields = vec![];
                        while !self.at(K::RParen) && !self.at(K::Eof) {
                            fields.push(Field {
                                name: fields.len().to_string(),
                                ty: self.ty(),
                                modality: Mode::Linear,
                            });
                            if !self.eat(K::Comma) {
                                break;
                            }
                        }
                        self.expect(K::RParen);
                        fields
                    } else {
                        vec![]
                    };
                    variants.push(Variant {
                        name,
                        fields,
                        named,
                    });
                    if !self.eat(K::Comma) {
                        break;
                    }
                }
                self.expect(K::RBrace);
                Some(Item::Adt(Adt {
                    name,
                    mode,
                    generics,
                    variants,
                    is_enum: true,
                    span: start..self.end(),
                }))
            }
            K::Effect => {
                self.bump();
                let name = self.upper();
                let generics = self.generics();
                self.expect(K::LBrace);
                let mut ops = vec![];
                while self.at(K::Fn) {
                    ops.push(self.function(true));
                }
                self.expect(K::RBrace);
                Some(Item::Effect(EffectDecl {
                    name,
                    linear: mode == Mode::Linear,
                    generics,
                    ops,
                    span: start..self.end(),
                }))
            }
            K::Fn => Some(Item::Function(Box::new(self.function(false)))),
            K::Impl => {
                self.bump();
                let generics = self.generics();
                let old_rows = self.row_names.clone();
                self.row_names
                    .extend(generics.iter().filter(|g| g.row).map(|g| g.name.clone()));
                let ty = self.ty();
                self.expect(K::LBrace);
                let mut methods = vec![];
                while self.at(K::Fn) {
                    methods.push(self.function(false));
                }
                self.expect(K::RBrace);
                self.row_names = old_rows;
                Some(Item::Impl {
                    ty,
                    generics,
                    methods,
                })
            }
            _ => None,
        }
    }
    fn function(&mut self, op: bool) -> Function {
        let start = self.start();
        self.expect(K::Fn);
        let name = self.lower();
        let old_rows = self.row_names.clone();
        let old_values = self.bound_values.clone();
        let generics = self.generics();
        self.row_names
            .extend(generics.iter().filter(|g| g.row).map(|g| g.name.clone()));
        self.expect(K::LParen);
        let params = self.params(K::RParen);
        self.bound_values
            .extend(params.iter().map(|p| p.name.clone()));
        self.expect(K::RParen);
        let result = if self.eat(K::Arrow) {
            self.ty()
        } else {
            Type::unit()
        };
        let effects = self.effects();
        let body = if op {
            self.expect(K::Semi);
            None
        } else {
            Some(self.block())
        };
        self.row_names = old_rows;
        self.bound_values = old_values;
        Function {
            name,
            generics,
            params,
            result,
            effects,
            body,
            span: start..self.end(),
        }
    }
    fn params(&mut self, end: K) -> Vec<Param> {
        let mut out = vec![];
        while !self.at(end) && !self.at(K::Eof) {
            let start = self.start();
            let (pass, _) = self.pass();
            let name = self.lower();
            let projections = if self.eat(K::Dot) {
                self.place_tree()
            } else {
                vec![]
            };
            let ty = if self.eat(K::Colon) {
                self.ty()
            } else {
                Type::Infer
            };
            out.push(Param {
                name,
                pass,
                projections,
                ty,
                span: start..self.end(),
            });
            if !self.eat(K::Comma) {
                break;
            }
        }
        out
    }
    fn place_tree(&mut self) -> Vec<Vec<String>> {
        if self.eat(K::LBrace) {
            let mut out = vec![];
            while !self.at(K::RBrace) && !self.at(K::Eof) {
                out.extend(self.place_tree());
                if !self.eat(K::Comma) {
                    break;
                }
            }
            self.expect(K::RBrace);
            out
        } else {
            let name = self.lower();
            if self.eat(K::Dot) {
                self.place_tree()
                    .into_iter()
                    .map(|mut p| {
                        p.insert(0, name.clone());
                        p
                    })
                    .collect()
            } else {
                vec![vec![name]]
            }
        }
    }
    fn ty(&mut self) -> Type {
        self.builder.start_node(TYPE);
        let result = if self.eat(K::Fn) {
            let mode = if self.eat(K::At) {
                self.mode().unwrap_or(Mode::Affine)
            } else {
                Mode::Affine
            };
            self.expect(K::LParen);
            let mut params = vec![];
            while !self.at(K::RParen) && !self.at(K::Eof) {
                let (p, _) = self.pass();
                params.push((p, self.ty()));
                if !self.eat(K::Comma) {
                    break;
                }
            }
            self.expect(K::RParen);
            let result = if self.eat(K::Arrow) {
                self.ty()
            } else {
                Type::unit()
            };
            let effects = self.effects();
            Type::Function {
                params,
                result: Box::new(result),
                effects,
                mode,
            }
        } else if self.eat(K::LParen) {
            let mut args = vec![];
            while !self.at(K::RParen) && !self.at(K::Eof) {
                args.push(self.ty());
                if !self.eat(K::Comma) {
                    break;
                }
            }
            self.expect(K::RParen);
            Type::Tuple(args)
        } else {
            let name = self.ident();
            let args = self.type_args();
            Type::Named(name, args)
        };
        self.builder.finish_node();
        result
    }
    fn type_args(&mut self) -> Vec<Type> {
        let mut args = vec![];
        if self.eat(K::LBracket) {
            while !self.at(K::RBracket) && !self.at(K::Eof) {
                if self.at(K::Lower) && !self.primitive() {
                    let label = self.bump();
                    if self.eat(K::Colon) {
                        let name = self.upper();
                        let types = self.type_args();
                        let tail = if self.eat(K::Pipe) {
                            Some(self.lower())
                        } else {
                            None
                        };
                        args.push(Type::Row(Effects {
                            instances: vec![Effect {
                                label: Some(label),
                                name,
                                args: types,
                            }],
                            tail,
                        }));
                    } else {
                        args.push(Type::Row(Effects {
                            instances: vec![],
                            tail: Some(label),
                        }));
                    }
                } else {
                    let t = self.ty();
                    if self.eat(K::Pipe) {
                        let tail = Some(self.lower());
                        if let Type::Named(name, types) = t {
                            args.push(Type::Row(Effects {
                                instances: vec![Effect {
                                    label: None,
                                    name,
                                    args: types,
                                }],
                                tail,
                            }));
                        } else {
                            self.error("an effect row requires an effect name");
                            args.push(t);
                        }
                    } else {
                        args.push(t);
                    }
                }
                if !self.eat(K::Comma) {
                    break;
                }
            }
            self.expect(K::RBracket);
        }
        args
    }
    fn primitive(&self) -> bool {
        matches!(
            &self.source[self.span()],
            "u8" | "u32" | "u64" | "i64" | "f64" | "bool"
        )
    }
    fn effects(&mut self) -> Effects {
        if !self.eat(K::Eff) {
            return Effects::default();
        }
        self.expect(K::LBracket);
        let mut out = Effects::default();
        while !self.at(K::RBracket) && !self.at(K::Eof) {
            if self.eat(K::Pipe) {
                out.tail = Some(self.lower());
                break;
            }
            if self.at(K::Lower) && self.nth(1) != K::Colon {
                out.tail = Some(self.lower());
                break;
            }
            let label = if self.at(K::Lower) {
                let l = self.lower();
                self.expect(K::Colon);
                Some(l)
            } else {
                None
            };
            let name = self.upper();
            let args = self.type_args();
            out.instances.push(Effect { label, name, args });
            if !self.eat(K::Comma) && !self.at(K::Pipe) {
                break;
            }
        }
        self.expect(K::RBracket);
        out
    }
    fn block(&mut self) -> Expr {
        let outer_values = self.bound_values.clone();
        let start = self.start();
        self.expect(K::LBrace);
        let mut statements = vec![];
        let mut tail = None;
        while !self.at(K::RBrace) && !self.at(K::Eof) {
            let before = self.pos;
            if self.eat(K::Let) {
                let pat = self.pattern();
                let ty = if self.eat(K::Colon) {
                    self.ty()
                } else {
                    Type::Infer
                };
                self.expect(K::Eq);
                let value = self.expr();
                self.expect(K::Semi);
                pattern_bindings(&pat, &mut self.bound_values);
                statements.push(Statement::Let { pat, ty, value });
            } else {
                let e = self.expr();
                if self.eat(K::Semi) {
                    statements.push(Statement::Expr(e));
                } else {
                    tail = Some(Box::new(e));
                    break;
                }
            }
            if before == self.pos {
                self.bump();
            }
        }
        self.expect(K::RBrace);
        self.bound_values = outer_values;
        Expr {
            kind: ExprKind::Block { statements, tail },
            span: start..self.end(),
        }
    }
    fn expr(&mut self) -> Expr {
        self.builder.start_node(EXPR);
        let e = match self.kind() {
            K::Handle => self.handle(),
            K::Match => self.match_expr(),
            K::If => self.if_expr(),
            K::Pipe | K::Or => self.closure(),
            _ => self.binary(0),
        };
        self.builder.finish_node();
        e
    }
    fn restricted(&mut self) -> Expr {
        let old = self.no_struct;
        self.no_struct = true;
        let e = self.expr();
        self.no_struct = old;
        e
    }
    fn if_expr(&mut self) -> Expr {
        let start = self.start();
        self.expect(K::If);
        let condition = Box::new(self.restricted());
        let then = Box::new(self.block());
        let otherwise = if self.eat(K::Else) {
            Some(Box::new(if self.at(K::If) {
                self.if_expr()
            } else {
                self.block()
            }))
        } else {
            None
        };
        Expr {
            kind: ExprKind::If {
                condition,
                then,
                otherwise,
            },
            span: start..self.end(),
        }
    }
    fn match_expr(&mut self) -> Expr {
        let start = self.start();
        self.expect(K::Match);
        let value = Box::new(self.restricted());
        self.expect(K::LBrace);
        let mut arms = vec![];
        while !self.at(K::RBrace) && !self.at(K::Eof) {
            let pat = self.pattern();
            self.expect(K::FatArrow);
            let outer_values = self.bound_values.clone();
            pattern_bindings(&pat, &mut self.bound_values);
            let body = self.expr();
            self.bound_values = outer_values;
            arms.push((pat, body));
            if !self.eat(K::Comma) && !self.at(K::RBrace) {
                self.error("expected ',' between match arms");
                break;
            }
        }
        self.expect(K::RBrace);
        Expr {
            kind: ExprKind::Match { value, arms },
            span: start..self.end(),
        }
    }
    fn closure(&mut self) -> Expr {
        let start = self.start();
        let params = if self.eat(K::Or) {
            vec![]
        } else {
            self.expect(K::Pipe);
            let p = self.params(K::Pipe);
            self.expect(K::Pipe);
            p
        };
        let mode = if self.eat(K::At) { self.mode() } else { None };
        let result = if self.eat(K::Arrow) {
            self.ty()
        } else {
            Type::Infer
        };
        let infer_effects = !self.at(K::Eff);
        let effects = self.effects();
        let outer_values = self.bound_values.clone();
        self.bound_values
            .extend(params.iter().map(|p| p.name.clone()));
        let body = Box::new(self.expr());
        self.bound_values = outer_values;
        Expr {
            kind: ExprKind::Closure {
                params,
                mode,
                result,
                effects,
                infer_effects,
                body,
            },
            span: start..self.end(),
        }
    }
    fn handle(&mut self) -> Expr {
        let start = self.start();
        self.expect(K::Handle);
        let label = if self.at(K::Lower) {
            let l = self.lower();
            self.expect(K::Colon);
            Some(l)
        } else {
            None
        };
        let name = self.upper();
        let args = self.type_args();
        let state = if self.eat(K::State) {
            let p = self.pattern();
            self.expect(K::Eq);
            Some((p, Box::new(self.expr())))
        } else {
            None
        };
        self.expect(K::In);
        let body = Box::new(self.block());
        self.expect(K::With);
        self.expect(K::LBrace);
        self.expect(K::Return);
        let pat = self.pattern();
        self.expect(K::FatArrow);
        let outer_values = self.bound_values.clone();
        pattern_bindings(&pat, &mut self.bound_values);
        let ret = Box::new(Clause {
            name: "return".into(),
            pats: vec![pat],
            body: self.expr(),
        });
        self.bound_values = outer_values;
        self.eat(K::Comma);
        let mut ops = vec![];
        while !self.at(K::RBrace) && !self.at(K::Eof) {
            let name = self.lower();
            self.expect(K::LParen);
            let pats = self.pattern_list(K::RParen);
            self.expect(K::RParen);
            self.expect(K::FatArrow);
            let outer_values = self.bound_values.clone();
            for p in &pats {
                pattern_bindings(p, &mut self.bound_values);
            }
            let body = self.expr();
            self.bound_values = outer_values;
            ops.push(Clause { name, pats, body });
            self.eat(K::Comma);
        }
        self.expect(K::RBrace);
        Expr {
            kind: ExprKind::Handle {
                effect: Effect { label, name, args },
                state,
                body,
                ret,
                ops,
            },
            span: start..self.end(),
        }
    }
    fn binary(&mut self, min: u8) -> Expr {
        let mut lhs = if matches!(self.kind(), K::Minus | K::Bang) {
            let start = self.start();
            let op = self.bump();
            let v = self.binary(12);
            let kind = if op == "-" {
                if let ExprKind::Literal(Literal::Int(n)) = v.kind {
                    ExprKind::Literal(Literal::Int(-n))
                } else {
                    ExprKind::Unary(op, Box::new(v))
                }
            } else {
                ExprKind::Unary(op, Box::new(v))
            };
            Expr {
                kind,
                span: start..self.end(),
            }
        } else {
            self.postfix()
        };
        loop {
            let (prec, right) = match self.kind() {
                K::Eq | K::PlusEq | K::MinusEq | K::StarEq | K::SlashEq | K::PercentEq => (1, true),
                K::Or => (2, false),
                K::And => (3, false),
                K::EqEq | K::Ne => (4, false),
                K::Lt | K::Le | K::Gt | K::Ge => (5, false),
                K::Plus | K::Minus => (6, false),
                K::Star | K::Slash | K::Percent => (7, false),
                _ => (0, false),
            };
            if prec == 0 || prec < min {
                break;
            }
            let op = self.bump();
            let rhs = if right {
                self.expr()
            } else {
                self.binary(prec + 1)
            };
            let span = lhs.span.start..rhs.span.end;
            let kind = if right {
                ExprKind::Assign(op, Box::new(lhs), Box::new(rhs))
            } else {
                ExprKind::Binary(op, Box::new(lhs), Box::new(rhs))
            };
            lhs = Expr { kind, span };
        }
        lhs
    }
    fn postfix(&mut self) -> Expr {
        let mut e = self.primary();
        loop {
            let start = e.span.start;
            let kind = match self.kind() {
                K::LParen => {
                    self.bump();
                    let args = self.args();
                    self.expect(K::RParen);
                    ExprKind::Call {
                        callee: Box::new(e),
                        types: vec![],
                        args,
                    }
                }
                K::LBracket => {
                    let named_row = self.nth(1) == K::Lower && self.nth(2) == K::Colon;
                    let scoped_row = {
                        let i = self.index(1);
                        let text = &self.source[self.tokens[i].span.clone()];
                        self.row_names.contains(text)
                            && !matches!(&e.kind,ExprKind::Name(n) if self.bound_values.contains(n))
                    };
                    let generic = named_row || scoped_row || self.nth(1) == K::Upper || {
                        let i = self.index(1);
                        matches!(
                            &self.source[self.tokens[i].span.clone()],
                            "u8" | "u32" | "u64" | "i64" | "f64" | "bool" | "fn"
                        )
                    };
                    if generic {
                        let types = self.type_args();
                        self.expect(K::LParen);
                        let args = self.args();
                        self.expect(K::RParen);
                        ExprKind::Call {
                            callee: Box::new(e),
                            types,
                            args,
                        }
                    } else {
                        self.bump();
                        let index = self.expr();
                        self.expect(K::RBracket);
                        ExprKind::Index(Box::new(e), Box::new(index))
                    }
                }
                K::Dot => {
                    self.bump();
                    ExprKind::Field(Box::new(e), self.lower())
                }
                K::Question => {
                    self.bump();
                    ExprKind::Try(Box::new(e))
                }
                _ => break,
            };
            e = Expr {
                kind,
                span: start..self.end(),
            };
        }
        e
    }
    fn args(&mut self) -> Vec<Arg> {
        let mut args = vec![];
        while !self.at(K::RParen) && !self.at(K::Eof) {
            let (pass, explicit) = self.pass();
            args.push(Arg {
                pass,
                explicit,
                expr: self.expr(),
            });
            if !self.eat(K::Comma) {
                break;
            }
        }
        args
    }
    fn literal(&mut self) -> Option<Literal> {
        match self.kind() {
            K::Int => {
                let text = self.bump().replace('_', "");
                let n = if text.starts_with("0x") || text.starts_with("0X") {
                    i128::from_str_radix(&text[2..], 16)
                } else {
                    text.parse()
                };
                Some(Literal::Int(
                    n.ok()
                        .filter(|n| *n <= u64::MAX as i128)
                        .unwrap_or_else(|| {
                            self.error("integer literal exceeds u64 range");
                            0
                        }),
                ))
            }
            K::Float => {
                let s = self.bump();
                Some(Literal::Float(s.parse().unwrap_or(0.0)))
            }
            K::True => {
                self.bump();
                Some(Literal::Bool(true))
            }
            K::False => {
                self.bump();
                Some(Literal::Bool(false))
            }
            K::String => {
                let s = self.bump();
                Some(Literal::String(serde_json::from_str(&s).unwrap_or_else(
                    |_| {
                        self.error("invalid string escape");
                        String::new()
                    },
                )))
            }
            _ => None,
        }
    }
    fn primary(&mut self) -> Expr {
        let start = self.start();
        let kind = if let Some(l) = self.literal() {
            ExprKind::Literal(l)
        } else {
            match self.kind() {
                K::Lower | K::SelfKw => ExprKind::Name(self.bump()),
                K::Upper => {
                    let mut name = self.bump();
                    if self.eat(K::ColonColon) {
                        name.push_str("::");
                        name.push_str(&self.ident());
                    }
                    if self.at(K::LBrace) && !self.no_struct {
                        self.bump();
                        let mut fields = vec![];
                        while !self.at(K::RBrace) && !self.at(K::Eof) {
                            let start = self.start();
                            let field = self.lower();
                            let value = if self.eat(K::Colon) {
                                self.expr()
                            } else {
                                Expr {
                                    kind: ExprKind::Name(field.clone()),
                                    span: start..self.end(),
                                }
                            };
                            fields.push((field, value));
                            if !self.eat(K::Comma) {
                                break;
                            }
                        }
                        self.expect(K::RBrace);
                        ExprKind::Struct { name, fields }
                    } else {
                        ExprKind::Name(name)
                    }
                }
                K::LParen => {
                    self.bump();
                    let mut values = vec![];
                    let mut comma = false;
                    while !self.at(K::RParen) && !self.at(K::Eof) {
                        values.push(self.expr());
                        comma = self.eat(K::Comma);
                        if !comma {
                            break;
                        }
                    }
                    self.expect(K::RParen);
                    if values.len() == 1 && !comma {
                        return values.remove(0);
                    }
                    ExprKind::Tuple(values)
                }
                K::LBrace => return self.block(),
                _ => {
                    self.error("expected expression");
                    if !self.at(K::Eof) {
                        self.bump();
                    }
                    ExprKind::Tuple(vec![])
                }
            }
        };
        Expr {
            kind,
            span: start..self.end(),
        }
    }
    fn pattern_list(&mut self, end: K) -> Vec<Pattern> {
        let mut pats = vec![];
        while !self.at(end) && !self.at(K::Eof) {
            pats.push(self.pattern());
            if !self.eat(K::Comma) {
                break;
            }
        }
        pats
    }
    fn pattern(&mut self) -> Pattern {
        self.builder.start_node(PATTERN);
        let start = self.start();
        let negative = self.eat(K::Minus);
        let kind = if let Some(l) = self.literal() {
            PatternKind::Literal(if negative {
                match l {
                    Literal::Int(n) => Literal::Int(-n),
                    Literal::Float(n) => Literal::Float(-n),
                    l => {
                        self.error("only numeric patterns can be negative");
                        l
                    }
                }
            } else {
                l
            })
        } else {
            if negative {
                self.error("only numeric patterns can be negative");
            }
            match self.kind() {
                K::Lower => {
                    let name = self.bump();
                    if name == "_" {
                        PatternKind::Wild
                    } else {
                        PatternKind::Bind(name)
                    }
                }
                K::Upper => {
                    let mut name = self.upper();
                    if self.eat(K::ColonColon) {
                        name.push_str("::");
                        name.push_str(&self.upper());
                    }
                    let named = self.at(K::LBrace);
                    let mut fields = vec![];
                    if self.eat(K::LParen) {
                        for (i, p) in self.pattern_list(K::RParen).into_iter().enumerate() {
                            fields.push((i.to_string(), p));
                        }
                        self.expect(K::RParen);
                    } else if self.eat(K::LBrace) {
                        while !self.at(K::RBrace) && !self.at(K::Eof) {
                            let start = self.start();
                            let field = self.lower();
                            let p = if self.eat(K::Colon) {
                                self.pattern()
                            } else {
                                Pattern {
                                    kind: PatternKind::Bind(field.clone()),
                                    span: start..self.end(),
                                }
                            };
                            fields.push((field, p));
                            if !self.eat(K::Comma) {
                                break;
                            }
                        }
                        self.expect(K::RBrace);
                    }
                    PatternKind::Constructor {
                        name,
                        fields,
                        named,
                    }
                }
                K::LParen => {
                    self.bump();
                    let p = self.pattern_list(K::RParen);
                    self.expect(K::RParen);
                    PatternKind::Tuple(p)
                }
                _ => {
                    self.error("expected pattern");
                    if !self.at(K::Eof) {
                        self.bump();
                    }
                    PatternKind::Wild
                }
            }
        };
        self.builder.finish_node();
        Pattern {
            kind,
            span: start..self.end(),
        }
    }
}

fn pattern_bindings(p: &Pattern, names: &mut std::collections::BTreeSet<String>) {
    match &p.kind {
        PatternKind::Bind(n) => {
            names.insert(n.clone());
        }
        PatternKind::Tuple(ps) => {
            for p in ps {
                pattern_bindings(p, names);
            }
        }
        PatternKind::Constructor { fields, .. } => {
            for (_, p) in fields {
                pattern_bindings(p, names);
            }
        }
        _ => {}
    }
}
