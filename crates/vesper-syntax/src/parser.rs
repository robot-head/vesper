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
/// Editor-only CST kinds. Legacy batch parsing never inserts these wrappers.
pub const BLOCK: SyntaxKind = SyntaxKind(205);
pub const ERROR_NODE: SyntaxKind = SyntaxKind(206);
pub const INCOMPLETE: SyntaxKind = SyntaxKind(207);
mod macro_driver;
mod recovery_driver;
pub(crate) use macro_driver::{parse_source_header, parse_source_item};
#[allow(
    unused_imports,
    reason = "consumed by separately integrated incremental module"
)]
pub(crate) use recovery_driver::{BodyContext, capture_body_context, parse_body_fragment};
pub use recovery_driver::{parse_complete_items, parse_recovery};

#[derive(Clone)]
struct HeaderNote {
    start: usize,
    body_start: usize,
    clean: bool,
    nongeneric: bool,
    row_names: std::collections::BTreeSet<String>,
    bound_values: std::collections::BTreeSet<String>,
}
struct TraceSite {
    span: Span,
    kind: crate::recovery::SiteKind,
    receiver: Option<crate::recovery::Receiver>,
}

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
/// Combined guarded descent/AST wrapping depth; additional helper frames are
/// bounded by this count. This is not a checker/runtime termination guarantee.
pub const HARD_PARSE_DEPTH: usize = 128;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseLimits {
    pub max_source_bytes: usize,
    pub max_tokens: usize,
    pub max_diagnostics: usize,
    pub max_depth: usize,
}
impl Default for ParseLimits {
    fn default() -> Self {
        let lex = lexer::LexLimits::default();
        Self {
            max_source_bytes: lex.max_source_bytes,
            max_tokens: lex.max_tokens,
            max_diagnostics: lex.max_diagnostics,
            max_depth: HARD_PARSE_DEPTH,
        }
    }
}
/// Compatibility API. Oversized input yields E0003, an empty AST and a lossless
/// bounded UTF-8 prefix CST. Within the byte budget all failure CSTs retain the
/// complete source. Explicit callers should use parse_with_limits for preflight.
pub fn parse(source: &str) -> Parse {
    match parse_with_limits(source, ParseLimits::default()) {
        Ok(parsed) => parsed,
        Err(mut limit) => {
            let retained = lexer::prefix_boundary(source, ParseLimits::default().max_source_bytes);
            limit.retained_bytes = retained;
            let mut builder = GreenNodeBuilder::new();
            builder.start_node(MODULE);
            if retained != 0 {
                builder.token(SyntaxKind(K::Error as u16), &source[..retained]);
            }
            builder.finish_node();
            Parse {
                module: Module::default(),
                green: builder.finish(),
                diagnostics: vec![limit.diagnostic(retained)],
            }
        }
    }
}
/// Configuration/source byte rejection happens before lexer/CST allocation.
/// Token/diagnostic/depth exhaustion returns an empty AST and E0003 with the
/// complete byte-bounded source preserved losslessly in balanced CST nodes.
pub fn parse_with_limits(source: &str, limits: ParseLimits) -> Result<Parse, lexer::LexLimit> {
    let lex_limits = lexer::LexLimits {
        max_source_bytes: limits.max_source_bytes,
        max_tokens: limits.max_tokens,
        max_diagnostics: limits.max_diagnostics,
    };
    lex_limits.validate()?;
    if limits.max_depth == 0 || limits.max_depth > HARD_PARSE_DEPTH {
        return Err(lexer::LexLimit {
            kind: lexer::LexLimitKind::Configuration,
            limit: HARD_PARSE_DEPTH,
            observed: limits.max_depth,
            retained_bytes: 0,
        });
    }
    let lexer::Lexed {
        tokens,
        diagnostics,
        limit,
    } = lexer::lex_with_limits(source, lex_limits)?;
    if limit.is_some() {
        let mut builder = GreenNodeBuilder::new();
        builder.start_node(MODULE);
        for token in &tokens {
            if token.kind != K::Eof {
                builder.token(SyntaxKind(token.kind as u16), &source[token.span.clone()]);
            }
        }
        builder.finish_node();
        return Ok(Parse {
            module: Module::default(),
            green: builder.finish(),
            diagnostics,
        });
    }
    let mut p = Parser {
        source,
        tokens,
        pos: 0,
        builder: GreenNodeBuilder::new(),
        diagnostics,
        no_struct: false,
        row_names: Default::default(),
        bound_values: Default::default(),
        model_uses: vec![],
        trait_bounds: vec![],
        pending_bounds: vec![],
        limits,
        depth: 0,
        halted: false,
        halt_kind: None,
        expr_heights: Default::default(),
        editor: false,
        site_limit: 0,
        site_exhausted: false,
        sites: vec![],
        headers: vec![],
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
    module.model_uses = std::mem::take(&mut p.model_uses);
    module.trait_bounds = std::mem::take(&mut p.trait_bounds);
    if p.halted {
        module = Module::default();
    }
    Ok(Parse {
        module,
        green: p.builder.finish(),
        diagnostics: p.diagnostics,
    })
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
    model_uses: Vec<ModelUse>,
    trait_bounds: Vec<TraitBoundSyntax>,
    /// Trait bounds read by the latest `generics` call (`[T: Trait]`).
    pending_bounds: Vec<TraitBoundSyntax>,
    limits: ParseLimits,
    depth: usize,
    halted: bool,
    halt_kind: Option<lexer::LexLimitKind>,
    expr_heights: std::collections::BTreeMap<(usize, usize), usize>,
    editor: bool,
    site_limit: usize,
    site_exhausted: bool,
    sites: Vec<TraceSite>,
    headers: Vec<HeaderNote>,
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
    /// Source text of the `n`th upcoming non-trivia token.
    fn text(&self, n: usize) -> &str {
        &self.source[self.tokens[self.index(n)].span.clone()]
    }
    /// A contextual keyword: a lower-case identifier with this spelling.
    fn at_word(&self, n: usize, word: &str) -> bool {
        self.nth(n) == K::Lower && self.text(n) == word
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
            if self.editor {
                let delimiter = matches!(k, K::RParen | K::RBracket | K::RBrace);
                let span = if delimiter {
                    self.start()..self.start()
                } else {
                    self.span()
                };
                self.site(
                    span,
                    if delimiter {
                        crate::recovery::SiteKind::MissingDelimiter
                    } else {
                        crate::recovery::SiteKind::Unexpected
                    },
                    None,
                );
            }
            self.error(format!("expected {k:?}, found {:?}", self.kind()));
        }
    }
    fn site(
        &mut self,
        span: Span,
        kind: crate::recovery::SiteKind,
        receiver: Option<crate::recovery::Receiver>,
    ) {
        if self.sites.len() >= self.site_limit {
            self.site_exhausted = true;
            return;
        }
        self.sites.push(TraceSite {
            span,
            kind,
            receiver,
        });
    }
    fn error(&mut self, msg: impl Into<String>) {
        if self.halted {
            return;
        }
        if self.diagnostics.len() >= self.limits.max_diagnostics {
            self.exhaust(
                lexer::LexLimitKind::Diagnostics,
                self.limits.max_diagnostics,
            );
        } else {
            self.diagnostics
                .push(Diagnostic::new("E0002", msg, self.span()));
        }
    }
    fn diagnostic(&mut self, diagnostic: Diagnostic) {
        if self.halted {
            return;
        }
        if self.diagnostics.len() >= self.limits.max_diagnostics {
            self.exhaust(
                lexer::LexLimitKind::Diagnostics,
                self.limits.max_diagnostics,
            );
        } else {
            self.diagnostics.push(diagnostic);
        }
    }
    fn exhaust(&mut self, kind: lexer::LexLimitKind, limit: usize) {
        if self.halted {
            return;
        }
        let at = self.start();
        self.halted = true;
        self.halt_kind = Some(kind);
        let exceeded = lexer::LexLimit {
            kind,
            limit,
            observed: limit + 1,
            retained_bytes: self.source.len(),
        };
        lexer::push_limit(
            &mut self.diagnostics,
            exceeded.diagnostic(at),
            self.limits.max_diagnostics,
        );
        while self.pos < self.tokens.len() && self.tokens[self.pos].kind != K::Eof {
            let token = &self.tokens[self.pos];
            self.builder.token(
                SyntaxKind(token.kind as u16),
                &self.source[token.span.clone()],
            );
            self.pos += 1;
        }
    }
    fn enter(&mut self) -> bool {
        if self.halted {
            return false;
        }
        if self.depth >= self.limits.max_depth {
            self.exhaust(lexer::LexLimitKind::Depth, self.limits.max_depth);
            return false;
        }
        self.depth += 1;
        true
    }
    fn expr_height(&self, expr: &Expr) -> usize {
        self.expr_heights
            .get(&(expr.span.start, expr.span.end))
            .copied()
            .unwrap_or(1)
    }
    // Cache direct-child heights so parenthesized iterative chains cannot hide
    // deep ASTs that would overflow later recursive visitors or Drop.
    fn record_expr(&mut self, expr: Expr) -> Expr {
        if self.halted {
            return self.placeholder_expr();
        }
        let height = |e: &Expr| self.expr_height(e);
        let children = match &expr.kind {
            ExprKind::Literal(_) | ExprKind::Name(_) => 0,
            ExprKind::Tuple(values) => values.iter().map(height).max().unwrap_or(0),
            ExprKind::Struct { fields, .. } => {
                fields.iter().map(|(_, e)| height(e)).max().unwrap_or(0)
            }
            ExprKind::Block { statements, tail } => statements
                .iter()
                .map(|s| match s {
                    Statement::Let { value, .. } | Statement::Expr(value) => height(value),
                })
                .chain(tail.iter().map(|e| height(e)))
                .max()
                .unwrap_or(0),
            ExprKind::Call { callee, args, .. } => args
                .iter()
                .map(|a| height(&a.expr))
                .chain(std::iter::once(height(callee)))
                .max()
                .unwrap_or(0),
            ExprKind::Field(e, _) | ExprKind::Try(e) | ExprKind::Unary(_, e) => height(e),
            ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Assign(_, a, b) => {
                height(a).max(height(b))
            }
            ExprKind::If {
                condition,
                then,
                otherwise,
            } => height(condition)
                .max(height(then))
                .max(otherwise.as_ref().map_or(0, |e| height(e))),
            ExprKind::Match { value, arms } => arms
                .iter()
                .map(|(_, e)| height(e))
                .chain(std::iter::once(height(value)))
                .max()
                .unwrap_or(0),
            ExprKind::Closure { body, .. } => height(body),
            ExprKind::Handle {
                state,
                body,
                ret,
                ops,
                ..
            } => ops
                .iter()
                .map(|c| height(&c.body))
                .chain(std::iter::once(height(body)))
                .chain(std::iter::once(height(&ret.body)))
                .chain(state.iter().map(|(_, e)| height(e)))
                .max()
                .unwrap_or(0),
        };
        let height = children + 1;
        if height > self.limits.max_depth {
            self.exhaust(lexer::LexLimitKind::Depth, self.limits.max_depth);
            return self.placeholder_expr();
        }
        self.expr_heights
            .insert((expr.span.start, expr.span.end), height);
        expr
    }
    fn placeholder_expr(&self) -> Expr {
        Expr {
            kind: ExprKind::Tuple(vec![]),
            span: self.start()..self.start(),
        }
    }
    fn placeholder_pattern(&self) -> Pattern {
        Pattern {
            kind: PatternKind::Wild,
            span: self.start()..self.start(),
        }
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
                } else if self.at(K::Upper) {
                    let start = self.start();
                    let trait_ref = self.trait_ref();
                    self.pending_bounds.push(TraitBoundSyntax {
                        param: name.clone(),
                        trait_ref,
                        span: start..self.end(),
                    });
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
        if self.at_word(0, "use") && self.at_word(1, "model") {
            self.use_model(0..self.source.len());
            return None;
        }
        let public =
            self.at_word(0, "pub") && (self.at_word(1, "trait") || self.at_word(1, "model"));
        if public {
            self.bump();
        }
        if self.at_word(0, "trait") {
            return Some(Item::Trait(self.trait_decl(start, public)));
        }
        if self.at_word(0, "model") {
            return Some(Item::Conformance(self.model_decl(start, public)));
        }
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
                let bounds = std::mem::take(&mut self.pending_bounds);
                let old_rows = self.row_names.clone();
                self.row_names
                    .extend(generics.iter().filter(|g| g.row).map(|g| g.name.clone()));
                if self.at(K::At) {
                    let trait_ref = self.trait_ref();
                    if self.at_word(0, "for") {
                        self.bump();
                    } else {
                        self.error("expected 'for' after the impl's trait");
                    }
                    let c = self.conformance_rest(
                        start,
                        ConformanceKind::Canonical,
                        generics,
                        bounds,
                        trait_ref,
                    );
                    self.row_names = old_rows;
                    return Some(Item::Conformance(c));
                }
                let trait_start = self.start();
                let ty = self.ty();
                if self.at_word(0, "for") {
                    self.bump();
                    let trait_ref = match ty {
                        Type::Named(name, args) => TraitRef {
                            package: None,
                            name,
                            args,
                            span: trait_start..self.end(),
                        },
                        _ => {
                            self.error("expected a trait name before 'for'");
                            TraitRef {
                                package: None,
                                name: "<error>".into(),
                                args: vec![],
                                span: trait_start..self.end(),
                            }
                        }
                    };
                    let c = self.conformance_rest(
                        start,
                        ConformanceKind::Canonical,
                        generics,
                        bounds,
                        trait_ref,
                    );
                    self.row_names = old_rows;
                    return Some(Item::Conformance(c));
                }
                for b in bounds {
                    self.diagnostic(Diagnostic::new(
                        "E0002",
                        "trait bounds are only allowed on 'impl Trait for Type' and model generics",
                        b.span,
                    ));
                }
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
    fn function_header(&mut self) -> crate::macro_source::FunctionHeader {
        let start = self.start();
        self.expect(K::Fn);
        let name_span = self.span();
        let name = self.lower();
        let generics = self.generics();
        self.trait_bounds.append(&mut self.pending_bounds);
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
        if self.at_word(0, "where") {
            for w in self.where_clauses() {
                let param = w.ty.name().to_owned();
                self.trait_bounds.push(TraitBoundSyntax {
                    param,
                    trait_ref: w.trait_ref,
                    span: w.span,
                });
            }
        }
        crate::macro_source::FunctionHeader {
            name,
            name_span,
            generics,
            params,
            result,
            effects,
            span: start..self.start(),
        }
    }
    fn function(&mut self, op: bool) -> Function {
        let diagnostics_before = self.diagnostics.len();
        let old_rows = self.row_names.clone();
        let old_values = self.bound_values.clone();
        let header = self.function_header();
        let crate::macro_source::FunctionHeader {
            name,
            generics,
            params,
            result,
            effects,
            span,
            ..
        } = header;
        let start = span.start;
        if self.editor && !op {
            self.headers.push(HeaderNote {
                start,
                body_start: self.start(),
                clean: self.diagnostics.len() == diagnostics_before,
                nongeneric: generics.is_empty(),
                row_names: self.row_names.clone(),
                bound_values: self.bound_values.clone(),
            });
        }
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
        let previous = self.depth;
        if !self.enter() {
            return vec![];
        }
        let result = self.place_tree_inner();
        self.depth = previous;
        result
    }
    fn place_tree_inner(&mut self) -> Vec<Vec<String>> {
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
        let previous = self.depth;
        if !self.enter() {
            return Type::Infer;
        }
        let result = self.ty_inner();
        self.depth = previous;
        result
    }
    fn ty_inner(&mut self) -> Type {
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
        } else if self.eat(K::Lt) {
            // `<T as Trait[args]>::Assoc`
            let self_ty = self.ty();
            if self.at_word(0, "as") {
                self.bump();
            } else {
                self.error("expected 'as' in a qualified projection");
            }
            let trait_name = self.upper();
            let trait_args = self.type_args();
            self.expect(K::Gt);
            self.expect(K::ColonColon);
            let assoc = self.upper();
            projection_type(self_ty, Some((trait_name, trait_args)), assoc)
        } else {
            let name = self.ident();
            if self.at(K::ColonColon) && self.nth(1) == K::Upper {
                // Shorthand projection `T::Assoc` (trait inferred).
                self.bump();
                let assoc = self.upper();
                projection_type(Type::Named(name, vec![]), None, assoc)
            } else {
                let args = self.type_args();
                Type::Named(name, args)
            }
        };
        self.builder.finish_node();
        result
    }
    fn trait_ref(&mut self) -> TraitRef {
        let start = self.start();
        let package = if self.at(K::At) {
            // `@scope/name::Trait`: a dependency package's trait.
            let mut package = self.bump();
            while matches!(self.kind(), K::Lower | K::Minus | K::Slash | K::Int)
                && self.tokens[self.index(0)].span.start == self.end()
            {
                package.push_str(&self.bump());
            }
            self.expect(K::ColonColon);
            Some(package)
        } else {
            None
        };
        let name = self.upper();
        let args = self.type_args();
        TraitRef {
            package,
            name,
            args,
            span: start..self.end(),
        }
    }
    /// `where T: Trait[args], U: Other` (the leading `where` is current).
    fn where_clauses(&mut self) -> Vec<WhereClause> {
        let mut out = vec![];
        self.bump();
        loop {
            let start = self.start();
            let ty = self.ty();
            self.expect(K::Colon);
            let trait_ref = self.trait_ref();
            out.push(WhereClause {
                ty,
                trait_ref,
                span: start..self.end(),
            });
            if !self.eat(K::Comma) || self.at(K::LBrace) || self.at(K::Semi) {
                break;
            }
        }
        out
    }
    /// `use model Name;` with the given scope.
    fn use_model(&mut self, scope: Span) {
        let start = self.start();
        self.bump();
        self.bump();
        let model = self.upper();
        self.expect(K::Semi);
        self.model_uses.push(ModelUse {
            model,
            span: start..self.end(),
            scope,
        });
    }
    /// `trait Name[params] { type A: mod m; fn sig; }` (at `trait`).
    fn trait_decl(&mut self, start: usize, public: bool) -> TraitDecl {
        self.bump();
        let name = self.upper();
        let generics = self.generics();
        for b in std::mem::take(&mut self.pending_bounds) {
            self.diagnostic(Diagnostic::new(
                "E0002",
                "trait parameters take modal or row bounds, not trait bounds",
                b.span,
            ));
        }
        let old_rows = self.row_names.clone();
        self.row_names
            .extend(generics.iter().filter(|g| g.row).map(|g| g.name.clone()));
        self.expect(K::LBrace);
        let mut assoc = vec![];
        let mut methods = vec![];
        while !self.at(K::RBrace) && !self.at(K::Eof) {
            let before = self.pos;
            if self.at_word(0, "type") {
                let start = self.start();
                self.bump();
                let name = self.upper();
                let bound = if self.eat(K::Colon) {
                    self.expect(K::Mod);
                    self.mode().unwrap_or_else(|| {
                        self.error("expected modal bound");
                        Mode::Linear
                    })
                } else {
                    Mode::Linear
                };
                self.expect(K::Semi);
                assoc.push(AssocTypeDecl {
                    name,
                    bound,
                    span: start..self.end(),
                });
            } else if self.at(K::Fn) {
                methods.push(self.function(true));
            } else {
                self.error("expected 'type' or 'fn' in a trait");
            }
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(K::RBrace);
        self.row_names = old_rows;
        TraitDecl {
            name,
            public,
            generics,
            assoc,
            methods,
            span: start..self.end(),
        }
    }
    /// `model Name[params]: Trait[args] for Type where ... { ... }` (at `model`).
    fn model_decl(&mut self, start: usize, public: bool) -> Conformance {
        self.bump();
        let name = self.upper();
        let generics = self.generics();
        let bounds = std::mem::take(&mut self.pending_bounds);
        let old_rows = self.row_names.clone();
        self.row_names
            .extend(generics.iter().filter(|g| g.row).map(|g| g.name.clone()));
        self.expect(K::Colon);
        let trait_ref = self.trait_ref();
        if self.at_word(0, "for") {
            self.bump();
        } else {
            self.error("expected 'for' after the model's trait");
        }
        let c = self.conformance_rest(
            start,
            ConformanceKind::Model { name, public },
            generics,
            bounds,
            trait_ref,
        );
        self.row_names = old_rows;
        c
    }
    /// The self type, `where` clauses and body of an impl or model.
    fn conformance_rest(
        &mut self,
        start: usize,
        kind: ConformanceKind,
        generics: Vec<Generic>,
        bounds: Vec<TraitBoundSyntax>,
        trait_ref: TraitRef,
    ) -> Conformance {
        let self_ty = self.ty();
        let mut where_clauses: Vec<WhereClause> = bounds
            .into_iter()
            .map(|b| WhereClause {
                ty: Type::Named(b.param, vec![]),
                trait_ref: b.trait_ref,
                span: b.span,
            })
            .collect();
        if self.at_word(0, "where") {
            where_clauses.extend(self.where_clauses());
        }
        self.expect(K::LBrace);
        let mut assoc = vec![];
        let mut methods = vec![];
        while !self.at(K::RBrace) && !self.at(K::Eof) {
            let before = self.pos;
            if self.at_word(0, "type") {
                let start = self.start();
                self.bump();
                let name = self.upper();
                self.expect(K::Eq);
                let ty = self.ty();
                self.expect(K::Semi);
                assoc.push(AssocTypeBinding {
                    name,
                    ty,
                    span: start..self.end(),
                });
            } else if self.at(K::Fn) {
                methods.push(self.function(false));
            } else {
                self.error("expected 'type' or 'fn' in an impl or model");
            }
            if self.pos == before {
                self.bump();
            }
        }
        self.expect(K::RBrace);
        Conformance {
            kind,
            generics,
            trait_ref,
            self_ty,
            where_clauses,
            assoc,
            methods,
            span: start..self.end(),
        }
    }
    fn type_args(&mut self) -> Vec<Type> {
        let previous = self.depth;
        if !self.enter() {
            return vec![];
        }
        let result = self.type_args_inner();
        self.depth = previous;
        result
    }
    fn type_args_inner(&mut self) -> Vec<Type> {
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
                } else if self.at(K::Int) {
                    // A decimal lane count, as in `Vec[u8, 16]` (§14.3). No
                    // identifier can spell an all-digit type name.
                    self.builder.start_node(TYPE);
                    let digits = self.bump();
                    self.builder.finish_node();
                    args.push(Type::Named(digits, vec![]));
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
    fn statement_anchor(&self) -> bool {
        matches!(
            self.kind(),
            K::Let | K::Match | K::Handle | K::Return | K::If
        )
    }
    fn recover_statement(&mut self) {
        if self.at(K::Semi) {
            self.bump();
            return;
        }
        if self.statement_anchor() || matches!(self.kind(), K::RBrace | K::Eof) {
            return;
        }
        let start = self.start();
        self.builder.start_node(ERROR_NODE);
        let mut closing = Vec::new();
        while !self.at(K::Eof) {
            let kind = self.kind();
            if closing.is_empty()
                && (self.statement_anchor() || matches!(kind, K::Semi | K::RBrace))
            {
                break;
            }
            let expected = match kind {
                K::LParen => Some(K::RParen),
                K::LBracket => Some(K::RBracket),
                K::LBrace => Some(K::RBrace),
                _ => None,
            };
            if let Some(expected) = expected {
                if closing.len() >= self.limits.max_depth {
                    self.exhaust(lexer::LexLimitKind::Depth, self.limits.max_depth);
                    break;
                }
                closing.push(expected);
            } else if matches!(kind, K::RParen | K::RBracket | K::RBrace) {
                if closing.last() == Some(&kind) {
                    closing.pop();
                } else if kind == K::RBrace {
                    // A mismatched function closer is a boundary, never a guessed inner anchor.
                    break;
                }
            }
            self.bump();
        }
        let end = self.end();
        self.builder.finish_node();
        if start < end {
            self.site(start..end, crate::recovery::SiteKind::Unexpected, None);
        }
        if self.at(K::Semi) {
            self.bump();
        }
    }
    fn block(&mut self) -> Expr {
        let previous = self.depth;
        if !self.enter() {
            return self.placeholder_expr();
        }
        let result = self.block_inner();
        self.depth = previous;
        self.record_expr(result)
    }
    fn block_inner(&mut self) -> Expr {
        if self.editor {
            self.trivia();
            self.builder.start_node(BLOCK);
        }
        let outer_values = self.bound_values.clone();
        let start = self.start();
        self.expect(K::LBrace);
        let mut statements = vec![];
        let mut tail = None;
        while !self.at(K::RBrace) && !self.at(K::Eof) {
            let before = self.pos;
            if self.at_word(0, "use") && self.at_word(1, "model") {
                // The scope is completed when the block closes.
                self.use_model(start..start);
                continue;
            }
            if self.eat(K::Let) {
                let pat = self.pattern();
                let ty = if self.eat(K::Colon) {
                    self.ty()
                } else {
                    Type::Infer
                };
                self.expect(K::Eq);
                let value = self.expr();
                let missing_semi = self.editor && !self.at(K::Semi);
                self.expect(K::Semi);
                if missing_semi {
                    self.recover_statement();
                }
                pattern_bindings(&pat, &mut self.bound_values);
                statements.push(Statement::Let { pat, ty, value });
            } else {
                let diagnostics_before = self.diagnostics.len();
                let e = self.expr();
                if self.eat(K::Semi) {
                    statements.push(Statement::Expr(e));
                } else if self.editor
                    && !matches!(self.kind(), K::RBrace | K::Eof)
                    && (self.diagnostics.len() > diagnostics_before || self.statement_anchor())
                {
                    if self.diagnostics.len() == diagnostics_before {
                        self.error("expected ';' before following statement");
                    }
                    self.recover_statement();
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
        let span = start..self.end();
        for u in &mut self.model_uses {
            if u.scope == (start..start) {
                u.scope = span.clone();
            }
        }
        if self.editor {
            self.builder.finish_node();
        }
        Expr {
            kind: ExprKind::Block { statements, tail },
            span,
        }
    }
    fn expr(&mut self) -> Expr {
        let previous = self.depth;
        if !self.enter() {
            return self.placeholder_expr();
        }
        let result = self.expr_inner();
        self.depth = previous;
        self.record_expr(result)
    }
    fn expr_inner(&mut self) -> Expr {
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
        let previous = self.depth;
        if !self.enter() {
            return self.placeholder_expr();
        }
        let result = self.if_expr_inner();
        self.depth = previous;
        self.record_expr(result)
    }
    fn if_expr_inner(&mut self) -> Expr {
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
        let previous = self.depth;
        if !self.enter() {
            return self.placeholder_expr();
        }
        let result = self.binary_inner(min);
        self.depth = previous;
        self.record_expr(result)
    }
    fn binary_inner(&mut self, min: u8) -> Expr {
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
            if self.expr_height(&lhs) >= self.limits.max_depth {
                self.exhaust(lexer::LexLimitKind::Depth, self.limits.max_depth);
                break;
            }
            if !self.enter() {
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
            lhs = self.record_expr(Expr { kind, span });
        }
        lhs
    }
    fn postfix(&mut self) -> Expr {
        let previous = self.depth;
        if !self.enter() {
            return self.placeholder_expr();
        }
        let result = self.postfix_inner();
        self.depth = previous;
        self.record_expr(result)
    }
    fn postfix_inner(&mut self) -> Expr {
        let checkpoint = self.builder.checkpoint();
        let mut e = self.primary();
        loop {
            if !matches!(self.kind(), K::LParen | K::LBracket | K::Dot | K::Question) {
                break;
            }
            if self.expr_height(&e) >= self.limits.max_depth {
                self.exhaust(lexer::LexLimitKind::Depth, self.limits.max_depth);
                break;
            }
            if !self.enter() {
                break;
            }
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
                    if self.editor && !matches!(self.kind(), K::Lower | K::SelfKw) {
                        let span = self.end()..self.end();
                        let receiver = recovery_driver::receiver(&e);
                        self.site(span, crate::recovery::SiteKind::MissingMember, receiver);
                        self.error("expected a lower-case member name after '.'");
                        self.builder.start_node_at(checkpoint, INCOMPLETE);
                        self.builder.finish_node();
                        ExprKind::Field(Box::new(e), "<incomplete>".into())
                    } else {
                        ExprKind::Field(Box::new(e), self.lower())
                    }
                }
                K::Question => {
                    self.bump();
                    ExprKind::Try(Box::new(e))
                }
                _ => break,
            };
            e = self.record_expr(Expr {
                kind,
                span: start..self.end(),
            });
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
        let previous = self.depth;
        if !self.enter() {
            return self.placeholder_expr();
        }
        let result = self.primary_inner();
        self.depth = previous;
        self.record_expr(result)
    }
    fn primary_inner(&mut self) -> Expr {
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
                    if self.editor
                        && matches!(
                            self.kind(),
                            K::Semi
                                | K::RBrace
                                | K::RParen
                                | K::RBracket
                                | K::Comma
                                | K::Eof
                                | K::Let
                        )
                    {
                        self.site(start..start, crate::recovery::SiteKind::Unexpected, None);
                        return Expr {
                            kind: ExprKind::Tuple(vec![]),
                            span: start..start,
                        };
                    }
                    if !self.at(K::Eof) {
                        if self.editor {
                            self.builder.start_node(ERROR_NODE);
                        }
                        self.bump();
                        if self.editor {
                            self.builder.finish_node();
                        }
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
        let previous = self.depth;
        if !self.enter() {
            return self.placeholder_pattern();
        }
        let result = self.pattern_inner();
        self.depth = previous;
        result
    }
    fn pattern_inner(&mut self) -> Pattern {
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
