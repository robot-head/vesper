//! Doctest fence extraction and the token preflight of `vesper test --doc`
//! behind a swap point (self-hosting SH5 lane 14, #1337).
//!
//! The Rust implementation (moved here unchanged from `src/doctest.rs`) is
//! the default [`DocTestExtractTable`]. `@vesper/test-runner`
//! (packages/vesper-test-runner/src/doctest.ves) implements the same two
//! functions; tests/self_host_doctest_extract.rs runs both on the
//! repository's corpus and requires equal results.

use crate::lexer::{self, TokenKind as K};
use vesper_doc::sim::SimSpec;

/// The limits [`preflight`] enforces (`DocTestLimits` fields).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreflightLimits {
    pub max_tokens: usize,
    pub max_nesting: usize,
    pub max_segment_tokens: usize,
    pub max_declarations: usize,
}

/// The limits [`extract`] enforces (`DocTestLimits` fields).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtractLimits {
    pub max_snippet_bytes: usize,
    pub max_examples: usize,
}

/// One code line of an example: its text (the doc comment after `///` and
/// one space) and that text's byte offset in the source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub offset: usize,
}

/// One `vesper` fence of a doc comment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Example {
    pub lines: Vec<Line>,
    /// The byte offset of the opening fence line's text.
    pub offset: usize,
    /// `compile_fail(E....)`.
    pub expected: Option<String>,
    /// `entry=name`.
    pub entry: Option<String>,
    /// `sim(...)`.
    pub sim: Option<SimSpec>,
}

/// The swappable functions.
#[derive(Clone, Copy)]
pub struct DocTestExtractTable {
    /// [`preflight`].
    pub preflight: fn(&str, PreflightLimits) -> Result<(), String>,
    /// [`extract`].
    pub extract: fn(&str, ExtractLimits) -> Result<Vec<Example>, String>,
}

/// The Rust implementation, the default [`DocTestExtractTable`].
pub const RUST_DOCTEST_EXTRACT: DocTestExtractTable = DocTestExtractTable {
    preflight: rust_preflight,
    extract: rust_extract,
};

static TABLE: std::sync::OnceLock<DocTestExtractTable> = std::sync::OnceLock::new();

/// Replace [`RUST_DOCTEST_EXTRACT`] for the rest of the process. Only the
/// first call takes effect; a later one returns its table back.
pub fn install_doctest_extract(table: DocTestExtractTable) -> Result<(), DocTestExtractTable> {
    TABLE.set(table)
}

fn table() -> &'static DocTestExtractTable {
    TABLE.get().unwrap_or(&RUST_DOCTEST_EXTRACT)
}

/// The token, declaration, nesting and expression-segment limits of
/// `source`, checked before it is parsed.
pub fn preflight(source: &str, limits: PreflightLimits) -> Result<(), String> {
    (table().preflight)(source, limits)
}

/// The `vesper` fences of the doc comments (`///`, `//!`) of `source`.
pub fn extract(source: &str, limits: ExtractLimits) -> Result<Vec<Example>, String> {
    (table().extract)(source, limits)
}

