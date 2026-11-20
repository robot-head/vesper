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
mod scope;
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
/// The total token weight of the match-arm bodies that or-pattern
/// alternatives may copy in one parse. Each extra alternative copies its
/// body, and nested or-patterns multiply, so without a budget a short
/// source could expand into an exponentially large AST.
pub const OR_PATTERN_EXPANSION: usize = 1 << 18;

/// Built-in effect of a lexical Strata region (design §15.2). A source
/// `region r { body }` block parses to a handler of this effect labelled `r`
/// whose return clause binds [`REGION_MARKER`]; no source text can spell
/// that binder, so only region blocks produce it.
pub const REGION_EFFECT: &str = "Region";
/// Return-clause binder of a region block. A block with an explicit capacity
/// (`region r(4096) { ... }`) binds `$region:4096`.
pub const REGION_MARKER: &str = "$region";
/// Type name of a tensor shape literal `[d, ...]` (§28.2). No identifier can
/// spell it, so it never names a declared type.
pub const SHAPE_TYPE: &str = "[]";
/// Type name of a dimension product `a * b` inside a shape literal.
pub const DIM_PRODUCT: &str = "*";
/// Largest explicit region capacity in bytes accepted by the parser (1 GiB).
pub const MAX_REGION_CAPACITY: u64 = 1 << 30;
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
    let item_names = contextual_item_names(&tokens, source);
    let mut p = Parser {
        source,
        tokens,
        pos: 0,
        builder: GreenNodeBuilder::new(),
        diagnostics,
        no_struct: false,
        row_names: Default::default(),
        bound_values: item_names.into(),
        model_uses: vec![],
        trait_bounds: vec![],
        uses: vec![],
        public_items: vec![],
        pending_bounds: vec![],
        limits,
        depth: 0,
        halted: false,
        halt_kind: None,
        expr_heights: Default::default(),
        expansion: 0,
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
    module.uses = std::mem::take(&mut p.uses);
    module.public_items = std::mem::take(&mut p.public_items);
    if p.halted {
        module = Module::default();
    }
    lower_const_uses(&mut module);
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
    bound_values: scope::ScopedNames,
    model_uses: Vec<ModelUse>,
    trait_bounds: Vec<TraitBoundSyntax>,
    /// Item-level `use` imports (resolved by the package loader).
    uses: Vec<UseDecl>,
    /// Names of `pub` functions, types and effects, in source order.
    public_items: Vec<String>,
    /// Trait bounds read by the latest `generics` call (`[T: Trait]`).
    pending_bounds: Vec<TraitBoundSyntax>,
    limits: ParseLimits,
    depth: usize,
    halted: bool,
    halt_kind: Option<lexer::LexLimitKind>,
    expr_heights: std::collections::BTreeMap<(usize, usize), usize>,
    /// Token weight of the or-pattern bodies copied so far (see
    /// [`OR_PATTERN_EXPANSION`]).
    expansion: usize,
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
            ExprKind::Closure { body, .. } | ExprKind::Loop(body) => height(body),
            ExprKind::For { iter, body, .. } => height(iter).max(height(body)),
            ExprKind::Range { start, end, .. } => height(start).max(height(end)),
            ExprKind::Break(value) | ExprKind::Return(value) => {
                value.as_ref().map_or(0, |e| height(e))
            }
            ExprKind::Continue => 0,
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
        if self.at_contextual_name() {
            return self.bump();
        }
        if !matches!(self.kind(), K::Lower | K::SelfKw) {
            match self.kind() {
                K::Copy
                | K::Affine
                | K::Mod
                | K::Eff
                | K::In
                | K::Inout
                | K::Sink
                | K::Handle
                | K::Return
                | K::Match
                | K::If
                | K::Else
                | K::Let
                | K::Fn
                | K::Struct
                | K::Enum
                | K::Effect
                | K::Impl => {
                    let word = self.text(0).to_owned();
                    self.error(format!(
                        "expected a lower-case identifier, found the keyword `{word}`, which cannot be a name"
                    ));
                }
                _ => self.error("expected a lower-case identifier"),
            }
        }
        self.ident()
    }
    /// `state`, `linear` and `with` are keywords only where their forms
    /// start (`handle E state p = ..`, `linear struct`, `.. with { .. }`);
    /// everywhere a name is expected they are ordinary lower-case names.
    fn at_contextual_name(&self) -> bool {
        matches!(self.kind(), K::State | K::Linear | K::With)
    }
    /// At the root of a qualified path: `self::`, `super::` or
    /// `@scope/name::` (manuscript ch 11.3, "Qualified paths").
    fn at_path_root(&self) -> bool {
        match self.kind() {
            K::SelfKw => self.nth(1) == K::ColonColon,
            K::Lower => self.text(0) == "super" && self.nth(1) == K::ColonColon,
            K::At => {
                // The adjacent tokens of a package name, then `::`.
                let mut end = self.tokens[self.index(0)].span.end;
                let mut n = 1;
                while matches!(self.nth(n), K::Lower | K::Minus | K::Slash | K::Int)
                    && self.tokens[self.index(n)].span.start == end
                {
                    end = self.tokens[self.index(n)].span.end;
                    n += 1;
                }
                n > 1 && self.nth(n) == K::ColonColon
            }
            _ => false,
        }
    }
    /// A qualified path at [`Self::at_path_root`], joined by `::`:
    /// `self::m::f`, `super::m::T`, `@scope/name::m::E::V`.
    fn qualified_path(&mut self) -> String {
        let mut path = if self.at(K::At) {
            self.package_name()
        } else {
            self.bump()
        };
        while self.at(K::ColonColon)
            && matches!(
                self.nth(1),
                K::Lower | K::Upper | K::State | K::Linear | K::With
            )
        {
            self.bump();
            path.push_str("::");
            path.push_str(&self.bump());
        }
        path
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
                } else if self.at_word(0, "dim") {
                    // `N: dim` (§28.2): a tensor dimension parameter. Its kind
                    // is recovered from use by `vesper::tensor_types`.
                    self.bump();
                } else {
                    // `T: Ord`, `T: mod copy`, or several joined by `+`
                    // (`T: mod copy + Ord + Hash`).
                    loop {
                        if self.at(K::Upper) {
                            let start = self.start();
                            let trait_ref = self.trait_ref();
                            self.pending_bounds.push(TraitBoundSyntax {
                                param: name.clone(),
                                trait_ref,
                                span: start..self.end(),
                                owner: String::new(),
                            });
                        } else {
                            self.expect(K::Mod);
                            bound = self.mode().unwrap_or_else(|| {
                                self.error("expected modal bound");
                                Mode::Linear
                            });
                        }
                        if !self.eat(K::Plus) {
                            break;
                        }
                    }
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
            // `pub` is contextual: `pub: T` remains a field named `pub`.
            if self.at_word(0, "pub")
                && ((self.nth(1) == K::Lower && self.nth(2) == K::Colon)
                    || ((self.at_word(1, "hot") || self.at_word(1, "cold"))
                        && self.nth(2) == K::Lower
                        && self.nth(3) == K::Colon))
            {
                self.bump();
            }
            let temperature = if self.at_word(0, "hot") && self.nth(1) == K::Lower {
                self.bump();
                Some(FieldTemperature::Hot)
            } else if self.at_word(0, "cold") && self.nth(1) == K::Lower {
                self.bump();
                Some(FieldTemperature::Cold)
            } else {
                None
            };
            let name = self.lower();
            self.expect(K::Colon);
            let ty = self.ty();
            let modality = if self.eat(K::AtAt) {
                Some(self.mode().unwrap_or_else(|| {
                    self.error("expected field modality");
                    Mode::Linear
                }))
            } else {
                None
            };
            out.push(Field {
                name,
                ty,
                modality,
                temperature,
            });
            if !self.eat(K::Comma) {
                break;
            }
        }
        self.expect(K::RBrace);
        out
    }
    fn fip_attr(&mut self) -> bool {
        if self.at(K::At) && self.at_word(1, "fip") {
            self.bump();
            self.bump();
            true
        } else {
            false
        }
    }
    fn item(&mut self) -> Option<Item> {
        // `pub` (before or after `@fip`) marks an importable item.
        let public = self.at_word(0, "pub")
            || (self.at(K::At) && self.at_word(1, "fip") && self.at_word(2, "pub"));
        let item = self.declaration()?;
        if public {
            match &item {
                Item::Adt(a) => self.public_items.push(a.name.clone()),
                Item::Function(f) => self.public_items.push(f.name.clone()),
                Item::Effect(e) => self.public_items.push(e.name.clone()),
                Item::Impl { .. } | Item::Trait(_) | Item::Conformance(_) => {}
            }
        }
        Some(item)
    }
    fn declaration(&mut self) -> Option<Item> {
        let start = self.start();
        if self.at_word(0, "use") && self.at_word(1, "model") {
            self.use_model(0..self.source.len());
            return None;
        }
        if self.at_word(0, "use")
            && (matches!(self.nth(1), K::At | K::SelfKw) || self.at_word(1, "super"))
        {
            self.use_item(false);
            return None;
        }
        if self.at_word(0, "pub")
            && self.at_word(1, "use")
            && (matches!(self.nth(2), K::At | K::SelfKw) || self.at_word(2, "super"))
        {
            self.bump();
            self.use_item(true);
            return None;
        }
        if self.at_word(0, "use") && self.nth(1) == K::Lower && self.nth(2) == K::ColonColon {
            self.error("an import path starts with `@scope/package`, `self` or `super`");
            self.bump();
            self.bump();
            while self.eat(K::ColonColon) {
                if !self.eat(K::Lower) && !self.eat(K::Upper) {
                    break;
                }
            }
            self.eat(K::Semi);
            return None;
        }
        let mut fip = self.fip_attr();
        let public = self.at_word(0, "pub")
            && (self.at_word(1, "trait")
                || (self.at_word(1, "const") && self.nth(2) == K::Upper)
                || self.at_word(1, "model")
                || matches!(self.nth(1), K::Fn | K::Struct | K::Enum | K::Effect)
                || (matches!(self.nth(1), K::Copy | K::Affine | K::Linear)
                    && matches!(self.nth(2), K::Struct | K::Enum | K::Effect))
                || (!fip
                    && self.nth(1) == K::At
                    && self.at_word(2, "fip")
                    && self.nth(3) == K::Fn));
        if public {
            self.bump();
        }
        if !fip {
            fip = self.fip_attr();
        }
        if fip && !self.at(K::Fn) {
            self.error("@fip may only annotate a fn declaration");
        }
        if self.at_word(0, "const") && self.nth(1) == K::Upper {
            return Some(Item::Function(Box::new(self.const_item(start))));
        }
        if self.at_word(0, "trait") {
            return Some(Item::Trait(self.trait_decl(start, public)));
        }
        if self.at_word(0, "model") {
            return Some(Item::Conformance(self.model_decl(start, public)));
        }
        let tile_width = if self.at_word(0, "tiled") && self.nth(1) == K::LParen {
            self.bump();
            self.expect(K::LParen);
            let width = if self.at(K::Int) {
                let digits = self.bump();
                digits.parse::<u32>().unwrap_or_else(|_| {
                    self.error("tile width overflows u32");
                    0
                })
            } else {
                self.error("expected integer tile width (8, 16, 32, or 64)");
                0
            };
            self.expect(K::RParen);
            Some(width)
        } else {
            None
        };
        // `wire struct` (§13.2, §28.2): a padding-free byte layout. It is a
        // copy struct in the AST; `vesper::tensor_types` checks its layout.
        let mode = if self.at_word(0, "wire") && self.nth(1) == K::Struct {
            self.bump();
            Mode::Copy
        } else {
            self.mode().unwrap_or_default()
        };
        if tile_width.is_some() && !self.at(K::Struct) {
            self.error("tiled(N) may only annotate a struct declaration");
        }
        match self.kind() {
            K::Struct => {
                self.bump();
                let name = self.upper();
                let generics = self.generics();
                self.own_type_bounds(&name);
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
                    tile_width,
                    span: start..self.end(),
                }))
            }
            K::Enum => {
                self.bump();
                let name = self.upper();
                let generics = self.generics();
                self.own_type_bounds(&name);
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
                                modality: None,
                                temperature: None,
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
                    tile_width: None,
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
                    let mark = self.trait_bounds.len();
                    ops.push(self.function(true, false));
                    self.unsupported_owned(mark);
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
            K::Fn => {
                let mut function = self.function(false, fip);
                if public || fip {
                    function.span.start = start;
                }
                Some(Item::Function(Box::new(function)))
            }
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
                self.expect(K::LBrace);
                let mut methods = vec![];
                let mark = self.trait_bounds.len();
                while self.at_method() {
                    methods.push(self.method());
                }
                self.expect(K::RBrace);
                // Method bounds are owned by `Type::method`; the impl's own
                // bounds constrain every method (whose generics are the
                // impl's generics followed by the method's).
                let owner = ty.name().to_owned();
                for b in &mut self.trait_bounds[mark..] {
                    b.owner = format!("{owner}::{}", b.owner);
                }
                for b in bounds {
                    for m in &methods {
                        let mut b = b.clone();
                        b.owner = format!("{owner}::{}", m.name);
                        self.trait_bounds.push(b);
                    }
                }
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
        for mut b in std::mem::take(&mut self.pending_bounds) {
            b.owner = name.clone();
            self.trait_bounds.push(b);
        }
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
                    owner: name.clone(),
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
    fn at_method(&self) -> bool {
        self.at(K::Fn)
            || (self.at_word(0, "pub")
                && (self.nth(1) == K::Fn || (self.nth(1) == K::At && self.at_word(2, "fip"))))
            || (self.at(K::At) && self.at_word(1, "fip"))
    }
    fn method(&mut self) -> Function {
        let start = self.start();
        let mut fip = self.fip_attr();
        let public = self.at_word(0, "pub")
            && (self.nth(1) == K::Fn || (!fip && self.nth(1) == K::At && self.at_word(2, "fip")));
        if public {
            self.bump();
        }
        if !fip {
            fip = self.fip_attr();
        }
        if fip && !self.at(K::Fn) {
            self.error("@fip may only annotate a fn declaration");
        }
        let mut function = self.function(false, fip);
        if public || fip {
            function.span.start = start;
        }
        function
    }
    fn function(&mut self, op: bool, fip: bool) -> Function {
        let diagnostics_before = self.diagnostics.len();
        let old_rows = self.row_names.clone();
        let old_values = self.bound_values.mark();
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
                bound_values: self.bound_values.to_set(),
            });
        }
        let body = if op {
            self.expect(K::Semi);
            None
        } else {
            Some(self.block())
        };
        self.row_names = old_rows;
        self.bound_values.restore(old_values);
        Function {
            name,
            fip,
            generics,
            params,
            result,
            effects,
            body,
            span: start..self.end(),
        }
    }
    /// `const NAME: T = expr;` (§2.2 `ConstDecl`): the parameterless
    /// function [`CONST_PREFIX`]`NAME`, whose every use the checker reads
    /// as a call (`vesper::arrays`).
    fn const_item(&mut self, start: usize) -> Function {
        self.bump();
        let name = self.upper();
        self.expect(K::Colon);
        let result = self.ty();
        self.expect(K::Eq);
        let value = self.expr();
        self.expect(K::Semi);
        let span = start..self.end();
        Function {
            name: format!("{CONST_PREFIX}{name}"),
            fip: false,
            generics: vec![],
            params: vec![],
            result,
            effects: Effects::default(),
            body: Some(Expr {
                span: value.span.clone(),
                kind: ExprKind::Block {
                    statements: vec![],
                    tail: Some(Box::new(value)),
                },
            }),
            span,
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
    /// `(pass T, ..) -> R eff [E]` after `fn` (and its mode) or a
    /// function-object trait name.
    fn function_signature(&mut self, mode: Mode) -> Type {
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
    }
    /// Whether the `[` at the cursor opens an array type `[T; N]`: a `;`
    /// at bracket depth one before the matching `]`. A tensor shape
    /// `[M, N]` has none.
    fn array_type_ahead(&self) -> bool {
        let mut depth = 0usize;
        for token in &self.tokens[self.pos..] {
            let k = token.kind;
            if k.trivia() {
                continue;
            }
            match k {
                K::LBracket | K::LParen | K::LBrace => depth += 1,
                K::RBracket | K::RParen | K::RBrace => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return false;
                    }
                }
                K::Semi if depth == 1 => return true,
                K::Eof => return false,
                _ => {}
            }
        }
        false
    }
    /// The length `N` of `[T; N]` or `[x; N]`: a decimal or hex literal.
    fn array_length(&mut self) -> usize {
        if !self.at(K::Int) {
            self.error("expected an integer literal array length");
            return 0;
        }
        let text = self.bump().replace('_', "");
        let n = if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            usize::from_str_radix(hex, 16).ok()
        } else {
            text.parse().ok()
        };
        match n.filter(|n| *n <= MAX_ARRAY_LENGTH) {
            Some(n) => n,
            None => {
                self.error(format!(
                    "array length exceeds the limit of {MAX_ARRAY_LENGTH} elements"
                ));
                0
            }
        }
    }
    fn ty_inner(&mut self) -> Type {
        self.builder.start_node(TYPE);
        let result = if self.eat(K::Fn) {
            let mode = if self.eat(K::At) {
                self.mode().unwrap_or(Mode::Affine)
            } else {
                Mode::Affine
            };
            self.function_signature(mode)
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
        } else if self.at(K::LBracket) && self.array_type_ahead() {
            // A fixed-size array `[T; N]` (§5): the homogeneous `N`-tuple.
            self.bump();
            let element = self.ty();
            self.expect(K::Semi);
            let mut n = self.array_length();
            self.expect(K::RBracket);
            if type_leaves(&element).saturating_mul(n) > MAX_ARRAY_LENGTH {
                self.error(format!(
                    "a nested array type holds more than {MAX_ARRAY_LENGTH} elements in all"
                ));
                n = 0;
            }
            Type::Tuple(vec![element; n])
        } else if self.eat(K::LBracket) {
            // A tensor shape `[M, N * K, 128]` (§28.2), encoded as the
            // unspellable `[]` type over its dimensions (see SHAPE_TYPE).
            let mut dims = vec![];
            while !self.at(K::RBracket) && !self.at(K::Eof) {
                let before = self.pos;
                dims.push(self.dimension());
                if !self.eat(K::Comma) || self.pos == before {
                    break;
                }
            }
            self.expect(K::RBracket);
            Type::Named(SHAPE_TYPE.into(), dims)
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
        } else if self.at_word(0, DYN_TYPE) && self.nth(1) == K::Upper {
            // `dyn Trait[args]` (manuscript §5 `DynType`, §7.1).
            self.bump();
            let name = self.upper();
            if matches!(name.as_str(), "Fn" | "FnMut" | "FnOnce") && self.at(K::LParen) {
                // A function object `dyn FnMut(A) -> R eff [E]` (§7.3).
                let signature = self.function_signature(Mode::Affine);
                Type::Named(DYN_TYPE.into(), vec![Type::Named(name, vec![signature])])
            } else {
                let args = self.type_args();
                Type::Named(DYN_TYPE.into(), vec![Type::Named(name, args)])
            }
        } else if self.at_path_root() {
            // `self::m::T[args]`, `super::m::T`, `@scope/name::m::T`.
            let name = self.qualified_path();
            let args = self.type_args();
            Type::Named(name, args)
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
    /// One dimension of a shape: a product of decimal literals and dimension
    /// names, left associative (`QHeads * HeadDim`).
    fn dimension(&mut self) -> Type {
        let mut dim = self.dimension_factor();
        while self.eat(K::Star) {
            let right = self.dimension_factor();
            dim = Type::Named(DIM_PRODUCT.into(), vec![dim, right]);
        }
        dim
    }
    fn dimension_factor(&mut self) -> Type {
        if self.at(K::Int) {
            self.builder.start_node(TYPE);
            let digits = self.bump();
            self.builder.finish_node();
            Type::Named(digits, vec![])
        } else {
            self.ty()
        }
    }
    /// `@scope/name` at `@`: the adjacent tokens of a package name.
    fn package_name(&mut self) -> String {
        let mut package = self.bump();
        while matches!(self.kind(), K::Lower | K::Minus | K::Slash | K::Int)
            && self.tokens[self.index(0)].span.start == self.end()
        {
            package.push_str(&self.bump());
        }
        package
    }
    /// `pub? use (@scope/name | self | super (:: super)*) (:: segment)* ::
    /// (item (as alias)? | { list });` at `use` (manuscript ch 2.2
    /// `UseDecl`); a `pub` before it has been consumed.
    fn use_item(&mut self, public: bool) {
        let start = self.start();
        self.bump();
        let mut supers = 0;
        let package = if self.at(K::At) {
            Some(self.package_name())
        } else if self.at_word(0, "super") {
            self.bump();
            supers = 1;
            while self.at(K::ColonColon) && self.at_word(1, "super") {
                self.bump();
                self.bump();
                supers += 1;
            }
            None
        } else {
            self.bump();
            None
        };
        let mut path = vec![];
        let mut names = vec![];
        loop {
            if !self.eat(K::ColonColon) {
                self.error("expected `::` and an imported item");
                break;
            }
            if self.at(K::LBrace) {
                self.bump();
                while !self.at(K::RBrace) && !self.at(K::Eof) {
                    let before = self.pos;
                    names.push(self.use_name());
                    if !self.eat(K::Comma) || self.pos == before {
                        break;
                    }
                }
                self.expect(K::RBrace);
                if names.is_empty() {
                    self.error("an import list names at least one item");
                }
                break;
            }
            if (self.at(K::Lower) || self.at_contextual_name()) && self.nth(1) == K::ColonColon {
                path.push(self.bump());
                continue;
            }
            names.push(self.use_name());
            break;
        }
        self.expect(K::Semi);
        self.uses.push(UseDecl {
            package,
            supers,
            path,
            names,
            public,
            span: start..self.end(),
        });
    }
    fn use_name(&mut self) -> UseName {
        let start = self.start();
        let name = if matches!(self.kind(), K::Lower | K::Upper) || self.at_contextual_name() {
            self.bump()
        } else {
            self.error("expected an imported item name");
            "<error>".into()
        };
        let alias = if self.at_word(0, "as") {
            self.bump();
            if matches!(self.kind(), K::Lower | K::Upper) || self.at_contextual_name() {
                Some(self.bump())
            } else {
                self.error("expected a name after `as`");
                None
            }
        } else {
            None
        };
        UseName {
            name,
            alias,
            span: start..self.end(),
        }
    }
    fn trait_ref(&mut self) -> TraitRef {
        let start = self.start();
        let package = if self.at(K::At) {
            // `@scope/name::Trait`: a dependency package's trait.
            let package = self.package_name();
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
                let mark = self.trait_bounds.len();
                methods.push(self.function(true, false));
                self.unsupported_owned(mark);
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
    /// Moves the trait bounds of a type's generic parameters to the module
    /// with the type as their owner.
    fn own_type_bounds(&mut self, name: &str) {
        for mut b in std::mem::take(&mut self.pending_bounds) {
            b.owner = name.to_owned();
            self.trait_bounds.push(b);
        }
    }
    /// Marks the bounds recorded since `mark` as belonging to a trait,
    /// conformance or effect operation (which the frontend rejects).
    fn unsupported_owned(&mut self, mark: usize) {
        for b in &mut self.trait_bounds[mark..] {
            b.owner = format!("$unsupported::{}", b.owner);
        }
    }
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
            } else if self.at_method() {
                let mark = self.trait_bounds.len();
                methods.push(self.method());
                self.unsupported_owned(mark);
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
                if self.at(K::Lower)
                    && !self.primitive()
                    && !self.at_path_root()
                    && !(self.at_word(0, DYN_TYPE) && self.nth(1) == K::Upper)
                {
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
        vesper_core::numeric::is_scalar_name(&self.source[self.span()])
            && !matches!(&self.source[self.span()], "Unit" | "Never")
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
            if self.at(K::Lower) && self.nth(1) != K::Colon && !self.at_path_root() {
                out.tail = Some(self.lower());
                break;
            }
            let label = if self.at(K::Lower) && !self.at_path_root() {
                let l = self.lower();
                self.expect(K::Colon);
                Some(l)
            } else {
                None
            };
            // `eff [self::m::E]`: a qualified effect name (ch 11.3).
            let name = if self.at_path_root() {
                self.qualified_path()
            } else {
                self.upper()
            };
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
        let outer_values = self.bound_values.mark();
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
                // `let mut` (manuscript `LetStmt`); `mut` is a contextual word.
                let mutable = self.at_word(0, "mut") && self.nth(1) != K::Colon;
                if mutable {
                    self.bump();
                }
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
                statements.push(Statement::Let {
                    pat,
                    ty,
                    value,
                    mutable,
                });
            } else {
                let diagnostics_before = self.diagnostics.len();
                let e = self.expr();
                if self.eat(K::Semi) {
                    statements.push(Statement::Expr(e));
                } else if matches!(
                    e.kind,
                    ExprKind::Loop(_)
                        | ExprKind::For { .. }
                        | ExprKind::If { .. }
                        | ExprKind::Match { .. }
                ) && self.source[..e.span.end.min(self.source.len())].ends_with('}')
                    && !matches!(self.kind(), K::RBrace | K::Eof)
                {
                    // A block-like statement ending in `}` needs no `;` before
                    // the next statement (manuscript `Stmt`).
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
        self.bound_values.restore(outer_values);
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
            K::Lower
                if self.at_word(0, "region")
                    && self.nth(1) == K::Lower
                    && matches!(self.nth(2), K::LBrace | K::LParen) =>
            {
                self.region()
            }
            K::Match => self.match_expr(),
            K::If => self.if_expr(),
            K::Lower if self.at_word(0, "loop") && self.nth(1) == K::LBrace => self.loop_expr(),
            K::Lower if self.at_word(0, "for") && self.for_header() => self.for_expr(),
            K::Lower if self.at_word(0, "break") && !self.bound_values.contains("break") => {
                self.jump_expr(false)
            }
            K::Lower if self.at_continue() => self.continue_expr(),
            K::Return => self.jump_expr(true),
            K::Pipe | K::Or => self.closure(),
            _ => self.binary(0),
        };
        self.builder.finish_node();
        e
    }
    /// `loop { body }` (manuscript `LoopExpr`).
    fn loop_expr(&mut self) -> Expr {
        let start = self.start();
        self.bump();
        let body = Box::new(self.block());
        Expr {
            kind: ExprKind::Loop(body),
            span: start..self.end(),
        }
    }
    /// Whether `for` starts a `ForExpr` (`for Pat in`), not a value named `for`.
    fn for_header(&self) -> bool {
        !self.bound_values.contains("for")
            && matches!(
                self.nth(1),
                K::Lower | K::Upper | K::LParen | K::Int | K::True | K::False
            )
    }
    /// `for pat in start..end { body }` (manuscript `ForExpr`). The iterable
    /// is any restricted expression; `a..b` and `a..=b` build a range.
    fn for_expr(&mut self) -> Expr {
        let start = self.start();
        self.bump();
        let pat = self.pattern();
        self.expect(K::In);
        let old = self.no_struct;
        self.no_struct = true;
        let first = self.binary(2);
        let iter = if matches!(self.kind(), K::DotDot | K::DotDotEq) {
            let inclusive = self.at(K::DotDotEq);
            self.bump();
            let end = self.binary(2);
            let span = first.span.start..end.span.end;
            self.record_expr(Expr {
                kind: ExprKind::Range {
                    start: Box::new(first),
                    end: Box::new(end),
                    inclusive,
                },
                span,
            })
        } else {
            first
        };
        self.no_struct = old;
        let outer_values = self.bound_values.mark();
        pattern_bindings(&pat, &mut self.bound_values);
        let body = Box::new(self.block());
        self.bound_values.restore(outer_values);
        Expr {
            kind: ExprKind::For {
                pat,
                iter: Box::new(iter),
                body,
            },
            span: start..self.end(),
        }
    }
    /// `return value?` or `break value?` (manuscript `ReturnExpr`, `BreakExpr`).
    fn jump_expr(&mut self, ret: bool) -> Expr {
        let start = self.start();
        self.bump();
        let value = if matches!(
            self.kind(),
            K::Semi | K::RBrace | K::RParen | K::RBracket | K::Comma | K::Eof
        ) {
            None
        } else {
            Some(Box::new(self.expr()))
        };
        Expr {
            kind: if ret {
                ExprKind::Return(value)
            } else {
                ExprKind::Break(value)
            },
            span: start..self.end(),
        }
    }
    /// Whether the parser is at `continue` (manuscript `ContinueExpr`): a
    /// contextual word, like `break`, that stays an ordinary name where a
    /// binding or item of that name is in scope or a value follows it.
    fn at_continue(&self) -> bool {
        self.at_word(0, "continue")
            && !self.bound_values.contains("continue")
            && matches!(
                self.nth(1),
                K::Semi | K::RBrace | K::RParen | K::RBracket | K::Comma | K::Eof
            )
    }
    /// `continue` (manuscript `ContinueExpr`).
    fn continue_expr(&mut self) -> Expr {
        let start = self.start();
        self.bump();
        Expr {
            kind: ExprKind::Continue,
            span: start..self.end(),
        }
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
            let before = self.pos;
            // `p | q => e` is one arm per alternative sharing the body: the
            // AST holds consecutive arms whose bodies are the same expression
            // (same span), which the checker groups again.
            let mut alternatives = vec![self.pattern()];
            while self.eat(K::Pipe) {
                alternatives.push(self.pattern());
            }
            self.expect(K::FatArrow);
            let outer_values = self.bound_values.mark();
            for pat in &alternatives {
                pattern_bindings(pat, &mut self.bound_values);
            }
            let (body_start, expanded) = (self.pos, self.expansion);
            let body = self.expr();
            let block_body = matches!(body.kind, ExprKind::Block { .. });
            self.bound_values.restore(outer_values);
            // A body's weight is its tokens plus the copies nested in it.
            let weight = (self.pos - body_start) + (self.expansion - expanded);
            let copies = weight.saturating_mul(alternatives.len() - 1);
            self.expansion = self.expansion.saturating_add(copies);
            if self.expansion > OR_PATTERN_EXPANSION {
                self.error("or-pattern alternatives copy too much code; split the match");
                alternatives.truncate(1);
            }
            for pat in alternatives {
                arms.push((pat, body.clone()));
            }
            if !self.eat(K::Comma) && !self.at(K::RBrace) && !(block_body && self.pos > before) {
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
        let outer_values = self.bound_values.mark();
        self.bound_values
            .extend(params.iter().map(|p| p.name.clone()));
        let body = Box::new(self.expr());
        self.bound_values.restore(outer_values);
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
        let name = if self.at_path_root() {
            self.qualified_path()
        } else {
            self.upper()
        };
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
        let outer_values = self.bound_values.mark();
        pattern_bindings(&pat, &mut self.bound_values);
        let ret = Box::new(Clause {
            name: "return".into(),
            pats: vec![pat],
            body: self.expr(),
        });
        self.bound_values.restore(outer_values);
        self.eat(K::Comma);
        let mut ops = vec![];
        while !self.at(K::RBrace) && !self.at(K::Eof) {
            let name = self.lower();
            self.expect(K::LParen);
            let pats = self.pattern_list(K::RParen);
            self.expect(K::RParen);
            self.expect(K::FatArrow);
            let outer_values = self.bound_values.mark();
            for p in &pats {
                pattern_bindings(p, &mut self.bound_values);
            }
            let body = self.expr();
            self.bound_values.restore(outer_values);
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
    /// `region r { body }` or `region r(capacity) { body }` (design §15.2):
    /// a lexical region capability `r`, encoded as a handler of the built-in
    /// [`REGION_EFFECT`] with a reserved return binder and no clauses.
    fn region(&mut self) -> Expr {
        let start = self.start();
        self.bump();
        let label = self.lower();
        let mut marker = REGION_MARKER.to_owned();
        if self.eat(K::LParen) {
            let capacity = if self.at(K::Int) {
                match self.literal() {
                    Some(Literal::Int(n)) if n > 0 && n <= MAX_REGION_CAPACITY as i128 => n,
                    _ => {
                        self.error("region capacity must be between 1 byte and 1 GiB");
                        1
                    }
                }
            } else {
                self.error("expected an integer region capacity in bytes");
                1
            };
            self.expect(K::RParen);
            marker = format!("{REGION_MARKER}:{capacity}");
        }
        let body = Box::new(self.block());
        let span = start..self.end();
        // The closing brace: no other expression or pattern starts there.
        let at = body.span.end.saturating_sub(1)..body.span.end;
        let ret = Box::new(Clause {
            name: "return".into(),
            pats: vec![Pattern {
                kind: PatternKind::Bind(marker.clone()),
                span: at.clone(),
            }],
            body: Expr {
                kind: ExprKind::Name(marker),
                span: at,
            },
        });
        Expr {
            kind: ExprKind::Handle {
                effect: Effect {
                    label: Some(label),
                    name: REGION_EFFECT.into(),
                    args: vec![],
                },
                state: None,
                body,
                ret,
                ops: vec![],
            },
            span,
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
        let mut lhs = if matches!(self.kind(), K::Minus | K::Bang | K::Tilde) {
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
                // Bit operators bind tighter than comparisons, as in Rust.
                K::Pipe => (6, false),
                K::Caret => (7, false),
                K::Amp => (8, false),
                K::Shl | K::Shr => (9, false),
                K::Plus | K::Minus => (10, false),
                K::Star | K::Slash | K::Percent => (11, false),
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
                    let tiled_col_arg = match &e.kind {
                        ExprKind::Field(_, m) => {
                            matches!(m.as_str(), "load_tile_vec" | "col_get" | "col_set")
                        }
                        ExprKind::Name(n) => matches!(
                            n.as_str(),
                            "TiledSeq::load_tile_vec" | "TiledSeq::col_get" | "TiledSeq::col_set"
                        ),
                        _ => false,
                    } && self.nth(1) == K::Lower
                        && self.nth(2) == K::RBracket
                        && self.nth(3) == K::LParen;
                    let scoped_row = {
                        let i = self.index(1);
                        let text = &self.source[self.tokens[i].span.clone()];
                        self.row_names.contains(text)
                            && !matches!(&e.kind,ExprKind::Name(n) if self.bound_values.contains(n))
                    };
                    // `Tensor::reshape[[M, N]](x)`: no index expression starts with `[`.
                    let generic = named_row
                        || tiled_col_arg
                        || scoped_row
                        || self.nth(1) == K::Upper
                        || self.nth(1) == K::LBracket
                        // `f[self::m::T](x)`, `f[@scope/name::T](x)`: a
                        // qualified type argument (ch 11.3).
                        || self.nth(1) == K::At
                        || (matches!(self.nth(1), K::SelfKw) && self.nth(2) == K::ColonColon)
                        || {
                            let i = self.index(1);
                            let text = &self.source[self.tokens[i].span.clone()];
                            text == "fn"
                                || (text == "super" && self.nth(2) == K::ColonColon)
                                // `Box::new[dyn Trait](x)` (§7.3)
                                || (text == DYN_TYPE && self.nth(2) == K::Upper)
                                || (vesper_core::numeric::is_scalar_name(text)
                                    && !matches!(text, "Unit" | "Never"))
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
                        let index = if self.at(K::DotDot) {
                            // `p[..]` and `p[..b]`: open slice intervals.
                            self.open_interval(&e, None)
                        } else {
                            let index = self.expr();
                            // A slice interval `p[a..b]` (§2.2) is an index whose
                            // expression is the binary `..` of its bounds.
                            if self.kind() == K::DotDot {
                                if self.nth(1) == K::RBracket {
                                    self.open_interval(&e, Some(index))
                                } else {
                                    let op = self.bump();
                                    let hi = self.expr();
                                    let span = index.span.start..hi.span.end;
                                    self.record_expr(Expr {
                                        kind: ExprKind::Binary(op, Box::new(index), Box::new(hi)),
                                        span,
                                    })
                                }
                            } else {
                                index
                            }
                        };
                        self.expect(K::RBracket);
                        ExprKind::Index(Box::new(e), Box::new(index))
                    }
                }
                K::Dot => {
                    self.bump();
                    if self.editor
                        && !matches!(self.kind(), K::Lower | K::SelfKw)
                        && !self.at_contextual_name()
                    {
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
                _ if self.at_path_root() => {
                    // `self::m::f`, `super::m::T { .. }`, `@scope/name::m::E::V`.
                    let name = self.qualified_path();
                    let last_upper = name
                        .rsplit("::")
                        .next()
                        .is_some_and(|s| s.starts_with(|c: char| c.is_ascii_uppercase()));
                    if last_upper && self.at(K::LBrace) && !self.no_struct {
                        self.struct_literal(name)
                    } else {
                        ExprKind::Name(name)
                    }
                }
                K::Lower | K::SelfKw | K::State | K::Linear | K::With => {
                    ExprKind::Name(self.bump())
                }
                K::Upper => {
                    let mut name = self.bump();
                    while self.eat(K::ColonColon) {
                        name.push_str("::");
                        name.push_str(&self.ident());
                    }
                    if self.at(K::LBrace) && !self.no_struct {
                        self.struct_literal(name)
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
                K::LBracket => self.array_literal(start),
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
    /// `[a, b, c]` or `[x; N]` (§5 fixed-size arrays), both the tuple of
    /// their elements. A repeated operand that is not a literal or a name
    /// is evaluated once into a hidden binding.
    fn array_literal(&mut self, start: usize) -> ExprKind {
        self.expect(K::LBracket);
        let mut values = vec![];
        while !self.at(K::RBracket) && !self.at(K::Eof) {
            let value = self.expr();
            if values.is_empty() && self.eat(K::Semi) {
                let mut n = self.array_length();
                self.expect(K::RBracket);
                if expr_leaves(&value).saturating_mul(n) > MAX_ARRAY_LENGTH {
                    self.error(format!(
                        "a nested array literal holds more than {MAX_ARRAY_LENGTH} elements in all"
                    ));
                    n = 0;
                }
                return splat(value, n, start..self.end());
            }
            values.push(value);
            if !self.eat(K::Comma) {
                break;
            }
        }
        self.expect(K::RBracket);
        ExprKind::Tuple(values)
    }
    /// An interval with an open end at `..` inside `p[`: `p[..]`, `p[..b]`
    /// and `p[a..]` mean `p[0..p.len()]`, `p[0..b]` and `p[a..p.len()]`
    /// (§2.2). An open upper bound reads `p.len()`, so `p` must be a place
    /// path (a name and fields). The copy of the path and the `len()` call
    /// get synthetic spans that start inside the `..` token, where no source
    /// node starts, so no span-keyed table sees two nodes at one span.
    fn open_interval(&mut self, base: &Expr, lo: Option<Expr>) -> Expr {
        let dots = self.span();
        let op = self.bump();
        let mut fresh = dots.start + 1;
        let mut next_span = || {
            let s = dots.start + 1..fresh;
            fresh += 1;
            s
        };
        let lo = lo.unwrap_or(Expr {
            kind: ExprKind::Literal(Literal::Int(0)),
            span: dots.start..dots.start,
        });
        let hi = if self.at(K::RBracket) {
            fn copy(e: &Expr, next: &mut dyn FnMut() -> Span) -> Option<Expr> {
                let kind = match &e.kind {
                    ExprKind::Name(n) => ExprKind::Name(n.clone()),
                    ExprKind::Field(inner, f) => {
                        ExprKind::Field(Box::new(copy(inner, next)?), f.clone())
                    }
                    _ => return None,
                };
                Some(Expr { kind, span: next() })
            }
            match copy(base, &mut next_span) {
                Some(place) => {
                    let len = Expr {
                        kind: ExprKind::Field(Box::new(place), "len".into()),
                        span: next_span(),
                    };
                    Expr {
                        kind: ExprKind::Call {
                            callee: Box::new(len),
                            types: vec![],
                            args: vec![],
                        },
                        span: next_span(),
                    }
                }
                None => {
                    self.error(
                        "an interval without an upper bound needs a named sequence (`s[a..]`); bind the value to a name first",
                    );
                    self.placeholder_expr()
                }
            }
        } else {
            self.expr()
        };
        let span = lo.span.start.min(dots.start)..hi.span.end.max(dots.end);
        self.record_expr(Expr {
            kind: ExprKind::Binary(op, Box::new(lo), Box::new(hi)),
            span,
        })
    }
    /// `Name { field: value, field }` at `{`.
    fn struct_literal(&mut self, name: String) -> ExprKind {
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
                _ if self.at_path_root() => {
                    let name = self.qualified_path();
                    self.constructor_pattern(name)
                }
                K::Lower | K::State | K::Linear | K::With => {
                    let name = self.bump();
                    if name == "_" {
                        PatternKind::Wild
                    } else {
                        PatternKind::Bind(name)
                    }
                }
                K::Upper => {
                    let mut name = self.upper();
                    while self.eat(K::ColonColon) {
                        name.push_str("::");
                        name.push_str(&self.upper());
                    }
                    self.constructor_pattern(name)
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
    /// The fields of a constructor pattern after its name: `(p, ..)`,
    /// `{ f: p, f }` or none.
    fn constructor_pattern(&mut self, name: String) -> PatternKind {
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
}

fn pattern_bindings(p: &Pattern, names: &mut scope::ScopedNames) {
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

/// Top-level functions named after a contextual loop word (`fn break()`)
/// keep that word an ordinary name in calls, like a local binding does.
pub(crate) fn contextual_item_names(
    tokens: &[Token],
    source: &str,
) -> std::collections::BTreeSet<String> {
    let mut names = std::collections::BTreeSet::new();
    let mut significant = tokens
        .iter()
        .filter(|t| !matches!(t.kind, K::Whitespace | K::Comment | K::BlockComment));
    let mut previous = None;
    for t in &mut significant {
        if previous == Some(K::Fn) && t.kind == K::Lower {
            let name = &source[t.span.clone()];
            if matches!(name, "break" | "continue" | "for" | "loop") {
                names.insert(name.to_owned());
            }
        }
        previous = Some(t.kind);
    }
    names
}

/// The function name prefix of a module constant `const NAME: T = e;`.
pub const CONST_PREFIX: &str = "const$";
/// The largest `N` of an array type `[T; N]` or literal `[x; N]`.
pub const MAX_ARRAY_LENGTH: usize = 1 << 16;
/// The binding a repeated array operand `[f(); N]` is evaluated into.
pub const ARRAY_SPLAT_BINDING: &str = "$splat";

/// The scalar elements of a value of type `t`, through nested tuples.
fn type_leaves(t: &Type) -> usize {
    match t {
        Type::Tuple(ts) => ts
            .iter()
            .fold(0usize, |n, t| n.saturating_add(type_leaves(t)))
            .max(1),
        _ => 1,
    }
}

/// The scalar elements of an array literal operand, through nested tuples
/// and the tails of blocks.
fn expr_leaves(e: &Expr) -> usize {
    match &e.kind {
        ExprKind::Tuple(es) => es
            .iter()
            .fold(0usize, |n, e| n.saturating_add(expr_leaves(e)))
            .max(1),
        ExprKind::Block { tail: Some(t), .. } => expr_leaves(t),
        _ => 1,
    }
}

/// Whether `e` is built from literals and names by operators only, so a
/// copy of it evaluates to the same value and keeps its contextual type
/// (`[-1.0; 2]` as `[f32; 2]`).
fn pure_operand(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Literal(_) | ExprKind::Name(_) => true,
        ExprKind::Unary(_, v) => pure_operand(v),
        ExprKind::Binary(_, a, b) => pure_operand(a) && pure_operand(b),
        _ => false,
    }
}

/// `[value; n]` as the `n`-tuple of `value`.
fn splat(value: Expr, n: usize, span: Span) -> ExprKind {
    if pure_operand(&value) {
        return ExprKind::Tuple(vec![value; n]);
    }
    let name = Expr {
        kind: ExprKind::Name(ARRAY_SPLAT_BINDING.into()),
        span: value.span.clone(),
    };
    ExprKind::Block {
        statements: vec![Statement::Let {
            pat: Pattern {
                kind: PatternKind::Bind(ARRAY_SPLAT_BINDING.into()),
                span: value.span.clone(),
            },
            ty: Type::Infer,
            value,
            mutable: false,
        }],
        tail: Some(Box::new(Expr {
            kind: ExprKind::Tuple(vec![name; n]),
            span,
        })),
    }
}

/// Turns every use `NAME` of a constant this module declares into a call of
/// its function [`CONST_PREFIX`]`NAME`, before a package loader renames the
/// module's items. Returns whether the module declares a constant.
pub fn lower_const_uses(module: &mut Module) -> bool {
    let consts = module
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Function(f) if f.name.starts_with(CONST_PREFIX) => {
                Some(f.name[CONST_PREFIX.len()..].to_owned())
            }
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    if consts.is_empty() {
        return false;
    }
    let mut rewrite = |e: &mut Expr| {
        if let ExprKind::Name(n) = &e.kind
            && consts.contains(n)
        {
            // The callee has an empty span of its own: a callee whose span
            // has a type is a function value, and the call's span has one.
            e.kind = ExprKind::Call {
                callee: Box::new(Expr {
                    kind: ExprKind::Name(format!("{CONST_PREFIX}{n}")),
                    span: e.span.start..e.span.start,
                }),
                types: vec![],
                args: vec![],
            };
        }
    };
    for item in &mut module.items {
        match item {
            Item::Function(f) => function_exprs(f, &mut rewrite),
            Item::Impl { methods, .. } => {
                for f in methods {
                    function_exprs(f, &mut rewrite);
                }
            }
            Item::Conformance(c) => {
                for f in &mut c.methods {
                    function_exprs(f, &mut rewrite);
                }
            }
            Item::Trait(t) => {
                for f in &mut t.methods {
                    function_exprs(f, &mut rewrite);
                }
            }
            Item::Adt(_) | Item::Effect(_) => {}
        }
    }
    true
}

fn function_exprs(f: &mut Function, g: &mut dyn FnMut(&mut Expr)) {
    if let Some(body) = &mut f.body {
        walk_exprs(body, g);
    }
}

/// Applies `g` to every expression of `e`, children first.
pub fn walk_exprs(e: &mut Expr, g: &mut dyn FnMut(&mut Expr)) {
    match &mut e.kind {
        ExprKind::Literal(_) | ExprKind::Name(_) | ExprKind::Continue => {}
        ExprKind::Tuple(es) => es.iter_mut().for_each(|e| walk_exprs(e, g)),
        ExprKind::Struct { fields, .. } => fields.iter_mut().for_each(|(_, e)| walk_exprs(e, g)),
        ExprKind::Block { statements, tail } => {
            for s in statements {
                match s {
                    Statement::Let { value, .. } | Statement::Expr(value) => walk_exprs(value, g),
                }
            }
            if let Some(t) = tail {
                walk_exprs(t, g);
            }
        }
        ExprKind::Call { callee, args, .. } => {
            walk_exprs(callee, g);
            for a in args {
                walk_exprs(&mut a.expr, g);
            }
        }
        ExprKind::Field(b, _) | ExprKind::Try(b) | ExprKind::Unary(_, b) => walk_exprs(b, g),
        ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) | ExprKind::Assign(_, a, b) => {
            walk_exprs(a, g);
            walk_exprs(b, g);
        }
        ExprKind::If {
            condition,
            then,
            otherwise,
        } => {
            walk_exprs(condition, g);
            walk_exprs(then, g);
            if let Some(o) = otherwise {
                walk_exprs(o, g);
            }
        }
        ExprKind::Match { value, arms } => {
            walk_exprs(value, g);
            for (_, a) in arms {
                walk_exprs(a, g);
            }
        }
        ExprKind::Closure { body, .. } => walk_exprs(body, g),
        ExprKind::Handle {
            state,
            body,
            ret,
            ops,
            ..
        } => {
            if let Some((_, s)) = state {
                walk_exprs(s, g);
            }
            walk_exprs(body, g);
            walk_exprs(&mut ret.body, g);
            for op in ops {
                walk_exprs(&mut op.body, g);
            }
        }
        k @ (ExprKind::Loop(_)
        | ExprKind::For { .. }
        | ExprKind::Range { .. }
        | ExprKind::Break(_)
        | ExprKind::Return(_)) => {
            for c in k.control_children_mut().into_iter().flatten() {
                walk_exprs(c, g);
            }
        }
    }
    g(e);
}
