//! Tier 0: lossless CST classification of arbitrary or incomplete snippets.
//!
//! The classifier walks the leaf tokens of the `vesper-syntax` rowan CST (the
//! batch parser recovers from errors and keeps every byte) and assigns each
//! token one [`VesperTokenClass`] from its token kind, its parent node kind
//! and its significant neighbours. There is no regular expression and no
//! backtracking; work is linear in the number of tokens.
use crate::{ChromaError, HighlightSpan, VesperTokenClass as C, flags, verify_lossless_spans};
use rowan::{NodeOrToken, SyntaxKind, WalkEvent};
use vesper_syntax::{
    lexer::TokenKind as K,
    parser::{self, ParseLimits},
};

/// Default byte budget: the parser's default (1 MiB).
pub const DEFAULT_MAX_SOURCE_BYTES: usize = 1024 * 1024;

/// Every lexer token kind, in discriminant order (the CST stores tokens as
/// `SyntaxKind(TokenKind as u16)`).
const TOKEN_KINDS: [K; 81] = [
    K::Whitespace,
    K::Comment,
    K::BlockComment,
    K::Upper,
    K::Lower,
    K::Float,
    K::Int,
    K::String,
    K::Struct,
    K::Enum,
    K::Effect,
    K::Impl,
    K::Fn,
    K::Let,
    K::Copy,
    K::Affine,
    K::Linear,
    K::Mod,
    K::Eff,
    K::In,
    K::Inout,
    K::Sink,
    K::SelfKw,
    K::Handle,
    K::State,
    K::With,
    K::Return,
    K::Match,
    K::If,
    K::Else,
    K::True,
    K::False,
    K::LParen,
    K::RParen,
    K::LBrace,
    K::RBrace,
    K::LBracket,
    K::RBracket,
    K::Comma,
    K::Semi,
    K::Colon,
    K::ColonColon,
    K::Dot,
    K::Question,
    K::At,
    K::AtAt,
    K::Arrow,
    K::FatArrow,
    K::Eq,
    K::PlusEq,
    K::StarEq,
    K::SlashEq,
    K::PercentEq,
    K::MinusEq,
    K::Plus,
    K::Minus,
    K::Star,
    K::Slash,
    K::Percent,
    K::EqEq,
    K::Ne,
    K::Lt,
    K::Le,
    K::Gt,
    K::Ge,
    K::And,
    K::Or,
    K::Bang,
    K::Pipe,
    K::Error,
    K::Eof,
    K::Dollar,
    K::Tilde,
    K::PipeEq,
    K::AmpEq,
    K::DotDot,
    K::DotDotEq,
    K::Amp,
    K::Caret,
    K::Shl,
    K::Shr,
];

fn token_kind(raw: SyntaxKind) -> K {
    match TOKEN_KINDS.get(usize::from(raw.0)) {
        Some(kind) if *kind as u16 == raw.0 => *kind,
        _ => K::Error,
    }
}

#[derive(Clone, Copy)]
struct Tok {
    kind: K,
    start: usize,
    end: usize,
    in_type: bool,
    recovered: bool,
}

/// Tier 0 with the default byte budget.
pub fn highlight_tier0(src: &str) -> Result<Vec<HighlightSpan>, ChromaError> {
    highlight_tier0_with_limit(src, DEFAULT_MAX_SOURCE_BYTES)
}

