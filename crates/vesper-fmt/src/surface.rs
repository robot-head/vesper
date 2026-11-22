//! Validate syntax extensions before using their lossless token locations.
use super::{Error, Limits, PARSE_UNITS_PER_TOKEN, Work, macro_source};
use vesper_syntax::{
    lexer::{self, Token, TokenKind as K},
    parser::{self, SyntaxNode},
};

#[derive(Default, PartialEq, Eq)]
pub(super) struct Profile {
    pub(super) allow_macro_tokens: bool,
    // Sugar distinguishes layouts with the same tokens (e.g. a clause on
    // the same line vs. a new statement). Compare its CST after rendering.
    pub(super) nodes: Vec<(u16, usize, usize)>,
}

/// A cheap prefilter. The surface parser decides whether the word actually
/// introduces a block, including when a comment follows it.
pub(super) fn may_have_test_block(source: &str) -> bool {
    source.match_indices("test").any(|(i, word)| {
        let after = source[i + word.len()..].trim_start();
        after.starts_with('{') || after.starts_with("//") || after.starts_with("/*")
    })
}

fn contains_test_block(source: &str, tokens: &[Token]) -> bool {
    let mut after_test = false;
    for token in tokens.iter().filter(|t| !t.kind.trivia()) {
        if after_test && token.kind == K::LBrace {
            return true;
        }
        after_test = token.kind == K::Lower && &source[token.span.clone()] == "test";
    }
    false
}

fn parse_limits() -> parser::ParseLimits {
    parser::ParseLimits {
        max_source_bytes: lexer::HARD_SOURCE_BYTES,
        max_tokens: lexer::HARD_TOKENS,
        ..Default::default()
    }
}

fn macro_tokens_in_groups(syntax: &SyntaxNode) -> bool {
    let mut previous = None;
    for token in syntax
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
    {
        let kind = token.kind().0;
        if [K::Whitespace, K::Comment, K::BlockComment]
            .iter()
            .any(|k| *k as u16 == kind)
        {
            continue;
        }
        let opaque = matches!(token.text(), "quote" | "quasiquote" | "syntax_quote")
            || (kind == K::Bang as u16
                && matches!(previous, Some(p) if p == K::Upper as u16 || p == K::Lower as u16));
        if opaque
            && !token.parent_ancestors().any(|n| {
                matches!(
                    n.kind(),
                    parser::SUGAR_GROUP
                        | parser::SUGAR_TEMPLATE
                        | parser::SUGAR_PATTERN
                        | parser::SURFACE_MACRO_CALL
                        | parser::SURFACE_MACRO_DECL
                )
            })
        {
            return false;
        }
        previous = Some(kind);
    }
    true
}

pub(super) fn syntax(
    source: &str,
    tokens: &[Token],
    limits: Limits,
    work: &mut Work,
    opaque: bool,
) -> Result<(SyntaxNode, Profile), Error> {
    let profile = Profile {
        allow_macro_tokens: opaque,
        ..Default::default()
    };
    // Ordinary modules use the compiler parser. Its batch mode skips the
    // contents of test blocks, so parse those in surface mode as well.
    let test_block = may_have_test_block(source) && contains_test_block(source, tokens);
    let core_error = if !opaque && !test_block {
        let core =
            parser::parse_with_limits(source, parse_limits()).map_err(|_| Error::TokenLimit)?;
        match core.diagnostics.first() {
            None => return Ok((core.syntax(), profile)),
            Some(d) => Some(Error::Syntax {
                offset: d.span.start,
            }),
        }
    } else {
        None
    };
    if core_error.is_some() || opaque {
        work.charge(
            tokens
                .len()
                .checked_mul(PARSE_UNITS_PER_TOKEN)
                .ok_or(Error::Overflow)?,
        )?;
    }
    // Bound validation before entering the surface parser. Bill its profile
    // only for real macro syntax; a sugar token tree may itself spell `macro`.
    let macro_limits = opaque
        .then(|| macro_source::profile_limits(source, tokens, limits))
        .transpose()?;
    let surface = if let Some(mut macros) = macro_limits {
        macros.max_work = macros.max_work.min(work.max - work.used);
        parser::parse_surface_with_macro_limits(source, parse_limits(), macros)
    } else {
        parser::parse_surface_with_limits(source, parse_limits())
    }
    .map_err(|_| Error::TokenLimit)?;
    let syntax = surface.syntax();
    if let Some(macros) = macro_limits
        && syntax.descendants().any(|node| {
            matches!(
                node.kind(),
                parser::SURFACE_MACRO_DECL | parser::SURFACE_MACRO_CALL
            )
        })
    {
        macro_source::charge_profile(tokens, macros, work)?;
    }
    let has_sugar = syntax
        .descendants()
        .any(|n| parser::is_sugar_node(n.kind()));
    if let Some(d) = surface.diagnostics.first() {
        // The batch macro parser skips test-only items. A mixed-module error
        // there must not be accepted through its recovery CST.
        if opaque && !has_sugar && !test_block {
            return Ok((macro_source::syntax(source, tokens, limits, work)?, profile));
        }
        return Err(core_error.unwrap_or(Error::Syntax {
            offset: d.span.start,
        }));
    }
    let significant: Vec<_> = tokens
        .iter()
        .filter(|t| !matches!(t.kind, K::Whitespace | K::Eof))
        .collect();
    let ordinal = |offset: usize| significant.partition_point(|t| t.span.end <= offset);
    let nodes: Vec<_> = syntax
        .descendants()
        .filter(|n| {
            parser::is_sugar_node(n.kind())
                || matches!(
                    n.kind(),
                    parser::SURFACE_MACRO_DECL
                        | parser::SURFACE_MACRO_CALL
                        | parser::SURFACE_MACRO_ARG
                )
        })
        .map(|n| {
            let range = n.text_range();
            (
                n.kind().0,
                ordinal(range.start().into()),
                ordinal(range.end().into()),
            )
        })
        .collect();
    if opaque {
        if !has_sugar {
            return Ok((macro_source::syntax(source, tokens, limits, work)?, profile));
        }
        if !macro_tokens_in_groups(&syntax) {
            return Err(Error::OpaqueMacro);
        }
    }
    if nodes.is_empty()
        && let Some(error) = core_error
    {
        return Err(error);
    }
    Ok((syntax, Profile { nodes, ..profile }))
}
