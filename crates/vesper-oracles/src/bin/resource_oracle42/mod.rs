//! Data-only executable endpoint for the existing finite resource model.
#![forbid(unsafe_code)]

pub mod process;
pub mod protocol;

pub use process::{Config, HarnessError, HarnessErrorKind, HarnessLimits, Session, start};