/// Tier 0 with an explicit byte budget (at most the parser's hard limit).
pub fn highlight_tier0_with_limit(
    src: &str,
    max_source_bytes: usize,
) -> Result<Vec<HighlightSpan>, ChromaError> {
    let limit = max_source_bytes.min(vesper_syntax::lexer::HARD_SOURCE_BYTES);
    if src.len() > limit {
        return Err(ChromaError::SourceTooLarge {
            len: src.len(),
            limit,
        });
    }
    let limits = ParseLimits {
        max_source_bytes: limit.max(1),
        // Token exhaustion keeps a flat, still lossless CST.
        max_tokens: vesper_syntax::lexer::HARD_TOKENS,
        ..ParseLimits::default()
    };
    let parsed =
        parser::parse_with_limits(src, limits).map_err(|_| ChromaError::SourceTooLarge {
            len: src.len(),
            limit,
        })?;
    let root = parsed.syntax();
    let mut toks = Vec::with_capacity(src.len() / 3 + 1);
    let mut cursor = 0usize;
    // Depth counters of enclosing TYPE and recovery nodes, maintained over
    // the preorder walk instead of re-walking ancestors per token.
    let (mut types, mut recovering) = (0usize, 0usize);
    for event in root.preorder_with_tokens() {
        let element = match event {
            WalkEvent::Enter(e) => e,
            WalkEvent::Leave(NodeOrToken::Node(node)) => {
                let kind = node.kind();
                types -= usize::from(kind == parser::TYPE);
                recovering -= usize::from(kind == parser::ERROR_NODE || kind == parser::INCOMPLETE);
                continue;
            }
            WalkEvent::Leave(NodeOrToken::Token(_)) => continue,
        };
        let token = match element {
            NodeOrToken::Node(node) => {
                let kind = node.kind();
                types += usize::from(kind == parser::TYPE);
                recovering += usize::from(kind == parser::ERROR_NODE || kind == parser::INCOMPLETE);
                continue;
            }
            NodeOrToken::Token(token) => token,
        };
        let range = token.text_range();
        let (start, end) = (usize::from(range.start()), usize::from(range.end()));
        if start != cursor || src.get(start..end) != Some(token.text()) {
            return Err(ChromaError::CstMismatch {
                cst_len: usize::from(root.text_range().end()),
                src_len: src.len(),
            });
        }
        cursor = end;
        let kind = token_kind(token.kind());
        toks.push(Tok {
            kind,
            start,
            end,
            in_type: types > 0,
            recovered: recovering > 0 || kind == K::Error,
        });
    }
    if cursor != src.len() {
        return Err(ChromaError::CstMismatch {
            cst_len: cursor,
            src_len: src.len(),
        });
    }
    let spans = classify(src, &toks, false);
    verify_lossless_spans(src.len(), &spans)?;
    Ok(spans)
}

/// Tier 0 token mode: classifies the lossless lexer token stream (the CST's
/// leaves) without running the parser. The classes are the same as
/// [`highlight_tier0`]'s except that identifiers inside type syntax are
/// recognised only for primitive scalars after `:`, `->`, `[` or `,`, and the
/// recovery flag marks lexer errors only. This is the path the wasm module
/// uses: it is about 30x faster than building the CST and does not link the
/// parser (see the lane report for measurements).
pub fn highlight_tier0_tokens(src: &str) -> Result<Vec<HighlightSpan>, ChromaError> {
    highlight_tier0_tokens_with_limit(src, DEFAULT_MAX_SOURCE_BYTES)
}

/// [`highlight_tier0_tokens`] with an explicit byte budget.
pub fn highlight_tier0_tokens_with_limit(
    src: &str,
    max_source_bytes: usize,
) -> Result<Vec<HighlightSpan>, ChromaError> {
    let limit = max_source_bytes.min(vesper_syntax::lexer::HARD_SOURCE_BYTES);
    let too_large = ChromaError::SourceTooLarge {
        len: src.len(),
        limit,
    };
    if src.len() > limit {
        return Err(too_large);
    }
    let lexed = vesper_syntax::lexer::lex_with_limits(
        src,
        vesper_syntax::lexer::LexLimits {
            max_source_bytes: limit,
            max_tokens: vesper_syntax::lexer::HARD_TOKENS,
            max_diagnostics: vesper_syntax::lexer::HARD_DIAGNOSTICS,
        },
    )
    .map_err(|_| too_large)?;
    let mut toks = Vec::with_capacity(lexed.tokens.len());
    let mut cursor = 0usize;
    for t in lexed.tokens.iter().filter(|t| t.kind != K::Eof) {
        if t.span.start != cursor || t.span.end < t.span.start || t.span.end > src.len() {
            return Err(ChromaError::CstMismatch {
                cst_len: cursor,
                src_len: src.len(),
            });
        }
        cursor = t.span.end;
        toks.push(Tok {
            kind: t.kind,
            start: t.span.start,
            end: t.span.end,
            in_type: false,
            recovered: t.kind == K::Error,
        });
    }
    if cursor != src.len() {
        return Err(ChromaError::CstMismatch {
            cst_len: cursor,
            src_len: src.len(),
        });
    }
    let spans = classify(src, &toks, true);
    verify_lossless_spans(src.len(), &spans)?;
    Ok(spans)
}

