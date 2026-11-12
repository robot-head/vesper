//! AST-free, source-bound recovery syntax. Completeness describes syntax only.
//! Construction is sealed inside this crate; no checked program, World, type,
//! liveness, edit permission, or proof authority is stored or exposed.
use crate::{
    ast::{Diagnostic, Span},
    lexer::TokenKind,
    parser::{ParseLimits, SyntaxNode},
};
use rowan::{GreenNode, NodeOrToken, WalkEvent};
use std::{collections::BTreeSet, fmt, mem::size_of, sync::Arc};

/// Agreed parser editor BLOCK kind; independent of parser entry-point wiring.
#[allow(
    dead_code,
    reason = "shared with separately integrated parser recovery code"
)]
pub(crate) const BLOCK_KIND: rowan::SyntaxKind = rowan::SyntaxKind(205);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub parse: ParseLimits,
    pub max_sites: usize,
    pub max_declarations: usize,
    pub max_metadata_bytes: usize,
    pub max_work: usize,
    pub max_identifier_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            parse: ParseLimits {
                max_source_bytes: 128 * 1024,
                max_tokens: 16_384,
                max_diagnostics: 64,
                max_depth: 48,
            },
            max_sites: 256,
            max_declarations: 256,
            max_metadata_bytes: 1024 * 1024,
            max_work: 16 * 1024 * 1024,
            max_identifier_bytes: 256,
        }
    }
}
impl Limits {
    pub const HARD: Self = Self {
        parse: ParseLimits {
            max_source_bytes: 1024 * 1024,
            max_tokens: 65_536,
            max_diagnostics: 128,
            max_depth: 128,
        },
        max_sites: 4096,
        max_declarations: 4096,
        max_metadata_bytes: 16 * 1024 * 1024,
        max_work: 128 * 1024 * 1024,
        max_identifier_bytes: 1024,
    };
    pub fn validate(self) -> Result<(), Error> {
        let a = [
            self.parse.max_source_bytes,
            self.parse.max_tokens,
            self.parse.max_diagnostics,
            self.parse.max_depth,
            self.max_sites,
            self.max_declarations,
            self.max_metadata_bytes,
            self.max_work,
            self.max_identifier_bytes,
        ];
        let h = Self::HARD;
        let b = [
            h.parse.max_source_bytes,
            h.parse.max_tokens,
            h.parse.max_diagnostics,
            h.parse.max_depth,
            h.max_sites,
            h.max_declarations,
            h.max_metadata_bytes,
            h.max_work,
            h.max_identifier_bytes,
        ];
        if self.parse.max_depth == 0 || a.into_iter().zip(b).any(|(a, b)| a > b) {
            return Err(Error::InvalidLimits);
        }
        Ok(())
    }
    /// Validate before a parser clones source or allocates its token/CST state.
    /// This preflight checks config/source/work; inventory checks happen in the
    /// internal assembler after actual parsing, before snapshot publication.
    pub fn preflight(self, source: &str) -> Result<(), Error> {
        self.validate()?;
        bound(source.len(), self.parse.max_source_bytes, Resource::Source)?;
        bound(source.len(), self.max_work, Resource::Work)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resource {
    Source,
    Tokens,
    Diagnostics,
    Depth,
    Sites,
    Declarations,
    Metadata,
    Work,
    Identifier,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidLimits,
    Budget(Resource),
    InvalidSpan,
    CstMismatch,
    InvalidInventory,
    Allocation,
    Unsupported,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidLimits => "invalid recovery limits",
            Self::Budget(_) => "recovery resource budget exceeded",
            Self::InvalidSpan => "invalid recovery source range",
            Self::CstMismatch => "recovery CST does not match source",
            Self::InvalidInventory => "invalid recovery syntax inventory",
            Self::Allocation => "recovery allocation failed",
            Self::Unsupported => "unsupported recovery syntax context",
        })
    }
}
impl std::error::Error for Error {}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeclarationKind {
    Function,
    Struct,
    Enum,
    Effect,
    Trait,
    Impl,
    Model,
    Use,
    Other,
}
impl DeclarationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Effect => "effect",
            Self::Trait => "trait",
            Self::Impl => "impl",
            Self::Model => "model",
            Self::Use => "use",
            Self::Other => "other",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeclarationStatus {
    Complete,
    Recovered,
    Opaque,
}
impl DeclarationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Recovered => "recovered",
            Self::Opaque => "opaque",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SiteKind {
    MissingMember,
    MissingDelimiter,
    Unexpected,
    OpaqueLexical,
    OpaqueMacro,
}
impl SiteKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingMember => "missing_member",
            Self::MissingDelimiter => "missing_delimiter",
            Self::Unexpected => "unexpected",
            Self::OpaqueLexical => "opaque_lexical",
            Self::OpaqueMacro => "opaque_macro",
        }
    }
}
/// Actual receiver token ranges only; this does not resolve a place or binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receiver {
    pub root: Span,
    pub projections: Vec<Span>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoverySite {
    pub span: Span,
    pub kind: SiteKind,
    pub owner: Option<usize>,
    pub receiver: Option<Receiver>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declaration {
    pub kind: DeclarationKind,
    pub name_span: Option<Span>,
    pub span: Span,
    pub header_span: Span,
    pub body_span: Option<Span>,
    pub body_green_span: Option<Span>,
    pub status: DeclarationStatus,
    // Only the trusted parser may mark a complete top-level nongeneric body as
    // eligible. A public descriptor copy cannot construct a sealed snapshot.
    pub(crate) body_context: Option<BlockContext>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BlockContext {
    pub(crate) row_names: BTreeSet<String>,
    pub(crate) bound_values: BTreeSet<String>,
    pub(crate) no_struct: bool,
}
/// Immutable original bytes and actual lossless syntax, with no retained AST.
///
/// ```compile_fail
/// use vesper_syntax::recovery::RecoverySyntax;
/// let forged = RecoverySyntax { };
/// ```
#[derive(Debug)]
pub struct RecoverySyntax {
    source: Arc<str>,
    source_hash: [u8; 32],
    green: GreenNode,
    diagnostics: Vec<Diagnostic>,
    declarations: Vec<Declaration>,
    sites: Vec<RecoverySite>,
    #[allow(dead_code, reason = "read by separately integrated incremental code")]
    limits: Limits,
}
impl RecoverySyntax {
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn source_arc(&self) -> &Arc<str> {
        &self.source
    }
    pub fn source_hash(&self) -> [u8; 32] {
        self.source_hash
    }
    pub fn syntax(&self) -> SyntaxNode {
        SyntaxNode::new_root(self.green().clone())
    }
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
    pub fn declarations(&self) -> &[Declaration] {
        &self.declarations
    }
    pub fn sites(&self) -> &[RecoverySite] {
        &self.sites
    }
    pub(crate) fn green(&self) -> &GreenNode {
        &self.green
    }
    #[allow(
        dead_code,
        reason = "consumed by separately integrated incremental code"
    )]
    pub(crate) fn limits(&self) -> Limits {
        self.limits
    }
}
fn bound(n: usize, cap: usize, r: Resource) -> Result<(), Error> {
    if n > cap {
        Err(Error::Budget(r))
    } else {
        Ok(())
    }
}
// This sealed constructor has no production caller until the independent
// parser/incremental lanes are integrated. Keep its model-only build testable.
#[allow(
    dead_code,
    reason = "sealed validation is consumed by separately integrated parser/incremental lanes"
)]
mod assembly {
    use super::*;
    fn add(a: usize, b: usize, r: Resource) -> Result<usize, Error> {
        a.checked_add(b).ok_or(Error::Budget(r))
    }
    fn mul(a: usize, b: usize, r: Resource) -> Result<usize, Error> {
        a.checked_mul(b).ok_or(Error::Budget(r))
    }
    struct Budget {
        work: usize,
        metadata: usize,
        limits: Limits,
    }
    impl Budget {
        fn work(&mut self, n: usize) -> Result<(), Error> {
            self.work = add(self.work, n, Resource::Work)?;
            bound(self.work, self.limits.max_work, Resource::Work)
        }
        fn metadata(&mut self, n: usize) -> Result<(), Error> {
            self.metadata = add(self.metadata, n, Resource::Metadata)?;
            bound(
                self.metadata,
                self.limits.max_metadata_bytes,
                Resource::Metadata,
            )
        }
        fn text(&mut self, s: &str) -> Result<(), Error> {
            self.work(s.len())?;
            self.metadata(s.len())
        }
        fn span(&mut self, source: &str, span: &Span) -> Result<(), Error> {
            self.work(2)?;
            if span.start > span.end || span.end > source.len() {
                return Err(Error::InvalidSpan);
            }
            for at in [span.start, span.end] {
                if !source.is_char_boundary(at)
                    || (at > 0
                        && source.as_bytes().get(at - 1) == Some(&b'\r')
                        && source.as_bytes().get(at) == Some(&b'\n'))
                {
                    return Err(Error::InvalidSpan);
                }
            }
            Ok(())
        }
    }
    fn contains(outer: &Span, inner: &Span) -> bool {
        outer.start <= inner.start && inner.end <= outer.end
    }
    fn token_range(token: &crate::parser::SyntaxNode) -> Span {
        let r = token.text_range();
        usize::from(r.start())..usize::from(r.end())
    }
    fn actual_token(
        syntax: &SyntaxNode,
        span: &Span,
        allowed: &[TokenKind],
        budget: &mut Budget,
    ) -> Result<(), Error> {
        budget.work(mul(budget.limits.parse.max_depth, 2, Resource::Work)?)?;
        let at = rowan::TextSize::try_from(span.start).map_err(|_| Error::InvalidSpan)?;
        if syntax.token_at_offset(at).into_iter().any(|token| {
            let r = token.text_range();
            usize::from(r.start()) == span.start
                && usize::from(r.end()) == span.end
                && allowed.iter().any(|k| token.kind().0 == *k as u16)
        }) {
            Ok(())
        } else {
            Err(Error::InvalidInventory)
        }
    }
    fn identifier(text: &str, cap: usize) -> Result<(), Error> {
        bound(text.len(), cap, Resource::Identifier)?;
        let mut bytes = text.bytes();
        if !bytes
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
            || !bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(Error::InvalidInventory);
        }
        Ok(())
    }
    /// Trusted parser/incremental entry point. Supplied vectors/GreenNode already
    /// belong to those bounded producers; this function validates without copying
    /// source text or metadata and publishes only the complete checked aggregate.
    pub(crate) fn assemble(
        source: Arc<str>,
        green: GreenNode,
        diagnostics: Vec<Diagnostic>,
        declarations: Vec<Declaration>,
        sites: Vec<RecoverySite>,
        limits: Limits,
    ) -> Result<RecoverySyntax, Error> {
        limits.preflight(&source)?;
        bound(
            diagnostics.len(),
            limits.parse.max_diagnostics,
            Resource::Diagnostics,
        )?;
        bound(
            declarations.len(),
            limits.max_declarations,
            Resource::Declarations,
        )?;
        bound(sites.len(), limits.max_sites, Resource::Sites)?;
        let mut budget = Budget {
            work: 0,
            metadata: 0,
            limits,
        };
        budget.work(source.len())?; // eventual hash traversal, before hashing
        budget.metadata(mul(
            diagnostics.len(),
            size_of::<Diagnostic>(),
            Resource::Metadata,
        )?)?;
        budget.metadata(mul(
            declarations.len(),
            size_of::<Declaration>(),
            Resource::Metadata,
        )?)?;
        budget.metadata(mul(
            sites.len(),
            size_of::<RecoverySite>(),
            Resource::Metadata,
        )?)?;
        if green.kind() != crate::parser::MODULE {
            return Err(Error::CstMismatch);
        }
        let syntax = SyntaxNode::new_root(green.clone());
        let (mut depth, mut cursor, mut tokens) = (0usize, 0usize, 0usize);
        for event in syntax.preorder_with_tokens() {
            budget.work(1)?;
            match event {
                WalkEvent::Enter(NodeOrToken::Node(_)) => {
                    depth = add(depth, 1, Resource::Depth)?;
                    bound(depth, limits.parse.max_depth, Resource::Depth)?;
                }
                WalkEvent::Leave(NodeOrToken::Node(_)) => {
                    depth = depth.checked_sub(1).ok_or(Error::CstMismatch)?;
                }
                WalkEvent::Enter(NodeOrToken::Token(token)) => {
                    tokens = add(tokens, 1, Resource::Tokens)?;
                    bound(tokens, limits.parse.max_tokens, Resource::Tokens)?;
                    let text = token.text();
                    budget.work(text.len())?;
                    let end = add(cursor, text.len(), Resource::Source)?;
                    if source.get(cursor..end) != Some(text) {
                        return Err(Error::CstMismatch);
                    }
                    if matches!(token.kind().0,k if k==TokenKind::Lower as u16||k==TokenKind::Upper as u16)
                    {
                        identifier(text, limits.max_identifier_bytes)?;
                    }
                    cursor = end;
                }
                WalkEvent::Leave(NodeOrToken::Token(_)) => {}
            }
        }
        if cursor != source.len() || depth != 0 {
            return Err(Error::CstMismatch);
        }
        for diagnostic in &diagnostics {
            budget.span(&source, &diagnostic.span)?;
            budget.text(&diagnostic.code)?;
            budget.text(&diagnostic.message)?;
            if let Some(help) = &diagnostic.help {
                budget.text(help)?;
            }
            budget.metadata(mul(
                diagnostic.related.len(),
                size_of::<crate::ast::DiagnosticLabel>(),
                Resource::Metadata,
            )?)?;
            // This count is bounded by metadata before iterating, including zero-byte labels.
            budget.work(diagnostic.related.len())?;
            for label in &diagnostic.related {
                budget.span(&source, &label.span)?;
                budget.text(&label.message)?;
            }
        }
        let mut previous_end = 0;
        for declaration in &declarations {
            budget.span(&source, &declaration.span)?;
            budget.span(&source, &declaration.header_span)?;
            if declaration.span.start < previous_end
                || !contains(&declaration.span, &declaration.header_span)
            {
                return Err(Error::InvalidInventory);
            }
            previous_end = declaration.span.end;
            if let Some(name) = &declaration.name_span {
                budget.span(&source, name)?;
                if !contains(&declaration.header_span, name) {
                    return Err(Error::InvalidInventory);
                }
                actual_token(
                    &syntax,
                    name,
                    &[TokenKind::Lower, TokenKind::Upper, TokenKind::SelfKw],
                    &mut budget,
                )?;
                identifier(
                    source.get(name.clone()).ok_or(Error::InvalidSpan)?,
                    limits.max_identifier_bytes,
                )?;
            }
            if let Some(body) = &declaration.body_span {
                budget.span(&source, body)?;
                if !contains(&declaration.span, body) || declaration.header_span.end > body.start {
                    return Err(Error::InvalidInventory);
                }
            }
            if let Some(body) = &declaration.body_green_span {
                budget.span(&source, body)?;
                if declaration.body_span.as_ref() != Some(body) {
                    return Err(Error::InvalidInventory);
                }
                let mut found = false;
                for node in syntax.descendants().skip(1) {
                    budget.work(1)?;
                    if token_range(&node) == *body
                        && (declaration.body_context.is_none() || node.kind() == BLOCK_KIND)
                    {
                        found = true;
                        break;
                    }
                }
                if !found {
                    return Err(Error::InvalidInventory);
                }
            }
            if let Some(context) = &declaration.body_context {
                if declaration.kind != DeclarationKind::Function
                    || declaration.status != DeclarationStatus::Complete
                    || declaration.name_span.is_none()
                    || declaration.body_green_span.is_none()
                {
                    return Err(Error::InvalidInventory);
                }
                for names in [&context.row_names, &context.bound_values] {
                    budget.metadata(mul(names.len(), size_of::<String>(), Resource::Metadata)?)?;
                    budget.work(names.len())?;
                    for name in names {
                        budget.text(name)?;
                        identifier(name, limits.max_identifier_bytes)?;
                    }
                }
            }
        }
        for site in &sites {
            budget.span(&source, &site.span)?;
            if site.owner.is_some_and(|at| at >= declarations.len()) {
                return Err(Error::InvalidInventory);
            }
            if matches!(
                site.kind,
                SiteKind::MissingMember | SiteKind::MissingDelimiter
            ) && site.span.start != site.span.end
            {
                return Err(Error::InvalidInventory);
            }
            if let Some(receiver) = &site.receiver {
                budget.span(&source, &receiver.root)?;
                actual_token(
                    &syntax,
                    &receiver.root,
                    &[TokenKind::Lower, TokenKind::SelfKw],
                    &mut budget,
                )?;
                budget.metadata(mul(
                    receiver.projections.len(),
                    size_of::<Span>(),
                    Resource::Metadata,
                )?)?;
                budget.work(receiver.projections.len())?;
                let mut previous = receiver.root.end;
                for projection in &receiver.projections {
                    budget.span(&source, projection)?;
                    if projection.start < previous {
                        return Err(Error::InvalidInventory);
                    }
                    actual_token(
                        &syntax,
                        projection,
                        &[TokenKind::Lower, TokenKind::Int],
                        &mut budget,
                    )?;
                    previous = projection.end;
                }
            }
        }
        let source_hash = *blake3::hash(source.as_bytes()).as_bytes();
        Ok(RecoverySyntax {
            source,
            source_hash,
            green,
            diagnostics,
            declarations,
            sites,
            limits,
        })
    }
}
#[allow(
    unused_imports,
    reason = "sealed constructor is consumed by separately integrated parser/incremental lanes"
)]
pub(crate) use assembly::assemble;

#[cfg(test)]
mod model_tests;
