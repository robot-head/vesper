use crate::ast::{Diagnostic, Span};
use logos::Logos;

#[derive(Logos, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum TokenKind {
    #[regex(r"[ \t\r\n\x0C]+")]
    Whitespace,
    #[regex(r"//[^\n]*")]
    Comment,
    #[token("/*", block_comment)]
    BlockComment,
    #[regex(r"[A-Z][A-Za-z0-9_]*")]
    Upper,
    #[regex(r"[a-z_][A-Za-z0-9_]*")]
    Lower,
    #[regex(r"[0-9]+\.[0-9]+([eE][+-]?[0-9]+)?|[0-9]+[eE][+-]?[0-9]+")]
    Float,
    #[regex(r"0[xX][0-9a-fA-F_]+|[0-9][0-9_]*")]
    Int,
    #[regex(r#""([^"\\]|\\.)*""#)]
    String,
    #[token("struct")]
    Struct,
    #[token("enum")]
    Enum,
    #[token("effect")]
    Effect,
    #[token("impl")]
    Impl,
    #[token("fn")]
    Fn,
    #[token("let")]
    Let,
    #[token("copy")]
    Copy,
    #[token("affine")]
    Affine,
    #[token("linear")]
    Linear,
    #[token("mod")]
    Mod,
    #[token("eff")]
    Eff,
    #[token("in")]
    In,
    #[token("inout")]
    Inout,
    #[token("sink")]
    Sink,
    #[token("self")]
    SelfKw,
    #[token("handle")]
    Handle,
    #[token("state")]
    State,
    #[token("with")]
    With,
    #[token("return")]
    Return,
    #[token("match")]
    Match,
    #[token("if")]
    If,
    #[token("else")]
    Else,
    #[token("true")]
    True,
    #[token("false")]
    False,
    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,
    #[token("[")]
    LBracket,
    #[token("]")]
    RBracket,
    #[token(",")]
    Comma,
    #[token(";")]
    Semi,
    #[token(":")]
    Colon,
    #[token("::")]
    ColonColon,
    #[token(".")]
    Dot,
    #[token("?")]
    Question,
    #[token("@")]
    At,
    #[token("@@")]
    AtAt,
    #[token("->")]
    Arrow,
    #[token("=>")]
    FatArrow,
    #[token("=")]
    Eq,
    #[token("+=")]
    PlusEq,
    #[token("*=")]
    StarEq,
    #[token("/=")]
    SlashEq,
    #[token("%=")]
    PercentEq,
    #[token("-=")]
    MinusEq,
    #[token("+")]
    Plus,
    #[token("-")]
    Minus,
    #[token("*")]
    Star,
    #[token("/")]
    Slash,
    #[token("%")]
    Percent,
    #[token("==")]
    EqEq,
    #[token("!=")]
    Ne,
    #[token("<")]
    Lt,
    #[token("<=")]
    Le,
    #[token(">")]
    Gt,
    #[token(">=")]
    Ge,
    #[token("&&")]
    And,
    #[token("||")]
    Or,
    #[token("!")]
    Bang,
    #[token("|")]
    Pipe,
    Error,
    Eof,
    // Append only: existing token discriminants are stable. Batch parsing still
    // rejects macro source and does not gain macro execution.
    #[token("$")]
    Dollar,
}
fn block_comment(lex: &mut logos::Lexer<TokenKind>) -> bool {
    let rest = lex.remainder();
    let mut depth = 1;
    let mut i = 0;
    while i + 1 < rest.len() {
        match &rest.as_bytes()[i..i + 2] {
            b"/*" => {
                depth += 1;
                i += 2;
            }
            b"*/" => {
                depth -= 1;
                i += 2;
                if depth == 0 {
                    lex.bump(i);
                    return true;
                }
            }
            _ => i += 1,
        }
    }
    lex.bump(rest.len());
    false
}
impl TokenKind {
    pub fn trivia(self) -> bool {
        matches!(self, Self::Whitespace | Self::Comment | Self::BlockComment)
    }
}
#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}
/// Logical resource ceilings, excluding allocator bookkeeping/RSS. All tokens,
/// including trivia/error tokens, count; the EOF sentinel is one extra slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LexLimits {
    pub max_source_bytes: usize,
    pub max_tokens: usize,
    pub max_diagnostics: usize,
}
pub const HARD_SOURCE_BYTES: usize = 8 * 1024 * 1024;
pub const HARD_TOKENS: usize = 262_144;
pub const HARD_DIAGNOSTICS: usize = 1024;
impl Default for LexLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: 1024 * 1024,
            max_tokens: 65_536,
            max_diagnostics: 128,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LexLimitKind {
    SourceBytes,
    Tokens,
    Diagnostics,
    Depth,
    Configuration,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LexLimit {
    pub kind: LexLimitKind,
    pub limit: usize,
    pub observed: usize,
    pub retained_bytes: usize,
}
impl core::fmt::Display for LexLimit {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{:?} limit {}; observed {}; retained {} bytes",
            self.kind, self.limit, self.observed, self.retained_bytes
        )
    }
}
impl std::error::Error for LexLimit {}
impl LexLimit {
    pub(crate) fn diagnostic(self, at: usize) -> Diagnostic {
        Diagnostic::new("E0003", self.to_string(), at..at)
    }
}
impl LexLimits {
    pub(crate) fn validate(self) -> Result<(), LexLimit> {
        for (value, hard, min) in [
            (self.max_source_bytes, HARD_SOURCE_BYTES, 0),
            (self.max_tokens, HARD_TOKENS, 1),
            (self.max_diagnostics, HARD_DIAGNOSTICS, 1),
        ] {
            if value > hard || value < min {
                return Err(LexLimit {
                    kind: LexLimitKind::Configuration,
                    limit: hard,
                    observed: value,
                    retained_bytes: 0,
                });
            }
        }
        Ok(())
    }
}
#[derive(Debug)]
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub diagnostics: Vec<Diagnostic>,
    /// Some means the lexical stream is lossless but must not be accepted as AST.
    pub limit: Option<LexLimit>,
}
/// Source/configuration preflight fails before lexing/allocating. Token or
/// diagnostic exhaustion returns a capped stream with one opaque Error tail;
/// concatenating all non-EOF token spans still yields the complete input.
pub fn lex_with_limits(source: &str, limits: LexLimits) -> Result<Lexed, LexLimit> {
    limits.validate()?;
    if source.len() > limits.max_source_bytes {
        return Err(LexLimit {
            kind: LexLimitKind::SourceBytes,
            limit: limits.max_source_bytes,
            observed: source.len(),
            retained_bytes: 0,
        });
    }
    let mut tokens: Vec<Token> = Vec::with_capacity(source.len().min(limits.max_tokens) + 1);
    let mut diagnostics = Vec::with_capacity(limits.max_diagnostics);
    let mut limit = None;
    for (kind, span) in TokenKind::lexer(source).spanned() {
        let kind = kind.unwrap_or(TokenKind::Error);
        if tokens.len() >= limits.max_tokens {
            let last = tokens.pop().expect("positive token limit");
            let exhausted = LexLimit {
                kind: LexLimitKind::Tokens,
                limit: limits.max_tokens,
                observed: limits.max_tokens + 1,
                retained_bytes: source.len(),
            };
            tokens.push(Token {
                kind: TokenKind::Error,
                span: last.span.start..source.len(),
            });
            push_limit(
                &mut diagnostics,
                exhausted.diagnostic(span.start),
                limits.max_diagnostics,
            );
            limit = Some(exhausted);
            break;
        }
        if kind == TokenKind::Error {
            if diagnostics.len() >= limits.max_diagnostics {
                let exhausted = LexLimit {
                    kind: LexLimitKind::Diagnostics,
                    limit: limits.max_diagnostics,
                    observed: limits.max_diagnostics + 1,
                    retained_bytes: source.len(),
                };
                push_limit(
                    &mut diagnostics,
                    exhausted.diagnostic(span.start),
                    limits.max_diagnostics,
                );
                tokens.push(Token {
                    kind: TokenKind::Error,
                    span: span.start..source.len(),
                });
                limit = Some(exhausted);
                break;
            }
            diagnostics.push(Diagnostic::new(
                "E0001",
                "invalid token or unterminated comment",
                span.clone(),
            ));
        }
        tokens.push(Token { kind, span });
    }
    tokens.push(Token {
        kind: TokenKind::Eof,
        span: source.len()..source.len(),
    });
    Ok(Lexed {
        tokens,
        diagnostics,
        limit,
    })
}
pub(crate) fn push_limit(diagnostics: &mut Vec<Diagnostic>, diagnostic: Diagnostic, max: usize) {
    if diagnostics.len() >= max {
        diagnostics.pop();
    }
    diagnostics.push(diagnostic);
}
pub(crate) fn prefix_boundary(source: &str, max: usize) -> usize {
    let mut retained = source.len().min(max);
    while !source.is_char_boundary(retained) {
        retained -= 1;
    }
    retained
}
/// Compatibility API with conservative defaults. Oversized source returns only
/// a bounded UTF-8 prefix token and E0003; it never silently accepts truncation.
/// Use lex_with_limits to receive an explicit allocation-free preflight error.
pub fn lex(source: &str) -> (Vec<Token>, Vec<Diagnostic>) {
    let limits = LexLimits::default();
    match lex_with_limits(source, limits) {
        Ok(lexed) => (lexed.tokens, lexed.diagnostics),
        Err(mut limit) => {
            let retained = prefix_boundary(source, limits.max_source_bytes);
            limit.retained_bytes = retained;
            let mut tokens = Vec::new();
            if retained != 0 {
                tokens.push(Token {
                    kind: TokenKind::Error,
                    span: 0..retained,
                });
            }
            tokens.push(Token {
                kind: TokenKind::Eof,
                span: retained..retained,
            });
            (tokens, vec![limit.diagnostic(retained)])
        }
    }
}
