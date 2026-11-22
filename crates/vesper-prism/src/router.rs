//! Bounded static/parameter routing over a decoded [`Target`].
//!
//! Patterns are written in decoded form: `/users/:id/posts`, `/`, `/files/`.
//! A segment beginning with `:` captures one decoded path segment; every other
//! segment matches decoded bytes exactly. A trailing slash is significant.
//! There is no regex, wildcard or catch-all segment.
//!
//! Precedence is deterministic and independent of insertion order: segments are
//! compared left to right and a static segment beats a parameter at the first
//! position where two candidate routes differ. The highest-precedence route
//! whose *path* matches is selected; its methods then decide between a match
//! and [`Resolution::MethodNotAllowed`]. Lower-precedence paths are not tried
//! for a different method. `HEAD` uses the `GET` route of the selected path
//! when no explicit `HEAD` route exists there.
//!
//! Registration rejects exact duplicates (same method and shape) and
//! parameters with different names at the same position of an otherwise
//! shared prefix, which would make captured names depend on the request.

use std::fmt;

use crate::target::Segment;

pub use crate::forward::router::Router;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RouterLimits {
    pub max_routes: usize,
    pub max_route_segments: usize,
    /// Trie nodes visited while resolving one target, including backtracking.
    pub max_match_work: usize,
}
impl Default for RouterLimits {
    fn default() -> Self {
        Self {
            max_routes: 1024,
            max_route_segments: 32,
            max_match_work: 4096,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteError {
    InvalidLimits,
    InvalidMethod,
    InvalidPattern,
    InvalidParameterName,
    DuplicateParameterName,
    RouteLimit,
    SegmentLimit,
    DuplicateRoute,
    /// Two routes use different parameter names at one shared position.
    AmbiguousParameter {
        existing: String,
        new: String,
    },
    WorkLimit,
    AllocationFailed,
}
impl fmt::Display for RouteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "routing error: {self:?}")
    }
}
impl std::error::Error for RouteError {}

/// Opaque registration identity, stable for the router's lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RouteId(pub(crate) usize);
impl RouteId {
    pub fn index(self) -> usize {
        self.0
    }
}

#[derive(Debug)]
pub struct RouteMatch<'a, T> {
    pub id: RouteId,
    pub value: &'a T,
    /// True when a HEAD request was served by the path's GET route.
    pub head_fallback: bool,
    pub(crate) params: Vec<(&'a str, &'a Segment)>,
}
impl<'a, T> RouteMatch<'a, T> {
    /// The already-decoded segment captured by `:name`.
    pub fn param(&self, name: &str) -> Option<&'a Segment> {
        self.params
            .iter()
            .find(|(param, _)| *param == name)
            .map(|(_, segment)| *segment)
    }
    pub fn params(&self) -> &[(&'a str, &'a Segment)] {
        &self.params
    }
}

#[derive(Debug)]
pub enum Resolution<'a, T> {
    Matched(RouteMatch<'a, T>),
    /// The selected path exists; methods are sorted, with HEAD implied by GET.
    MethodNotAllowed(Vec<&'a str>),
    NotFound,
}