const PRIMITIVES: [&str; 15] = [
    "u8", "u16", "u32", "u64", "u128", "i8", "i16", "i32", "i64", "i128", "f32", "f64", "bool",
    "usize", "isize",
];

/// `u16be`, `i64le`, `f32be`, ...: a wire-struct endian scalar (§13.2).
fn is_wire_scalar(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() < 4 || !matches!(bytes[0], b'u' | b'i' | b'f') {
        return false;
    }
    let (width, order) = text[1..].split_at(text.len() - 3);
    matches!(order, "be" | "le")
        && match bytes[0] {
            b'f' => matches!(width, "32" | "64"),
            _ => matches!(width, "8" | "16" | "32" | "64" | "128"),
        }
}

/// Most distinct effect names Tier 0 tracks per snippet.
const MAX_EFFECT_NAMES: usize = 64;

const CONTEXTUAL_DECL: [&str; 9] = [
    "pub", "use", "trait", "model", "wire", "type", "where", "region", "dim",
];

fn classify(src: &str, toks: &[Tok], token_mode: bool) -> Vec<HighlightSpan> {
    let text = |t: &Tok| &src[t.start..t.end];
    // Indices of significant (non-trivia) tokens and each token's position in it.
    let sig: Vec<usize> = (0..toks.len())
        .filter(|&i| !toks[i].kind.trivia())
        .collect();
    let mut sig_pos = vec![usize::MAX; toks.len()];
    for (p, &i) in sig.iter().enumerate() {
        sig_pos[i] = p;
    }
    let prev = |i: usize| -> Option<&Tok> {
        let p = sig_pos[i];
        (p != usize::MAX && p > 0).then(|| &toks[sig[p - 1]])
    };
    let next = |i: usize| -> Option<&Tok> {
        let p = sig_pos[i];
        (p != usize::MAX).then(|| sig.get(p + 1).map(|&j| &toks[j]))?
    };

    // Pre-pass: effect names (declared, in `eff [...]` rows, or handled).
    // (name, linear); a short list, searched linearly (snippets declare few
    // effects, and a map would dominate the wasm module's size).
    let mut effects: Vec<(&str, bool)> = vec![("Fail", false), ("Cancel", false)];
    fn note<'s>(effects: &mut Vec<(&'s str, bool)>, name: &'s str, linear: Option<bool>) {
        match effects.iter().position(|(n, _)| *n == name) {
            Some(i) => {
                if let Some(l) = linear {
                    effects[i].1 = l;
                }
            }
            // Bounded so adversarial input cannot make lookups quadratic;
            // names past the cap stay `TypeNominal`.
            None if effects.len() < MAX_EFFECT_NAMES => {
                effects.push((name, linear.unwrap_or(false)))
            }
            None => {}
        }
    }
    let mut declared_linear: Vec<&str> = vec![];
    let mut rows: Vec<bool> = vec![];
    for (p, &i) in sig.iter().enumerate() {
        match toks[i].kind {
            K::Effect => {
                let linear = p > 0 && toks[sig[p - 1]].kind == K::Linear;
                if let Some(&j) = sig.get(p + 1)
                    && toks[j].kind == K::Upper
                {
                    let name = text(&toks[j]);
                    if linear {
                        declared_linear.push(name);
                    }
                    note(&mut effects, name, Some(linear));
                }
            }
            K::Handle => {
                if let Some(&j) = sig.get(p + 1)
                    && toks[j].kind == K::Upper
                {
                    note(&mut effects, text(&toks[j]), None);
                }
            }
            // One pass over brackets: `row` marks a `[` opened right after
            // `eff`, so effect names are its direct elements. Unclosed
            // brackets in incomplete input cost nothing extra.
            K::LBracket => rows.push(p > 0 && toks[sig[p - 1]].kind == K::Eff),
            K::RBracket => {
                rows.pop();
            }
            K::Upper if rows.last() == Some(&true) => {
                let before = toks[sig[p - 1]].kind;
                if matches!(before, K::LBracket | K::Comma | K::Colon) {
                    note(&mut effects, text(&toks[i]), None);
                }
            }
            _ => {}
        }
    }
    for name in declared_linear {
        note(&mut effects, name, Some(true));
    }

    let mut spans = Vec::with_capacity(toks.len());
    // Projection leaves of `inout name.{a, b.c}`: (brace depth) while scanning.
    let mut projection: Option<usize> = None;
    for (i, t) in toks.iter().enumerate() {
        let mut flag = if t.recovered { flags::RECOVERED } else { 0 };
        let class = match t.kind {
            K::Whitespace => C::Whitespace,
            K::Comment | K::BlockComment => C::CommentTrivia,
            K::Struct
            | K::Enum
            | K::Effect
            | K::Impl
            | K::Fn
            | K::Let
            | K::Eff
            | K::SelfKw
            | K::Handle
            | K::State
            | K::With
            | K::Return
            | K::Match
            | K::If
            | K::Else => C::KeywordControl,
            K::Copy | K::Affine | K::Linear | K::Mod => C::ModalSpecifier,
            K::In => C::PassModeIn,
            K::Inout => {
                if next(i).is_some_and(|n| n.kind == K::Lower) {
                    projection = Some(0);
                }
                C::PassModeInOut
            }
            K::Sink => C::PassModeSink,
            K::True | K::False | K::Int | K::Float | K::String => C::LiteralScalar,
            K::Upper => match effects.iter().find(|(n, _)| *n == text(t)) {
                Some((_, true)) => C::EffectLinear,
                Some((_, false)) => C::EffectAbortable,
                None => C::TypeNominal,
            },
            K::Lower => {
                let word = text(t);
                let after = next(i).map(|n| n.kind);
                let before = prev(i).map(|p| p.kind);
                let p = sig_pos[i];
                let lookahead = |offset: usize| -> Option<K> {
                    (p != usize::MAX)
                        .then(|| sig.get(p + offset).map(|&j| toks[j].kind))
                        .flatten()
                };
                let is_tiled_decl = word == "tiled"
                    && before != Some(K::Fn)
                    && lookahead(1) == Some(K::LParen)
                    && lookahead(2) == Some(K::Int)
                    && lookahead(3) == Some(K::RParen)
                    && matches!(
                        lookahead(4),
                        Some(K::Struct | K::Copy | K::Affine | K::Linear | K::Mod)
                    );
                let is_field_temp = matches!(word, "hot" | "cold")
                    && before.is_some_and(|b| matches!(b, K::LBrace | K::Comma))
                    && after == Some(K::Lower)
                    && lookahead(2) == Some(K::Colon);
                if projection.is_some()
                    && before.is_some_and(|b| matches!(b, K::Dot | K::LBrace | K::Comma))
                {
                    C::FieldLeafMut
                } else if is_wire_scalar(word) {
                    C::WireEndianScalar
                } else if before == Some(K::At) && word == "proof" {
                    C::DocProofAnchor
                } else if (before == Some(K::At) && word == "fip")
                    || is_tiled_decl
                    || is_field_temp
                    || is_loop_keyword(word, before, after)
                {
                    C::KeywordControl
                } else if before == Some(K::Fn) || after == Some(K::LParen) {
                    C::FunctionIdent
                } else if CONTEXTUAL_DECL.contains(&word)
                    && after.is_some_and(|a| {
                        matches!(
                            a,
                            K::Upper
                                | K::Lower
                                | K::Fn
                                | K::Struct
                                | K::Enum
                                | K::Effect
                                | K::Impl
                                | K::Linear
                                | K::Copy
                                | K::Affine
                                | K::LBrace
                        )
                    })
                    || matches!(word, "as" | "for")
                        && before.is_some_and(|b| matches!(b, K::Upper | K::RBracket))
                        && after == Some(K::Upper)
                {
                    C::KeywordControl
                } else if t.in_type
                    || token_mode
                        && PRIMITIVES.contains(&word)
                        && before.is_some_and(|b| {
                            matches!(b, K::Colon | K::Arrow | K::LBracket | K::Comma)
                        })
                {
                    C::TypeNominal
                } else {
                    C::PassModeIn
                }
            }
            K::At if next(i).is_some_and(|n| n.kind == K::Lower && text(n) == "proof") => {
                C::DocProofAnchor
            }
            K::At if next(i).is_some_and(|n| n.kind == K::Lower && text(n) == "fip") => {
                C::KeywordControl
            }
            K::Arrow
            | K::FatArrow
            | K::ColonColon
            | K::EqEq
            | K::Ne
            | K::Le
            | K::Ge
            | K::And
            | K::Or => {
                flag |= flags::LIGATURE;
                C::PunctLigature
            }
            _ => C::PunctLigature,
        };
        // Track the end of an `inout name.{...}` projection list.
        if let Some(depth) = projection.as_mut() {
            match t.kind {
                K::LBrace => *depth += 1,
                K::RBrace => {
                    *depth = depth.saturating_sub(1);
                    if *depth == 0 {
                        projection = None;
                    }
                }
                K::Colon | K::Comma | K::RParen if *depth == 0 => projection = None,
                _ => {}
            }
        }
        let mut span = HighlightSpan::new(t.start, t.end, class);
        span.flags = flag;
        spans.push(span);
    }
    spans
}

