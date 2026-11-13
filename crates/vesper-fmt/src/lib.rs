//! Canonical formatting of the currently supported lossless Vesper source grammar.
//! Every successful result is reparsed, token/comment conserved and idempotent.
//! Supported source expression macros are syntax-validated without execution.
//! Unsupported macro/quotation syntax and invalid/incomplete buffers are errors:
//! callers keep their original source, including all opaque payload trivia.
#![forbid(unsafe_code)]
mod doc;
mod macro_source;
mod proof_lean;
pub mod proof_model;
mod proof_source;
pub mod recovery;
pub use doc::{DEFAULT_LINE_WIDTH, Doc, DocId, Document};
pub use proof_model::{DocumentModel, ProofError, ProofLimits};
pub use proof_source::{FormatVerification, format_for_verification};
pub use recovery::{RecoveryEdit, RecoveryFormatResult, format_recovery_source};
use vesper_syntax::{
    lexer::{self, Token, TokenKind as K},
    parser,
};

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_input_bytes: usize,
    pub max_output_bytes: usize,
    pub max_tokens: usize,
    pub max_doc_nodes: usize,
    pub max_depth: usize,
    pub max_work: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 256 * 1024,
            max_output_bytes: 1024 * 1024,
            max_tokens: 8192,
            max_doc_nodes: 100_000,
            max_depth: 24,
            max_work: 128 * 1024 * 1024,
        }
    }
}
pub type FormatLimits = Limits;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    InputLimit,
    OutputLimit,
    TokenLimit,
    NodeLimit,
    DepthLimit,
    WorkLimit,
    Overflow,
    Allocation,
    InvalidDoc,
    Syntax { offset: usize },
    OpaqueMacro,
    Conservation,
    NotIdempotent,
}
pub type FormatError = Error;
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
#[derive(Debug)]
pub struct FormatResult {
    pub output: String,
    pub changed: bool,
}
pub(crate) struct Work {
    used: usize,
    max: usize,
}
impl Work {
    fn new(max: usize) -> Self {
        Self { used: 0, max }
    }
    fn charge(&mut self, n: usize) -> Result<(), Error> {
        self.used = self.used.checked_add(n).ok_or(Error::Overflow)?;
        if self.used > self.max {
            Err(Error::WorkLimit)
        } else {
            Ok(())
        }
    }
}
#[derive(Clone, Copy)]
struct Piece<'a> {
    kind: K,
    text: &'a str,
}
fn pieces<'a>(source: &'a str, tokens: &[Token]) -> Vec<Piece<'a>> {
    tokens
        .iter()
        .filter(|t| !matches!(t.kind, K::Whitespace | K::Eof))
        .map(|t| Piece {
            kind: t.kind,
            text: &source[t.span.clone()],
        })
        .collect()
}
fn guarded_lex(
    source: &str,
    limits: Limits,
    work: &mut Work,
    allow_macros: bool,
) -> Result<Vec<Token>, Error> {
    work.charge(source.len())?;
    let (tokens, errors) = lexer::lex(source);
    if let Some(error) = errors.first() {
        return Err(Error::Syntax {
            offset: error.span.start,
        });
    }
    if tokens.len() > limits.max_tokens.min(8192) {
        return Err(Error::TokenLimit);
    }
    let mut stack = Vec::new();
    let mut path_counts = vec![0usize];
    let mut previous = None;
    for token in &tokens {
        if token.kind.trivia() || token.kind == K::Eof {
            continue;
        }
        if !allow_macros && token.kind == K::Bang && matches!(previous, Some(K::Lower | K::Upper)) {
            return Err(Error::OpaqueMacro);
        }
        let text = &source[token.span.clone()];
        if !allow_macros && matches!(text, "quote" | "quasiquote" | "syntax_quote") {
            return Err(Error::OpaqueMacro);
        }
        // Count chains at every delimiter level, collapsing a nested group
        // into one parent component. Child commas/semicolons cannot reset an
        // ancestor's expression chain (e.g. f(1,2)+f(1,2)+...).
        let counter = path_counts.last_mut().ok_or(Error::Conservation)?;
        if matches!(token.kind, K::Semi | K::Comma) {
            *counter = 0;
        } else {
            *counter = counter.checked_add(1).ok_or(Error::Overflow)?;
        }
        if path_counts
            .iter()
            .try_fold(0usize, |n, count| n.checked_add(*count))
            .ok_or(Error::Overflow)?
            > 128
        {
            return Err(Error::DepthLimit);
        }
        match token.kind {
            K::LParen | K::LBracket | K::LBrace => {
                stack.push(token.kind);
                path_counts.push(0);
                if stack.len() > limits.max_depth.min(24) {
                    return Err(Error::DepthLimit);
                }
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
                path_counts.pop();
                if stack.is_empty() {
                    path_counts[0] = 0;
                }
            }
            _ => {}
        }
        previous = Some(token.kind);
    }
    if !stack.is_empty() {
        return Err(Error::Syntax {
            offset: source.len(),
        });
    }
    // Conservative deterministic preflight work units for existing parser calls.
    work.charge(
        tokens
            .len()
            .checked_mul(tokens.len())
            .ok_or(Error::Overflow)?,
    )?;
    Ok(tokens)
}
pub(crate) struct ModelCapture<'a> {
    pub(crate) limits: ProofLimits,
    pub(crate) model: &'a mut Option<DocumentModel>,
    pub(crate) error: &'a mut Option<ProofError>,
}
fn once(
    source: &str,
    limits: Limits,
    work: &mut Work,
    capture: Option<ModelCapture<'_>>,
) -> Result<(String, bool), Error> {
    let (tokens, macros) = match guarded_lex(source, limits, work, false) {
        Ok(tokens) => (tokens, false),
        Err(Error::OpaqueMacro) => (guarded_lex(source, limits, work, true)?, true),
        Err(error) => return Err(error),
    };
    let syntax = if macros {
        macro_source::syntax(source, &tokens, limits, work)?
    } else {
        let parsed = parser::parse(source);
        if let Some(error) = parsed.diagnostics.first() {
            return Err(Error::Syntax {
                offset: error.span.start,
            });
        }
        parsed.syntax()
    };
    // The CST is the lowering source. Exact source slices are anchored by each
    // lossless token's byte range; compare against lexer evidence before use.
    let mut atoms = Vec::new();
    let mut expected = tokens.iter().filter(|t| t.kind != K::Eof);
    for element in syntax.descendants_with_tokens() {
        if let Some(token) = element.into_token() {
            let start = usize::from(token.text_range().start());
            let end = usize::from(token.text_range().end());
            let evidence = expected.next().ok_or(Error::Conservation)?;
            if evidence.span != (start..end) {
                return Err(Error::Conservation);
            }
            let kind = evidence.kind;
            if token.text() != &source[start..end] || token.kind().0 != kind as u16 {
                return Err(Error::Conservation);
            }
            if kind != K::Whitespace {
                atoms.push(Piece {
                    kind,
                    text: &source[start..end],
                });
            }
        }
    }
    if expected.next().is_some() {
        return Err(Error::Conservation);
    }
    let original = pieces(source, &tokens);
    if atoms.len() != original.len()
        || atoms
            .iter()
            .zip(&original)
            .any(|(a, b)| a.kind != b.kind || a.text != b.text)
    {
        return Err(Error::Conservation);
    }
    let mut doc_limits = limits;
    doc_limits.max_work = work.max - work.used;
    let mut document = Document::new(doc_limits)?;
    let mut pos = 0;
    let root = sequence(&atoms, &mut pos, None, false, &mut document)?;
    let end = document.push(Doc::HardLine)?;
    let root = document.push(Doc::Concat(vec![root, end]))?;
    let output = document.render(root)?;
    work.charge(document.work_used())?;
    work.charge(output.len())?;
    if let Some(capture) = capture {
        match document.proof_model(root, capture.limits) {
            Ok(model) => *capture.model = Some(model),
            Err(error) => {
                *capture.error = Some(error);
                // The verification adapter returns the precise projection
                // error. Ordinary formatting never enters this capture path.
                return Err(Error::Conservation);
            }
        }
    }
    Ok((output, macros))
}
/// Width is a 100 Unicode-scalar ribbon with four-space nesting. Indivisible
/// token/comment bytes are never wrapped, so those leaves may exceed the ribbon.
pub fn format_source(source: &str, limits: Limits) -> Result<FormatResult, Error> {
    let mut work = Work::new(limits.max_work);
    format_with_work(source, limits, &mut work)
}
pub(crate) fn format_with_work(
    source: &str,
    limits: Limits,
    work: &mut Work,
) -> Result<FormatResult, Error> {
    format_with_capture(source, limits, work, None)
}
pub(crate) fn format_with_capture(
    source: &str,
    limits: Limits,
    work: &mut Work,
    capture: Option<ModelCapture<'_>>,
) -> Result<FormatResult, Error> {
    if source.len() > limits.max_input_bytes {
        return Err(Error::InputLimit);
    }
    let (output, macros) = once(source, limits, work, capture)?;
    let original = guarded_lex(source, limits, work, macros)?;
    let rewritten = guarded_lex(&output, limits, work, macros)?;
    let a = pieces(source, &original);
    let b = pieces(&output, &rewritten);
    if a.len() != b.len()
        || a.iter()
            .zip(&b)
            .any(|(a, b)| a.kind != b.kind || a.text != b.text)
    {
        return Err(Error::Conservation);
    }
    let (second, second_macros) = once(&output, limits, work, None)?;
    if second != output || macros != second_macros {
        return Err(Error::NotIdempotent);
    }
    Ok(FormatResult {
        changed: source != output,
        output,
    })
}
fn concat_group<'a>(
    ids: &mut Vec<DocId>,
    document: &mut Document<'a>,
    out: &mut Vec<DocId>,
) -> Result<(), Error> {
    if !ids.is_empty() {
        let node = document.push(Doc::Concat(std::mem::take(ids)))?;
        out.push(document.push(Doc::Group(node))?);
    }
    Ok(())
}
#[derive(Clone, Copy)]
enum Gap {
    None,
    Soft,
    Empty,
    Hard,
}
fn gap(previous: Piece<'_>, current: Piece<'_>, block: bool, after_block: bool) -> Gap {
    if previous.kind == K::Dollar
        || current.kind == K::Bang && matches!(previous.kind, K::Lower | K::Upper)
    {
        return Gap::None;
    }
    if previous.kind == K::Comment || previous.kind == K::Semi {
        return Gap::Hard;
    }
    if previous.kind == K::Comma {
        return if block { Gap::Hard } else { Gap::Soft };
    }
    if after_block
        && !matches!(
            current.kind,
            K::Comma | K::Semi | K::RParen | K::RBracket | K::Dot | K::Question | K::Else | K::With
        )
    {
        return Gap::Hard;
    }
    if matches!(current.kind, K::Comment | K::BlockComment) || previous.kind == K::BlockComment {
        return Gap::Soft;
    }
    if current.kind == K::Dot && matches!(previous.kind, K::Int | K::Float) {
        return Gap::Soft;
    }
    if matches!(current.kind, K::Dot | K::ColonColon) {
        return Gap::Empty;
    }
    if matches!(
        current.kind,
        K::Comma
            | K::Semi
            | K::Colon
            | K::Dot
            | K::ColonColon
            | K::Question
            | K::RParen
            | K::RBracket
    ) {
        return Gap::None;
    }
    if matches!(previous.kind, K::Dot | K::ColonColon | K::At | K::AtAt) {
        return if previous.kind == K::At && current.kind == K::At {
            Gap::Soft
        } else {
            Gap::None
        };
    }
    if current.kind == K::LParen
        && !matches!(previous.kind, K::If | K::Match | K::In | K::Inout | K::Sink)
    {
        return Gap::None;
    }
    if current.kind == K::LBracket && previous.kind != K::Eff {
        return Gap::None;
    }
    Gap::Soft
}
fn sequence<'a>(
    atoms: &[Piece<'a>],
    pos: &mut usize,
    end: Option<K>,
    block: bool,
    document: &mut Document<'a>,
) -> Result<DocId, Error> {
    let mut output = Vec::new();
    let mut line = Vec::new();
    let mut previous: Option<Piece<'a>> = None;
    let mut previous_block = false;
    while *pos < atoms.len() && Some(atoms[*pos].kind) != end {
        let current = atoms[*pos];
        let is_block = current.kind == K::LBrace && previous.is_none_or(|p| p.kind != K::Dot);
        let spacing = previous.map_or(Gap::None, |p| gap(p, current, block, previous_block));
        match spacing {
            Gap::Hard => {
                concat_group(&mut line, document, &mut output)?;
                output.push(document.push(Doc::HardLine)?);
            }
            Gap::Soft if !is_block => line.push(document.push(Doc::SoftLineOrSpace)?),
            Gap::Empty => line.push(document.push(Doc::SoftLineOrEmpty)?),
            _ => {}
        }
        if is_block {
            concat_group(&mut line, document, &mut output)?;
            if matches!(spacing, Gap::Soft) {
                output.push(document.push(Doc::Text(" "))?);
            }
        }
        *pos += 1;
        let node = if matches!(current.kind, K::LParen | K::LBracket | K::LBrace) {
            let close = match current.kind {
                K::LParen => K::RParen,
                K::LBracket => K::RBracket,
                _ => K::RBrace,
            };
            let empty = atoms.get(*pos).is_some_and(|a| a.kind == close);
            let start = document.push(Doc::Text(current.text))?;
            let inner = sequence(atoms, pos, Some(close), is_block, document)?;
            let last_is_line_comment = *pos > 0 && atoms[*pos - 1].kind == K::Comment;
            let closing = atoms
                .get(*pos)
                .filter(|a| a.kind == close)
                .ok_or(Error::Conservation)?;
            *pos += 1;
            let finish = document.push(Doc::Text(closing.text))?;
            if empty {
                document.push(Doc::Concat(vec![start, finish]))?
            } else {
                let lead = document.push(if is_block {
                    Doc::HardLine
                } else {
                    Doc::SoftLineOrEmpty
                })?;
                let content = document.push(Doc::Concat(vec![lead, inner]))?;
                let nest = document.push(Doc::Nest(4, content))?;
                let trail = document.push(if is_block || last_is_line_comment {
                    Doc::HardLine
                } else {
                    Doc::SoftLineOrEmpty
                })?;
                let all = document.push(Doc::Concat(vec![start, nest, trail, finish]))?;
                document.push(Doc::Group(all))?
            }
        } else {
            document.push(if matches!(current.kind, K::Comment | K::BlockComment) {
                Doc::Comment(current.text)
            } else {
                Doc::Text(current.text)
            })?
        };
        if is_block {
            output.push(node);
        } else {
            line.push(node);
        }
        previous_block = is_block;
        previous = Some(Piece {
            kind: match current.kind {
                K::LParen => K::RParen,
                K::LBracket => K::RBracket,
                K::LBrace => K::RBrace,
                _ => current.kind,
            },
            text: current.text,
        });
    }
    concat_group(&mut line, document, &mut output)?;
    document.push(Doc::Concat(output))
}
