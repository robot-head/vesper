//! VMM threads that run under the compiled seccomp profile (§37.1.5).
//!
//! [`spawn_vmm_thread`] starts a named thread that installs a compiled
//! [`crate::seccomp`] program on itself (`PR_SET_NO_NEW_PRIVS`, then
//! `seccomp(SECCOMP_SET_MODE_FILTER)` without `TSYNC`, so the filter binds
//! only that thread and threads it creates) before it runs any VMM work. The
//! spawning thread learns the outcome of the install before `spawn_vmm_thread`
//! returns: a refused install is an error and the work never runs.
//!
//! With [`crate::seccomp::DefaultAction::KillThread`] a system call outside
//! the profile kills the VMM thread at the call, and the rest of the process
//! keeps running: [`VmmThread::wait`] then reports [`VmmWaitError::Killed`]
//! (the work never returned and the kernel task is gone). With `KillProcess`
//! the whole process dies with `SIGSYS`. The thread's stack and anything it
//! owned are leaked when it is killed; nothing it held is released.
//!
//! Liveness is read from `/proc/self/task/<tid>` (Linux); elsewhere the
//! install itself is unavailable, so no VMM thread starts.
#![forbid(unsafe_code)]

use crate::host::Unavailable;
use crate::seccomp::SockFilter;
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum VmmSpawnError {
    Spawn(std::io::Error),
    /// The kernel (or this build) refused the filter; the work did not run.
    Install(Unavailable),
    /// The thread ended before reporting the install.
    Lost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VmmWaitError {
    /// Still running when the timeout passed.
    Timeout,
    /// The thread is gone without a result: the filter killed it (or the work
    /// panicked, which also ends the thread without a result).
    Killed,
}

/// A running (or finished, or killed) VMM thread.
#[derive(Debug)]
pub struct VmmThread<T> {
    tid: Option<u32>,
    result: mpsc::Receiver<T>,
}

/// The kernel thread id of the calling thread, from `/proc/thread-self`.
fn current_tid() -> Option<u32> {
    let link = std::fs::read_link("/proc/thread-self").ok()?;
    link.file_name()?.to_str()?.parse().ok()
}

/// Spawns `name`, installs `program` on it, then runs `work` there.
pub fn spawn_vmm_thread<T, F>(
    name: &str,
    program: Vec<SockFilter>,
    work: F,
) -> Result<VmmThread<T>, VmmSpawnError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (ready_tx, ready_rx) = mpsc::channel::<Result<Option<u32>, Unavailable>>();
    let (done_tx, done_rx) = mpsc::channel::<T>();
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            // Everything that needs a call outside the profile happens first.
            let tid = current_tid();
            if let Err(e) = crate::host::install_seccomp(&program) {
                let _ = ready_tx.send(Err(e));
                return;
            }
            let _ = ready_tx.send(Ok(tid));
            drop(ready_tx);
            let r = work();
            let _ = done_tx.send(r);
        })
        .map_err(VmmSpawnError::Spawn)?;
    match ready_rx.recv() {
        Ok(Ok(tid)) => Ok(VmmThread {
            tid,
            result: done_rx,
        }),
        Ok(Err(e)) => Err(VmmSpawnError::Install(e)),
        Err(_) => Err(VmmSpawnError::Lost),
    }
}

impl<T> VmmThread<T> {
    /// The kernel thread id, where `/proc` reports it.
    pub fn tid(&self) -> Option<u32> {
        self.tid
    }
    /// Whether the kernel still has the thread. `None` without `/proc`.
    pub fn is_alive(&self) -> Option<bool> {
        let tid = self.tid?;
        Some(std::path::Path::new(&format!("/proc/self/task/{tid}")).exists())
    }
    /// Waits up to `timeout` for the work's result. A thread that the kernel
    /// no longer has and that sent nothing was killed.
    pub fn wait(&self, timeout: Duration) -> Result<T, VmmWaitError> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.result.recv_timeout(Duration::from_millis(10)) {
                Ok(r) => return Ok(r),
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(VmmWaitError::Killed),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if self.is_alive() == Some(false) {
                // A result sent just before exiting is still in the channel.
                return self.result.try_recv().map_err(|_| VmmWaitError::Killed);
            }
            if Instant::now() >= deadline {
                return Err(VmmWaitError::Timeout);
            }
        }
    }
}