/// `loop`, `for`, `break`, `continue` and `let mut` are contextual words (they stay valid
/// names elsewhere), classified as keywords where the parser reads them so.
fn is_loop_keyword(word: &str, before: Option<K>, after: Option<K>) -> bool {
    let statement_start =
        before.is_none_or(|b| matches!(b, K::LBrace | K::RBrace | K::Semi | K::FatArrow | K::Eq));
    match word {
        "loop" => after == Some(K::LBrace),
        "for" => statement_start && matches!(after, Some(K::Lower | K::LParen)),
        "break" => {
            statement_start
                && !matches!(
                    after,
                    Some(K::Eq | K::Dot | K::Colon | K::ColonColon | K::Comma)
                )
        }
        "continue" => {
            statement_start
                && matches!(
                    after,
                    None | Some(K::Semi | K::RBrace | K::RParen | K::RBracket | K::Comma)
                )
        }
        "mut" => before == Some(K::Let),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_table_matches_discriminants() {
        for (i, kind) in TOKEN_KINDS.iter().enumerate() {
            assert_eq!(*kind as usize, i, "{kind:?}");
            assert_eq!(token_kind(SyntaxKind(i as u16)), *kind);
        }
        assert_eq!(token_kind(SyntaxKind(500)), K::Error);
    }

    #[test]
    fn loop_words_are_keywords_where_they_parse_so() {
        let src = "fn f() { let mut i = 0; loop { break; } for j in 0..3 { continue; } let loop = 1; }";
        let spans = highlight_tier0_tokens(src).unwrap();
        let class = |at: usize| spans.iter().find(|s| s.start == at).unwrap().class;
        for word in ["mut", "loop {", "break", "for", "continue"] {
            let at = src.find(word).unwrap();
            assert_eq!(class(at), C::KeywordControl, "{word}");
        }
        assert_ne!(class(src.rfind("loop").unwrap()), C::KeywordControl);
    }

    #[test]
    fn wire_scalars() {
        for ok in ["u32be", "u64le", "i8be", "u128le", "f32be", "f64le"] {
            assert!(is_wire_scalar(ok), "{ok}");
        }
        for no in ["u32", "u3be", "ube", "x32be", "f16le", "u32bel", "be"] {
            assert!(!is_wire_scalar(no), "{no}");
        }
    }
}
