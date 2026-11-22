//! Forwards [`crate::router::Router`] to the `vesper/router` package. Vesper holds the
//! trie and decides every match; the adapter keeps each route's payload `T`,
//! method and parameter names, indexed by the route ids Vesper hands out.

use crate::router::{Resolution, RouteError, RouteId, RouteMatch, RouterLimits};
use crate::target::Target;

/// The types at the library boundary (`--rust-types`).
pub(crate) mod abi {
    pub use crate::router::RouterLimits;

    /// Terminal records (`RouterCore::terms`).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Term {
        pub next: u64,
        pub method_lo: u64,
        pub method_hi: u64,
        pub route: u64,
    }

    /// `RouteError`, with the ambiguous parameter names as byte ranges: the
    /// existing name in the router, the new one in the pattern.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum RouteFault {
        InvalidLimits,
        InvalidMethod,
        InvalidPattern,
        InvalidParameterName,
        DuplicateParameterName,
        RouteLimit,
        SegmentLimit,
        DuplicateRoute,
        AmbiguousParameter {
            existing_lo: u64,
            existing_hi: u64,
            new_lo: u64,
            new_hi: u64,
        },
        WorkLimit,
        AllocationFailed,
    }
}

#[allow(unsafe_code)]
#[rustfmt::skip]
#[path = "../../port/router.rs"]
mod binding;

use abi::RouteFault;

thread_local! {
    /// `Router::resolve`'s segment buffer and end offsets.
    static SCRATCH: std::cell::RefCell<(Vec<u8>, Vec<u64>)> =
        const { std::cell::RefCell::new((Vec::new(), Vec::new())) };
}

/// What `Resolved::kind` means (see the `vesper/router` package).
const MATCHED: u64 = 0;
const METHOD_NOT_ALLOWED: u64 = 1;
/// The `allowed` entry for the HEAD that a GET route implies.
const IMPLIED_HEAD: u64 = u64::MAX;

pub struct Router<T> {
    core: binding::RouterCore,
    routes: Vec<RouteEntry<T>>,
}
struct RouteEntry<T> {
    value: T,
    method: String,
    params: Vec<String>,
}

impl<T> Router<T> {
    pub fn new(limits: RouterLimits) -> Result<Self, RouteError> {
        let core = binding::RouterCore::new(limits).map_err(|fault| route_error(fault, &[], ""))?;
        Ok(Self {
            core,
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
        match self.core.insert(method.as_bytes(), pattern.as_bytes()) {
            Err(fault) => Err(route_error(
                fault,
                &self.core.as_ref().names().to_vec(),
                pattern,
            )),
            Ok(id) => {
                let ranges = binding::pattern_params(pattern.as_bytes());
                let params = ranges
                    .chunks(2)
                    .map(|range| pattern[range[0] as usize..range[1] as usize].to_owned())
                    .collect();
                self.routes.push(RouteEntry {
                    value,
                    method: method.to_owned(),
                    params,
                });
                Ok(RouteId(id as usize))
            }
        }
    }

    /// Resolve a decoded target. `OPTIONS *` never matches. Exhausting the
    /// work budget is an error, never a not-found result.
    pub fn resolve<'a>(
        &'a self,
        method: &str,
        target: &'a Target,
    ) -> Result<Resolution<'a, T>, RouteError> {
        // The segments go to Vesper as one byte buffer and their end
        // offsets, kept per thread so a resolve allocates neither.
        let resolved = SCRATCH
            .with(|scratch| {
                let (bytes, ends) = &mut *scratch.borrow_mut();
                bytes.clear();
                ends.clear();
                let mut trailing = false;
                if let Target::Origin(origin) = target {
                    for segment in &origin.segments {
                        bytes.extend_from_slice(segment.as_bytes());
                        ends.push(bytes.len() as u64);
                    }
                    trailing = origin.trailing_slash;
                }
                self.core.resolve(
                    method.as_bytes(),
                    matches!(target, Target::Asterisk),
                    bytes,
                    ends,
                    trailing,
                )
            })
            .map_err(|fault| route_error(fault, &[], ""))?;
        let view = resolved.as_ref();
        Ok(match (view.kind(), target) {
            (MATCHED, Target::Origin(origin)) => {
                let id = view.route() as usize;
                let route = &self.routes[id];
                let params = route
                    .params
                    .iter()
                    .map(String::as_str)
                    .zip(
                        view.captured()
                            .iter()
                            .map(|index| &origin.segments[index as usize]),
                    )
                    .collect();
                Resolution::Matched(RouteMatch {
                    id: RouteId(id),
                    value: &route.value,
                    head_fallback: view.head_fallback(),
                    params,
                })
            }
            (METHOD_NOT_ALLOWED, _) => Resolution::MethodNotAllowed(
                view.allowed()
                    .iter()
                    .map(|id| match id {
                        IMPLIED_HEAD => "HEAD",
                        id => self.routes[id as usize].method.as_str(),
                    })
                    .collect(),
            ),
            _ => Resolution::NotFound,
        })
    }
}

fn route_error(fault: RouteFault, names: &[u8], pattern: &str) -> RouteError {
    match fault {
        RouteFault::InvalidLimits => RouteError::InvalidLimits,
        RouteFault::InvalidMethod => RouteError::InvalidMethod,
        RouteFault::InvalidPattern => RouteError::InvalidPattern,
        RouteFault::InvalidParameterName => RouteError::InvalidParameterName,
        RouteFault::DuplicateParameterName => RouteError::DuplicateParameterName,
        RouteFault::RouteLimit => RouteError::RouteLimit,
        RouteFault::SegmentLimit => RouteError::SegmentLimit,
        RouteFault::DuplicateRoute => RouteError::DuplicateRoute,
        RouteFault::AmbiguousParameter {
            existing_lo,
            existing_hi,
            new_lo,
            new_hi,
        } => RouteError::AmbiguousParameter {
            existing: String::from_utf8_lossy(&names[existing_lo as usize..existing_hi as usize])
                .into_owned(),
            new: pattern[new_lo as usize..new_hi as usize].to_owned(),
        },
        RouteFault::WorkLimit => RouteError::WorkLimit,
        RouteFault::AllocationFailed => RouteError::AllocationFailed,
    }
}
