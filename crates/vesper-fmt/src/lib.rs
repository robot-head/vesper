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
/// Default token ceiling: twice the 64k-token target file, so a whole port
/// fits in one source file. Every token, trivia included, counts.
pub const DEFAULT_MAX_TOKENS: usize = 128 * 1024;
/// Work units charged per lexed token for each ordinary parser call. Parsing
/// is linear in the token count, so the charge is too (it used to be N²).
const PARSE_UNITS_PER_TOKEN: usize = 64;
/// Expression-chain components the preflight accepts before refusing input
/// that the parser's own height limit would reject anyway.
const MAX_CHAIN: usize = 128;
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 1024 * 1024,
            max_output_bytes: 4 * 1024 * 1024,
            max_tokens: DEFAULT_MAX_TOKENS,
            max_doc_nodes: 1_000_000,
            max_depth: 24,
            max_work: 512 * 1024 * 1024,
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
    /// The source whitespace before this token held a blank line.
    blank: bool,
    /// No whitespace separates this token from the previous one.
    glued: bool,
}
fn pieces<'a>(source: &'a str, tokens: &[Token]) -> Vec<Piece<'a>> {
    tokens
        .iter()
        .filter(|t| !matches!(t.kind, K::Whitespace | K::Eof))
        .map(|t| Piece {
            kind: t.kind,
            text: &source[t.span.clone()],
            blank: false,
            glued: false,
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
    if source.len() > lexer::HARD_SOURCE_BYTES {
        return Err(Error::InputLimit);
    }
    let lexed = lexer::lex_with_limits(
        source,
        lexer::LexLimits {
            max_source_bytes: lexer::HARD_SOURCE_BYTES,
            max_tokens: lexer::HARD_TOKENS,
            max_diagnostics: 1,
        },
    )
    .map_err(|_| Error::TokenLimit)?;
    if lexed.limit.is_some() {
        return Err(Error::TokenLimit);
    }
    if let Some(error) = lexed.diagnostics.first() {
        return Err(Error::Syntax {
            offset: error.span.start,
        });
    }
    let tokens = lexed.tokens;
    if tokens.len() > limits.max_tokens.min(lexer::HARD_TOKENS) {
        return Err(Error::TokenLimit);
    }
    let mut stack = Vec::new();
    let mut path_counts = vec![0usize];
    let mut previous = None;
    // A block closed at the current level; the next token there decides
    // whether the chain continues (`{..} + x`, `}.f()`, `} else`) or a new
    // item or statement starts (`} fn g()`, `} let x`).
    let mut after_block = false;
    let mut scopes = ScopeCopies::default();
    for token in &tokens {
        if token.kind.trivia() || token.kind == K::Eof {
            continue;
        }
        scopes.visit(token.kind, &source[token.span.clone()], stack.len())?;
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
        if std::mem::take(&mut after_block) && !continues_chain(token.kind, text) {
            *counter = 0;
        }
        if matches!(token.kind, K::Semi | K::Comma) {
            *counter = 0;
        } else {
            *counter = counter.checked_add(1).ok_or(Error::Overflow)?;
        }
        if path_counts
            .iter()
            .try_fold(0usize, |n, count| n.checked_add(*count))
            .ok_or(Error::Overflow)?
            > MAX_CHAIN
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
                after_block = token.kind == K::RBrace;
            }
            _ => {}
        }
        if stack.is_empty() && matches!(token.kind, K::RBrace | K::Semi) {
            scopes.end_item()?;
        }
        previous = Some(token.kind);
    }
    scopes.flush()?;
    if !stack.is_empty() {
        return Err(Error::Syntax {
            offset: source.len(),
        });
    }
    // Deterministic preflight work units for the parser call that follows:
    // linear per token, plus its scope-set copies (see `ScopeCopies`).
    work.charge(
        tokens
            .len()
            .checked_mul(PARSE_UNITS_PER_TOKEN)
            .ok_or(Error::Overflow)?,
    )?;
    work.charge(
        scopes
            .total
            .checked_mul(SCOPE_COPY_UNITS)
            .ok_or(Error::Overflow)?,
    )?;
    Ok(tokens)
}
/// Work units per copied binding name in the parser's scope sets.
const SCOPE_COPY_UNITS: usize = 4;
/// An upper bound on the parser's scope-set copies. The parser clones the
/// set of bound names on entry to every function, block, `for`, match arm,
/// closure and handler clause, and that set grows with each binding of the
/// enclosing function, so one function with thousands of block-initialized
/// locals costs quadratic time. Per function (a top-level function, or a
/// method of a top-level `impl`/`trait`/`model` block, whose scope is
/// restored when it ends), copy sites times identifier tokens bounds it.
/// Ordinary functions are small, so this stays near-linear for real files.
#[derive(Default)]
struct ScopeCopies {
    sites: usize,
    names: usize,
    total: usize,
    /// The current top-level item is a function, so a `fn` one level down is
    /// nested in its body and still sees its bindings.
    in_function: bool,
}
impl ScopeCopies {
    fn visit(&mut self, kind: K, text: &str, depth: usize) -> Result<(), Error> {
        if kind == K::Fn && (depth == 0 || depth == 1 && !self.in_function) {
            self.flush()?;
            self.in_function |= depth == 0;
        }
        let site = matches!(kind, K::Fn | K::LBrace | K::FatArrow | K::Pipe)
            || kind == K::Lower && matches!(text, "for" | "handle" | "match");
        if site {
            self.sites = self.sites.checked_add(1).ok_or(Error::Overflow)?;
        }
        if kind == K::Lower {
            self.names = self.names.checked_add(1).ok_or(Error::Overflow)?;
        }
        Ok(())
    }
    fn end_item(&mut self) -> Result<(), Error> {
        self.in_function = false;
        self.flush()
    }
    fn flush(&mut self) -> Result<(), Error> {
        let copies = self.sites.checked_mul(self.names).ok_or(Error::Overflow)?;
        self.total = self.total.checked_add(copies).ok_or(Error::Overflow)?;
        self.sites = 0;
        self.names = 0;
        Ok(())
    }
}
/// Whether a token right after a closing `}` extends the same expression, so
/// the preflight keeps counting its chain. Anything else starts a new item or
/// statement. Unknown cases count as continuations, which only refuses more.
fn continues_chain(kind: K, text: &str) -> bool {
    is_binary_operator(kind)
        || matches!(
            kind,
            K::Dot
                | K::Question
                | K::Else
                | K::With
                | K::LParen
                | K::LBracket
                | K::DotDot
                | K::DotDotEq
                | K::Eq
                | K::PlusEq
                | K::MinusEq
                | K::StarEq
                | K::SlashEq
                | K::PercentEq
                | K::Arrow
                | K::FatArrow
                | K::Colon
                | K::ColonColon
        )
        || text == "as"
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
        let parsed = parser::parse_with_limits(
            source,
            parser::ParseLimits {
                max_source_bytes: lexer::HARD_SOURCE_BYTES,
                max_tokens: lexer::HARD_TOKENS,
                ..Default::default()
            },
        )
        .map_err(|_| Error::TokenLimit)?;
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
    let mut blank = false;
    let mut glued = false;
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
            if kind == K::Whitespace {
                blank |= source[start..end].matches('\n').nth(1).is_some();
                glued = false;
            } else {
                atoms.push(Piece {
                    kind,
                    text: &source[start..end],
                    blank: std::mem::take(&mut blank),
                    glued: std::mem::replace(&mut glued, true),
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
    /// Break point before an infix operator; nested one level when broken.
    Operator,
    /// Break point before `->`, in its own group so it breaks only when the
    /// return type cannot follow an already laid-out parameter list.
    Return,
    Hard,
}
fn is_binary_operator(kind: K) -> bool {
    matches!(
        kind,
        K::Plus
            | K::Minus
            | K::Star
            | K::Slash
            | K::Percent
            | K::EqEq
            | K::Ne
            | K::Lt
            | K::Le
            | K::Gt
            | K::Ge
            | K::And
            | K::Or
            | K::Amp
            | K::Caret
            | K::Shl
            | K::Shr
    )
}
/// Whether a token can start the upper bound of a range.
fn starts_range_bound(kind: K) -> bool {
    matches!(
        kind,
        K::Int | K::Float | K::Lower | K::Upper | K::LParen | K::Minus | K::SelfKw
    )
}
/// Whether a token can end an operand, so a following `-`/`*` is infix.
fn ends_operand(kind: K) -> bool {
    matches!(
        kind,
        K::Lower
            | K::Upper
            | K::Int
            | K::Float
            | K::String
            | K::True
            | K::False
            | K::SelfKw
            | K::RParen
            | K::RBracket
            | K::RBrace
    )
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
    if current.kind == K::Dot {
        return Gap::Empty;
    }
    if current.kind == K::ColonColon {
        return Gap::None;
    }
    // Ranges (`for i in 0..n`) are written tight on both sides.
    if matches!(current.kind, K::DotDot | K::DotDotEq) && ends_operand(previous.kind)
        || matches!(previous.kind, K::DotDot | K::DotDotEq) && starts_range_bound(current.kind)
    {
        return Gap::None;
    }
    if current.kind == K::Arrow {
        return Gap::Return;
    }
    if is_binary_operator(current.kind) && ends_operand(previous.kind) {
        return Gap::Operator;
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
    // Calls, tuple-struct patterns, macro invocations and generic arguments
    // attach to what they apply to; after a keyword or operator they are a
    // separate parenthesized/bracketed operand.
    if matches!(current.kind, K::LParen | K::LBracket)
        && (ends_operand(previous.kind) || matches!(previous.kind, K::Bang | K::Fn))
    {
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
    // A delimited, non-block list breaks at its comma separators: those soft
    // lines belong to the enclosing delimiter group, and every item is its own
    // group. Ordinary spaces never break, so an overlong line breaks its
    // delimiters (and member-chain dots) before anything else.
    let list = end.is_some() && !block;
    let mut output = Vec::new();
    let mut line = Vec::new();
    let mut previous: Option<Piece<'a>> = None;
    let mut previous_block = false;
    let mut previous_unary = false;
    // Inside a package name `@scope/name` (`use` and trait paths), whose
    // tokens must stay adjacent for the parser.
    let mut package = false;
    while *pos < atoms.len() && Some(atoms[*pos].kind) != end {
        let current = atoms[*pos];
        let is_block = current.kind == K::LBrace && previous.is_none_or(|p| p.kind != K::Dot);
        package = current.glued
            && previous.is_some_and(|p| {
                (p.kind == K::At && current.kind == K::Lower)
                    || (package && matches!(current.kind, K::Lower | K::Minus | K::Slash | K::Int))
            });
        let spacing = if previous_unary || package {
            Gap::None
        } else {
            previous.map_or(Gap::None, |p| gap(p, current, block, previous_block))
        };
        let after_comma = previous.is_some_and(|p| p.kind == K::Comma);
        let near_comment = matches!(current.kind, K::Comment | K::BlockComment)
            || previous.is_some_and(|p| p.kind == K::BlockComment);
        match spacing {
            Gap::Hard => {
                concat_group(&mut line, document, &mut output)?;
                // Keep at most one blank line where the source had one or more
                // between two items; the bare newline leaves no indentation.
                if current.blank {
                    output.push(document.push(Doc::Text("\n"))?);
                }
                output.push(document.push(Doc::HardLine)?);
            }
            Gap::Soft if list && after_comma => {
                concat_group(&mut line, document, &mut output)?;
                output.push(document.push(Doc::SoftLineOrSpace)?);
            }
            Gap::Soft if !is_block && near_comment => {
                line.push(document.push(Doc::SoftLineOrSpace)?)
            }
            Gap::Soft if !is_block => line.push(document.push(Doc::Text(" "))?),
            Gap::Return => {
                concat_group(&mut line, document, &mut output)?;
                let lead = document.push(Doc::SoftLineOrSpace)?;
                line.push(document.push(Doc::Nest(4, lead))?);
            }
            Gap::Empty | Gap::Operator => {
                let lead = document.push(if matches!(spacing, Gap::Empty) {
                    Doc::SoftLineOrEmpty
                } else {
                    Doc::SoftLineOrSpace
                })?;
                line.push(document.push(Doc::Nest(4, lead))?);
            }
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
            // The formatter never adds or removes tokens, so a source trailing
            // comma is kept and selects the vertical layout.
            let trailing_comma = !is_block && *pos > 0 && atoms[*pos - 1].kind == K::Comma;
            let closing = atoms
                .get(*pos)
                .filter(|a| a.kind == close)
                .ok_or(Error::Conservation)?;
            *pos += 1;
            let finish = document.push(Doc::Text(closing.text))?;
            if empty {
                document.push(Doc::Concat(vec![start, finish]))?
            } else {
                let lead = document.push(if is_block || trailing_comma {
                    Doc::HardLine
                } else {
                    Doc::SoftLineOrEmpty
                })?;
                let content = document.push(Doc::Concat(vec![lead, inner]))?;
                let nest = document.push(Doc::Nest(4, content))?;
                let trail =
                    document.push(if is_block || last_is_line_comment || trailing_comma {
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
        // A prefix `-`/`!` binds to its operand with no space.
        previous_unary = matches!(current.kind, K::Minus | K::Bang | K::Tilde)
            && previous.is_none_or(|p| !ends_operand(p.kind));
        previous = Some(Piece {
            kind: match current.kind {
                K::LParen => K::RParen,
                K::LBracket => K::RBracket,
                K::LBrace => K::RBrace,
                _ => current.kind,
            },
            text: current.text,
            blank: false,
            glued: current.glued,
        });
    }
    concat_group(&mut line, document, &mut output)?;
    document.push(Doc::Concat(output))
}
