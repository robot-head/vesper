//! Syntax-only formatting of independently complete functions in an original
//! damaged buffer. Recovered/opaque regions never provide semantic authority.
use super::{Error, FormatResult, Limits, Work, format_with_work};
use std::{mem::size_of, ops::Range};
use vesper_syntax::{
    ast::Item,
    lexer::{self, Token, TokenKind as K},
    parser,
    recovery::{self as syntax, DeclarationKind, DeclarationStatus, RecoverySyntax},
};

// Each parser call has a finite prepaid allowance. The normal formatter and
// both recovery passes share the remaining outer Work, including all copies.
const PARSER_WORK: usize = 8 * 1024 * 1024;
const MAX_TOKENS: usize = 8192;
const MAX_DECLARATIONS: usize = 256;

#[derive(Debug)]
pub struct RecoveryEdit {
    span: Range<usize>,
    replacement: String,
}
impl RecoveryEdit {
    pub fn span(&self) -> Range<usize> {
        self.span.clone()
    }
    pub fn replacement(&self) -> &str {
        &self.replacement
    }
}

/// A proposal tied to the exact original bytes, not a checked program.
///
/// ```compile_fail
/// use vesper_fmt::recovery::RecoveryFormatResult;
/// let forged = RecoveryFormatResult { };
/// ```
#[derive(Debug)]
pub struct RecoveryFormatResult {
    output: String,
    edits: Vec<RecoveryEdit>,
    source_hash: [u8; 32],
    formatted_declarations: usize,
    changed: bool,
}
impl RecoveryFormatResult {
    pub fn output(&self) -> &str {
        &self.output
    }
    pub fn changed(&self) -> bool {
        self.changed
    }
    pub fn edits(&self) -> &[RecoveryEdit] {
        &self.edits
    }
    pub fn source_hash(&self) -> [u8; 32] {
        self.source_hash
    }
    pub fn formatted_declarations(&self) -> usize {
        self.formatted_declarations
    }
    pub fn into_output(self) -> String {
        self.output
    }
}

