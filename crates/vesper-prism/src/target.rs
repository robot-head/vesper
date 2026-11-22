//! The single URI target interpretation shared by routing and middleware.
//!
//! [`Target::parse`] decodes a request target exactly once into typed path
//! segments and query entries. No other Prism API percent-decodes; routes and
//! middleware compare the decoded bytes stored here. The policy is fixed and
//! documented rather than configurable per consumer:
//!
//! * The path is split on literal `/` *before* decoding, so `%2F` can never
//!   create a separator. Decoded segments containing `/` (`%2F`), `\` (`%5C`)
//!   or NUL (`%00`) are rejected rather than passed through.
//! * Segments equal to `.` or `..` after decoding (including `%2e` forms) are
//!   rejected; there is no dot-segment normalization.
//! * Empty interior segments (`/a//b`) are rejected. A single trailing slash is
//!   recorded as [`OriginTarget::trailing_slash`] and is significant for
//!   matching. `/` has zero segments and no trailing slash.
//! * `+` is a literal byte in the path. In the query, `+` decodes to a space
//!   (form convention) and `%2B` decodes to `+`.
//! * The query is split on literal `&` and each entry on its first literal `=`
//!   before decoding. An entry without `=` has no value, which is distinct from
//!   an empty value. Empty entries (`a&&b`, trailing `&`) and empty keys are
//!   rejected; an empty query after `?` has zero entries. Encoded NUL is
//!   rejected in keys and values. Duplicate keys are preserved in order;
//!   [`Query::unique`] reports them as an error instead of picking one.
//! * Decoded bytes must be UTF-8 under [`Utf8Policy::Require`] (the default);
//!   [`Utf8Policy::AllowBytes`] keeps arbitrary bytes.
//! * Hex digits may be upper or lower case. An escape of an unreserved byte is
//!   equivalent to that byte (`/%61` matches the literal route `a`).
//! * Decoding happens once: `%252e` yields the three bytes `%2e`, which are
//!   data, never a dot segment or a second escape.

use std::fmt;

use crate::forward::target as imp;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Utf8Policy {
    Require,
    AllowBytes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TargetLimits {
    pub max_target_bytes: usize,
    pub max_segments: usize,
    pub max_query_entries: usize,
    pub utf8: Utf8Policy,
}
impl Default for TargetLimits {
    fn default() -> Self {
        Self {
            max_target_bytes: 8 * 1024,
            max_segments: 32,
            max_query_entries: 64,
            utf8: Utf8Policy::Require,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetError {
    TooLong,
    TooManySegments,
    TooManyQueryEntries,
    /// Not origin-form/`*`, a fragment, a raw non-ASCII byte or similar.
    InvalidSyntax,
    InvalidEscape,
    EmptySegment,
    DotSegment,
    EncodedSlash,
    EncodedBackslash,
    EncodedNul,
    InvalidUtf8,
    EmptyQueryEntry,
    EmptyQueryKey,
    AllocationFailed,
}
impl fmt::Display for TargetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "request target error: {self:?}")
    }
}
impl std::error::Error for TargetError {}

/// A decoded path segment. Never decoded again by any consumer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment(pub(crate) Vec<u8>);
impl Segment {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
    /// Always `Some` under [`Utf8Policy::Require`].
    pub fn as_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.0).ok()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryEntry {
    pub key: Vec<u8>,
    /// `None` for `?flag`, `Some(empty)` for `?flag=`.
    pub value: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DuplicateQueryKey;

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Query {
    pub(crate) entries: Vec<QueryEntry>,
}
impl Query {
    pub fn entries(&self) -> &[QueryEntry] {
        &self.entries
    }
    /// Every entry for a decoded key, in wire order.
    pub fn all<'a>(&'a self, key: &'a [u8]) -> impl Iterator<Item = &'a QueryEntry> + 'a {
        self.entries.iter().filter(move |entry| entry.key == key)
    }
    /// The single entry for a key; repeated keys are an error, not first/last-wins.
    pub fn unique<'a>(
        &'a self,
        key: &'a [u8],
    ) -> Result<Option<&'a QueryEntry>, DuplicateQueryKey> {
        let mut found = None;
        for entry in self.all(key) {
            if found.is_some() {
                return Err(DuplicateQueryKey);
            }
            found = Some(entry);
        }
        Ok(found)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OriginTarget {
    pub(crate) segments: Vec<Segment>,
    pub(crate) trailing_slash: bool,
    pub(crate) query: Option<Query>,
}
impl OriginTarget {
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }
    pub fn trailing_slash(&self) -> bool {
        self.trailing_slash
    }
    /// `None` when the target had no `?`; `Some` (possibly empty) otherwise.
    pub fn query(&self) -> Option<&Query> {
        self.query.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// `OPTIONS *`; never matches a route.
    Asterisk,
    Origin(OriginTarget),
}
impl Target {
    /// Validate and decode a raw target exactly once. Counts and length are
    /// checked before any segment or entry is allocated.
    pub fn parse(raw: &str, limits: TargetLimits) -> Result<Self, TargetError> {
        imp::parse(raw, limits)
    }
    pub fn origin(&self) -> Option<&OriginTarget> {
        match self {
            Self::Origin(origin) => Some(origin),
            Self::Asterisk => None,
        }
    }
}
