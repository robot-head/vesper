//! Waiting for GPU timeline points from a Pulse driver (§19.9; no `unsafe`).
//!
//! [`SyncFdWaiter`] submits a one-byte read of each watched [`SyncFd`] to a
//! Pulse [`EpollDriver`], so GPU completion arrives through the same
//! completion interface as sockets and files. Pulse's epoll driver arms the
//! descriptor first and reads only after readiness, which is what a
//! `sync_file` needs: it polls readable (`EPOLLIN`) once its fence signals,
//! and a read then fails with `EINVAL` (a sync_file has no read method).
//! A bridged fd is a pipe that receives one byte. Pulse's io_uring driver is
//! not used here: io_uring issues the read immediately, which fails with
//! `EINVAL` before the fence signals (and `io_uring_setup` is denied in the
//! cloud sandbox anyway).

use crate::error::VulkanError;
use crate::vk::SyncFd;
use std::io::ErrorKind;
use vesper_pulse::driver::{Completion, CompletionDriver, Operation, Request};
use vesper_pulse::epoll::EpollDriver;

/// Watches sync fds on a Pulse epoll driver; see the module docs.
pub struct SyncFdWaiter {
    driver: EpollDriver<u64>,
}

impl std::fmt::Debug for SyncFdWaiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncFdWaiter")
            .field("pending", &self.driver.pending())
            .finish()
    }
}

fn io(e: std::io::Error) -> VulkanError {
    VulkanError::Io(e.to_string())
}

impl SyncFdWaiter {
    /// A waiter for up to `capacity` fds at once.
    pub fn new(capacity: u32) -> Result<Self, VulkanError> {
        Ok(Self {
            driver: EpollDriver::new(capacity).map_err(io)?,
        })
    }

    /// Watches `fd` (through a duplicate descriptor); [`poll`](Self::poll)
    /// reports `token` once the fd's timeline point has signalled.
    pub fn watch(&mut self, fd: &SyncFd, token: u64) -> Result<(), VulkanError> {
        let request = Request {
            file: fd.try_clone_file()?,
            buffer: vec![0; 1],
            continuation: token,
            operation: Operation::Read,
            offset: 0,
        };
        self.driver
            .submit(request)
            .map(|_| ())
            .map_err(|(e, _)| io(e))
    }

    /// Fds watched and not yet reported.
    pub fn pending(&self) -> usize {
        self.driver.pending()
    }

    /// The tokens of fds that signalled; with `wait`, blocks until at least
    /// one has (unless nothing is watched).
    pub fn poll(&mut self, wait: bool) -> Result<Vec<u64>, VulkanError> {
        let completions = self.driver.poll(wait).map_err(|e| io(e.error))?;
        completions.into_iter().map(signalled).collect()
    }
}

fn signalled(c: Completion<u64>) -> Result<u64, VulkanError> {
    match c.result {
        // A bridged fd: the waiting thread wrote its byte.
        Ok(n) if n > 0 => Ok(c.request.continuation),
        Ok(_) => Err(VulkanError::Io(
            "sync fd bridge closed without a signal".into(),
        )),
        // A sync_file that polled readable: its fence has signalled.
        Err(e) if e.kind() == ErrorKind::InvalidInput => Ok(c.request.continuation),
        Err(e) => Err(io(e)),
    }
}
