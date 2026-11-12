//! Package-qualified external definition identities.
//!
//! An external identity is derived only from the package identity, the
//! definition kind and the declared name (or, for anonymous impls, an explicit
//! producer-chosen key). Spans, declaration order, whitespace, process
//! addresses, local variable names and checker-internal `$cap` labels never
//! participate, so reformatting or reordering a source file cannot change it.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Identity of one package node of the resolved dependency graph.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PackageIdentity {
    pub name: String,
    pub version: String,
}

/// Reserved package that owns primitives, tuples and the checker prelude
/// (`Option`, `Result`, `RuntimeError`, `Fail`).
pub const BUILTIN_PACKAGE: &str = "@vesper/builtin";

impl PackageIdentity {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        PackageIdentity {
            name: name.into(),
            version: version.into(),
        }
    }

    pub fn builtin() -> Self {
        PackageIdentity::new(BUILTIN_PACKAGE, "0.0.0")
    }

    pub fn is_builtin(&self) -> bool {
        self.name == BUILTIN_PACKAGE
    }

    /// Rejects empty or non-canonical spellings so that one package cannot be
    /// referred to by two different strings.
    pub fn validate(&self) -> Result<(), String> {
        let name_ok = !self.name.is_empty()
            && self
                .name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "@/-_.".contains(c));
        if !name_ok {
            return Err(format!(
                "package name {:?} must be non-empty lowercase ASCII [a-z0-9@/-_.]",
                self.name
            ));
        }
        let version_ok = !self.version.is_empty()
            && self
                .version
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
        if !version_ok {
            return Err(format!(
                "package version {:?} must be non-empty ASCII [A-Za-z0-9+-.]",
                self.version
            ));
        }
        Ok(())
    }
}

impl fmt::Display for PackageIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.name, self.version)
    }
}

/// Namespace of a definition. Each kind has its own Pact id space.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum DefKind {
    Type,
    Effect,
    Trait,
    Impl,
    Model,
}

/// A deterministic package-qualified external definition identity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DefPath {
    pub package: PackageIdentity,
    pub kind: DefKind,
    /// Declared name; for anonymous impls the producer-chosen stable key.
    pub name: String,
}

impl DefPath {
    pub fn new(package: PackageIdentity, kind: DefKind, name: impl Into<String>) -> Self {
        DefPath {
            package,
            kind,
            name: name.into(),
        }
    }
}

impl fmt::Display for DefPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let prefix = match self.kind {
            DefKind::Impl => "impl ",
            DefKind::Model => "model ",
            DefKind::Effect => "effect ",
            DefKind::Trait => "trait ",
            DefKind::Type => "",
        };
        write!(f, "{prefix}{}::{}", self.package, self.name)
    }
}

/// Valid declared identifier (shared by trait/model names and impl keys).
pub(crate) fn valid_ident(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Valid impl key: identifier segments joined by `.`, `-` or `/`.
pub(crate) fn valid_key(s: &str) -> bool {
    !s.is_empty() && s.split(['.', '-', '/']).all(valid_ident_or_digits) && !s.starts_with('$')
}

fn valid_ident_or_digits(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}
