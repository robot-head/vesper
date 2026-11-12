//! Owned frame storage and completion scheduling independent of the compiler.
//!
//! Frame storage is portable. The io_uring driver and scheduler are currently
//! Linux-only. The evaluator embedding bridge stays in the vesper facade;
//! native continuation lowering is still a separate integration milestone.
pub mod frames;
#[cfg(target_os = "linux")]
pub mod io_driver;
#[cfg(target_os = "linux")]
pub mod scheduler;
