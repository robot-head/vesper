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

use crate::target::{OriginTarget, Segment, Target};

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
pub struct RouteId(usize);
impl RouteId {
    pub fn index(self) -> usize {
        self.0
    }
}

#[derive(Default)]
struct Node {
    /// Sorted by decoded bytes for binary search.
    statics: Vec<(Vec<u8>, usize)>,
    param: Option<(String, usize)>,
    /// Indexed by trailing slash; sorted by method.
    terminals: [Vec<(String, RouteId)>; 2],
}

pub struct Router<T> {
    limits: RouterLimits,
    nodes: Vec<Node>,
    routes: Vec<RouteEntry<T>>,
}
struct RouteEntry<T> {
    value: T,
    params: Vec<String>,
}

#[derive(Debug)]
pub struct RouteMatch<'a, T> {
    pub id: RouteId,
    pub value: &'a T,
    /// True when a HEAD request was served by the path's GET route.
    pub head_fallback: bool,
    params: Vec<(&'a str, &'a Segment)>,
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

enum Piece<'p> {
    Static(&'p [u8]),
    Param(&'p str),
}

impl<T> Router<T> {
    pub fn new(limits: RouterLimits) -> Result<Self, RouteError> {
        if limits.max_routes == 0 || limits.max_match_work == 0 {
            return Err(RouteError::InvalidLimits);
        }
        let mut nodes = Vec::new();
        nodes
            .try_reserve(1)
            .map_err(|_| RouteError::AllocationFailed)?;
        nodes.push(Node::default());
        Ok(Self {
            limits,
            nodes,
            routes: Vec::new(),
        })
    }
    pub fn len(&self) -> usize {
        self.routes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }
    pub fn value(&self, id: RouteId) -> Option<&T> {
        self.routes.get(id.0).map(|route| &route.value)
    }

    /// Register a route. The router is unchanged on every error.
    pub fn insert(&mut self, method: &str, pattern: &str, value: T) -> Result<RouteId, RouteError> {
        if method.is_empty() || !method.bytes().all(crate::token) {
            return Err(RouteError::InvalidMethod);
        }
        if self.routes.len() == self.limits.max_routes {
            return Err(RouteError::RouteLimit);
        }
        let (pieces, trailing) = parse_pattern(pattern, self.limits.max_route_segments)?;
        // Dry run: find conflicts and count new nodes without mutating.
        let mut node = 0;
        let mut new_nodes = 0usize;
        let mut walking = true;
        for piece in &pieces {
            if !walking {
                new_nodes += 1;
                continue;
            }
            let current = &self.nodes[node];
            let next = match piece {
                Piece::Static(bytes) => current
                    .statics
                    .binary_search_by(|(key, _)| key.as_slice().cmp(bytes))
                    .ok()
                    .map(|index| current.statics[index].1),
                Piece::Param(name) => match &current.param {
                    Some((existing, child)) if existing == name => Some(*child),
                    Some((existing, _)) => {
                        return Err(RouteError::AmbiguousParameter {
                            existing: existing.clone(),
                            new: (*name).to_owned(),
                        });
                    }
                    None => None,
                },
            };
            match next {
                Some(child) => node = child,
                None => {
                    walking = false;
                    new_nodes += 1;
                }
            }
        }
        if walking
            && self.nodes[node].terminals[usize::from(trailing)]
                .binary_search_by(|(m, _)| m.as_str().cmp(method))
                .is_ok()
        {
            return Err(RouteError::DuplicateRoute);
        }
        self.nodes
            .try_reserve(new_nodes)
            .map_err(|_| RouteError::AllocationFailed)?;
        self.routes
            .try_reserve(1)
            .map_err(|_| RouteError::AllocationFailed)?;
        // Every rejection was found by the dry run above, so no Err path exists
        // below and a failed insert never leaves partial nodes. The small
        // per-node vector/key copies below use infallible (abort-on-OOM)
        // allocation; the node and route tables were reserved above.
        let id = RouteId(self.routes.len());
        let mut node = 0;
        let mut params = Vec::new();
        for piece in &pieces {
            node = match piece {
                Piece::Static(bytes) => {
                    match self.nodes[node]
                        .statics
                        .binary_search_by(|(key, _)| key.as_slice().cmp(bytes))
                    {
                        Ok(index) => self.nodes[node].statics[index].1,
                        Err(index) => {
                            let child = self.nodes.len();
                            self.nodes.push(Node::default());
                            self.nodes[node]
                                .statics
                                .insert(index, (bytes.to_vec(), child));
                            child
                        }
                    }
                }
                Piece::Param(name) => {
                    params.push((*name).to_owned());
                    match &self.nodes[node].param {
                        Some((_, child)) => *child,
                        None => {
                            let child = self.nodes.len();
                            self.nodes.push(Node::default());
                            self.nodes[node].param = Some(((*name).to_owned(), child));
                            child
                        }
                    }
                }
            };
        }
        let terminals = &mut self.nodes[node].terminals[usize::from(trailing)];
        let index = terminals
            .binary_search_by(|(m, _)| m.as_str().cmp(method))
            .expect_err("duplicate checked above");
        terminals.insert(index, (method.to_owned(), id));
        self.routes.push(RouteEntry { value, params });
        Ok(id)
    }

    /// Resolve a decoded target. `OPTIONS *` never matches. Exhausting the
    /// work budget is an error, never a not-found result.
    pub fn resolve<'a>(
        &'a self,
        method: &str,
        target: &'a Target,
    ) -> Result<Resolution<'a, T>, RouteError> {
        let Target::Origin(origin) = target else {
            return Ok(Resolution::NotFound);
        };
        let mut work = 0usize;
        let mut captured = Vec::new();
        captured
            .try_reserve_exact(origin.segments.len())
            .map_err(|_| RouteError::AllocationFailed)?;
        let Some(node) = self.find(0, origin, 0, &mut captured, &mut work)? else {
            return Ok(Resolution::NotFound);
        };
        let terminals = &self.nodes[node].terminals[usize::from(origin.trailing_slash)];
        let lookup = |name: &str| {
            terminals
                .binary_search_by(|(m, _)| m.as_str().cmp(name))
                .ok()
                .map(|index| terminals[index].1)
        };
        let (id, head_fallback) = match lookup(method) {
            Some(id) => (id, false),
            None if method == "HEAD" => match lookup("GET") {
                Some(id) => (id, true),
                None => return Ok(self.not_allowed(terminals)),
            },
            None => return Ok(self.not_allowed(terminals)),
        };
        let route = &self.routes[id.0];
        let params = route
            .params
            .iter()
            .map(String::as_str)
            .zip(captured.iter().map(|index| &origin.segments[*index]))
            .collect();
        Ok(Resolution::Matched(RouteMatch {
            id,
            value: &route.value,
            head_fallback,
            params,
        }))
    }
    fn not_allowed<'a>(&self, terminals: &'a [(String, RouteId)]) -> Resolution<'a, T> {
        let mut allowed: Vec<&str> = terminals.iter().map(|(m, _)| m.as_str()).collect();
        if allowed.contains(&"GET") && !allowed.contains(&"HEAD") {
            let index = allowed.binary_search(&"HEAD").expect_err("absent");
            allowed.insert(index, "HEAD");
        }
        Resolution::MethodNotAllowed(allowed)
    }
    /// Depth-first, static before parameter: the first complete path found is
    /// the highest-precedence one. `captured` holds segment indexes of params.
    fn find(
        &self,
        node: usize,
        target: &OriginTarget,
        depth: usize,
        captured: &mut Vec<usize>,
        work: &mut usize,
    ) -> Result<Option<usize>, RouteError> {
        *work += 1;
        if *work > self.limits.max_match_work {
            return Err(RouteError::WorkLimit);
        }
        let current = &self.nodes[node];
        let Some(segment) = target.segments.get(depth) else {
            let has_path = !current.terminals[usize::from(target.trailing_slash)].is_empty();
            return Ok(has_path.then_some(node));
        };
        if let Ok(index) = current
            .statics
            .binary_search_by(|(key, _)| key.as_slice().cmp(segment.as_bytes()))
            && let Some(found) =
                self.find(current.statics[index].1, target, depth + 1, captured, work)?
        {
            return Ok(Some(found));
        }
        if let Some((_, child)) = &current.param {
            captured.push(depth);
            if let Some(found) = self.find(*child, target, depth + 1, captured, work)? {
                return Ok(Some(found));
            }
            captured.pop();
        }
        Ok(None)
    }
}

