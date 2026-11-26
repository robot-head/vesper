//! Canonical formatting of the currently supported lossless Vesper source grammar.
//! Every successful result is reparsed, token/comment conserved and idempotent.
//! Supported source expression macros are syntax-validated without execution.
//! Unsupported macro/quotation syntax and invalid/incomplete buffers are errors:
//! callers keep their original source, including all opaque payload trivia.
// The seed build (the compiler's copy) stays safe. The port-backed build
// links the Vesper port through the generated binding, which calls the C ABI.
#![cfg_attr(not(vesper_port), forbid(unsafe_code))]
#![cfg_attr(vesper_port, deny(unsafe_code))]
mod doc;
mod macro_source;
mod proof_lean;
pub mod proof_model;
mod proof_source;
pub mod recovery;
mod sugar_layout;
mod surface;
// Generated and committed, but kept outside src/ so formatters, which check
// every file under src/, never reflow it (see the porting recipe).
#[cfg(vesper_port)]
#[allow(unsafe_code)]
#[rustfmt::skip]
#[path = "../port/binding.rs"]
mod port_binding;
#[cfg(vesper_port)]
mod port;

/// Which build this is: `"vesper"` for `vesper-fmt` with the `vesper-port`
/// feature, `"rust"` otherwise (and for the seed copy `vesper-fmt-seed` that
/// the compiler links).
pub const IMPLEMENTATION: &str = if cfg!(vesper_port) { "vesper" } else { "rust" };

/// Live allocations of the port's runtime, for leak tests.
#[cfg(vesper_port)]
#[doc(hidden)]
pub fn port_live_allocations() -> i64 {
    port::live_allocations()
}

