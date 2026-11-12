use std::fmt;

use serde::{Deserialize, Serialize};

use crate::identity::{ExpansionId, ScopeId, SpliceHandle};
use crate::registry::{DefId, DefKind, PackageId};
use crate::resolve::Resolution;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum IdentityKind {
    Scope,
    Expansion,
    Splice,
    Binder,
    Symbol,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum SpliceFault {
    /// Minted by another session.
    Foreign,
    /// Minted for a different (earlier or sibling) expansion.
    Stale,
    /// Never minted by this host for this expansion.
    Forged,
}

/// Hygiene, provenance, resolution and schema failures. All are terminal:
/// nothing here degrades to acceptance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ForgeError {
    IdentityExhausted {
        kind: IdentityKind,
        limit: u32,
    },
    ForeignScope(ScopeId),
    ForgedScope(ScopeId),
    ForgedSymbol(u32),
    ForeignExpansion(ExpansionId),
    UnknownExpansion(ExpansionId),
    ExpansionClosed(ExpansionId),
    ExpansionNotFinished(ExpansionId),
    Splice {
        fault: SpliceFault,
        handle: SpliceHandle,
    },
    UnknownPackage(PackageId),
    UnknownDefinition(DefId),
    NotAMacro(DefId),
    NotADependency {
        from: PackageId,
        def: DefId,
    },
    PrivateDefinition {
        from: PackageId,
        def: DefId,
    },
    PrintedNameMismatch {
        def: DefId,
        printed: String,
        registered: String,
    },
    DuplicateEntry {
        what: &'static str,
        id: String,
    },
    QuoteLimit {
        what: &'static str,
        limit: usize,
    },
    UnbalancedDelimiters {
        token: usize,
    },
    Parse {
        token: usize,
        expected: &'static str,
    },
    Unbound {
        token: usize,
        name: String,
    },
    Ambiguous {
        token: usize,
        name: String,
        candidates: Vec<Resolution>,
    },
    PreResolvedBinder {
        token: usize,
        def: DefId,
    },
    MixedPath {
        token: usize,
    },
    ExpectedGlobal {
        token: usize,
    },
    WrongDefinitionKind {
        token: usize,
        def: DefId,
        expected: DefKind,
    },
    MacroReportedError {
        message: String,
    },
    Transport(String),
    Schema {
        what: &'static str,
        found: u32,
        expected: u32,
    },
    Decode(String),
}

impl fmt::Display for ForgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use ForgeError::*;
        match self {
            IdentityExhausted { kind, limit } => {
                write!(
                    f,
                    "FORGE-H001 {kind:?} identities exhausted at limit {limit}"
                )
            }
            ForeignScope(s) => write!(f, "FORGE-H002 scope {s} belongs to another session"),
            ForgedScope(s) => write!(f, "FORGE-H003 scope {s} was never minted"),
            ForgedSymbol(n) => write!(f, "FORGE-H004 symbol {n} was never interned"),
            ForeignExpansion(x) => write!(f, "FORGE-H005 expansion {x} belongs to another session"),
            UnknownExpansion(x) => write!(f, "FORGE-H006 expansion {x} was never minted"),
            ExpansionClosed(x) => write!(f, "FORGE-H007 expansion {x} is already closed"),
            ExpansionNotFinished(x) => {
                write!(f, "FORGE-H008 expansion {x} has not finished successfully")
            }
            Splice { fault, handle } => write!(
                f,
                "FORGE-H009 {fault:?} splice handle for expansion {}",
                handle.expansion()
            ),
            UnknownPackage(p) => write!(f, "FORGE-R001 unknown package {p}"),
            UnknownDefinition(d) => write!(f, "FORGE-R002 unknown definition {d}"),
            NotAMacro(d) => write!(f, "FORGE-R003 definition {d} is not a macro"),
            NotADependency { from, def } => write!(
                f,
                "FORGE-R004 {from} cannot name {def}: its package is not a dependency"
            ),
            PrivateDefinition { from, def } => {
                write!(f, "FORGE-R005 {from} cannot name private definition {def}")
            }
            PrintedNameMismatch {
                def,
                printed,
                registered,
            } => write!(
                f,
                "FORGE-R006 claim {def} printed as `{printed}` but registered as `{registered}`"
            ),
            DuplicateEntry { what, id } => write!(f, "FORGE-R007 duplicate {what} {id}"),
            QuoteLimit { what, limit } => write!(f, "FORGE-Q001 {what} exceed limit {limit}"),
            UnbalancedDelimiters { token } => {
                write!(f, "FORGE-Q002 unbalanced delimiter at token {token}")
            }
            Parse { token, expected } => write!(f, "FORGE-P001 token {token}: expected {expected}"),
            Unbound { token, name } => write!(f, "FORGE-B001 token {token}: `{name}` is unbound"),
            Ambiguous {
                token,
                name,
                candidates,
            } => write!(
                f,
                "FORGE-B002 token {token}: `{name}` is ambiguous between {candidates:?}"
            ),
            PreResolvedBinder { token, def } => {
                write!(
                    f,
                    "FORGE-B003 token {token}: binder carries pre-resolved {def}"
                )
            }
            MixedPath { token } => write!(
                f,
                "FORGE-B004 token {token}: path segments disagree on origin, scopes or claim"
            ),
            ExpectedGlobal { token } => {
                write!(f, "FORGE-B005 token {token}: expected a global definition")
            }
            WrongDefinitionKind {
                token,
                def,
                expected,
            } => write!(f, "FORGE-B006 token {token}: {def} is not a {expected:?}"),
            MacroReportedError { message } => write!(f, "FORGE-M001 macro reported: {message}"),
            Transport(m) => write!(f, "FORGE-M002 transport violation: {m}"),
            Schema {
                what,
                found,
                expected,
            } => write!(
                f,
                "FORGE-S001 {what} schema version {found} is not supported (expected {expected})"
            ),
            Decode(m) => write!(f, "FORGE-S002 decode failure: {m}"),
        }
    }
}

impl std::error::Error for ForgeError {}