fn parse_pattern(pattern: &str, max_segments: usize) -> Result<(Vec<Piece<'_>>, bool), RouteError> {
    let rest = pattern
        .strip_prefix('/')
        .ok_or(RouteError::InvalidPattern)?;
    let (rest, trailing) = match rest.strip_suffix('/') {
        Some(stripped) if !stripped.is_empty() => (stripped, true),
        Some(_) => return Err(RouteError::InvalidPattern), // `//`
        None => (rest, false),
    };
    if rest.is_empty() {
        return Ok((Vec::new(), false));
    }
    let count = rest.split('/').count();
    if count > max_segments {
        return Err(RouteError::SegmentLimit);
    }
    let mut pieces = Vec::new();
    pieces
        .try_reserve_exact(count)
        .map_err(|_| RouteError::AllocationFailed)?;
    let mut names: Vec<&str> = Vec::new();
    for segment in rest.split('/') {
        if let Some(name) = segment.strip_prefix(':') {
            let mut chars = name.chars();
            if !chars
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                return Err(RouteError::InvalidParameterName);
            }
            if names.contains(&name) {
                return Err(RouteError::DuplicateParameterName);
            }
            names.push(name);
            pieces.push(Piece::Param(name));
        } else {
            // Literals are decoded text, so they obey the target policy:
            // no separators, NUL, dot segments or percent (no second decode).
            if segment.is_empty()
                || segment == "."
                || segment == ".."
                || segment.contains(['\\', '\0', '%', '?', '#'])
                || segment.chars().any(char::is_control)
            {
                return Err(RouteError::InvalidPattern);
            }
            pieces.push(Piece::Static(segment.as_bytes()));
        }
    }
    Ok((pieces, trailing))
}
