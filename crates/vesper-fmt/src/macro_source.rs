//! A separate syntax gate for the known expression-macro profile. Recovery CST
//! nodes supply lossless token locations only after that gate succeeds.
use super::{Error, Limits, Work};
use vesper_syntax::{
    lexer::Token,
    macro_source,
    parser::{self, SyntaxNode},
    recovery,
};

/// Work units the macro-profile parser may spend. Besides the quadratic
/// name-resolution envelope, that parser reserves `64 * max_depth` units for
/// every token of each function header, body and invocation stage (a token can
/// be covered by up to three nested stages), so a linear per-token term is
/// required: without it, ordinary small macro modules with two or more
/// functions were refused with `WorkLimit`.
const STAGE_UNITS_PER_TOKEN: usize = 64 * 24 * 4;
fn allowance(source: &str, tokens: &[Token]) -> Result<usize, Error> {
    tokens
        .len()
        .checked_mul(tokens.len())
        .and_then(|n| n.checked_mul(4))
        .and_then(|n| {
            tokens
                .len()
                .checked_mul(STAGE_UNITS_PER_TOKEN)
                .and_then(|t| n.checked_add(t))
        })
        .and_then(|n| source.len().checked_mul(16).and_then(|s| n.checked_add(s)))
        .and_then(|n| n.checked_add(64 * 1024))
        // The estimate is conservative; the parser enforces its actual budget.
        .map(|n| n.min(16 * 1024 * 1024))
        .ok_or(Error::WorkLimit)
}

fn macro_error(error: macro_source::Error) -> Error {
    match error.kind {
        macro_source::ErrorKind::Budget(_) => Error::WorkLimit,
        macro_source::ErrorKind::Allocation => Error::Allocation,
        _ => Error::OpaqueMacro,
    }
}

pub(super) fn syntax(
    source: &str,
    tokens: &[Token],
    limits: Limits,
    work: &mut Work,
) -> Result<SyntaxNode, Error> {
    let stage = allowance(source, tokens)?;
    work.charge(stage)?;
    let parsed = macro_source::parse(
        source,
        macro_source::Limits {
            max_source_bytes: limits.max_input_bytes.min(1024 * 1024),
            max_tokens: limits.max_tokens.min(8192),
            max_depth: limits.max_depth.min(24),
            max_work: stage,
            ..Default::default()
        },
    )
    .map_err(macro_error)?;
    // A bare quote-spelled ordinary identifier is not enough to opt into a
    // profile that would otherwise permit formatting opaque payloads.
    if parsed.definitions().is_empty() {
        return Err(Error::OpaqueMacro);
    }
    if parsed.tokens().len() != tokens.len()
        || parsed
            .tokens()
            .iter()
            .zip(tokens)
            .any(|(a, b)| a.kind != b.kind || a.span != b.span)
    {
        return Err(Error::Conservation);
    }
    work.charge(stage)?;
    let recovered = parser::parse_recovery(
        source,
        recovery::Limits {
            parse: parser::ParseLimits {
                max_source_bytes: limits.max_input_bytes.min(1024 * 1024),
                max_tokens: limits.max_tokens.min(8192),
                max_depth: limits.max_depth.min(24),
                ..Default::default()
            },
            max_work: stage,
            ..Default::default()
        },
    )
    .map_err(|error| match error {
        recovery::Error::Budget(_) => Error::WorkLimit,
        recovery::Error::Allocation => Error::Allocation,
        _ => Error::Conservation,
    })?;
    if recovered.source() != source {
        return Err(Error::Conservation);
    }
    Ok(recovered.syntax())
}
