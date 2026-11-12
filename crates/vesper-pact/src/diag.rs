//! Deterministic, name-free diagnostics.
//!
//! Diagnostics mention only stable ids and rendered terms; the compiler
//! adapter maps ids back to source names and spans. Collections of
//! diagnostics are always sorted by `(code, subject, message)`.

use crate::ids::*;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum DiagCode {
    SchemaVersion,
    IdNotDense,
    UnknownId,
    Arity,
    KindMismatch,
    VarScope,
    Malformed,
    UnconstrainedParam,
    Unsupported,
    OrphanImpl,
    OverlappingImpls,
    ImplNotWellFormed,
}

impl DiagCode {
    /// Stable code string.
    pub fn as_str(self) -> &'static str {
        match self {
            DiagCode::SchemaVersion => "PACT0001",
            DiagCode::IdNotDense => "PACT0002",
            DiagCode::UnknownId => "PACT0003",
            DiagCode::Arity => "PACT0004",
            DiagCode::KindMismatch => "PACT0005",
            DiagCode::VarScope => "PACT0006",
            DiagCode::Malformed => "PACT0007",
            DiagCode::UnconstrainedParam => "PACT0008",
            DiagCode::Unsupported => "PACT0009",
            DiagCode::OrphanImpl => "PACT0101",
            DiagCode::OverlappingImpls => "PACT0102",
            DiagCode::ImplNotWellFormed => "PACT0103",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Subject {
    Program,
    TypeCtor(TypeCtorId),
    Effect(EffectId),
    Trait(TraitId),
    Impl(ImplId),
    ImplPair(ImplId, ImplId),
    Model(ModelId),
    Query,
}

impl fmt::Display for Subject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Subject::Program => f.write_str("program"),
            Subject::TypeCtor(i) => write!(f, "{i}"),
            Subject::Effect(i) => write!(f, "{i}"),
            Subject::Trait(i) => write!(f, "{i}"),
            Subject::Impl(i) => write!(f, "{i}"),
            Subject::ImplPair(a, b) => write!(f, "{a} & {b}"),
            Subject::Model(i) => write!(f, "{i}"),
            Subject::Query => f.write_str("query"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: DiagCode,
    pub subject: Subject,
    pub message: String,
}

impl Diagnostic {
    pub fn new(code: DiagCode, subject: Subject, message: String) -> Self {
        Diagnostic {
            code,
            subject,
            message,
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} [{}] {}",
            self.code.as_str(),
            self.subject,
            self.message
        )
    }
}

/// Renders a sorted diagnostic list, one per line.
pub fn render(diags: &[Diagnostic]) -> String {
    let mut out = String::new();
    for d in diags {
        out.push_str(&d.to_string());
        out.push('\n');
    }
    out
}
