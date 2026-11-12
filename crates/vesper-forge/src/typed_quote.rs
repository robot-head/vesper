//! Sealed, bounded Expr quotations. Expr is a grammar category, not a type proof.
use crate::{
    Delim, Expansion, ExpansionId, ForgeError, GeneratedToken, HostToken, HygieneHost,
    InvocationSite, Origin, PackageId, Quotation, QuoteItem, ReportedSpan, ResolvedFragment,
    SessionId, TokenKind,
};
use std::{fmt, ops::Range, sync::Arc};
use vesper_syntax::lexer::{self, Token, TokenKind as K};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TypedLimits {
    pub max_slots: usize,
    pub max_input_tokens: usize,
    pub max_input_bytes: usize,
    pub max_output_tokens: usize,
    pub max_output_bytes: usize,
    pub max_scope_entries: usize,
    pub max_slot_occurrences: usize,
    pub max_depth: usize,
    pub max_metadata_bytes: usize,
    pub max_work: usize,
}
impl Default for TypedLimits {
    fn default() -> Self {
        Self {
            max_slots: 64,
            max_input_tokens: 16384,
            max_input_bytes: 128 * 1024,
            max_output_tokens: 32768,
            max_output_bytes: 256 * 1024,
            max_scope_entries: 262144,
            max_slot_occurrences: 4096,
            max_depth: 48,
            max_metadata_bytes: 1024 * 1024,
            max_work: 16 * 1024 * 1024,
        }
    }
}
impl TypedLimits {
    pub const HARD: Self = Self {
        max_slots: 64,
        max_input_tokens: 65536,
        max_input_bytes: 1024 * 1024,
        max_output_tokens: 65536,
        max_output_bytes: 1024 * 1024,
        max_scope_entries: 262144,
        max_slot_occurrences: 4096,
        max_depth: 128,
        max_metadata_bytes: 16 * 1024 * 1024,
        max_work: 128 * 1024 * 1024,
    };
    pub fn validate(self) -> Result<(), TypedError> {
        let h = Self::HARD;
        for (n, max) in [
            (self.max_slots, h.max_slots),
            (self.max_input_tokens, h.max_input_tokens),
            (self.max_input_bytes, h.max_input_bytes),
            (self.max_output_tokens, h.max_output_tokens),
            (self.max_output_bytes, h.max_output_bytes),
            (self.max_scope_entries, h.max_scope_entries),
            (self.max_slot_occurrences, h.max_slot_occurrences),
            (self.max_depth, h.max_depth),
            (self.max_metadata_bytes, h.max_metadata_bytes),
            (self.max_work, h.max_work),
        ] {
            if n == 0 || n > max {
                return Err(TypedError::InvalidLimits);
            }
        }
        Ok(())
    }
}
#[derive(Debug)]
pub enum TypedError {
    InvalidLimits,
    Budget(&'static str),
    InvalidSpan,
    Lexical,
    Unsupported,
    Category,
    Foreign,
    SourceMismatch,
    Slot,
    Structural,
    Forge(ForgeError),
}
impl fmt::Display for TypedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "typed Expr quotation: {self:?}")
    }
}
impl std::error::Error for TypedError {}
impl From<ForgeError> for TypedError {
    fn from(e: ForgeError) -> Self {
        Self::Forge(e)
    }
}
#[derive(Debug)]
pub struct TypedBudget {
    limits: TypedLimits,
    remaining: usize,
    metadata_used: usize,
    scopes_used: usize,
}
impl TypedBudget {
    pub fn new(limits: TypedLimits) -> Result<Self, TypedError> {
        limits.validate()?;
        Ok(Self {
            limits,
            remaining: limits.max_work,
            metadata_used: 0,
            scopes_used: 0,
        })
    }
    pub fn limits(&self) -> TypedLimits {
        self.limits
    }
    pub fn used(&self) -> usize {
        self.limits.max_work - self.remaining
    }
    pub fn remaining(&self) -> usize {
        self.remaining
    }
    pub fn remaining_work(&self) -> usize {
        self.remaining
    }
    pub fn charge_work(&mut self, n: usize) -> Result<(), TypedError> {
        if n > self.remaining {
            self.remaining = 0;
            return Err(TypedError::Budget("work"));
        }
        self.remaining -= n;
        Ok(())
    }
    #[cfg(feature = "sandbox-transport")]
    pub(crate) fn charge_metadata(&mut self, n: usize) -> Result<(), TypedError> {
        self.metadata(n)
    }
    fn parse(&mut self, n: usize) -> Result<(), TypedError> {
        self.charge_work(
            n.checked_add(1)
                .and_then(|x| x.checked_mul(x))
                .ok_or(TypedError::Budget("work"))?,
        )
    }
    fn metadata(&mut self, n: usize) -> Result<(), TypedError> {
        let total = add(self.metadata_used, n)?;
        if total > self.limits.max_metadata_bytes {
            return Err(TypedError::Budget("metadata"));
        }
        self.metadata_used = total;
        self.charge_work(n)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenProvenance {
    Source {
        span: Range<usize>,
    },
    Generated {
        expansion: ExpansionId,
        reported_span: Option<ReportedSpan>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExprShape {
    Ref {
        tokens: Vec<usize>,
    },
    Literal {
        token: usize,
    },
    Call {
        callee: Box<ExprShape>,
        open: usize,
        close: usize,
        args: Vec<ExprShape>,
    },
    Block {
        open: usize,
        close: usize,
        statements: Vec<ExprStatement>,
        tail: Option<Box<ExprShape>>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExprStatement {
    Let { binder: usize, value: ExprShape },
    Expr(ExprShape),
}
#[derive(Debug)]
pub struct ExprSource {
    owner: Arc<()>,
    session: SessionId,
    package: PackageId,
    source: Arc<str>,
    tokens: Vec<Token>,
}
impl ExprSource {
    pub fn original_source(&self) -> &str {
        &self.source
    }
}
#[derive(Debug)]
pub struct ExprInput {
    owner: Arc<()>,
    session: SessionId,
    source: Arc<str>,
    tokens: Vec<HostToken>,
    provenance: Vec<TokenProvenance>,
    shape: ExprShape,
}
impl ExprInput {
    pub fn tokens(&self) -> &[HostToken] {
        &self.tokens
    }
    pub fn provenance(&self) -> &[TokenProvenance] {
        &self.provenance
    }
    pub fn original_source(&self) -> &str {
        &self.source
    }
}
#[derive(Debug)]
pub struct ExprSlot {
    expansion: ExpansionId,
    index: u32,
}
impl ExprSlot {
    pub fn expansion(&self) -> ExpansionId {
        self.expansion
    }
    pub fn index(&self) -> u32 {
        self.index
    }
}
#[derive(Debug)]
pub struct ExprExpansion {
    alive: Arc<std::sync::atomic::AtomicBool>,
    slots: Vec<ExprSlot>,
    owner: Arc<()>,
    expansion: Expansion,
    inputs: Vec<ExprInput>,
    source: Option<Arc<str>>,
}
impl Drop for ExprExpansion {
    fn drop(&mut self) {
        self.alive
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }
}
impl ExprExpansion {
    /// Validates this sealed expansion against its current, live host arena.
    pub fn validate_host(&self, host: &HygieneHost) -> Result<(), TypedError> {
        owner(&self.owner, host)?;
        if !self.alive.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(ForgeError::ExpansionClosed(self.id()).into());
        }
        host.expansion(self.id())?;
        Ok(())
    }
    pub fn slots(&self) -> &[ExprSlot] {
        &self.slots
    }
    pub fn id(&self) -> ExpansionId {
        self.expansion.id
    }
    pub fn inputs(&self) -> &[ExprInput] {
        &self.inputs
    }
}
#[derive(Clone, Debug)]
pub enum ExprQuoteItem {
    Generated(GeneratedToken),
    InputSlot(u32),
}
#[derive(Debug)]
pub struct ExprQuotation {
    items: Vec<ExprQuoteItem>,
}
impl ExprQuotation {
    pub fn items(&self) -> &[ExprQuoteItem] {
        &self.items
    }
}
#[derive(Debug)]
pub struct ExpandedExpr {
    owner: Arc<()>,
    session: SessionId,
    source: Option<Arc<str>>,
    tokens: Vec<HostToken>,
    provenance: Vec<TokenProvenance>,
    shape: ExprShape,
}
impl ExpandedExpr {
    pub fn tokens(&self) -> &[HostToken] {
        &self.tokens
    }
    pub fn provenance(&self) -> &[TokenProvenance] {
        &self.provenance
    }
    pub fn shape(&self) -> &ExprShape {
        &self.shape
    }
}
#[derive(Debug)]
pub struct ResolvedBody {
    owner: Arc<()>,
    session: SessionId,
    source: Arc<str>,
    fragment: ResolvedFragment,
    shape: ExprShape,
    provenance: Vec<TokenProvenance>,
}
impl ResolvedBody {
    pub fn into_fragment(self) -> ResolvedFragment {
        self.fragment
    }
    pub fn validate_host(&self, host: &HygieneHost) -> Result<(), TypedError> {
        owner(&self.owner, host)
    }
    pub fn session(&self) -> SessionId {
        self.session
    }
    pub fn original_source(&self) -> &str {
        &self.source
    }
    pub fn fragment(&self) -> &ResolvedFragment {
        &self.fragment
    }
    pub fn shape(&self) -> &ExprShape {
        &self.shape
    }
    pub fn provenance(&self) -> &[TokenProvenance] {
        &self.provenance
    }
}

fn add(a: usize, b: usize) -> Result<usize, TypedError> {
    a.checked_add(b).ok_or(TypedError::Budget("overflow"))
}
fn cap(n: usize, max: usize, name: &'static str) -> Result<(), TypedError> {
    if n > max {
        Err(TypedError::Budget(name))
    } else {
        Ok(())
    }
}
fn mapped(k: K) -> Result<TokenKind, TypedError> {
    Ok(match k {
        K::Lower | K::Upper | K::SelfKw | K::Let | K::True | K::False => TokenKind::Ident,
        K::Int => TokenKind::Int,
        K::String => TokenKind::Str,
        K::LParen => TokenKind::Open(Delim::Paren),
        K::RParen => TokenKind::Close(Delim::Paren),
        K::LBrace => TokenKind::Open(Delim::Brace),
        K::RBrace => TokenKind::Close(Delim::Brace),
        K::ColonColon | K::Comma | K::Semi | K::Eq => TokenKind::Punct,
        _ => return Err(TypedError::Unsupported),
    })
}
fn lexical(kind: TokenKind, text: &str) -> Result<(), TypedError> {
    let l = lexer::lex_with_limits(
        text,
        lexer::LexLimits {
            max_source_bytes: TypedLimits::HARD.max_input_bytes,
            max_tokens: 4,
            max_diagnostics: 2,
        },
    )
    .map_err(|_| TypedError::Lexical)?;
    if l.limit.is_some()
        || !l.diagnostics.is_empty()
        || l.tokens.len() != 2
        || l.tokens[0].span != (0..text.len())
        || mapped(l.tokens[0].kind).ok() != Some(kind)
    {
        return Err(TypedError::Lexical);
    }
    if kind == TokenKind::Int {
        let n = text.replace('_', "");
        let parsed = if n.starts_with("0x") || n.starts_with("0X") {
            u64::from_str_radix(&n[2..], 16)
        } else {
            n.parse::<u64>()
        };
        parsed.map_err(|_| TypedError::Lexical)?;
    }
    if kind == TokenKind::Str {
        serde_json::from_str::<String>(text).map_err(|_| TypedError::Lexical)?;
    }
    Ok(())
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum Node {
    Ref(Vec<usize>),
    Lit(usize),
    Slot(u32),
    Call {
        callee: Box<Node>,
        open: usize,
        close: usize,
        args: Vec<Node>,
    },
    Block {
        open: usize,
        close: usize,
        statements: Vec<Stmt>,
        tail: Option<Box<Node>>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum Stmt {
    Let { binder: usize, value: Node },
    Expr(Node),
}
struct View<'a> {
    kind: Option<TokenKind>,
    text: &'a str,
    slot: Option<u32>,
}
struct Parser<'a> {
    tokens: &'a [View<'a>],
    pos: usize,
    depth: usize,
    max: usize,
}
impl Parser<'_> {
    fn text(&self) -> Option<&str> {
        self.tokens.get(self.pos).map(|t| t.text)
    }
    fn eat(&mut self, s: &str) -> bool {
        if self.text() == Some(s) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, s: &str) -> Result<usize, TypedError> {
        let i = self.pos;
        if self.eat(s) {
            Ok(i)
        } else {
            Err(TypedError::Category)
        }
    }
    fn ident(&mut self) -> Result<usize, TypedError> {
        let i = self.pos;
        let t = self.tokens.get(i).ok_or(TypedError::Category)?;
        if t.kind != Some(TokenKind::Ident)
            || matches!(t.text, "let" | "true" | "false" | "handle" | "with")
        {
            return Err(TypedError::Category);
        }
        self.pos += 1;
        Ok(i)
    }
    fn expr(&mut self) -> Result<Node, TypedError> {
        if self.depth >= self.max {
            return Err(TypedError::Budget("depth"));
        }
        self.depth += 1;
        let result = self.inner();
        self.depth -= 1;
        result
    }
    fn inner(&mut self) -> Result<Node, TypedError> {
        let mut node = if let Some(slot) = self.tokens.get(self.pos).and_then(|t| t.slot) {
            self.pos += 1;
            Node::Slot(slot)
        } else {
            match self.tokens.get(self.pos).map(|t| t.kind) {
                Some(Some(TokenKind::Int | TokenKind::Str)) => {
                    let i = self.pos;
                    self.pos += 1;
                    Node::Lit(i)
                }
                Some(Some(TokenKind::Ident)) if matches!(self.text(), Some("true" | "false")) => {
                    let i = self.pos;
                    self.pos += 1;
                    Node::Lit(i)
                }
                Some(Some(TokenKind::Open(Delim::Paren))) => {
                    self.pos += 1;
                    let n = self.expr()?;
                    self.expect(")")?;
                    n
                }
                Some(Some(TokenKind::Open(Delim::Brace))) => return self.block(),
                _ => {
                    let mut path = vec![self.ident()?];
                    while self.eat("::") {
                        path.push(self.ident()?)
                    }
                    Node::Ref(path)
                }
            }
        };
        let mut calls = 0;
        while self.eat("(") {
            calls += 1;
            if self.depth + calls >= self.max {
                return Err(TypedError::Budget("depth"));
            }
            let open = self.pos - 1;
            let mut args = Vec::new();
            if self.text() != Some(")") {
                loop {
                    args.push(self.expr()?);
                    if !self.eat(",") {
                        break;
                    }
                }
            }
            let close = self.expect(")")?;
            node = Node::Call {
                callee: Box::new(node),
                open,
                close,
                args,
            };
        }
        Ok(node)
    }
    fn block(&mut self) -> Result<Node, TypedError> {
        let open = self.expect("{")?;
        let mut statements = Vec::new();
        let mut tail = None;
        while self.text() != Some("}") {
            if statements.len() >= self.max {
                return Err(TypedError::Budget("depth"));
            }
            if self.eat("let") {
                let binder = self.ident()?;
                self.expect("=")?;
                let value = self.expr()?;
                self.expect(";")?;
                statements.push(Stmt::Let { binder, value });
            } else {
                let e = self.expr()?;
                if self.eat(";") {
                    statements.push(Stmt::Expr(e));
                } else {
                    tail = Some(Box::new(e));
                    break;
                }
            }
        }
        let close = self.expect("}")?;
        Ok(Node::Block {
            open,
            close,
            statements,
            tail,
        })
    }
}
fn parse(tokens: &[View<'_>], budget: &mut TypedBudget) -> Result<Node, TypedError> {
    budget.parse(tokens.len())?;
    let mut p = Parser {
        tokens,
        pos: 0,
        depth: 0,
        max: budget.limits.max_depth,
    };
    let n = p.expr()?;
    if p.pos != tokens.len() {
        return Err(TypedError::Category);
    }
    Ok(n)
}
fn shape(node: Node) -> Result<ExprShape, TypedError> {
    Ok(match node {
        Node::Ref(tokens) => ExprShape::Ref { tokens },
        Node::Lit(token) => ExprShape::Literal { token },
        Node::Slot(_) => return Err(TypedError::Slot),
        Node::Call {
            callee,
            open,
            close,
            args,
        } => ExprShape::Call {
            callee: Box::new(shape(*callee)?),
            open,
            close,
            args: args.into_iter().map(shape).collect::<Result<_, _>>()?,
        },
        Node::Block {
            open,
            close,
            statements,
            tail,
        } => ExprShape::Block {
            open,
            close,
            statements: statements
                .into_iter()
                .map(|s| {
                    Ok(match s {
                        Stmt::Let { binder, value } => ExprStatement::Let {
                            binder,
                            value: shape(value)?,
                        },
                        Stmt::Expr(n) => ExprStatement::Expr(shape(n)?),
                    })
                })
                .collect::<Result<_, TypedError>>()?,
            tail: tail.map(|n| shape(*n).map(Box::new)).transpose()?,
        },
    })
}
fn views<'a>(host: &'a HygieneHost, tokens: &[HostToken]) -> Vec<View<'a>> {
    tokens
        .iter()
        .map(|t| View {
            kind: Some(t.kind()),
            text: host.text(t.text()),
            slot: None,
        })
        .collect()
}
fn owner(id: &Arc<()>, host: &HygieneHost) -> Result<(), TypedError> {
    if Arc::ptr_eq(id, &host.typed_owner) {
        Ok(())
    } else {
        Err(TypedError::Foreign)
    }
}
fn source_range(source: &ExprSource, span: Range<usize>) -> Result<Vec<&Token>, TypedError> {
    if span.start > span.end || source.source.get(span.clone()).is_none() {
        return Err(TypedError::InvalidSpan);
    }
    let start = source.tokens.partition_point(|t| t.span.start < span.start);
    let end = source
        .tokens
        .partition_point(|t| t.kind != K::Eof && t.span.end <= span.end);
    if start >= end
        || source.tokens[start].span.start != span.start
        || source.tokens[end - 1].span.end != span.end
    {
        return Err(TypedError::InvalidSpan);
    }
    Ok(source.tokens[start..end]
        .iter()
        .filter(|t| !t.kind.trivia())
        .collect())
}
fn cost(host: &HygieneHost, tokens: &[HostToken]) -> Result<(usize, usize), TypedError> {
    let mut bytes = 0;
    let mut scopes = 0;
    for t in tokens {
        bytes = add(bytes, host.text(t.text()).len())?;
        scopes = add(scopes, t.scopes().len())?;
    }
    Ok((bytes, scopes))
}
fn check_cost(
    n: usize,
    bytes: usize,
    scopes: usize,
    budget: &mut TypedBudget,
) -> Result<(), TypedError> {
    let l = budget.limits;
    cap(n, l.max_output_tokens, "output tokens")?;
    cap(bytes, l.max_output_bytes, "output bytes")?;
    let scope_total = add(budget.scopes_used, scopes)?;
    cap(scope_total, l.max_scope_entries, "scope entries")?;
    budget.scopes_used = scope_total;
    let nodes = n.checked_mul(256).ok_or(TypedError::Budget("metadata"))?;
    let scope_bytes = scopes
        .checked_mul(32)
        .ok_or(TypedError::Budget("metadata"))?;
    budget.metadata(add(nodes, add(bytes, scope_bytes)?)?)
}
impl HygieneHost {
    /// Imports original UTF-8 bytes with the actual Vesper lexer; no fixture lexer or source wrapper.
    pub fn import_expr_source(
        &mut self,
        package: PackageId,
        source: &str,
        budget: &mut TypedBudget,
    ) -> Result<ExprSource, TypedError> {
        cap(source.len(), budget.limits.max_input_bytes, "input bytes")?;
        budget.charge_work(source.len())?;
        let lexer_bound = source
            .len()
            .checked_add(1)
            .ok_or(TypedError::Budget("metadata"))?
            .min(budget.limits.max_input_tokens + 1);
        budget.metadata(add(
            source.len(),
            lexer_bound
                .checked_mul(32)
                .ok_or(TypedError::Budget("metadata"))?,
        )?)?;
        let lexed = lexer::lex_with_limits(
            source,
            lexer::LexLimits {
                max_source_bytes: budget.limits.max_input_bytes,
                max_tokens: budget.limits.max_input_tokens,
                max_diagnostics: 128,
            },
        )
        .map_err(|_| TypedError::Lexical)?;
        if lexed.limit.is_some() {
            return Err(TypedError::Budget("input tokens"));
        }
        if !lexed.diagnostics.is_empty() {
            return Err(TypedError::Lexical);
        }
        self.module_scope(package)?;
        Ok(ExprSource {
            owner: Arc::clone(&self.typed_owner),
            session: self.session(),
            package,
            source: Arc::from(source),
            tokens: lexed.tokens,
        })
    }
    fn source_tokens(
        &mut self,
        source: &ExprSource,
        span: Range<usize>,
        budget: &mut TypedBudget,
    ) -> Result<(Vec<HostToken>, Vec<TokenProvenance>), TypedError> {
        owner(&source.owner, self)?;
        if source.session != self.session() {
            return Err(TypedError::Foreign);
        }
        budget.metadata(
            source
                .tokens
                .len()
                .checked_mul(32)
                .ok_or(TypedError::Budget("metadata"))?,
        )?;
        let selected = source_range(source, span)?;
        cap(
            selected.len(),
            budget.limits.max_input_tokens,
            "input tokens",
        )?;
        let mut bytes = 0;
        for t in &selected {
            let text = &source.source[t.span.clone()];
            mapped(t.kind)?;
            bytes = add(bytes, text.len())?;
        }
        check_cost(selected.len(), bytes, selected.len(), budget)?;
        let scope = self.module_scope(source.package)?;
        let mut tokens = Vec::with_capacity(selected.len());
        let mut provenance = Vec::with_capacity(selected.len());
        for t in selected {
            let text = &source.source[t.span.clone()];
            let kind = mapped(t.kind)?;
            lexical(kind, text)?;
            tokens.push(HostToken {
                kind,
                text: self.intern(text)?,
                scopes: crate::ScopeSet::single(scope),
                origin: Origin::Source {
                    package: source.package,
                },
                claimed_def: None,
                reported_span: None,
            });
            provenance.push(TokenProvenance::Source {
                span: t.span.clone(),
            });
        }
        Ok((tokens, provenance))
    }
    pub fn expr_input(
        &mut self,
        source: &ExprSource,
        span: Range<usize>,
        budget: &mut TypedBudget,
    ) -> Result<ExprInput, TypedError> {
        let (tokens, provenance) = self.source_tokens(source, span, budget)?;
        let node = parse(&views(self, &tokens), budget)?;
        let shape = shape(node)?;
        Ok(ExprInput {
            owner: Arc::clone(&self.typed_owner),
            session: self.session(),
            source: Arc::clone(&source.source),
            tokens,
            provenance,
            shape,
        })
    }
    /// Compile real quotation tokens. Parameter names refer to actual source ranges, not guest identities.
    pub fn expr_quotation(
        &mut self,
        source: &ExprSource,
        macro_def: crate::DefId,
        span: Range<usize>,
        parameters: &[Range<usize>],
        budget: &mut TypedBudget,
    ) -> Result<ExprQuotation, TypedError> {
        owner(&source.owner, self)?;
        budget.metadata(
            source
                .tokens
                .len()
                .checked_mul(32)
                .ok_or(TypedError::Budget("metadata"))?,
        )?;
        cap(parameters.len(), budget.limits.max_slots, "slots")?;
        budget.metadata(
            source
                .tokens
                .len()
                .checked_mul(32)
                .ok_or(TypedError::Budget("metadata"))?,
        )?;
        let selected = source_range(source, span)?;
        budget.metadata(
            selected
                .len()
                .checked_mul(256)
                .ok_or(TypedError::Budget("metadata"))?,
        )?;
        let mut names = Vec::new();
        for p in parameters {
            let n = source
                .source
                .get(p.clone())
                .ok_or(TypedError::InvalidSpan)?;
            lexical(TokenKind::Ident, n)?;
            if names.contains(&n) {
                return Err(TypedError::Slot);
            }
            names.push(n);
        }
        let mut items = Vec::new();
        let mut i = 0;
        while i < selected.len() {
            let t = selected[i];
            let text = &source.source[t.span.clone()];
            if text == "$" {
                i += 1;
                let t = selected.get(i).ok_or(TypedError::Slot)?;
                let name = &source.source[t.span.clone()];
                let slot = names
                    .iter()
                    .position(|n| *n == name)
                    .ok_or(TypedError::Slot)?;
                items.push(ExprQuoteItem::InputSlot(
                    u32::try_from(slot).map_err(|_| TypedError::Slot)?,
                ));
            } else {
                let kind = mapped(t.kind)?;
                lexical(kind, text)?;
                items.push(ExprQuoteItem::Generated(GeneratedToken::new(kind, text)));
            }
            i += 1;
        }
        let tree = template(&items, parameters.len(), budget)?;
        pre_resolve(self, macro_def, &tree, &mut items, budget)?;
        Ok(ExprQuotation { items })
    }
    pub fn begin_expr_expansion(
        &mut self,
        macro_def: crate::DefId,
        site: InvocationSite,
        inputs: Vec<ExprInput>,
        budget: &mut TypedBudget,
    ) -> Result<ExprExpansion, TypedError> {
        cap(inputs.len(), budget.limits.max_slots, "slots")?;
        let mut count = 0;
        let mut bytes = 0;
        let mut scopes = 0;
        let mut source: Option<Arc<str>> = None;
        for input in &inputs {
            owner(&input.owner, self)?;
            if input.session != self.session() {
                return Err(TypedError::Foreign);
            }
            if let Some(s) = &source {
                if !Arc::ptr_eq(s, &input.source) {
                    return Err(TypedError::SourceMismatch);
                }
            } else {
                source = Some(Arc::clone(&input.source));
            }
            count = add(count, input.tokens.len())?;
            let (b, s) = cost(self, &input.tokens)?;
            bytes = add(bytes, b)?;
            scopes = add(scopes, s)?;
        }
        cap(count, budget.limits.max_input_tokens, "input tokens")?;
        cap(bytes, budget.limits.max_input_bytes, "input bytes")?;
        check_cost(count, bytes, scopes, budget)?;
        budget.charge_work(self.typed_retire_count())?;
        self.retire_typed_expansions();
        let args = inputs.iter().map(|i| i.tokens.clone()).collect();
        let expansion = self.begin_expansion(macro_def, site, args)?;
        let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
        self.typed_bind_expansion(expansion.id, Arc::clone(&alive))?;
        let slots = (0..inputs.len())
            .map(|i| ExprSlot {
                expansion: expansion.id,
                index: i as u32,
            })
            .collect();
        Ok(ExprExpansion {
            alive,
            slots,
            owner: Arc::clone(&self.typed_owner),
            expansion,
            inputs,
            source,
        })
    }
    pub fn abandon_expr(&mut self, expansion: ExprExpansion) -> Result<(), TypedError> {
        owner(&expansion.owner, self)?;
        self.abandon(expansion.id())?;
        Ok(())
    }
    pub fn finish_expr(
        &mut self,
        expansion: ExprExpansion,
        items: &[ExprQuoteItem],
        budget: &mut TypedBudget,
    ) -> Result<ExpandedExpr, TypedError> {
        owner(&expansion.owner, self)?;
        let id = expansion.id();
        let result = self.finish_expr_inner(&expansion, items, budget);
        if result.is_err() {
            let _ = self.abandon(id);
        }
        result
    }
    fn finish_expr_inner(
        &mut self,
        expansion: &ExprExpansion,
        items: &[ExprQuoteItem],
        budget: &mut TypedBudget,
    ) -> Result<ExpandedExpr, TypedError> {
        let node = template(items, expansion.inputs.len(), budget)?;
        let mut n = 0;
        let mut bytes = 0;
        let mut scopes = 0;
        let mut occurrences = 0;
        for item in items {
            match item {
                ExprQuoteItem::Generated(g) => {
                    n = add(n, 1)?;
                    bytes = add(bytes, g.text.len())?;
                    scopes = add(scopes, 2)?;
                }
                ExprQuoteItem::InputSlot(i) => {
                    occurrences = add(occurrences, 1)?;
                    cap(
                        occurrences,
                        budget.limits.max_slot_occurrences,
                        "slot occurrences",
                    )?;
                    let input = expansion.inputs.get(*i as usize).ok_or(TypedError::Slot)?;
                    let (b, s) = cost(self, &input.tokens)?;
                    n = add(n, add(input.tokens.len(), 2)?)?;
                    bytes = add(bytes, add(b, 2)?)?;
                    scopes = add(scopes, add(s, 4)?)?;
                }
            }
        }
        check_cost(n, bytes, scopes, budget)?;
        let expected = canonical_template(&node, items, &expansion.inputs, self)?;
        let mut q = Quotation {
            items: Vec::with_capacity(n),
        };
        let mut provenance = Vec::with_capacity(n);
        for item in items {
            match item {
                ExprQuoteItem::Generated(g) => {
                    q.items.push(QuoteItem::Token(g.clone()));
                    provenance.push(TokenProvenance::Generated {
                        expansion: expansion.id(),
                        reported_span: g.reported_span,
                    });
                }
                ExprQuoteItem::InputSlot(i) => {
                    let i = *i as usize;
                    q.items.push(QuoteItem::Token(GeneratedToken::new(
                        TokenKind::Open(Delim::Paren),
                        "(",
                    )));
                    provenance.push(TokenProvenance::Generated {
                        expansion: expansion.id(),
                        reported_span: None,
                    });
                    q.items
                        .push(QuoteItem::Splice(expansion.expansion.splices[i]));
                    provenance.extend(expansion.inputs[i].provenance.iter().cloned());
                    q.items.push(QuoteItem::Token(GeneratedToken::new(
                        TokenKind::Close(Delim::Paren),
                        ")",
                    )));
                    provenance.push(TokenProvenance::Generated {
                        expansion: expansion.id(),
                        reported_span: None,
                    });
                }
            }
        }
        let tokens = self.finish_quotation(expansion.id(), &q)?;
        let actual = parse(&views(self, &tokens), budget)?;
        let shape = shape(actual)?;
        if canonical_shape(&shape, self, &tokens) != expected {
            return Err(TypedError::Structural);
        }
        Ok(ExpandedExpr {
            owner: Arc::clone(&self.typed_owner),
            session: self.session(),
            source: expansion.source.clone(),
            tokens,
            provenance,
            shape,
        })
    }
}
fn template(
    items: &[ExprQuoteItem],
    slots: usize,
    budget: &mut TypedBudget,
) -> Result<Node, TypedError> {
    cap(
        items.len(),
        budget.limits.max_output_tokens,
        "quotation items",
    )?;
    budget.metadata(
        items
            .len()
            .checked_mul(256)
            .ok_or(TypedError::Budget("metadata"))?,
    )?;
    let mut bytes = 0;
    let mut tokens = Vec::with_capacity(items.len());
    for item in items {
        match item {
            ExprQuoteItem::Generated(g) => {
                bytes = add(bytes, g.text.len())?;
                cap(bytes, budget.limits.max_output_bytes, "output bytes")?;
                budget.charge_work(
                    g.text
                        .len()
                        .checked_mul(4)
                        .ok_or(TypedError::Budget("work"))?,
                )?;
                lexical(g.kind, &g.text)?;
                tokens.push(View {
                    kind: Some(g.kind),
                    text: &g.text,
                    slot: None,
                });
            }
            ExprQuoteItem::InputSlot(i) => {
                if *i as usize >= slots {
                    return Err(TypedError::Slot);
                }
                tokens.push(View {
                    kind: None,
                    text: "",
                    slot: Some(*i),
                });
            }
        }
    }
    parse(&tokens, budget)
}
#[derive(Debug, PartialEq, Eq)]
enum Sem {
    Tag(u8),
    Text(String),
}
fn emit_shape(n: &ExprShape, host: &HygieneHost, tokens: &[HostToken], out: &mut Vec<Sem>) {
    match n {
        ExprShape::Ref { tokens: path } => {
            out.push(Sem::Tag(1));
            for i in path {
                out.push(Sem::Text(host.text(tokens[*i].text()).to_owned()));
            }
            out.push(Sem::Tag(2));
        }
        ExprShape::Literal { token } => {
            out.push(Sem::Tag(3));
            out.push(Sem::Text(host.text(tokens[*token].text()).to_owned()));
        }
        ExprShape::Call { callee, args, .. } => {
            out.push(Sem::Tag(4));
            emit_shape(callee, host, tokens, out);
            for a in args {
                out.push(Sem::Tag(5));
                emit_shape(a, host, tokens, out)
            }
            out.push(Sem::Tag(6));
        }
        ExprShape::Block {
            statements, tail, ..
        } => {
            out.push(Sem::Tag(7));
            for s in statements {
                match s {
                    ExprStatement::Let { binder, value } => {
                        out.push(Sem::Tag(8));
                        out.push(Sem::Text(host.text(tokens[*binder].text()).to_owned()));
                        emit_shape(value, host, tokens, out)
                    }
                    ExprStatement::Expr(e) => {
                        out.push(Sem::Tag(9));
                        emit_shape(e, host, tokens, out)
                    }
                }
            }
            if let Some(t) = tail {
                out.push(Sem::Tag(10));
                emit_shape(t, host, tokens, out)
            } else {
                out.push(Sem::Tag(11))
            }
            out.push(Sem::Tag(12));
        }
    }
}
fn canonical_shape(n: &ExprShape, host: &HygieneHost, tokens: &[HostToken]) -> Vec<Sem> {
    let mut out = Vec::new();
    emit_shape(n, host, tokens, &mut out);
    out
}
fn canonical_template(
    n: &Node,
    items: &[ExprQuoteItem],
    inputs: &[ExprInput],
    host: &HygieneHost,
) -> Result<Vec<Sem>, TypedError> {
    fn text(items: &[ExprQuoteItem], i: usize) -> Result<String, TypedError> {
        match &items[i] {
            ExprQuoteItem::Generated(g) => Ok(g.text.clone()),
            _ => Err(TypedError::Slot),
        }
    }
    fn emit(
        n: &Node,
        items: &[ExprQuoteItem],
        inputs: &[ExprInput],
        host: &HygieneHost,
        out: &mut Vec<Sem>,
    ) -> Result<(), TypedError> {
        match n {
            Node::Ref(path) => {
                out.push(Sem::Tag(1));
                for i in path {
                    out.push(Sem::Text(text(items, *i)?))
                }
                out.push(Sem::Tag(2));
            }
            Node::Lit(i) => {
                out.push(Sem::Tag(3));
                out.push(Sem::Text(text(items, *i)?));
            }
            Node::Slot(i) => {
                let input = inputs.get(*i as usize).ok_or(TypedError::Slot)?;
                emit_shape(&input.shape, host, &input.tokens, out);
            }
            Node::Call { callee, args, .. } => {
                out.push(Sem::Tag(4));
                emit(callee, items, inputs, host, out)?;
                for a in args {
                    out.push(Sem::Tag(5));
                    emit(a, items, inputs, host, out)?
                }
                out.push(Sem::Tag(6));
            }
            Node::Block {
                statements, tail, ..
            } => {
                out.push(Sem::Tag(7));
                for s in statements {
                    match s {
                        Stmt::Let { binder, value } => {
                            out.push(Sem::Tag(8));
                            out.push(Sem::Text(text(items, *binder)?));
                            emit(value, items, inputs, host, out)?
                        }
                        Stmt::Expr(e) => {
                            out.push(Sem::Tag(9));
                            emit(e, items, inputs, host, out)?
                        }
                    }
                }
                if let Some(t) = tail {
                    out.push(Sem::Tag(10));
                    emit(t, items, inputs, host, out)?
                } else {
                    out.push(Sem::Tag(11));
                }
                out.push(Sem::Tag(12));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    emit(n, items, inputs, host, &mut out)?;
    Ok(out)
}
fn to_term(
    n: &ExprShape,
    host: &mut HygieneHost,
    all_tokens: &[HostToken],
) -> Result<crate::syntax::Term, TypedError> {
    use crate::syntax::{Path, Term};
    Ok(match n {
        ExprShape::Ref { tokens } => {
            let name = tokens
                .iter()
                .map(|i| host.text(all_tokens[*i].text()))
                .collect::<Vec<_>>()
                .join("::");
            Term::Ref(Path {
                tokens: tokens.clone(),
                symbol: host.intern(&name)?,
            })
        }
        ExprShape::Literal { token } => Term::Lit(*token),
        ExprShape::Call {
            callee,
            open,
            close,
            args,
        } => Term::Call {
            callee: Box::new(to_term(callee, host, all_tokens)?),
            open: *open,
            close: *close,
            args: args
                .iter()
                .map(|a| to_term(a, host, all_tokens))
                .collect::<Result<_, _>>()?,
        },
        ExprShape::Block {
            statements, tail, ..
        } => {
            let mut body = if let Some(t) = tail {
                to_term(t, host, all_tokens)?
            } else {
                Term::Block(vec![])
            };
            for statement in statements.iter().rev() {
                body = match statement {
                    ExprStatement::Let { binder, value } => Term::Let {
                        binder: *binder,
                        value: Box::new(to_term(value, host, all_tokens)?),
                        body: Box::new(body),
                    },
                    ExprStatement::Expr(e) => {
                        Term::Block(vec![to_term(e, host, all_tokens)?, body])
                    }
                };
            }
            Term::Block(vec![body])
        }
    })
}
impl HygieneHost {
    /// Assemble only actual body ranges and sealed expression outputs, then resolve the whole body.
    pub fn resolve_expr_body(
        &mut self,
        source: &ExprSource,
        body: Range<usize>,
        mut replacements: Vec<(Range<usize>, ExpandedExpr)>,
        budget: &mut TypedBudget,
    ) -> Result<ResolvedBody, TypedError> {
        owner(&source.owner, self)?;
        budget.metadata(
            source
                .tokens
                .len()
                .checked_mul(256)
                .ok_or(TypedError::Budget("metadata"))?,
        )?;
        cap(
            replacements.len(),
            budget.limits.max_slot_occurrences,
            "replacements",
        )?;
        let mut last = body.start;
        let mut n = 0;
        let mut bytes = 0;
        let mut scopes = 0;
        for (span, e) in &replacements {
            owner(&e.owner, self)?;
            if e.session != self.session()
                || span.start < last
                || span.end < span.start
                || span.end > body.end
            {
                return Err(TypedError::InvalidSpan);
            }
            if let Some(s) = &e.source
                && !Arc::ptr_eq(s, &source.source)
            {
                return Err(TypedError::SourceMismatch);
            }
            source_range(source, span.clone())?;
            let (b, s) = cost(self, &e.tokens)?;
            n = add(n, e.tokens.len())?;
            bytes = add(bytes, b)?;
            scopes = add(scopes, s)?;
            last = span.end;
        }
        // Preflight all untouched source tokens before constructing any output vectors.
        source_range(source, body.clone())?;
        let mut untouched = Vec::new();
        let mut cursor = body.start;
        for (span, _) in &replacements {
            if cursor < span.start {
                untouched.extend(source_range(source, cursor..span.start)?);
            }
            cursor = span.end;
        }
        if cursor < body.end {
            untouched.extend(source_range(source, cursor..body.end)?);
        }
        for t in &untouched {
            mapped(t.kind)?;
            n = add(n, 1)?;
            bytes = add(bytes, t.span.len())?;
            scopes = add(scopes, 1)?;
        }
        check_cost(n, bytes, scopes, budget)?;
        let mut tokens = Vec::with_capacity(n);
        let mut provenance = Vec::with_capacity(n);
        let scope = self.module_scope(source.package)?;
        cursor = body.start;
        for (span, expanded) in replacements.drain(..) {
            append_source(
                self,
                source,
                cursor..span.start,
                scope,
                &mut tokens,
                &mut provenance,
            )?;
            tokens.extend(expanded.tokens);
            provenance.extend(expanded.provenance);
            cursor = span.end;
        }
        append_source(
            self,
            source,
            cursor..body.end,
            scope,
            &mut tokens,
            &mut provenance,
        )?;
        let shape = shape(parse(&views(self, &tokens), budget)?)?;
        if !matches!(shape, ExprShape::Block { .. }) {
            return Err(TypedError::Category);
        }
        budget.parse(tokens.len())?;
        cap(term_height(&shape)?, budget.limits.max_depth, "depth")?;
        resolution_work(self, &shape, &tokens, budget)?;
        let term = to_term(&shape, self, &tokens)?;
        let scope_bound = tokens
            .len()
            .checked_mul(budget.limits.max_depth + 2)
            .ok_or(TypedError::Budget("scope entries"))?;
        cap(
            add(budget.scopes_used, scope_bound)?,
            budget.limits.max_scope_entries,
            "scope entries",
        )?;
        budget.scopes_used = add(budget.scopes_used, scope_bound)?;
        let fragment = self.resolve_preparsed(tokens, term)?;
        if fragment
            .operations
            .iter()
            .any(|o| matches!(o.kind, crate::OperationKind::IndirectCall))
        {
            return Err(TypedError::Unsupported);
        }
        Ok(ResolvedBody {
            owner: Arc::clone(&self.typed_owner),
            session: self.session(),
            source: Arc::clone(&source.source),
            fragment,
            shape,
            provenance,
        })
    }
}
fn append_source(
    host: &mut HygieneHost,
    source: &ExprSource,
    span: Range<usize>,
    scope: crate::ScopeId,
    tokens: &mut Vec<HostToken>,
    provenance: &mut Vec<TokenProvenance>,
) -> Result<(), TypedError> {
    if span.is_empty() {
        return Ok(());
    }
    for t in source_range(source, span)? {
        let text = &source.source[t.span.clone()];
        let kind = mapped(t.kind)?;
        lexical(kind, text)?;
        tokens.push(HostToken {
            kind,
            text: host.intern(text)?,
            scopes: crate::ScopeSet::single(scope),
            origin: Origin::Source {
                package: source.package,
            },
            claimed_def: None,
            reported_span: None,
        });
        provenance.push(TokenProvenance::Source {
            span: t.span.clone(),
        });
    }
    Ok(())
}
fn pre_resolve(
    host: &mut HygieneHost,
    macro_def: crate::DefId,
    tree: &Node,
    items: &mut [ExprQuoteItem],
    budget: &mut TypedBudget,
) -> Result<(), TypedError> {
    let definition = host
        .registry()
        .definition(macro_def)
        .ok_or(ForgeError::UnknownDefinition(macro_def))?;
    if definition.kind != crate::DefKind::Macro {
        return Err(ForgeError::NotAMacro(macro_def).into());
    }
    let package = macro_def.package;
    let module = host.module_scope(package)?;
    fn text(items: &[ExprQuoteItem], i: usize) -> Result<&str, TypedError> {
        match &items[i] {
            ExprQuoteItem::Generated(g) => Ok(&g.text),
            _ => Err(TypedError::Slot),
        }
    }
    fn walk(
        host: &HygieneHost,
        module: crate::ScopeId,
        package: PackageId,
        n: &Node,
        items: &mut [ExprQuoteItem],
        locals: &mut Vec<String>,
        budget: &mut TypedBudget,
    ) -> Result<(), TypedError> {
        budget.charge_work(1)?;
        match n {
            Node::Ref(path) => {
                let name = path
                    .iter()
                    .map(|i| text(items, *i))
                    .collect::<Result<Vec<_>, _>>()?
                    .join("::");
                if path.len() == 1 && locals.contains(&name) {
                    return Ok(());
                }
                budget.charge_work(host.globals.len())?;
                let mut found = None;
                for g in &host.globals {
                    if g.scopes.iter().any(|s| s == module) && host.text(g.symbol) == name {
                        if found.is_some_and(|d| d != g.def) {
                            return Err(TypedError::Category);
                        }
                        host.registry().validate_claim(package, &name, g.def)?;
                        found = Some(g.def);
                    }
                }
                let def = found.ok_or(TypedError::Category)?;
                if let ExprQuoteItem::Generated(g) = &mut items[path[0]] {
                    g.claimed_def = Some(def);
                }
            }
            Node::Call { callee, args, .. } => {
                walk(host, module, package, callee, items, locals, budget)?;
                for a in args {
                    walk(host, module, package, a, items, locals, budget)?
                }
            }
            Node::Block {
                statements, tail, ..
            } => {
                let old = locals.len();
                for s in statements {
                    match s {
                        Stmt::Let { binder, value } => {
                            walk(host, module, package, value, items, locals, budget)?;
                            locals.push(text(items, *binder)?.to_owned());
                        }
                        Stmt::Expr(n) => walk(host, module, package, n, items, locals, budget)?,
                    }
                }
                if let Some(t) = tail {
                    walk(host, module, package, t, items, locals, budget)?
                }
                locals.truncate(old);
            }
            Node::Lit(_) | Node::Slot(_) => {}
        }
        Ok(())
    }
    walk(host, module, package, tree, items, &mut Vec::new(), budget)
}
fn term_height(n: &ExprShape) -> Result<usize, TypedError> {
    Ok(match n {
        ExprShape::Ref { .. } | ExprShape::Literal { .. } => 1,
        ExprShape::Call { callee, args, .. } => {
            let mut h = term_height(callee)?;
            for a in args {
                h = h.max(term_height(a)?);
            }
            add(h, 1)?
        }
        ExprShape::Block {
            statements, tail, ..
        } => {
            let mut h = if let Some(t) = tail {
                term_height(t)?
            } else {
                1
            };
            for s in statements.iter().rev() {
                let value = match s {
                    ExprStatement::Let { value, .. } => value,
                    ExprStatement::Expr(e) => e,
                };
                h = add(h.max(term_height(value)?), 1)?;
            }
            add(h, 1)?
        }
    })
}
fn resolution_work(
    host: &HygieneHost,
    shape: &ExprShape,
    tokens: &[HostToken],
    budget: &mut TypedBudget,
) -> Result<(), TypedError> {
    let globals = host.globals.len();
    cap(globals, budget.limits.max_input_tokens, "global bindings")?;
    budget.charge_work(
        globals
            .checked_mul(globals)
            .ok_or(TypedError::Budget("work"))?,
    )?;
    let global_duplicates = host
        .globals
        .iter()
        .map(|g| host.globals.iter().filter(|h| h.symbol == g.symbol).count())
        .max()
        .unwrap_or(0);
    budget.metadata(
        tokens
            .len()
            .checked_mul(std::mem::size_of::<usize>())
            .ok_or(TypedError::Budget("metadata"))?,
    )?;
    fn binders(n: &ExprShape, out: &mut Vec<usize>) {
        match n {
            ExprShape::Block {
                statements, tail, ..
            } => {
                for s in statements {
                    match s {
                        ExprStatement::Let { binder, value } => {
                            out.push(*binder);
                            binders(value, out)
                        }
                        ExprStatement::Expr(e) => binders(e, out),
                    }
                }
                if let Some(t) = tail {
                    binders(t, out)
                }
            }
            ExprShape::Call { callee, args, .. } => {
                binders(callee, out);
                for a in args {
                    binders(a, out)
                }
            }
            _ => {}
        }
    }
    let mut locals = Vec::new();
    binders(shape, &mut locals);
    let local_duplicates = locals
        .iter()
        .map(|i| {
            locals
                .iter()
                .filter(|j| tokens[**j].text() == tokens[*i].text())
                .count()
        })
        .max()
        .unwrap_or(0);
    let candidates = add(global_duplicates, local_duplicates)?.max(1);
    let cost = tokens
        .len()
        .checked_mul(candidates)
        .and_then(|n| n.checked_mul(candidates))
        .and_then(|n| n.checked_mul(budget.limits.max_depth + 2))
        .ok_or(TypedError::Budget("work"))?;
    budget.charge_work(cost)
}