/// [`format_source`] through the port alone, for the differential tests:
/// `None` when the source is one the Rust code formats (the macro
/// profile).
#[cfg(vesper_port)]
#[doc(hidden)]
pub fn port_format_source(source: &str, limits: Limits) -> Option<Result<FormatResult, Error>> {
    port::format_source(source, limits)
}
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
/// Expression height (summed over the delimiter levels of the path) the
/// preflight accepts before refusing input that the parser's own height
/// limit (128) would reject anyway. The estimate also counts the enclosing
/// levels' structural steps (a parameter list, `->`, a body `{`), so it
/// keeps some slack over the parser's limit: every chain the parser admits
/// formats, and the parser refuses the few the slack lets through.
const MAX_CHAIN: usize = 160;
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
    /// Grammar-sensitive boundary from the unexpanded surface CST.
    sugar_gap: Option<Gap>,
    /// The token's CST parent is an expression or pattern node, where a `{`
    /// right after a type name opens a struct literal or pattern.
    expr: bool,
    /// The `{` of a tagged sugar call (`sum{ .. }`, `zip{ .. }`), from the
    /// surface CST.
    tagged: bool,
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
            sugar_gap: None,
            expr: false,
            tagged: false,
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
    let lexed = lexer::sugar_lex_with_limits(
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
    // Per delimiter level: an estimate of the expression height there, the
    // binary operators of the current chain plus the postfix steps of its
    // current operand (`ChainStep`). A long homogeneous chain such as
    // `f(x) || f(x) || ..` counts one per operator, as the parser's own
    // height limit does, not one per token.
    let mut path_counts = vec![0usize];
    // Per level: the postfix steps of the current operand, included above.
    let mut operand_steps = vec![0usize];
    // Per delimiter level: the chain count where the latest `if` that does
    // not continue an `else` started.
    let mut if_bases: Vec<Option<usize>> = vec![None];
    let mut previous = None;
    // A block closed at the current level; the next token there decides
    // whether the chain continues (`{..} + x`, `}.f()`, `} else`) or a new
    // item or statement starts (`} fn g()`, `} let x`).
    let mut after_block = false;
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
        let steps = operand_steps.last_mut().ok_or(Error::Conservation)?;
        let closed_block = std::mem::take(&mut after_block);
        if closed_block && token.kind == K::Else {
            // `} else` goes back to where its `if` started: every arm of an
            // `else if` chain is one component of the same expression, so a
            // chain of any length counts like a single `if`.
            if let Some(base) = if_bases.last().copied().flatten() {
                *counter = base;
                *steps = 0;
            }
        } else if closed_block && !continues_chain(token.kind, text) {
            *counter = 0;
            *steps = 0;
        }
        if token.kind == K::If
            && previous != Some(K::Else)
            && let Some(base) = if_bases.last_mut()
        {
            *base = Some(*counter);
        }
        match chain_step(token.kind, text) {
            ChainStep::End => {
                *counter = 0;
                *steps = 0;
            }
            // A new operand of the chain: the previous operand's postfix
            // steps no longer add to the height.
            ChainStep::Operator => {
                *counter = counter
                    .checked_sub(*steps)
                    .and_then(|c| c.checked_add(1))
                    .ok_or(Error::Overflow)?;
                *steps = 0;
            }
            ChainStep::Postfix => {
                *counter = counter.checked_add(1).ok_or(Error::Overflow)?;
                *steps = steps.checked_add(1).ok_or(Error::Overflow)?;
            }
            ChainStep::Operand => {}
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
                operand_steps.push(0);
                if_bases.push(None);
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
                operand_steps.pop();
                if_bases.pop();
                if stack.is_empty() {
                    path_counts[0] = 0;
                    operand_steps[0] = 0;
                }
                after_block = token.kind == K::RBrace;
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
    // Deterministic preflight work units for the parser call that follows:
    // linear per token. The parser's scope sets are an undo trail, so one
    // function with thousands of locals is linear too.
    work.charge(
        tokens
            .len()
            .checked_mul(PARSE_UNITS_PER_TOKEN)
            .ok_or(Error::Overflow)?,
    )?;
    Ok(tokens)
}
/// How a token changes the preflight's height estimate at its level.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ChainStep {
    /// `;` or `,`: the expression at this level ends.
    End,
    /// An infix or prefix operator, `else`, a range or an arm arrow: the
    /// chain gains a level and a new operand starts.
    Operator,
    /// A postfix step of the current operand: `.f`, a call or index group,
    /// `?`, `as`, `with`.
    Postfix,
    /// A name, literal, keyword or closing delimiter: no height.
    Operand,
}
fn chain_step(kind: K, text: &str) -> ChainStep {
    if matches!(kind, K::Semi | K::Comma) {
        ChainStep::End
    } else if is_binary_operator(kind)
        || matches!(
            kind,
            K::Bang
                | K::Tilde
                | K::Else
                | K::DotDot
                | K::DotDotEq
                | K::Eq
                | K::PlusEq
                | K::MinusEq
                | K::StarEq
                | K::SlashEq
                | K::PercentEq
                | K::PipeEq
                | K::AmpEq
                | K::Arrow
                | K::FatArrow
        )
    {
        ChainStep::Operator
    } else if matches!(
        kind,
        K::Dot | K::LParen | K::LBracket | K::LBrace | K::Question | K::With
    ) || text == "as"
    {
        ChainStep::Postfix
    } else {
        ChainStep::Operand
    }
}
/// Whether a token right after a closing `}` extends the same expression, so
/// the preflight keeps counting its chain. Anything else starts a new item or
/// statement. Unknown cases count as continuations, which only refuses more.
fn continues_chain(kind: K, text: &str) -> bool {
    infix_precedence(kind).is_some()
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
) -> Result<(String, surface::Profile), Error> {
    let (tokens, macros) = match guarded_lex(source, limits, work, false) {
        Ok(tokens) => (tokens, false),
        Err(Error::OpaqueMacro) => (guarded_lex(source, limits, work, true)?, true),
        Err(error) => return Err(error),
    };
    let (syntax, profile) = surface::syntax(source, &tokens, limits, work, macros)?;
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
                    sugar_gap: None,
                    expr: token
                        .parent()
                        .is_some_and(|n| matches!(n.kind(), parser::EXPR | parser::PATTERN)),
                    tagged: false,
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
    if !profile.nodes.is_empty() {
        sugar_layout::annotate(source, &tokens, &mut atoms, &profile.nodes);
    }
    let mut doc_limits = limits;
    doc_limits.max_work = work.max - work.used;
    let mut document = Document::new(doc_limits)?;
    let mut pos = 0;
    let root = sequence(
        &atoms,
        &mut pos,
        None,
        false,
        false,
        false,
        0,
        &mut document,
    )?;
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
    Ok((output, profile))
}
/// Width is a 100 Unicode-scalar ribbon with four-space nesting. Indivisible
/// token/comment bytes are never wrapped, so those leaves may exceed the ribbon.
pub fn format_source(source: &str, limits: Limits) -> Result<FormatResult, Error> {
    // The native port handles executable grammar, sugar and test blocks
    // (over @vesper/front's surface CST); the macro profile stays here.
    if source.len() > limits.max_input_bytes {
        return Err(Error::InputLimit);
    }
    #[cfg(vesper_port)]
    if let Some(result) = port::format_source(source, limits) {
        return result;
    }
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
    let (output, profile) = once(source, limits, work, capture)?;
    let original = guarded_lex(source, limits, work, profile.allow_macro_tokens)?;
    let rewritten = guarded_lex(&output, limits, work, profile.allow_macro_tokens)?;
    let a = pieces(source, &original);
    let b = pieces(&output, &rewritten);
    if a.len() != b.len()
        || a.iter()
            .zip(&b)
            .any(|(a, b)| a.kind != b.kind || a.text != b.text)
    {
        return Err(Error::Conservation);
    }
    let (second, second_profile) = once(&output, limits, work, None)?;
    if second != output || profile != second_profile {
        return Err(Error::NotIdempotent);
    }
    Ok(FormatResult {
        changed: source != output,
        output,
    })
}
/// A statement or list item. Breaks at the same precedence share a group,
/// while tighter operands can still fit on a single continuation line.
#[derive(Default)]
struct Line {
    docs: Vec<DocId>,
    breaks: Vec<(usize, u8)>,
}
impl Line {
    fn push(&mut self, doc: DocId) {
        self.docs.push(doc);
    }
    fn push_break(&mut self, lead: DocId, precedence: u8) {
        self.breaks.push((self.docs.len(), precedence));
        self.docs.push(lead);
    }
    fn flush(&mut self, document: &mut Document<'_>, out: &mut Vec<DocId>) -> Result<(), Error> {
        if !self.docs.is_empty() {
            let group = if self.breaks.is_empty() {
                let concat = document.push(Doc::Concat(std::mem::take(&mut self.docs)))?;
                document.push(Doc::Group(concat))?
            } else {
                let node = self.layout(document, 0, self.docs.len(), &self.breaks)?;
                self.docs.clear();
                self.breaks.clear();
                node
            };
            out.push(group);
        }
        Ok(())
    }
    // Recursion is bounded by the number of precedence levels, independent
    // of the number of operators in a sequence.
    fn layout(
        &self,
        document: &mut Document<'_>,
        start: usize,
        end: usize,
        breaks: &[(usize, u8)],
    ) -> Result<DocId, Error> {
        let Some(weakest) = breaks.iter().map(|&(_, level)| level).min() else {
            return document.push(Doc::Concat(self.docs[start..end].to_vec()));
        };
        let mut segments = Vec::new();
        let mut operand = start;
        let mut child_breaks = 0;
        let mut lead = None;
        for (index, &(offset, level)) in breaks.iter().enumerate() {
            if level != weakest {
                continue;
            }
            if operand < offset {
                let rhs = self.layout(document, operand, offset, &breaks[child_breaks..index])?;
                segments.push(nest_continuation(document, lead, rhs)?);
            }
            lead = Some(self.docs[offset]);
            operand = offset + 1;
            child_breaks = index + 1;
        }
        let rhs = self.layout(document, operand, end, &breaks[child_breaks..])?;
        segments.push(nest_continuation(document, lead, rhs)?);
        let concat = document.push(Doc::Concat(segments))?;
        document.push(Doc::Group(concat))
    }
}

