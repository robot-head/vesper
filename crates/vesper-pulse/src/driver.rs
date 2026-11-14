//! Backend-neutral completion-driver types.
//!
//! A [`Request`] owns its file, buffer and one-shot continuation from
//! submission until its matching [`Completion`]. Every driver (io_uring,
//! epoll, kqueue, IOCP) follows the same protocol:
//!
//! * `submit` either installs the request and returns a fresh [`RequestId`]
//!   (IDs are never reused), or rejects it and returns the request unchanged;
//! * `poll` returns every completion harvested in that batch, even when the
//!   batch also hit an error ([`PollError`]);
//! * `cancel`/`cancel_all` never release a request directly: a cancelled
//!   request still completes exactly once, normally with a cancellation error
//!   (see [`is_cancelled`]), or with its real result when it had already
//!   finished;
//! * [`CompletionDriver::drain`] cancels every in-flight request and polls
//!   until the driver owns nothing, returning every continuation.
use std::{fmt, fs::File, io};

/// The completion drivers this crate can run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DriverKind {
    /// Linux io_uring: the kernel performs the operation asynchronously.
    IoUring,
    /// Linux epoll readiness, with inline positional I/O for files that are
    /// always ready (regular files and most character devices).
    Epoll,
    /// BSD/macOS kqueue readiness, with the same always-ready rule.
    Kqueue,
    /// Windows I/O completion ports.
    Iocp,
}

impl DriverKind {
    pub const ALL: [DriverKind; 4] = [
        DriverKind::IoUring,
        DriverKind::Epoll,
        DriverKind::Kqueue,
        DriverKind::Iocp,
    ];

    /// Stable lowercase name, also accepted by `VESPER_PULSE_DRIVER`.
    pub fn name(self) -> &'static str {
        match self {
            DriverKind::IoUring => "io_uring",
            DriverKind::Epoll => "epoll",
            DriverKind::Kqueue => "kqueue",
            DriverKind::Iocp => "iocp",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim().to_ascii_lowercase();
        match name.as_str() {
            "io_uring" | "io-uring" | "uring" => Some(DriverKind::IoUring),
            "epoll" => Some(DriverKind::Epoll),
            "kqueue" => Some(DriverKind::Kqueue),
            "iocp" => Some(DriverKind::Iocp),
            _ => None,
        }
    }

    /// Whether this build contains the driver (it may still be denied at run
    /// time, as io_uring is on hosts whose seccomp policy rejects it).
    pub fn compiled(self) -> bool {
        match self {
            DriverKind::IoUring | DriverKind::Epoll => cfg!(target_os = "linux"),
            DriverKind::Kqueue => cfg!(target_os = "macos"),
            DriverKind::Iocp => cfg!(windows),
        }
    }
}

impl fmt::Display for DriverKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(pub(crate) u64);

impl RequestId {
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Read,
    Write,
}

/// Moving a request moves its Vec header, never the allocation exposed to the
/// kernel. A driver provides exclusive ownership until the matching completion.
///
/// `offset` is the file position for seekable files. Stream handles (pipes,
/// sockets, terminals) ignore it on io_uring and IOCP; the readiness drivers
/// reject a non-zero offset on a stream with `ESPIPE`.
pub struct Request<T> {
    pub file: File,
    pub buffer: Vec<u8>,
    pub continuation: T,
    pub operation: Operation,
    pub offset: u64,
}

/// A finished request. A successful read truncates `request.buffer` to the
/// bytes read; a write leaves the buffer as submitted.
pub struct Completion<T> {
    pub id: RequestId,
    pub request: Request<T>,
    pub result: io::Result<usize>,
}

impl<T> fmt::Debug for Completion<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Completion")
            .field("id", &self.id)
            .field("operation", &self.request.operation)
            .field("result", &self.result)
            .finish_non_exhaustive()
    }
}

/// A failed poll still returns all completions harvested in that batch.
pub struct PollError<T> {
    pub error: io::Error,
    pub completed: Vec<Completion<T>>,
}

impl<T> fmt::Debug for PollError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PollError")
            .field("error", &self.error)
            .field("completed", &self.completed.len())
            .finish()
    }
}

/// A rejected submission: the error and the untouched request.
pub type Rejected<T> = (io::Error, Request<T>);

/// A failed drain keeps the still-owned driver and every harvested completion,
/// so the caller can retry without losing a continuation.
pub struct DrainError<T, D> {
    pub error: io::Error,
    pub completed: Vec<Completion<T>>,
    pub driver: D,
}

impl<T, D: CompletionDriver<T>> fmt::Debug for DrainError<T, D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DrainError")
            .field("error", &self.error)
            .field("completed", &self.completed.len())
            .field("pending", &self.driver.pending())
            .finish()
    }
}

