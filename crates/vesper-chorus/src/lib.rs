#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

pub mod check;
pub mod chor;
pub mod parse;
pub mod project;
pub mod raft;
pub mod rdma;
pub mod session;
pub mod simnet;

pub use chor::{Choreography, GBranch, GlobalChor, RoleId, WellFormedError};
pub use parse::{ParseError, ParseErrorKind, parse_choreography};
pub use project::{LBranch, LocalType, ProjectionError, are_dual, project, project_all};