/// The member chain of the operand at the end of a `Line`: its flat width so
/// far and each `.` break with the width before it, so a chain breaks at its
/// dots like rustfmt. A short receiver keeps its first step (`self.f`), and a
/// chain whose one method call ends it keeps its dots on one line when that
/// call could not fit on a line of its own anyway: its arguments break instead
/// (`out.rows.push(Row {`).
struct Chain {
    width: Option<usize>,
    dots: Vec<(usize, Option<usize>)>,
    calls: usize,
    ends_with_call: bool,
}
impl Default for Chain {
    fn default() -> Self {
        Chain {
            width: Some(0),
            dots: Vec::new(),
            calls: 0,
            ends_with_call: false,
        }
    }
}
impl Chain {
    fn dot(&mut self, at: usize) {
        self.dots.push((at, self.width));
    }
    fn push(&mut self, width: Option<usize>, call: bool, kind: K) -> Result<(), Error> {
        self.width = match (self.width, width) {
            (Some(a), Some(b)) => Some(a.checked_add(b).ok_or(Error::Overflow)?),
            _ => None,
        };
        if call && !self.dots.is_empty() {
            self.calls += 1;
            self.ends_with_call = true;
        } else if !matches!(kind, K::Question | K::Semi | K::Comma) {
            self.ends_with_call = false;
        }
        Ok(())
    }
    fn finish(
        &mut self,
        line: &mut Line,
        indent: usize,
        document: &mut Document<'_>,
    ) -> Result<(), Error> {
        let chain = std::mem::take(self);
        let Some(&(first, _)) = chain.dots.first() else {
            return Ok(());
        };
        let before_last = chain.dots[chain.dots.len() - 1].1;
        // The last step's flat width on a line of its own, or `None` when it
        // holds a line break.
        let alone = match (chain.width, before_last) {
            (Some(w), Some(b)) => Some(
                indent
                    .checked_add(5)
                    .and_then(|i| i.checked_add(w - b))
                    .ok_or(Error::Overflow)?,
            ),
            _ => None,
        };
        let mut kept = 0;
        if chain.calls == 1 && chain.ends_with_call && alone.is_none_or(|w| w > DEFAULT_LINE_WIDTH)
        {
            kept = chain.dots.len();
        } else {
            while let Some(&(_, Some(root))) = chain.dots.get(kept) {
                if root > 4 {
                    break;
                }
                kept += 1;
            }
        }
        for &(at, _) in &chain.dots[..kept] {
            let (offset, _) = line.breaks[at];
            line.docs[offset] = document.push(Doc::Nil)?;
        }
        // No other break follows the chain's dots: an operator ends it.
        line.breaks.drain(first..first + kept);
        Ok(())
    }
}