/// The backend-neutral completion protocol implemented by every driver.
pub trait CompletionDriver<T> {
    fn kind(&self) -> DriverKind;
    /// Requests owned by the driver: in flight or completed but not yet
    /// returned by `poll`.
    fn pending(&self) -> usize;
    fn submit(&mut self, request: Request<T>) -> Result<RequestId, Rejected<T>>;
    /// Request cancellation. `Ok(false)` means the request is unknown, already
    /// being cancelled, or already finished; it still completes exactly once.
    fn cancel(&mut self, id: RequestId) -> io::Result<bool>;
    fn cancel_all(&mut self) -> io::Result<()>;
    /// Harvest completions. `wait` blocks until at least one completion is
    /// available, unless the driver owns nothing.
    fn poll(&mut self, wait: bool) -> Result<Vec<Completion<T>>, PollError<T>>;

    /// Cancel every request and poll until the driver owns nothing.
    fn drain(mut self) -> Result<Vec<Completion<T>>, DrainError<T, Self>>
    where
        Self: Sized,
    {
        let mut completed = vec![];
        if let Err(error) = self.cancel_all() {
            return Err(DrainError {
                error,
                completed,
                driver: self,
            });
        }
        while self.pending() > 0 {
            match self.poll(true) {
                Ok(mut batch) => completed.append(&mut batch),
                Err(mut e) => {
                    completed.append(&mut e.completed);
                    return Err(DrainError {
                        error: e.error,
                        completed,
                        driver: self,
                    });
                }
            }
        }
        Ok(completed)
    }
}

/// The OS error a driver reports for a request it cancelled: `ECANCELED` on
/// Unix, `ERROR_OPERATION_ABORTED` on Windows.
pub fn cancelled_error() -> io::Error {
    io::Error::from_raw_os_error(CANCELLED_CODE)
}

pub fn is_cancelled(error: &io::Error) -> bool {
    error.raw_os_error() == Some(CANCELLED_CODE)
}

#[cfg(all(unix, not(target_os = "macos")))]
const CANCELLED_CODE: i32 = 125;
#[cfg(target_os = "macos")]
const CANCELLED_CODE: i32 = 89;
#[cfg(windows)]
const CANCELLED_CODE: i32 = 995;
#[cfg(not(any(unix, windows)))]
const CANCELLED_CODE: i32 = 125;

static ABANDONED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Requests whose continuations were discarded because a readiness or IOCP
/// driver was dropped while it still owned them, instead of being drained
/// with [`CompletionDriver::drain`]. Memory is never released while a kernel
/// can still write to it; this counter makes the lost one-shot continuations
/// observable instead of silent.
pub fn abandoned_requests() -> u64 {
    ABANDONED.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", windows)),
    allow(dead_code)
)]
pub(crate) fn note_abandoned(n: usize) {
    if n > 0 {
        ABANDONED.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
    }
}

pub(crate) fn queue_full() -> io::Error {
    io::Error::new(io::ErrorKind::WouldBlock, "I/O submission queue is full")
}

pub(crate) fn check_entries(entries: u32) -> io::Result<usize> {
    if entries == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "I/O queue must have entries",
        ));
    }
    Ok(entries as usize)
}

/// Run `ids` through a fallible step, keeping going after errors and
/// reporting the first one, so one bad entry never strands its siblings.
pub(crate) fn first_error<I>(
    items: I,
    mut step: impl FnMut(I::Item) -> io::Result<()>,
) -> io::Result<()>
where
    I: IntoIterator,
{
    let mut first = None;
    for item in items {
        if let Err(e) = step(item)
            && first.is_none()
        {
            first = Some(e);
        }
    }
    first.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn driver_names_round_trip() {
        for kind in DriverKind::ALL {
            assert_eq!(DriverKind::from_name(kind.name()), Some(kind));
        }
        assert_eq!(
            DriverKind::from_name(" IO-URING "),
            Some(DriverKind::IoUring)
        );
        assert_eq!(DriverKind::from_name("select"), None);
    }
    #[test]
    fn exactly_the_host_drivers_are_compiled() {
        let compiled: Vec<_> = DriverKind::ALL
            .into_iter()
            .filter(|k| k.compiled())
            .collect();
        if cfg!(target_os = "linux") {
            assert_eq!(compiled, [DriverKind::IoUring, DriverKind::Epoll]);
        } else if cfg!(target_os = "macos") {
            assert_eq!(compiled, [DriverKind::Kqueue]);
        } else if cfg!(windows) {
            assert_eq!(compiled, [DriverKind::Iocp]);
        }
    }
    #[test]
    fn cancellation_is_recognised() {
        assert!(is_cancelled(&cancelled_error()));
        assert!(!is_cancelled(&io::Error::other("x")));
    }
}
