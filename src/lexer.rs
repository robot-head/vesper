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
pub fn lex(source: &str) -> (Vec<Token>, Vec<Diagnostic>) {
    let mut tokens = Vec::new();
    let mut errors = Vec::new();
    for (kind, span) in TokenKind::lexer(source).spanned() {
        let kind = kind.unwrap_or(TokenKind::Error);
        if kind == TokenKind::Error {
            errors.push(Diagnostic::new(
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
    (tokens, errors)
}
