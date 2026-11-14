//! Owned frame storage and completion scheduling independent of the compiler.
//!
//! Frame storage, the scheduler and the completion-driver protocol
//! ([`driver::CompletionDriver`]) are portable. The host drivers are io_uring
//! and epoll on Linux, kqueue on macOS and I/O completion ports on Windows;
//! [`select::open`] picks one at run time and reports which. The evaluator
//! embedding bridge stays in the vesper facade; native continuation lowering
//! is still a separate integration milestone.
#![deny(unsafe_code)]
pub mod driver;
#[cfg(target_os = "linux")]
pub mod epoll;
pub mod frames;
pub mod io_driver;
#[cfg(windows)]
pub mod iocp;
#[cfg(target_os = "macos")]
pub mod kqueue;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod readiness;
pub mod scheduler;
pub mod select;
#[cfg(target_os = "linux")]
pub mod uring;
