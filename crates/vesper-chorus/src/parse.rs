//! Textual choreography syntax.
//!
//! ```text
//! program := 'roles' ident (',' ident)* ';' stmt*
//! stmt    := ident '->' ident ':' ident '(' type? ')' ';'      message
//!          | ident '->' ident '{' arm+ '}'                     choice at the sender
//!          | ident ':' ident ';'                               local action
//!          | 'rec' ident '{' stmt* '}'                          recursion
//!          | 'continue' ident ';'                              loop back (last in block)
//!          | 'end' ';'                                         stop (last in block)
//! arm     := ident '(' type? ')' '{' stmt* '}'
//! type    := ident ('::' ident)* ('[' type (',' type)* ']')?
//! ```
//!
//! Statements are sequenced: whatever follows a choice or a `rec` block runs
//! after every branch (or loop exit) that does not itself `continue` or `end`.
//! `//` starts a comment to end of line. Inputs are bounded by
//! [`MAX_SOURCE_BYTES`] and the AST limits in [`crate::chor`]; violations are
//! typed errors.

use crate::chor::{Choreography, MAX_DEPTH, MAX_NESTING, MAX_NODES, WellFormedError};
use std::fmt;

pub const MAX_SOURCE_BYTES: usize = 1 << 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseErrorKind {
    TooLong {
        bytes: usize,
    },
    UnexpectedChar(char),
    UnexpectedToken {
        expected: &'static str,
        found: String,
    },
    UnexpectedEof {
        expected: &'static str,
    },
    UnknownRole(String),
    IdentifierTooLong,
    /// A statement follows `continue` or `end` in the same block.
    Unreachable,
    TooDeep,
    TooLarge,
    Invalid(WellFormedError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub line: u32,
    pub col: u32,
    pub kind: ParseErrorKind,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: ", self.line, self.col)?;
        match &self.kind {
            ParseErrorKind::TooLong { bytes } => {
                write!(f, "source is {bytes} bytes (limit {MAX_SOURCE_BYTES})")
            }
            ParseErrorKind::UnexpectedChar(c) => write!(f, "unexpected character {c:?}"),
            ParseErrorKind::UnexpectedToken { expected, found } => {
                write!(f, "expected {expected}, found `{found}`")
            }
            ParseErrorKind::UnexpectedEof { expected } => {
                write!(f, "expected {expected}, found end of input")
            }
            ParseErrorKind::UnknownRole(r) => write!(f, "role `{r}` is not declared"),
            ParseErrorKind::IdentifierTooLong => write!(f, "identifier too long"),
            ParseErrorKind::Unreachable => {
                write!(f, "statement after `continue` or `end` is unreachable")
            }
            ParseErrorKind::TooDeep => write!(
                f,
                "nesting exceeds {MAX_NESTING} braces or {MAX_DEPTH} sequential steps"
            ),
            ParseErrorKind::TooLarge => write!(f, "choreography expands beyond {MAX_NODES} nodes"),
            ParseErrorKind::Invalid(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Parse and check a choreography.
///
/// The lexer, parser, builder and checks are the Vesper library in
/// `vesper/src/parse.ves` (port wave P3b lane 6, #1068).
pub fn parse_choreography(src: &str) -> Result<Choreography, ParseError> {
    crate::port::parse_choreography(src)
}