/// Lexer/token/declaration/nested-segment limits are checked BEFORE recursive
/// parsing. Segment weight includes unfinished outer groups, preventing long
/// Pratt/operator spines hidden inside nested blocks from evading the guard.
pub fn rust_preflight(source: &str, limits: PreflightLimits) -> Result<(), String> {
    let (tokens, _) = lexer::lex(source);
    let mut count = 0usize;
    let mut declarations = 0usize;
    // (unfinished outer segment, maximum completed statement, opener).
    let mut stack: Vec<(usize, usize, K)> = Vec::new();
    let mut segment = 0usize;
    let mut top_item = false;
    for token in tokens {
        if matches!(
            token.kind,
            K::Whitespace | K::Comment | K::BlockComment | K::Eof
        ) {
            continue;
        }
        count = count.checked_add(1).ok_or("token count overflow")?;
        if matches!(
            token.kind,
            K::Fn | K::Struct | K::Enum | K::Effect | K::Impl
        ) {
            declarations += 1;
        }
        if count > limits.max_tokens || declarations > limits.max_declarations {
            return Err("doctest token/declaration limit".into());
        }
        if stack.is_empty()
            && (matches!(
                token.kind,
                K::Fn | K::Struct | K::Enum | K::Effect | K::Impl | K::Copy | K::Affine | K::Linear
            ) || token.kind == K::Lower
                && matches!(&source[token.span.clone()], "trait" | "model" | "pub"))
        {
            top_item = true;
        }
        let mut completed_item = false;
        match token.kind {
            K::LParen | K::LBrace | K::LBracket => {
                stack.push((segment, 0, token.kind));
                segment = 0;
            }
            K::RParen | K::RBrace | K::RBracket => {
                let (outer, peak, opener) = stack.pop().unwrap_or((0, 0, token.kind));
                segment = outer
                    .checked_add(segment.max(peak))
                    .and_then(|n| n.checked_add(1))
                    .ok_or("segment overflow")?;
                completed_item =
                    stack.is_empty() && top_item && opener == K::LBrace && token.kind == K::RBrace;
            }
            K::Semi => {
                if let Some((_, peak, _)) = stack.last_mut() {
                    *peak = (*peak).max(segment);
                }
                segment = 0;
                if stack.is_empty() {
                    top_item = false;
                }
            }
            _ => segment = segment.checked_add(1).ok_or("segment overflow")?,
        }
        let weighted = stack.iter().try_fold(segment, |n, (outer, _, _)| {
            n.checked_add(*outer).ok_or("segment overflow")
        })?;
        if stack.len() > limits.max_nesting || weighted > limits.max_segment_tokens {
            return Err("doctest nesting/expression-segment limit".into());
        }
        if completed_item {
            segment = 0;
            top_item = false;
        }
    }
    Ok(())
}
fn fence(text: &str) -> Option<(char, usize, &str)> {
    let text = text.trim();
    let first = text.chars().next()?;
    if first != '`' && first != '~' {
        return None;
    }
    let count = text.chars().take_while(|c| *c == first).count();
    (count >= 3).then_some((first, count, text[count..].trim()))
}
/// Split fence info on commas outside parentheses.
fn attribute_parts(info: &str) -> Result<Vec<&str>, String> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0usize, 0usize);
    for (i, c) in info.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth = depth
                    .checked_sub(1)
                    .ok_or("unbalanced doctest fence attribute")?
            }
            ',' if depth == 0 => {
                parts.push(info[start..i].trim());
                start = i + 1;
            }
            _ => (),
        }
    }
    if depth != 0 {
        return Err("unbalanced doctest fence attribute".into());
    }
    parts.push(info[start..].trim());
    Ok(parts)
}
type Attributes = (Option<String>, Option<String>, Option<SimSpec>);
fn attributes(info: &str) -> Result<Attributes, String> {
    let mut expected = None;
    let mut entry = None;
    let mut simulated = None;
    for part in attribute_parts(info)?.into_iter().skip(1) {
        if let Some(code) = part
            .strip_prefix("compile_fail(")
            .and_then(|s| s.strip_suffix(')'))
        {
            if expected.is_some()
                || code.len() != 5
                || !code.starts_with('E')
                || !code[1..].bytes().all(|b| b.is_ascii_digit())
            {
                return Err("invalid doctest diagnostic code".into());
            }
            expected = Some(code.to_string());
        } else if let Some(name) = part.strip_prefix("entry=") {
            if entry.is_some()
                || name.is_empty()
                || name.len() > 128
                || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                || name.as_bytes()[0].is_ascii_digit()
            {
                return Err("invalid doctest entry".into());
            }
            entry = Some(name.to_string());
        } else if let Some(inner) = part.strip_prefix("sim(").and_then(|s| s.strip_suffix(')')) {
            if simulated.is_some() {
                return Err("duplicate sim doctest attribute".into());
            }
            simulated = Some(SimSpec::parse(inner).map_err(|e| e.to_string())?);
        } else {
            return Err(format!(
                "unsupported Vesper doctest fence attribute '{part}'"
            ));
        }
    }
    if expected.is_some() && entry.is_some() {
        return Err("compile_fail doctest cannot request execution entry".into());
    }
    if expected.is_some() && simulated.is_some() {
        return Err("compile_fail doctest cannot request simulated execution".into());
    }
    Ok((expected, entry, simulated))
}
pub fn rust_extract(source: &str, limits: ExtractLimits) -> Result<Vec<Example>, String> {
    let (tokens, _) = lexer::lex(source);
    let mut blocks: Vec<Vec<Line>> = Vec::new();
    let mut previous = None;
    for t in tokens {
        if t.kind != K::Comment {
            continue;
        }
        let raw = &source[t.span.clone()];
        if !(raw.starts_with("///") && !raw.starts_with("////") || raw.starts_with("//!")) {
            previous = None;
            continue;
        }
        let offset = t.span.start + 3 + usize::from(raw[3..].starts_with(' '));
        if previous.is_none_or(|end| !source[end..t.span.start].chars().all(char::is_whitespace)) {
            blocks.push(Vec::new());
        }
        blocks.last_mut().expect("new doc block").push(Line {
            text: source[offset..t.span.end].to_string(),
            offset,
        });
        previous = Some(t.span.end);
    }
    let mut examples = Vec::new();
    for block in blocks {
        let mut active: Option<(char, usize, Option<Example>)> = None;
        let mut code_bytes = 0usize;
        for line in block {
            if let Some((kind, width, example)) = &mut active {
                if fence(&line.text)
                    .is_some_and(|(c, n, info)| c == *kind && n >= *width && info.is_empty())
                {
                    if let Some(e) = example.take() {
                        examples.push(e);
                        if examples.len() > limits.max_examples {
                            return Err("doctest example limit".into());
                        }
                    }
                    active = None;
                    continue;
                }
                if let Some(e) = example {
                    code_bytes = code_bytes
                        .checked_add(line.text.len())
                        .and_then(|n| n.checked_add(1))
                        .ok_or("snippet byte overflow")?;
                    if code_bytes > limits.max_snippet_bytes {
                        return Err("doctest snippet byte limit".into());
                    }
                    e.lines.push(line);
                }
            } else if let Some((kind, width, info)) = fence(&line.text) {
                let language = info.split(',').next().unwrap_or("").trim();
                if language != "vesper" && language.split_whitespace().next() == Some("vesper") {
                    return Err("unsupported Vesper doctest fence metadata (use comma-separated attributes)".into());
                }
                let example = if language == "vesper" {
                    let (expected, entry, sim) = attributes(info)?;
                    Some(Example {
                        lines: Vec::new(),
                        offset: line.offset,
                        expected,
                        entry,
                        sim,
                    })
                } else {
                    None
                };
                code_bytes = 0;
                active = Some((kind, width, example));
            }
        }
        if active.is_some_and(|(_, _, e)| e.is_some()) {
            return Err("unterminated Vesper doctest fence".into());
        }
    }
    Ok(examples)
}