fn nest_continuation(
    document: &mut Document<'_>,
    lead: Option<DocId>,
    content: DocId,
) -> Result<DocId, Error> {
    if let Some(lead) = lead {
        let continuation = document.push(Doc::Concat(vec![lead, content]))?;
        document.push(Doc::Nest(4, continuation))
    } else {
        Ok(content)
    }
}
#[derive(Clone, Copy)]
enum Gap {
    None,
    /// A space that must stay on this line, even beside a comment.
    Space,
    Soft,
    Empty,
    /// Break point before an infix operator; nested one level when broken.
    Operator(u8),
    /// Break point before `->`, in its own group so it breaks only when the
    /// return type cannot follow an already laid-out parameter list.
    Return,
    Hard,
}
// These levels follow the expression parser. Assignment is spaced separately
// and a pipe may introduce a closure, so only unambiguous infix gaps break.
fn is_binary_operator(kind: K) -> bool {
    infix_precedence(kind).is_some()
}
fn infix_precedence(kind: K) -> Option<u8> {
    Some(match kind {
        K::Or => 2,
        K::And => 3,
        K::EqEq | K::Ne => 4,
        K::Lt | K::Le | K::Gt | K::Ge => 5,
        K::Caret => 7,
        K::Amp => 8,
        K::Shl | K::Shr => 9,
        K::Plus | K::Minus => 10,
        K::Star | K::Slash | K::Percent => 11,
        _ => return None,
    })
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
            | K::Question
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
            K::Comma
                | K::Semi
                | K::RParen
                | K::RBracket
                | K::Dot
                | K::Question
                | K::Else
                | K::With
                | K::Eq
                | K::FatArrow
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
    if let Some(precedence) = infix_precedence(current.kind).filter(|_| ends_operand(previous.kind))
    {
        return Gap::Operator(precedence);
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
    // `eff (` never starts an effect row, so `eff` is a name there
    // (`fn eff(..)`, `r.eff()`; contextual `eff`, #1175). `effect (`,
    // `handle (`, `sink (` and `inout (` are calls of a value of that name
    // too (#1244).
    if current.kind == K::LParen
        && matches!(
            previous.kind,
            K::Eff | K::Effect | K::Handle | K::Sink | K::Inout
        )
    {
        return Gap::None;
    }
    Gap::Soft
}
/// Whether `effect`, `handle`, `sink`, `inout`, `copy` or `affine` is a name
/// for spacing: after `.`, `fn` or `let`, or before an infix operator or `?`
/// (a pass mode or keyword is never followed by one), so `s.sink[0]` and
/// `sink - 1` keep their usual spacing (#1244). `copy (` and `affine (` are
/// calls of a value of that name except after `@` (`fn @copy (u64)`), and
/// `copy [` indexes one (#1327).
fn word_used_as_name(previous: Option<K>, current: K, next: Option<K>) -> bool {
    let modal = matches!(current, K::Copy | K::Affine);
    (matches!(current, K::Effect | K::Handle | K::Sink | K::Inout) || modal)
        && (matches!(previous, Some(K::Dot | K::Fn | K::Let))
            || next.is_some_and(|n| is_binary_operator(n) || n == K::Question)
            || (modal
                && next.is_some_and(|n| matches!(n, K::LParen | K::LBracket))
                && !matches!(previous, Some(K::At | K::AtAt))))
}
/// The widest short `if` expression that stays on one line, as rustfmt's
/// `single_line_if_else_max_width`.
const SHORT_IF_ELSE_WIDTH: usize = 50;
/// Where a short `if c { a } else { b }` starting at `start` ends: the index
/// of its last `}`, when each branch holds one expression and no comment, its
/// one-line width is at most `SHORT_IF_ELSE_WIDTH`, and it ends the
/// expression it is in. rustfmt keeps such an `if` on one line.
fn short_if_else(atoms: &[Piece<'_>], start: usize, end: Option<K>) -> Option<usize> {
    // 0: condition, 1: first branch, 2: `else`, 3: its `{`, 4: second branch.
    let mut state = 0;
    let mut depth = 0usize;
    let mut tokens = 0usize;
    let mut width = 0usize;
    let mut previous: Option<K> = None;
    let mut unary = false;
    let mut i = start;
    let close = loop {
        let atom = atoms.get(i)?;
        let kind = atom.kind;
        // A comment, a statement, a separator or a sugar boundary (`else` on
        // its own line in a template) can break the line, so the `if` cannot
        // be one group.
        if matches!(kind, K::Comment | K::BlockComment | K::Semi)
            || i > start && atom.sugar_gap.is_some()
            || depth == 0 && matches!(kind, K::Comma | K::Arrow)
        {
            return None;
        }
        if let Some(p) = previous {
            let space = if matches!(p, K::LParen | K::LBracket)
                || matches!(kind, K::RParen | K::RBracket)
                || unary
            {
                false
            } else if p == K::LBrace || kind == K::RBrace {
                true
            } else {
                let previous = Piece { kind: p, ..*atom };
                !matches!(
                    gap(previous, *atom, false, p == K::RBrace),
                    Gap::None | Gap::Empty
                )
            };
            width = width.checked_add(usize::from(space))?;
        }
        width = width.checked_add(atom.text.chars().count())?;
        if width > SHORT_IF_ELSE_WIDTH {
            return None;
        }
        unary =
            matches!(kind, K::Minus | K::Bang | K::Tilde) && !previous.is_some_and(ends_operand);
        previous = Some(kind);
        match kind {
            K::LBrace if depth == 0 && (state == 0 && tokens > 1 || state == 3) => {
                state += 1;
                tokens = 0;
                depth += 1;
            }
            K::LBrace => return None,
            K::RBrace if depth == 1 && matches!(state, 1 | 4) => {
                if tokens == 0 {
                    return None;
                }
                if state == 4 {
                    break i;
                }
                state = 2;
                depth = 0;
            }
            K::Else if depth == 0 && state == 2 => state = 3,
            _ if depth == 0 && state != 0 => return None,
            K::LParen | K::LBracket => {
                depth += 1;
                tokens += 1;
            }
            K::RParen | K::RBracket | K::RBrace => {
                depth = depth.checked_sub(1)?;
                tokens += 1;
            }
            _ => tokens += 1,
        }
        i += 1;
    };
    let ends = atoms.get(close + 1).is_none_or(|a| {
        Some(a.kind) == end
            || matches!(
                a.kind,
                K::Semi | K::Comma | K::RParen | K::RBracket | K::RBrace
            )
    });
    ends.then_some(close)
}
/// How a tagged sugar call lays out its braces.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tagged {
    /// Not a tagged call, or one whose contents hold a top-level `;` or a
    /// comment: an ordinary block.
    No,
    /// Contents that open with a block (`sum{ { .. } | x in xs }`): the
    /// call hugs the block, which stays on one line when it fits, and the
    /// clause after it follows its `}`.
    Hug,
    /// Other contents (`zip{ a, b }`): spaced like a struct literal, on one
    /// line when they fit.
    List,
}
fn tagged_layout(atoms: &[Piece<'_>], open: usize) -> Tagged {
    if !atoms[open].tagged {
        return Tagged::No;
    }
    let mut depth = 0usize;
    for atom in &atoms[open + 1..] {
        match atom.kind {
            K::LParen | K::LBracket | K::LBrace => depth += 1,
            K::RParen | K::RBracket | K::RBrace if depth == 0 => break,
            K::RParen | K::RBracket | K::RBrace => depth -= 1,
            K::Comment | K::BlockComment => return Tagged::No,
            K::Semi if depth == 0 => return Tagged::No,
            _ => {}
        }
    }
    if atoms.get(open + 1).is_some_and(|a| a.kind == K::LBrace) {
        Tagged::Hug
    } else {
        Tagged::List
    }
}
// One flag per enclosing layout, mirrored by the Vesper port's `sequence`.
#[allow(clippy::too_many_arguments)]
fn sequence<'a>(
    atoms: &[Piece<'a>],
    pos: &mut usize,
    end: Option<K>,
    block: bool,
    fill: bool,
    hug: bool,
    // The least column a line inside this sequence can start at: every
    // delimiter or block it is nested in adds 4 once it breaks, and a line
    // break inside one means it broke.
    indent: usize,
    document: &mut Document<'a>,
) -> Result<DocId, Error> {
    // A delimited, non-block list breaks at its comma separators: those soft
    // lines belong to the enclosing delimiter group, and every item is its own
    // group. Ordinary spaces never break, so an overlong line breaks its
    // delimiters (and member-chain dots) before anything else.
    let list = end.is_some() && !block;
    let mut output = Vec::new();
    let mut line = Line::default();
    let mut previous: Option<Piece<'a>> = None;
    let mut previous_block = false;
    let mut previous_unary = false;
    let mut previous_ends_operand = false;
    // Inside a package name `@scope/name` (`use` and trait paths), whose
    // tokens must stay adjacent for the parser.
    let mut package = false;
    // Inside the head of an `if`, `match`, `for` or `while` at this level,
    // up to its `{`: the parser admits no struct literal there, so a type
    // name before `{` ends the head (`for i in 0..N {`).
    let mut head = false;
    // A `use` group fills its lines like rustfmt: each separator and the name
    // after it break on their own, so names pack to the width. These are the
    // separators' places in `output`; a comment or trailing comma keeps the
    // plain one-name-per-line layout.
    let mut separators = Vec::new();
    let mut commented = false;
    // The previous atom was a non-empty parameter list before `->`.
    let mut previous_params = false;
    let mut previous_hugged = false;
    // The member chain being built on `line` (`out.rows.push(..)`).
    let mut chain = Chain::default();
    // A short `if c { a } else { b }` laid out on `line` as one group: the
    // index of its last `}`, and where its docs and breaks start on `line`.
    let mut short_if: Option<(usize, usize, usize)> = None;
    while *pos < atoms.len() && Some(atoms[*pos].kind) != end {
        let current = atoms[*pos];
        // An `if` used as a value (`let x = if ..`, an argument, a match arm,
        // a block's last expression), as rustfmt decides.
        let starts_short_if = short_if.is_none()
            && current.kind == K::If
            && !previous.is_some_and(|p| p.kind == K::Else);
        // An `if` that starts a statement is a value only as its block's last
        // expression.
        let starts_statement = block
            && previous.is_none_or(|p| {
                previous_block || matches!(p.kind, K::Semi | K::Comment | K::BlockComment)
            });
        let short_if_close = if starts_short_if {
            short_if_else(atoms, *pos, end).filter(|&close| {
                !starts_statement || atoms.get(close + 1).is_none_or(|a| Some(a.kind) == end)
            })
        } else {
            None
        };
        // `{` after `.` or `::` opens a list, not a block: a tagged literal or
        // a `use` group (`use a::{b, c};`), which stays on one line when it fits.
        let is_block = current.kind == K::LBrace
            && previous.is_none_or(|p| !matches!(p.kind, K::Dot | K::ColonColon));
        // A struct literal or pattern (`Point { x: 1, y: 2 }`) is a list
        // too, spaced inside its braces.
        let is_record =
            is_block && current.expr && !head && previous.is_some_and(|p| p.kind == K::Upper);
        let tagged = if is_block {
            tagged_layout(atoms, *pos)
        } else {
            Tagged::No
        };
        let is_block = is_block && !is_record && tagged == Tagged::No;
        // The block a hugging tagged call opens with.
        let hugged = hug && is_block && previous.is_none();
        // A branch of a short `if` breaks only with the whole `if`.
        let soft_block = is_block && short_if.is_some();
        if matches!(current.kind, K::If | K::Match)
            || current.kind == K::Lower && matches!(current.text, "for" | "while")
        {
            head = true;
        } else if matches!(current.kind, K::LBrace | K::Semi) {
            head = false;
        }
        package = current.glued
            && previous.is_some_and(|p| {
                (p.kind == K::At && current.kind == K::Lower)
                    || (package && matches!(current.kind, K::Lower | K::Minus | K::Slash | K::Int))
            });
        let spacing = if previous.is_none() {
            Gap::None
        } else if let Some(gap) = current.sugar_gap {
            gap
        } else if hug && previous_hugged {
            // The clause after a hugged block follows its `}`, and a `:` or
            // `,` stays glued to it.
            match previous.map(|p| gap(p, current, block, false)) {
                Some(Gap::None) => Gap::None,
                _ => Gap::Space,
            }
        } else if previous_unary || package {
            Gap::None
        } else if list && previous.is_some_and(|p| p.kind == K::Semi) {
            // `[T; N]` and `[x; N]` (§5): a `;` inside brackets is a
            // separator within one line, not the end of a statement.
            Gap::Soft
        } else {
            previous.map_or(Gap::None, |p| gap(p, current, block, previous_block))
        };
        // A return type follows its parameter list on the same line, as in
        // rustfmt: the list breaks first (`) -> T {`).
        let spacing = match spacing {
            Gap::Return if previous_params => Gap::Soft,
            spacing => spacing,
        };
        // Anything but an attached token ends the operand and its chain.
        if is_block || !matches!(spacing, Gap::None | Gap::Empty) {
            chain.finish(&mut line, indent, document)?;
        }
        let after_comma = previous.is_some_and(|p| p.kind == K::Comma);
        let near_comment = matches!(current.kind, K::Comment | K::BlockComment)
            || previous.is_some_and(|p| p.kind == K::BlockComment);
        commented |= matches!(current.kind, K::Comment | K::BlockComment);
        match spacing {
            Gap::Hard => {
                line.flush(document, &mut output)?;
                // Keep at most one blank line where the source had one or more
                // between two items; the bare newline leaves no indentation.
                if current.blank {
                    output.push(document.push(Doc::Text("\n"))?);
                }
                output.push(document.push(Doc::HardLine)?);
            }
            Gap::Soft if list && after_comma => {
                line.flush(document, &mut output)?;
                separators.push(output.len());
                output.push(document.push(Doc::SoftLineOrSpace)?);
            }
            Gap::Soft if !is_block && near_comment => {
                line.push(document.push(Doc::SoftLineOrSpace)?)
            }
            Gap::Space | Gap::Soft if !is_block || soft_block => {
                line.push(document.push(Doc::Text(" "))?)
            }
            Gap::Return => {
                line.flush(document, &mut output)?;
                let lead = document.push(Doc::SoftLineOrSpace)?;
                line.push_break(lead, 1);
            }
            Gap::Empty => {
                line.push_break(document.push(Doc::SoftLineOrEmpty)?, 12);
                chain.dot(line.breaks.len() - 1);
            }
            Gap::Operator(precedence) => {
                line.push_break(document.push(Doc::SoftLineOrSpace)?, precedence);
            }
            _ => {}
        }
        if is_block && !soft_block {
            line.flush(document, &mut output)?;
            if matches!(spacing, Gap::Soft | Gap::Space) {
                output.push(document.push(Doc::Text(" "))?);
            }
        }
        if let Some(close) = short_if_close {
            short_if = Some((close, line.docs.len(), line.breaks.len()));
        }
        *pos += 1;
        let mut params = false;
        let node = if matches!(current.kind, K::LParen | K::LBracket | K::LBrace) {
            let close = match current.kind {
                K::LParen => K::RParen,
                K::LBracket => K::RBracket,
                _ => K::RBrace,
            };
            let empty = atoms.get(*pos).is_some_and(|a| a.kind == close);
            let start = document.push(Doc::Text(current.text))?;
            let fill =
                current.kind == K::LBrace && previous.is_some_and(|p| p.kind == K::ColonColon);
            // A hugging call does not indent its contents.
            let inner_indent = if tagged == Tagged::Hug {
                indent
            } else {
                indent.checked_add(4).ok_or(Error::Overflow)?
            };
            let inner = sequence(
                atoms,
                pos,
                Some(close),
                is_block,
                fill,
                tagged == Tagged::Hug,
                inner_indent,
                document,
            )?;
            let last_is_line_comment = *pos > 0 && atoms[*pos - 1].kind == K::Comment;
            // The formatter never adds or removes tokens, so a source trailing
            // comma is kept and selects the vertical layout.
            let trailing_comma = !is_block
                && atoms[..*pos]
                    .iter()
                    .rev()
                    .find(|a| !matches!(a.kind, K::Comment | K::BlockComment))
                    .is_some_and(|a| a.kind == K::Comma);
            let closing = atoms
                .get(*pos)
                .filter(|a| a.kind == close)
                .ok_or(Error::Conservation)?;
            *pos += 1;
            let finish = document.push(Doc::Text(closing.text))?;
            params = current.kind == K::LParen
                && !empty
                && atoms.get(*pos).is_some_and(|a| a.kind == K::Arrow);
            if empty {
                document.push(Doc::Concat(vec![start, finish]))?
            } else {
                let lead = document.push(if hugged || soft_block {
                    Doc::SoftLineOrSpace
                } else if tagged == Tagged::Hug {
                    Doc::Text(" ")
                } else if is_block || trailing_comma {
                    Doc::HardLine
                } else if is_record || tagged == Tagged::List {
                    Doc::SoftLineOrSpace
                } else {
                    Doc::SoftLineOrEmpty
                })?;
                let content = document.push(Doc::Concat(vec![lead, inner]))?;
                // A hugging call does not indent: its block nests its own lines.
                let nest = if tagged == Tagged::Hug {
                    content
                } else {
                    document.push(Doc::Nest(4, content))?
                };
                let trail = document.push(if soft_block || hugged && !last_is_line_comment {
                    Doc::SoftLineOrSpace
                } else if tagged == Tagged::Hug {
                    Doc::Text(" ")
                } else if is_block || last_is_line_comment || trailing_comma {
                    Doc::HardLine
                } else if is_record || tagged == Tagged::List {
                    Doc::SoftLineOrSpace
                } else {
                    Doc::SoftLineOrEmpty
                })?;
                let all = document.push(Doc::Concat(vec![start, nest, trail, finish]))?;
                // A parameter list before `->` shares its line's group, so it
                // breaks whenever the signature with its return type cannot
                // fit on one line; a short `if`'s branch shares the `if`'s. A
                // hugged block shares the group of its call, so it breaks
                // before the clause after it does.
                if params || soft_block || hugged {
                    all
                } else {
                    document.push(Doc::Group(all))?
                }
            }
        } else {
            document.push(if matches!(current.kind, K::Comment | K::BlockComment) {
                Doc::Comment(current.text)
            } else {
                Doc::Text(current.text)
            })?
        };
        if is_block && !soft_block {
            output.push(node);
        } else {
            line.push(node);
            chain.push(
                document.width(node)?,
                current.kind == K::LParen,
                current.kind,
            )?;
        }
        // The short `if` ends: it becomes one group that breaks like any
        // other `if`, with the breaks in its condition kept on one line.
        if let Some((close, docs, breaks)) = short_if.filter(|&(close, _, _)| *pos > close) {
            let _ = close;
            short_if = None;
            // Unless the line broke inside the `if`, which `short_if_else`
            // rules out, leaving nothing to group.
            if docs <= line.docs.len() && breaks <= line.breaks.len() {
                for &(offset, level) in &line.breaks[breaks..] {
                    line.docs[offset] = document.push(if level == 12 {
                        Doc::Nil
                    } else {
                        Doc::Text(" ")
                    })?;
                }
                line.breaks.truncate(breaks);
                let inner = document.push(Doc::Concat(line.docs.split_off(docs)))?;
                line.push(document.push(Doc::Group(inner))?);
            }
        }
        previous_block = is_block;
        previous_params = params;
        previous_hugged = hugged;
        // A prefix `-`/`!` binds to its operand with no space.
        previous_unary =
            matches!(current.kind, K::Minus | K::Bang | K::Tilde) && !previous_ends_operand;
        let used_as_name = word_used_as_name(
            previous.map(|p| p.kind),
            current.kind,
            atoms.get(*pos).map(|a| a.kind),
        );
        let kind = match current.kind {
            K::LParen => K::RParen,
            K::LBracket => K::RBracket,
            K::LBrace => K::RBrace,
            // A contextual keyword used as a name spaces like one.
            _ if used_as_name => K::Lower,
            _ => current.kind,
        };
        if !matches!(kind, K::Comment | K::BlockComment) {
            previous_ends_operand = ends_operand(kind);
        }
        previous = Some(Piece {
            kind,
            text: current.text,
            blank: false,
            glued: current.glued,
            sugar_gap: None,
            expr: current.expr,
            tagged: false,
        });
    }
    chain.finish(&mut line, indent, document)?;
    line.flush(document, &mut output)?;
    if fill && !commented && previous.is_some_and(|p| p.kind != K::Comma) {
        let mut filled = Vec::new();
        let mut from = 0;
        for (i, &at) in separators.iter().enumerate() {
            if i == 0 {
                filled.extend_from_slice(&output[..at]);
            } else {
                let item = document.push(Doc::Concat(output[from..at].to_vec()))?;
                filled.push(document.push(Doc::Group(item))?);
            }
            from = at;
        }
        if !separators.is_empty() {
            let item = document.push(Doc::Concat(output[from..].to_vec()))?;
            filled.push(document.push(Doc::Group(item))?);
            output = filled;
        }
    }
    document.push(Doc::Concat(output))
}