fn add(a: usize, b: usize) -> Result<usize, Error> {
    a.checked_add(b).ok_or(Error::Overflow)
}
fn mul(a: usize, b: usize) -> Result<usize, Error> {
    a.checked_mul(b).ok_or(Error::Overflow)
}
fn reserve<T>(items: usize, work: &mut Work) -> Result<Vec<T>, Error> {
    work.charge(mul(items, size_of::<T>())?)?;
    let mut values = Vec::new();
    values
        .try_reserve_exact(items)
        .map_err(|_| Error::Allocation)?;
    Ok(values)
}
fn map_error(error: syntax::Error) -> Error {
    use syntax::{Error as E, Resource as R};
    match error {
        E::Budget(R::Source) => Error::InputLimit,
        E::Budget(R::Tokens) => Error::TokenLimit,
        E::Budget(R::Depth) => Error::DepthLimit,
        E::Budget(R::Work) => Error::WorkLimit,
        E::Budget(_) => Error::NodeLimit,
        E::Allocation => Error::Allocation,
        E::Unsupported => Error::OpaqueMacro,
        E::InvalidLimits => Error::WorkLimit,
        E::InvalidSpan | E::CstMismatch | E::InvalidInventory => Error::Conservation,
    }
}
fn parser_limits(limits: Limits) -> syntax::Limits {
    syntax::Limits {
        parse: parser::ParseLimits {
            max_source_bytes: limits
                .max_input_bytes
                .min(syntax::Limits::HARD.parse.max_source_bytes),
            max_tokens: limits.max_tokens.min(MAX_TOKENS),
            max_diagnostics: 64,
            max_depth: limits.max_depth.min(24),
        },
        max_declarations: limits.max_doc_nodes.min(MAX_DECLARATIONS),
        max_sites: 256,
        max_metadata_bytes: 1024 * 1024,
        max_work: PARSER_WORK,
        max_identifier_bytes: 256,
    }
}
fn lex(source: &str, limits: Limits, work: &mut Work) -> Result<Vec<Token>, Error> {
    if source.len()
        > limits
            .max_input_bytes
            .min(syntax::Limits::HARD.parse.max_source_bytes)
    {
        return Err(Error::InputLimit);
    }
    if limits.max_tokens == 0 {
        return Err(Error::TokenLimit);
    }
    // Pay for scanning and the maximum possible bounded token staging before
    // the real lexer allocates. Lexical uncertainty refuses the whole buffer.
    work.charge(mul(source.len(), 4)?)?;
    work.charge(mul(
        add(source.len().min(limits.max_tokens.min(MAX_TOKENS)), 1)?,
        size_of::<Token>(),
    )?)?;
    let lexed = lexer::lex_with_limits(
        source,
        lexer::LexLimits {
            max_source_bytes: limits
                .max_input_bytes
                .min(syntax::Limits::HARD.parse.max_source_bytes),
            max_tokens: limits.max_tokens.min(MAX_TOKENS),
            max_diagnostics: 1,
        },
    )
    .map_err(|_| Error::InputLimit)?;
    if let Some(limit) = lexed.limit {
        return Err(match limit.kind {
            lexer::LexLimitKind::Tokens => Error::TokenLimit,
            lexer::LexLimitKind::Depth => Error::DepthLimit,
            lexer::LexLimitKind::SourceBytes => Error::InputLimit,
            lexer::LexLimitKind::Diagnostics => Error::Syntax {
                offset: lexed
                    .diagnostics
                    .first()
                    .map_or(source.len(), |d| d.span.start),
            },
            lexer::LexLimitKind::Configuration => Error::WorkLimit,
        });
    }
    if let Some(error) = lexed.diagnostics.first() {
        return Err(Error::Syntax {
            offset: error.span.start,
        });
    }
    Ok(lexed.tokens)
}
fn snapshot(source: &str, limits: Limits, work: &mut Work) -> Result<RecoverySyntax, Error> {
    if limits.max_depth == 0 {
        return Err(Error::DepthLimit);
    }
    if limits.max_doc_nodes == 0 {
        return Err(Error::NodeLimit);
    }
    work.charge(PARSER_WORK)?;
    parser::parse_recovery(source, parser_limits(limits)).map_err(map_error)
}
struct Pass {
    output: String,
    edits: Vec<RecoveryEdit>,
    formatted_declarations: usize,
}
fn top_level_starts(
    tokens: &[Token],
    limits: Limits,
    work: &mut Work,
) -> Result<Vec<usize>, Error> {
    let mut starts = reserve::<usize>(tokens.len(), work)?;
    let mut stack = reserve::<K>(limits.max_depth.min(24), work)?;
    for token in tokens {
        work.charge(1)?;
        if token.kind.trivia() || token.kind == K::Eof {
            continue;
        }
        if stack.is_empty() {
            starts.push(token.span.start);
        }
        match token.kind {
            K::LParen | K::LBracket | K::LBrace => {
                if stack.len() >= limits.max_depth.min(24) {
                    return Err(Error::DepthLimit);
                }
                stack.push(token.kind);
            }
            K::RParen | K::RBracket | K::RBrace => {
                let expected = match token.kind {
                    K::RParen => K::LParen,
                    K::RBracket => K::LBracket,
                    _ => K::LBrace,
                };
                if stack.pop() != Some(expected) {
                    return Err(Error::Syntax {
                        offset: token.span.start,
                    });
                }
            }
            _ => {}
        }
    }
    // Unclosed delimiters are deliberately not repaired. Any apparent sibling
    // starting inside that open group is excluded, even if recovery resync gives
    // it a Complete declaration status.
    Ok(starts)
}
fn once(
    snapshot: &RecoverySyntax,
    tokens: &[Token],
    limits: Limits,
    work: &mut Work,
) -> Result<Pass, Error> {
    let top_level = top_level_starts(tokens, limits, work)?;
    let declarations = snapshot.declarations();
    let mut indices = reserve::<usize>(declarations.len(), work)?;
    for (index, declaration) in declarations.iter().enumerate() {
        work.charge(1)?;
        if declaration.kind == DeclarationKind::Function
            && declaration.status == DeclarationStatus::Complete
            && declaration.body_span.is_some()
            && top_level.binary_search(&declaration.span.start).is_ok()
        {
            indices.push(index);
        }
    }
    let mut edits = reserve::<RecoveryEdit>(indices.len(), work)?;
    let mut formatted_declarations = 0;
    if !indices.is_empty() {
        work.charge(PARSER_WORK)?;
        let actual = parser::parse_complete_items(snapshot, &indices, parser_limits(limits))
            .map_err(map_error)?;
        if actual.items.len() != indices.len()
            || actual
                .items
                .iter()
                .any(|item| !matches!(item, Item::Function(f) if f.body.is_some()))
        {
            return Err(Error::Conservation);
        }
        for (index, item) in indices.into_iter().zip(&actual.items) {
            let declaration = &declarations[index];
            let Item::Function(function) = item else {
                return Err(Error::Conservation);
            };
            if function.span != declaration.span {
                return Err(Error::Conservation);
            }
            let original = snapshot
                .source()
                .get(declaration.span.clone())
                .ok_or(Error::Conservation)?;
            let FormatResult { mut output, .. } = match format_with_work(original, limits, work) {
                Ok(result) => result,
                // A standalone declaration can lack an outer grammar context,
                // or contain an opaque payload. Leave that entire span intact.
                Err(Error::Syntax { .. } | Error::OpaqueMacro) => continue,
                Err(error) => return Err(error),
            };
            if !output.ends_with('\n') {
                return Err(Error::Conservation);
            }
            // The declaration ends before its outside trivia. Keeping the
            // formatter's extra terminal LF would grow that gap on every pass.
            output.pop();
            formatted_declarations = add(formatted_declarations, 1)?;
            work.charge(original.len().max(output.len()))?;
            if output != original {
                edits.push(RecoveryEdit {
                    span: declaration.span.clone(),
                    replacement: output,
                });
            }
        }
    }
    let mut size = snapshot.source().len();
    let mut end = 0;
    for edit in &edits {
        work.charge(1)?;
        if edit.span.start < end || snapshot.source().get(edit.span.clone()).is_none() {
            return Err(Error::Conservation);
        }
        end = edit.span.end;
        size = size
            .checked_sub(edit.span.len())
            .ok_or(Error::Conservation)?;
        size = add(size, edit.replacement.len())?;
    }
    if size > limits.max_output_bytes || size > syntax::Limits::HARD.parse.max_source_bytes {
        return Err(Error::OutputLimit);
    }
    work.charge(size)?;
    let mut output = String::new();
    output
        .try_reserve_exact(size)
        .map_err(|_| Error::Allocation)?;
    let mut cursor = 0;
    for edit in &edits {
        output.push_str(&snapshot.source()[cursor..edit.span.start]);
        output.push_str(&edit.replacement);
        cursor = edit.span.end;
    }
    output.push_str(&snapshot.source()[cursor..]);
    if output.len() != size {
        return Err(Error::Conservation);
    }
    Ok(Pass {
        output,
        edits,
        formatted_declarations,
    })
}
fn inventory_equal(a: &RecoverySyntax, b: &RecoverySyntax, work: &mut Work) -> Result<bool, Error> {
    work.charge(add(a.declarations().len(), b.declarations().len())?)?;
    work.charge(add(a.source().len(), b.source().len())?)?;
    if a.declarations().len() != b.declarations().len() || a.sites().len() != b.sites().len() {
        return Ok(false);
    }
    for (a_decl, b_decl) in a.declarations().iter().zip(b.declarations()) {
        if a_decl.kind != b_decl.kind
            || a_decl.status != b_decl.status
            || a_decl.body_span.is_some() != b_decl.body_span.is_some()
            || a_decl
                .name_span
                .as_ref()
                .map(|span| &a.source()[span.clone()])
                != b_decl
                    .name_span
                    .as_ref()
                    .map(|span| &b.source()[span.clone()])
        {
            return Ok(false);
        }
    }
    work.charge(add(a.sites().len(), b.sites().len())?)?;
    Ok(a.sites()
        .iter()
        .zip(b.sites())
        .all(|(a, b)| a.kind == b.kind && a.owner == b.owner))
}
/// Propose formatting only for syntax-complete independent top-level functions.
/// All opaque/damaged gaps are copied exactly. No source checking or execution
/// occurs, and zero eligible declarations returns an unchanged proposal.
pub fn format_recovery_source(source: &str, limits: Limits) -> Result<RecoveryFormatResult, Error> {
    let mut work = Work::new(limits.max_work);
    let before_tokens = lex(source, limits, &mut work)?;
    let before = snapshot(source, limits, &mut work)?;
    let first = once(&before, &before_tokens, limits, &mut work)?;
    let after_tokens = lex(
        &first.output,
        Limits {
            max_input_bytes: limits.max_output_bytes,
            ..limits
        },
        &mut work,
    )?;
    let after = snapshot(
        &first.output,
        Limits {
            max_input_bytes: limits.max_output_bytes,
            ..limits
        },
        &mut work,
    )?;
    work.charge(add(before_tokens.len(), after_tokens.len())?)?;
    work.charge(add(source.len(), first.output.len())?)?;
    let mut a = before_tokens
        .iter()
        .filter(|token| !matches!(token.kind, K::Whitespace | K::Eof));
    let mut b = after_tokens
        .iter()
        .filter(|token| !matches!(token.kind, K::Whitespace | K::Eof));
    loop {
        match (a.next(), b.next()) {
            (None, None) => break,
            (Some(a), Some(b))
                if a.kind == b.kind && source[a.span.clone()] == first.output[b.span.clone()] => {}
            _ => return Err(Error::Conservation),
        }
    }
    if !inventory_equal(&before, &after, &mut work)? {
        return Err(Error::Conservation);
    }
    let second = once(
        &after,
        &after_tokens,
        Limits {
            max_input_bytes: limits.max_output_bytes,
            ..limits
        },
        &mut work,
    )?;
    work.charge(first.output.len().max(second.output.len()))?;
    if first.output != second.output || !second.edits.is_empty() {
        return Err(Error::NotIdempotent);
    }
    Ok(RecoveryFormatResult {
        changed: source != first.output,
        output: first.output,
        edits: first.edits,
        source_hash: before.source_hash(),
        formatted_declarations: first.formatted_declarations,
    })
}
