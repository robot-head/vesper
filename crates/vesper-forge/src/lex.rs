//! A small lexer for the fragment's surface text, used by host-side source
//! fixtures and by quotation builders. It is not the Vesper lexer.
//!
//! Identifiers start with a letter, `_` or `@` and continue with letters,
//! digits, `_`, `-` or `/` (so `@vesper-wire/hash` is one segment). `::` is
//! one punctuation token; every other non-space character is a single-char
//! punctuation or delimiter token. Strings are `"..."` without escapes.

use crate::token::{Delim, GeneratedToken, TokenKind};

/// Lexes `src` into generated tokens (no scopes or origin).
pub fn lex_fragment(src: &str) -> Vec<GeneratedToken> {
    lex_generated(src)
}

pub(crate) fn lex_generated(src: &str) -> Vec<GeneratedToken> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        let kind = if c.is_alphabetic() || c == '_' || c == '@' {
            i += 1;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || matches!(chars[i], '_' | '-' | '/'))
            {
                i += 1;
            }
            TokenKind::Ident
        } else if c.is_ascii_digit() {
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            TokenKind::Int
        } else if c == '"' {
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                i += 1;
            }
            i = (i + 1).min(chars.len());
            TokenKind::Str
        } else if c == ':' && chars.get(i + 1) == Some(&':') {
            i += 2;
            TokenKind::Punct
        } else {
            i += 1;
            match c {
                '(' => TokenKind::Open(Delim::Paren),
                ')' => TokenKind::Close(Delim::Paren),
                '[' => TokenKind::Open(Delim::Bracket),
                ']' => TokenKind::Close(Delim::Bracket),
                '{' => TokenKind::Open(Delim::Brace),
                '}' => TokenKind::Close(Delim::Brace),
                _ => TokenKind::Punct,
            }
        };
        let text: String = chars[start..i].iter().collect();
        out.push(GeneratedToken::new(kind, text));
    }
    out
}
