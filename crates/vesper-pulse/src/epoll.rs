//! Linux epoll completion driver (see [`crate::readiness`] for the protocol).
//!
//! This is the driver the scheduler uses where `io_uring_setup` is denied
//! (seccomp-filtered containers answer `EPERM`). Registration is one-shot
//! (`EPOLLONESHOT`); a request completes after one readiness event and one
//! non-blocking read or write.
#![allow(unsafe_code)]
use crate::driver::{
    Completion, CompletionDriver, DriverKind, PollError, Rejected, Request, RequestId,
    check_entries,
};
use crate::readiness::{Armed, Interest, Poller, Readiness};
use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
};

const BATCH: usize = 64;

pub(crate) struct Epoll {
    fd: OwnedFd,
    events: Vec<libc::epoll_event>,
}

fn mask(interest: Interest) -> u32 {
    let base = (libc::EPOLLONESHOT | libc::EPOLLRDHUP) as u32;
    base | match interest {
        Interest::Read => libc::EPOLLIN as u32,
        Interest::Write => libc::EPOLLOUT as u32,
    }
}

fn check(rc: libc::c_int) -> io::Result<libc::c_int> {
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(rc)
    }
}

impl Epoll {
    fn new() -> io::Result<Self> {
        // SAFETY: epoll_create1 takes no pointers; ownership of the returned
        // descriptor moves into OwnedFd immediately.
        let fd = check(unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) })?;
        Ok(Self {
            // SAFETY: fd is a fresh descriptor owned by nobody else.
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
            events: vec![libc::epoll_event { events: 0, u64: 0 }; BATCH],
        })
    }
    fn ctl(&self, op: libc::c_int, fd: RawFd, interest: Interest, token: u64) -> io::Result<()> {
        let mut event = libc::epoll_event {
            events: mask(interest),
            u64: token,
        };
        // SAFETY: event is a valid epoll_event for the duration of the call.
        check(unsafe { libc::epoll_ctl(self.fd.as_raw_fd(), op, fd, &mut event) }).map(|_| ())
    }
}

impl Poller for Epoll {
    fn arm(&mut self, fd: RawFd, interest: Interest, token: u64) -> io::Result<Armed> {
        match self.ctl(libc::EPOLL_CTL_ADD, fd, interest, token) {
            Ok(()) => Ok(Armed::Registered),
            // epoll refuses descriptors whose file has no poll method: regular
            // files and devices such as /dev/null, which are always ready.
            Err(e) if e.raw_os_error() == Some(libc::EPERM) => Ok(Armed::AlwaysReady),
            Err(e) => Err(e),
        }
    }
    fn rearm(&mut self, fd: RawFd, interest: Interest, token: u64) -> io::Result<()> {
        self.ctl(libc::EPOLL_CTL_MOD, fd, interest, token)
    }
    fn release(&mut self, fd: RawFd, interest: Interest) -> io::Result<()> {
        self.deregister(fd, interest)
    }
    fn deregister(&mut self, fd: RawFd, _: Interest) -> io::Result<()> {
        // SAFETY: EPOLL_CTL_DEL ignores the event pointer on Linux >= 2.6.9.
        check(unsafe {
            libc::epoll_ctl(
                self.fd.as_raw_fd(),
                libc::EPOLL_CTL_DEL,
                fd,
                std::ptr::null_mut(),
            )
        })
        .map(|_| ())
    }
    fn wait(&mut self, block: bool, fired: &mut Vec<u64>) -> io::Result<()> {
        let timeout = if block { -1 } else { 0 };
        let n = loop {
            // SAFETY: events has BATCH initialised entries the kernel may fill.
            let rc = unsafe {
                libc::epoll_wait(
                    self.fd.as_raw_fd(),
                    self.events.as_mut_ptr(),
                    BATCH as libc::c_int,
                    timeout,
                )
            };
            match check(rc) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                other => break other? as usize,
            }
        };
        fired.extend(self.events[..n].iter().map(|e| e.u64));
        Ok(())
    }
}

/// The epoll completion driver.
pub struct EpollDriver<T> {
    inner: Readiness<T, Epoll>,
}

impl<T> EpollDriver<T> {
    /// `entries` bounds the requests the driver owns at once (in flight plus
    /// completed-but-unpolled), like an io_uring queue size.
    pub fn new(entries: u32) -> io::Result<Self> {
        let capacity = check_entries(entries)?;
        Ok(Self {
            inner: Readiness::new(Epoll::new()?, DriverKind::Epoll, capacity),
        })
    }
    /// Requests registered with epoll and not yet fired.
    pub fn in_flight(&self) -> usize {
        self.inner.in_flight()
    }
}

impl<T> CompletionDriver<T> for EpollDriver<T> {
    fn kind(&self) -> DriverKind {
        self.inner.kind()
    }
    fn pending(&self) -> usize {
        self.inner.pending()
    }
    fn submit(&mut self, request: Request<T>) -> Result<RequestId, Rejected<T>> {
        self.inner.submit(request)
    }
    fn cancel(&mut self, id: RequestId) -> io::Result<bool> {
        self.inner.cancel(id)
    }
    fn cancel_all(&mut self) -> io::Result<()> {
        self.inner.cancel_all()
    }
    fn poll(&mut self, wait: bool) -> Result<Vec<Completion<T>>, PollError<T>> {
        self.inner.poll(wait)
    }
}

impl<T> Drop for EpollDriver<T> {
    fn drop(&mut self) {
        crate::driver::note_abandoned(self.inner.abandon());
    }
}
