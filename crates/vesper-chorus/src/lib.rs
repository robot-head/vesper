#![doc = include_str!("../README.md")]
// The only unsafe code is the generated binding to the Vesper library
// (export ABI calls), which allows it locally.
#![deny(unsafe_code)]

pub mod check;
pub mod chor;
pub mod durable;
pub mod parse;
mod port;
pub mod project;
pub mod raft;
pub mod rdma;
pub mod replog;
pub mod session;
pub mod simnet;
pub mod warpnet;

pub use chor::{Choreography, GBranch, GlobalChor, RoleId, WellFormedError};
pub use parse::{ParseError, ParseErrorKind, parse_choreography};
pub use project::{LBranch, LocalType, ProjectionError, are_dual, project, project_all};

/// Which build of the ported entry points (parsing, well-formedness,
/// projection, duality, exploration) this is. The Vesper library is the
/// only build since port wave P3b lane 6 (#1068).
pub const IMPLEMENTATION: &str = "vesper";

/// Live sequence allocations of the Vesper library, for leak tests.
pub fn port_live_allocations() -> i64 {
    port::live_allocations()
}
