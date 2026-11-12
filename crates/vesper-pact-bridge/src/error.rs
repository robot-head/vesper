//! Source-facing diagnostics. Names and spans appear here only; Pact goals and
//! canonical forms never contain them.

use crate::identity::{DefPath, PackageIdentity};
use std::fmt;
use vesper::ast::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ErrorKind {
    /// The source did not parse or check.
    Source,
    /// Malformed bindings, unknown names, wrong arity or kinds.
    Invalid,
    /// A well-formed construct Pact cannot represent faithfully.
    Unsupported,
    /// Pact validation or coherence rejected the declaration set.
    Incoherent,
    /// A query that does not validate against the session.
    InvalidQuery,
    /// Evidence or a certificate from a different declaration revision.
    StaleEvidence,
    /// Evidence or a certificate that does not re-check.
    InvalidEvidence,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::Source => "source",
            ErrorKind::Invalid => "invalid",
            ErrorKind::Unsupported => "unsupported",
            ErrorKind::Incoherent => "incoherent",
            ErrorKind::InvalidQuery => "invalid-query",
            ErrorKind::StaleEvidence => "stale-evidence",
            ErrorKind::InvalidEvidence => "invalid-evidence",
        }
    }
}

/// A source location: package plus byte range in that package's source.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceSpan {
    pub package: PackageIdentity,
    pub start: usize,
    pub end: usize,
}

impl SourceSpan {
    pub fn new(package: PackageIdentity, span: Span) -> Self {
        SourceSpan {
            package,
            start: span.start,
            end: span.end,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BridgeDiagnostic {
    /// Stable code: a checker code (`E…`), a Pact code (`PACT…`) or `BRIDGE…`.
    pub code: String,
    pub subject: Option<DefPath>,
    pub span: Option<SourceSpan>,
    pub message: String,
}

impl fmt::Display for BridgeDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code)?;
        if let Some(s) = &self.subject {
            write!(f, " [{s}]")?;
        }
        if let Some(sp) = &self.span {
            write!(f, " at {}:{}..{}", sp.package, sp.start, sp.end)?;
        }
        write!(f, ": {}", self.message)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BridgeError {
    pub kind: ErrorKind,
    /// Sorted and deduplicated.
    pub diagnostics: Vec<BridgeDiagnostic>,
}

impl BridgeError {
    pub(crate) fn new(kind: ErrorKind, mut diagnostics: Vec<BridgeDiagnostic>) -> Self {
        diagnostics.sort();
        diagnostics.dedup();
        BridgeError { kind, diagnostics }
    }

    pub(crate) fn single(kind: ErrorKind, subject: Option<DefPath>, message: String) -> Self {
        BridgeError::new(
            kind,
            vec![BridgeDiagnostic {
                code: default_code(kind).into(),
                subject,
                span: None,
                message,
            }],
        )
    }

    pub(crate) fn source(package: PackageIdentity, diags: Vec<vesper::ast::Diagnostic>) -> Self {
        BridgeError::new(
            ErrorKind::Source,
            diags
                .into_iter()
                .map(|d| BridgeDiagnostic {
                    code: d.code,
                    subject: None,
                    span: Some(SourceSpan::new(package.clone(), d.span)),
                    message: d.message,
                })
                .collect(),
        )
    }

    /// Whether any diagnostic message contains `needle` (test helper).
    pub fn mentions(&self, needle: &str) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.to_string().contains(needle))
    }
}

pub(crate) fn default_code(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::Source => "BRIDGE0000",
        ErrorKind::Invalid => "BRIDGE0001",
        ErrorKind::Unsupported => "BRIDGE0002",
        ErrorKind::Incoherent => "BRIDGE0003",
        ErrorKind::InvalidQuery => "BRIDGE0004",
        ErrorKind::StaleEvidence => "BRIDGE0005",
        ErrorKind::InvalidEvidence => "BRIDGE0006",
    }
}

impl fmt::Display for BridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} error", self.kind.as_str())?;
        for d in &self.diagnostics {
            write!(f, "\n  {d}")?;
        }
        Ok(())
    }
}

impl std::error::Error for BridgeError {}

/// Collects diagnostics of one kind during lowering.
#[derive(Default)]
pub(crate) struct Sink {
    pub invalid: Vec<BridgeDiagnostic>,
    pub unsupported: Vec<BridgeDiagnostic>,
}

impl Sink {
    pub fn invalid(&mut self, subject: Option<DefPath>, span: Option<SourceSpan>, msg: String) {
        self.invalid.push(BridgeDiagnostic {
            code: default_code(ErrorKind::Invalid).into(),
            subject,
            span,
            message: msg,
        });
    }
    pub fn unsupported(&mut self, subject: Option<DefPath>, span: Option<SourceSpan>, msg: String) {
        self.unsupported.push(BridgeDiagnostic {
            code: default_code(ErrorKind::Unsupported).into(),
            subject,
            span,
            message: msg,
        });
    }
    /// Invalid input takes precedence over unsupported constructs.
    pub fn into_error(self) -> Option<BridgeError> {
        if !self.invalid.is_empty() {
            Some(BridgeError::new(ErrorKind::Invalid, self.invalid))
        } else if !self.unsupported.is_empty() {
            Some(BridgeError::new(ErrorKind::Unsupported, self.unsupported))
        } else {
            None
        }
    }
}
