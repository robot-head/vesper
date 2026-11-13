//! Actual full-source token ranges; no source padding or synthetic function body.
use super::*;
use crate::macro_source::{Error, ErrorKind, FunctionHeader, Limits, Resource};
fn parser<'a>(
    source: &'a str,
    tokens: &[Token],
    range: std::ops::Range<usize>,
    limits: Limits,
) -> Result<Parser<'a>, Error> {
    let selected = tokens
        .get(range)
        .ok_or_else(|| Error::new(ErrorKind::Syntax, None))?;
    let end = selected.last().map_or(0, |t| t.span.end);
    let mut owned = Vec::new();
    owned
        .try_reserve_exact(selected.len() + 1)
        .map_err(|_| Error::new(ErrorKind::Allocation, None))?;
    owned.extend_from_slice(selected);
    owned.push(Token {
        kind: K::Eof,
        span: end..end,
    });
    Ok(Parser {
        source,
        tokens: owned,
        pos: 0,
        builder: GreenNodeBuilder::new(),
        diagnostics: vec![],
        no_struct: false,
        row_names: Default::default(),
        bound_values: Default::default(),
        model_uses: vec![],
        trait_bounds: vec![],
        pending_bounds: vec![],
        limits: ParseLimits {
            max_source_bytes: limits.max_source_bytes,
            max_tokens: limits.max_tokens,
            max_diagnostics: 64,
            max_depth: limits.max_depth,
        },
        depth: 0,
        halted: false,
        halt_kind: None,
        expr_heights: Default::default(),
        editor: false,
        site_limit: 0,
        site_exhausted: false,
        sites: vec![],
        headers: vec![],
    })
}
fn failure(p: &Parser<'_>) -> Error {
    Error::new(
        if p.halted {
            ErrorKind::Budget(Resource::Depth)
        } else {
            ErrorKind::Syntax
        },
        p.diagnostics.first().map(|d| d.span.clone()),
    )
}
pub(crate) fn parse_source_header(
    source: &str,
    tokens: &[Token],
    range: std::ops::Range<usize>,
    limits: Limits,
) -> Result<FunctionHeader, Error> {
    let mut p = parser(source, tokens, range, limits)?;
    p.builder.start_node(MODULE);
    let header = p.function_header();
    if p.halted || !p.diagnostics.is_empty() || !p.at(K::LBrace) || !p.trait_bounds.is_empty() {
        return Err(failure(&p));
    }
    // The remaining opening brace is the actual body token, not placeholder AST.
    if p.index(1) != p.tokens.len() - 1 {
        return Err(Error::new(ErrorKind::Syntax, Some(p.span())));
    }
    Ok(header)
}
pub(crate) fn parse_source_item(
    source: &str,
    tokens: &[Token],
    range: std::ops::Range<usize>,
    limits: Limits,
) -> Result<Module, Error> {
    let mut p = parser(source, tokens, range, limits)?;
    p.builder.start_node(MODULE);
    let item = p.item();
    if p.halted || !p.diagnostics.is_empty() || !p.at(K::Eof) {
        return Err(failure(&p));
    }
    let Some(item) = item else {
        return Err(Error::new(ErrorKind::Unsupported, None));
    };
    Ok(Module {
        items: vec![item],
        model_uses: p.model_uses,
        trait_bounds: p.trait_bounds,
    })
}
