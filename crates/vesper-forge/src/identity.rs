//! Host-owned, versioned hygiene identities.
//!
//! Every identity is minted by a [`crate::HygieneHost`] from a checked,
//! bounded counter and is qualified by the host's [`SessionId`]. A counter
//! never wraps: when a configured limit is reached minting fails with
//! [`crate::ForgeError::IdentityExhausted`] and keeps failing. Identities
//! can be (de)serialized so the compiler can persist them, but a deserialized
//! value is only a *claim*; the host re-validates session and index before
//! using it.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{ForgeError, IdentityKind};

/// Version of the identity/scope encoding produced by this crate.
pub const IDENTITY_SCHEMA_VERSION: u32 = 1;

/// Namespace for every identity minted by one host session.
///
/// Identities from a different session are *foreign* and are rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SessionId(pub u64);

/// A host-minted scope (Flatt "sets of scopes" mark).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ScopeId {
    pub(crate) session: SessionId,
    pub(crate) index: u32,
}

impl ScopeId {
    pub fn session(self) -> SessionId {
        self.session
    }
    pub fn index(self) -> u32 {
        self.index
    }
}

impl fmt::Display for ScopeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "s{}", self.index)
    }
}

/// A host-minted macro expansion identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ExpansionId {
    pub(crate) session: SessionId,
    pub(crate) index: u32,
}

impl ExpansionId {
    pub fn session(self) -> SessionId {
        self.session
    }
    pub fn index(self) -> u32 {
        self.index
    }
}

impl fmt::Display for ExpansionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "x{}", self.index)
    }
}

/// Host-minted evidence that a quotation antiquote refers to caller syntax
/// the host stored when the expansion began.
///
/// The handle is only a reference into the host's own arena: splicing it
/// substitutes the host's stored tokens (with their original origin and
/// scopes), never data supplied alongside the handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SpliceHandle {
    pub(crate) session: SessionId,
    pub(crate) expansion: ExpansionId,
    pub(crate) nonce: u32,
}

impl SpliceHandle {
    pub fn expansion(self) -> ExpansionId {
        self.expansion
    }
}

/// Resolver-local binder identity, unique within one host session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BinderId(pub(crate) u32);

impl fmt::Display for BinderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "b{}", self.0)
    }
}

/// Interned identifier/token text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Symbol(pub(crate) u32);

/// A finite set of scopes, ordered for deterministic output.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ScopeSet(BTreeSet<ScopeId>);

impl ScopeSet {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn single(scope: ScopeId) -> Self {
        let mut s = Self::new();
        s.insert(scope);
        s
    }
    pub fn insert(&mut self, scope: ScopeId) {
        self.0.insert(scope);
    }
    pub fn contains(&self, scope: ScopeId) -> bool {
        self.0.contains(&scope)
    }
    pub fn is_subset(&self, other: &Self) -> bool {
        self.0.is_subset(&other.0)
    }
    pub fn union(&self, other: &Self) -> Self {
        Self(self.0.union(&other.0).copied().collect())
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn iter(&self) -> impl Iterator<Item = ScopeId> + '_ {
        self.0.iter().copied()
    }
}

impl fmt::Display for ScopeSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("{")?;
        for (i, s) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(",")?;
            }
            write!(f, "{s}")?;
        }
        f.write_str("}")
    }
}

/// Upper bounds for every identity counter. Reaching one is an explicit
/// error, never wraparound or reuse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityLimits {
    pub max_scopes: u32,
    pub max_expansions: u32,
    pub max_splices: u32,
    pub max_binders: u32,
    pub max_symbols: u32,
}

impl Default for IdentityLimits {
    fn default() -> Self {
        Self {
            max_scopes: u32::MAX,
            max_expansions: u32::MAX,
            max_splices: u32::MAX,
            max_binders: u32::MAX,
            max_symbols: u32::MAX,
        }
    }
}

/// A checked monotonic counter.
#[derive(Clone, Debug)]
pub(crate) struct Counter {
    next: u32,
    limit: u32,
    kind: IdentityKind,
}

impl Counter {
    pub(crate) fn new(kind: IdentityKind, limit: u32) -> Self {
        Self {
            next: 0,
            limit,
            kind,
        }
    }

    pub(crate) fn mint(&mut self) -> Result<u32, ForgeError> {
        if self.next >= self.limit {
            return Err(ForgeError::IdentityExhausted {
                kind: self.kind,
                limit: self.limit,
            });
        }
        let id = self.next;
        // `next < limit <= u32::MAX`, so this cannot overflow.
        self.next += 1;
        Ok(id)
    }

    pub(crate) fn minted(&self, index: u32) -> bool {
        index < self.next
    }
}
