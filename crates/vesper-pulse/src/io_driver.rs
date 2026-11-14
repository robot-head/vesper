//! Compatibility path for the request types, which are backend-neutral and
//! live in [`crate::driver`]. On Linux this module also exports the io_uring
//! driver, which lives in [`crate::uring`].
pub use crate::driver::{Completion, Operation, PollError, Request, RequestId};
#[cfg(target_os = "linux")]
pub use crate::uring::{IoDriver, ShutdownError};
