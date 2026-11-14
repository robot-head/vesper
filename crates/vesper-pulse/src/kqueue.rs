//! macOS kqueue completion driver (see [`crate::readiness`] for the protocol).
//!
//! Stream descriptors are registered with `EV_ADD | EV_ONESHOT` on
//! `EVFILT_READ` or `EVFILT_WRITE`; a fired one-shot filter is removed by the
//! kernel. Regular files are always ready and serviced inline, as are devices
//! that do not support kqueue filters (registration answers `EINVAL`,
//! `ENODEV` or `ENOTSUP`).
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

pub(crate) struct Kqueue {
    fd: OwnedFd,
    events: Vec<libc::kevent>,
}

fn check(rc: libc::c_int) -> io::Result<libc::c_int> {
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(rc)
    }
}

fn filter(interest: Interest) -> i16 {
    match interest {
        Interest::Read => libc::EVFILT_READ,
        Interest::Write => libc::EVFILT_WRITE,
    }
}

fn change(fd: RawFd, interest: Interest, flags: u16, token: u64) -> libc::kevent {
    libc::kevent {
        ident: fd as libc::uintptr_t,
        filter: filter(interest),
        flags,
        fflags: 0,
        data: 0,
        udata: token as usize as *mut libc::c_void,
    }
}

impl Kqueue {
    fn new() -> io::Result<Self> {
        // SAFETY: kqueue takes no arguments; the descriptor moves into OwnedFd.
        let fd = check(unsafe { libc::kqueue() })?;
        // SAFETY: fd is fresh and owned by nobody else.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        // SAFETY: fcntl on our own descriptor, no pointers.
        check(unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) })?;
        Ok(Self {
            fd,
            events: (0..BATCH)
                .map(|_| change(0, Interest::Read, 0, 0))
                .collect(),
        })
    }
    fn apply(&self, event: libc::kevent) -> io::Result<()> {
        // SAFETY: one valid change, no event list, null timeout.
        check(unsafe {
            libc::kevent(
                self.fd.as_raw_fd(),
                &event,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        })
        .map(|_| ())
    }
}

fn regular_file(fd: RawFd) -> io::Result<bool> {
    // SAFETY: fstat writes one stat struct we own.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    check(unsafe { libc::fstat(fd, &mut st) })?;
    Ok(st.st_mode & libc::S_IFMT == libc::S_IFREG)
}

impl Poller for Kqueue {
    fn arm(&mut self, fd: RawFd, interest: Interest, token: u64) -> io::Result<Armed> {
        if regular_file(fd)? {
            return Ok(Armed::AlwaysReady);
        }
        match self.apply(change(fd, interest, libc::EV_ADD | libc::EV_ONESHOT, token)) {
            Ok(()) => Ok(Armed::Registered),
            Err(e)
                if matches!(
                    e.raw_os_error(),
                    Some(libc::EINVAL | libc::ENODEV | libc::ENOTSUP)
                ) =>
            {
                Ok(Armed::AlwaysReady)
            }
            Err(e) => Err(e),
        }
    }
    fn rearm(&mut self, fd: RawFd, interest: Interest, token: u64) -> io::Result<()> {
        self.apply(change(fd, interest, libc::EV_ADD | libc::EV_ONESHOT, token))
    }
    fn release(&mut self, _: RawFd, _: Interest) -> io::Result<()> {
        // EV_ONESHOT filters are deleted by the kernel when they fire.
        Ok(())
    }
    fn deregister(&mut self, fd: RawFd, interest: Interest) -> io::Result<()> {
        match self.apply(change(fd, interest, libc::EV_DELETE, 0)) {
            // Already fired and removed by the kernel but not yet harvested.
            Err(e) if e.raw_os_error() == Some(libc::ENOENT) => Ok(()),
            other => other,
        }
    }
    fn wait(&mut self, block: bool, fired: &mut Vec<u64>) -> io::Result<()> {
        let zero = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        let timeout = if block { std::ptr::null() } else { &zero };
        let n = loop {
            // SAFETY: events holds BATCH entries the kernel may overwrite.
            let rc = unsafe {
                libc::kevent(
                    self.fd.as_raw_fd(),
                    std::ptr::null(),
                    0,
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
        let mut error = None;
        for event in &self.events[..n] {
            if event.flags & libc::EV_ERROR != 0 && event.data != 0 {
                error.get_or_insert(io::Error::from_raw_os_error(event.data as i32));
            }
            fired.push(event.udata as usize as u64);
        }
        error.map_or(Ok(()), Err)
    }
}

/// The kqueue completion driver.
pub struct KqueueDriver<T> {
    inner: Readiness<T, Kqueue>,
}

impl<T> KqueueDriver<T> {
    /// `entries` bounds the requests the driver owns at once.
    pub fn new(entries: u32) -> io::Result<Self> {
        let capacity = check_entries(entries)?;
        Ok(Self {
            inner: Readiness::new(Kqueue::new()?, DriverKind::Kqueue, capacity),
        })
    }
    /// Requests registered with kqueue and not yet fired.
    pub fn in_flight(&self) -> usize {
        self.inner.in_flight()
    }
}

impl<T> CompletionDriver<T> for KqueueDriver<T> {
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

impl<T> Drop for KqueueDriver<T> {
    fn drop(&mut self) {
        crate::driver::note_abandoned(self.inner.abandon());
    }
}
