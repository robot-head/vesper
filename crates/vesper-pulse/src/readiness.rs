//! Completion semantics over a readiness notifier (epoll or kqueue).
//!
//! A readiness kernel interface never holds a user buffer: the driver performs
//! the read or write itself once the descriptor is ready. Each request is in
//! exactly one place: `waiting` (registered with the poller) or `ready`
//! (finished, returned by the next `poll`).
//!
//! Descriptors the poller refuses because they are always ready (Linux epoll
//! answers `EPERM` for regular files and most character devices; kqueue is not
//! asked about regular files) are serviced inline with positional
//! `pread`/`pwrite` at the request offset, and their completion is delivered by
//! the next `poll`, never by `submit`. Stream descriptors (pipes, sockets,
//! terminals) are switched to `O_NONBLOCK` (and left so) and registered
//! one-shot; a spurious wake-up re-arms. Zero-length operations and operations
//! against the descriptor's access mode (`EBADF`) need no readiness and
//! complete at the next poll.
#![allow(unsafe_code)]
use crate::driver::{
    Completion, DriverKind, Operation, PollError, Rejected, Request, RequestId, cancelled_error,
    first_error, queue_full,
};
use std::{
    collections::{BTreeMap, VecDeque},
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, RawFd},
        unix::fs::FileExt,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Interest {
    Read,
    Write,
}

impl From<Operation> for Interest {
    fn from(op: Operation) -> Self {
        match op {
            Operation::Read => Interest::Read,
            Operation::Write => Interest::Write,
        }
    }
}

pub(crate) enum Armed {
    /// Registered one-shot; a later `wait` reports the token.
    Registered,
    /// The descriptor is always ready; the driver services it inline.
    AlwaysReady,
}

/// The system-call layer: epoll or kqueue.
pub(crate) trait Poller {
    fn arm(&mut self, fd: RawFd, interest: Interest, token: u64) -> io::Result<Armed>;
    /// Re-register after a spurious wake-up.
    fn rearm(&mut self, fd: RawFd, interest: Interest, token: u64) -> io::Result<()>;
    /// The request fired and finished: forget the registration.
    fn release(&mut self, fd: RawFd, interest: Interest) -> io::Result<()>;
    /// The request is cancelled before firing: remove the registration.
    fn deregister(&mut self, fd: RawFd, interest: Interest) -> io::Result<()>;
    /// Append fired tokens. `block` waits until at least one fires.
    fn wait(&mut self, block: bool, fired: &mut Vec<u64>) -> io::Result<()>;
}

pub(crate) struct Readiness<T, P> {
    pub(crate) poller: P,
    kind: DriverKind,
    capacity: usize,
    next: u64,
    waiting: BTreeMap<RequestId, Request<T>>,
    ready: VecDeque<Completion<T>>,
    fired: Vec<u64>,
}

impl<T, P: Poller> Readiness<T, P> {
    pub(crate) fn new(poller: P, kind: DriverKind, capacity: usize) -> Self {
        Self {
            poller,
            kind,
            capacity,
            next: 1,
            waiting: BTreeMap::new(),
            ready: VecDeque::new(),
            fired: Vec::new(),
        }
    }
    pub(crate) fn kind(&self) -> DriverKind {
        self.kind
    }
    pub(crate) fn pending(&self) -> usize {
        self.waiting.len() + self.ready.len()
    }
    pub(crate) fn in_flight(&self) -> usize {
        self.waiting.len()
    }
    pub(crate) fn submit(&mut self, mut request: Request<T>) -> Result<RequestId, Rejected<T>> {
        if self.pending() >= self.capacity {
            return Err((queue_full(), request));
        }
        if self.next == u64::MAX {
            return Err((io::Error::other("I/O request IDs exhausted"), request));
        }
        let id = RequestId(self.next);
        let interest = Interest::from(request.operation);
        let fd = request.file.as_raw_fd();
        let flags = match status_flags(fd) {
            Ok(flags) => flags,
            Err(e) => return Err((e, request)),
        };
        // Requests that can finish without readiness complete at the next
        // poll: a readiness event for them might never arrive (an empty pipe
        // never signals a zero-length read; a read-only end never signals
        // writability).
        let immediate = if wrong_direction(flags, request.operation) {
            Some(Err(io::Error::from_raw_os_error(libc::EBADF)))
        } else if request.buffer.is_empty() {
            Some(Ok(0))
        } else {
            None
        };
        if let Some(result) = immediate {
            self.next += 1;
            self.ready.push_back(finish(id, request, result));
            return Ok(id);
        }
        match self.poller.arm(fd, interest, id.0) {
            Err(e) => return Err((e, request)),
            Ok(Armed::Registered) => {
                if request.offset != 0 {
                    // A stream has no position; refusing beats silently
                    // reading from somewhere other than the requested offset.
                    let _ = self.poller.deregister(fd, interest);
                    return Err((io::Error::from_raw_os_error(libc::ESPIPE), request));
                }
                if let Err(e) = set_nonblocking(fd, flags) {
                    let _ = self.poller.deregister(fd, interest);
                    return Err((e, request));
                }
                self.next += 1;
                self.waiting.insert(id, request);
            }
            Ok(Armed::AlwaysReady) => {
                self.next += 1;
                let result = positional(&mut request);
                self.ready.push_back(finish(id, request, result));
            }
        }
        Ok(id)
    }
    pub(crate) fn cancel(&mut self, id: RequestId) -> io::Result<bool> {
        let Some(request) = self.waiting.get(&id) else {
            return Ok(false);
        };
        self.poller
            .deregister(request.file.as_raw_fd(), Interest::from(request.operation))?;
        let request = self.waiting.remove(&id).expect("checked above");
        self.ready.push_back(Completion {
            id,
            request,
            result: Err(cancelled_error()),
        });
        Ok(true)
    }
    pub(crate) fn cancel_all(&mut self) -> io::Result<()> {
        let ids: Vec<_> = self.waiting.keys().copied().collect();
        first_error(ids, |id| self.cancel(id).map(|_| ()))
    }
    pub(crate) fn poll(&mut self, wait: bool) -> Result<Vec<Completion<T>>, PollError<T>> {
        let mut error = None;
        if !self.waiting.is_empty() {
            let block = wait && self.ready.is_empty();
            self.fired.clear();
            let mut fired = std::mem::take(&mut self.fired);
            if let Err(e) = self.poller.wait(block, &mut fired) {
                error = Some(e);
            }
            for &token in &fired {
                if let Err(e) = self.service(RequestId(token))
                    && error.is_none()
                {
                    error = Some(e);
                }
            }
            self.fired = fired;
        }
        let completed = self.ready.drain(..).collect();
        match error {
            Some(error) => Err(PollError { error, completed }),
            None => Ok(completed),
        }
    }
    /// Perform one fired request. The request always reaches `ready` or goes
    /// back to `waiting`; an error here is a driver error, reported after the
    /// request itself has been accounted for.
    fn service(&mut self, id: RequestId) -> io::Result<()> {
        let Some(mut request) = self.waiting.remove(&id) else {
            return Err(io::Error::other("unknown or duplicate readiness event"));
        };
        let fd = request.file.as_raw_fd();
        let interest = Interest::from(request.operation);
        let result = streamed(&mut request);
        if matches!(&result, Err(e) if e.kind() == io::ErrorKind::WouldBlock) {
            return match self.poller.rearm(fd, interest, id.0) {
                Ok(()) => {
                    self.waiting.insert(id, request);
                    Ok(())
                }
                Err(e) => {
                    let detail = io::Error::new(e.kind(), format!("re-arm failed: {e}"));
                    self.ready.push_back(finish(id, request, Err(e)));
                    Err(detail)
                }
            };
        }
        let released = self.poller.release(fd, interest);
        self.ready.push_back(finish(id, request, result));
        released
    }
    /// Forget registrations without completing them (drop path only).
    pub(crate) fn abandon(&mut self) -> usize {
        for request in self.waiting.values() {
            let _ = self
                .poller
                .deregister(request.file.as_raw_fd(), Interest::from(request.operation));
        }
        let n = self.pending();
        self.waiting.clear();
        self.ready.clear();
        n
    }
}

fn finish<T>(id: RequestId, mut request: Request<T>, result: io::Result<usize>) -> Completion<T> {
    let result = match result {
        Ok(n) if n > request.buffer.len() => {
            Err(io::Error::other("kernel returned an invalid I/O length"))
        }
        other => other,
    };
    if request.operation == Operation::Read
        && let Ok(n) = result
    {
        request.buffer.truncate(n);
    }
    Completion {
        id,
        request,
        result,
    }
}

fn retry<R>(mut f: impl FnMut() -> io::Result<R>) -> io::Result<R> {
    loop {
        match f() {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            other => return other,
        }
    }
}

fn positional<T>(request: &mut Request<T>) -> io::Result<usize> {
    let offset = request.offset;
    let file = &request.file;
    match request.operation {
        Operation::Read => retry(|| file.read_at(&mut request.buffer, offset)),
        Operation::Write => retry(|| file.write_at(&request.buffer, offset)),
    }
}

fn status_flags(fd: RawFd) -> io::Result<libc::c_int> {
    // SAFETY: fcntl on a descriptor the request owns; no memory is passed.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(flags)
}

fn wrong_direction(flags: libc::c_int, op: Operation) -> bool {
    let mode = flags & libc::O_ACCMODE;
    match op {
        Operation::Read => mode == libc::O_WRONLY,
        Operation::Write => mode == libc::O_RDONLY,
    }
}

/// A registered stream is switched to `O_NONBLOCK` and left that way: the
/// flag belongs to the open file description, so restoring it after each
/// call could make a concurrent operation on a duplicate (`try_clone`, `dup`)
/// block. Callers that later use the file synchronously see `WouldBlock`.
fn set_nonblocking(fd: RawFd, flags: libc::c_int) -> io::Result<()> {
    if flags & libc::O_NONBLOCK != 0 {
        return Ok(());
    }
    // SAFETY: as in status_flags.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Stream I/O after readiness on a descriptor already in `O_NONBLOCK` mode.
fn streamed<T>(request: &mut Request<T>) -> io::Result<usize> {
    let mut file = &request.file;
    match request.operation {
        Operation::Read => retry(|| file.read(&mut request.buffer)),
        Operation::Write => retry(|| file.write(&request.buffer)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeSet, fs::File};

    /// A scripted poller: every descriptor registers, and `wait` fires
    /// whatever the test queued.
    #[derive(Default)]
    struct Script {
        armed: BTreeSet<u64>,
        fire: Vec<u64>,
        fail_wait: bool,
        always_ready: bool,
        deregistered: usize,
    }
    impl Poller for Script {
        fn arm(&mut self, _: RawFd, _: Interest, token: u64) -> io::Result<Armed> {
            if self.always_ready {
                return Ok(Armed::AlwaysReady);
            }
            self.armed.insert(token);
            Ok(Armed::Registered)
        }
        fn rearm(&mut self, _: RawFd, _: Interest, token: u64) -> io::Result<()> {
            self.armed.insert(token);
            Ok(())
        }
        fn release(&mut self, _: RawFd, _: Interest) -> io::Result<()> {
            Ok(())
        }
        fn deregister(&mut self, _: RawFd, _: Interest) -> io::Result<()> {
            self.deregistered += 1;
            Ok(())
        }
        fn wait(&mut self, _: bool, fired: &mut Vec<u64>) -> io::Result<()> {
            fired.append(&mut self.fire);
            if self.fail_wait {
                return Err(io::Error::other("wait failed"));
            }
            Ok(())
        }
    }
    fn request(file: File, op: Operation, buffer: Vec<u8>, offset: u64) -> Request<u32> {
        Request {
            file,
            buffer,
            continuation: 7,
            operation: op,
            offset,
        }
    }
    fn pipe() -> (File, File) {
        let (r, w) = std::io::pipe().unwrap();
        (
            File::from(std::os::fd::OwnedFd::from(r)),
            File::from(std::os::fd::OwnedFd::from(w)),
        )
    }

    #[test]
    fn unknown_and_duplicate_events_are_errors_not_completions() {
        let mut driver = Readiness::<u32, _>::new(Script::default(), DriverKind::Epoll, 4);
        let (r, mut w) = pipe();
        let id = driver
            .submit(request(r, Operation::Read, vec![0; 8], 0))
            .unwrap_or_else(|_| panic!());
        w.write_all(b"hi").unwrap();
        driver.poller.fire = vec![99, id.0, id.0];
        let e = driver.poll(false).unwrap_err();
        assert_eq!(e.completed.len(), 1);
        assert_eq!(e.completed[0].request.buffer, b"hi");
        assert_eq!(driver.pending(), 0);
    }
    #[test]
    fn spurious_wakeups_rearm_instead_of_completing() {
        let mut driver = Readiness::<u32, _>::new(Script::default(), DriverKind::Epoll, 4);
        let (r, _w) = pipe();
        let id = driver
            .submit(request(r, Operation::Read, vec![0; 8], 0))
            .unwrap_or_else(|_| panic!());
        driver.poller.fire = vec![id.0];
        assert!(driver.poll(false).unwrap().is_empty());
        assert_eq!(driver.in_flight(), 1);
        assert!(driver.cancel(id).unwrap());
        assert!(!driver.cancel(id).unwrap());
        let done = driver.poll(false).unwrap();
        assert!(crate::driver::is_cancelled(
            done[0].result.as_ref().unwrap_err()
        ));
        assert_eq!(
            done[0].request.buffer.len(),
            8,
            "a cancelled read keeps its buffer"
        );
    }
    #[test]
    fn a_failed_wait_still_returns_inline_completions() {
        let mut driver = Readiness::<u32, _>::new(Script::default(), DriverKind::Kqueue, 4);
        let (r, _w) = pipe();
        driver
            .submit(request(r, Operation::Read, vec![0; 8], 0))
            .unwrap_or_else(|_| panic!());
        driver.poller.always_ready = true;
        let file = tempfile();
        driver
            .submit(request(file, Operation::Write, b"abc".to_vec(), 3))
            .unwrap_or_else(|_| panic!());
        driver.poller.fail_wait = true;
        let e = driver.poll(true).unwrap_err();
        assert_eq!(e.completed.len(), 1);
        assert_eq!(*e.completed[0].result.as_ref().unwrap(), 3);
        assert_eq!(driver.pending(), 1);
    }
    #[test]
    fn capacity_and_stream_offsets_are_typed_rejections() {
        let mut driver = Readiness::<u32, _>::new(Script::default(), DriverKind::Epoll, 1);
        let (r, w) = pipe();
        let (e, mut back) = driver
            .submit(request(r, Operation::Read, vec![0; 8], 5))
            .err()
            .unwrap();
        assert_eq!(e.raw_os_error(), Some(libc::ESPIPE));
        assert_eq!(driver.poller.deregistered, 1);
        back.offset = 0;
        driver.submit(back).unwrap_or_else(|_| panic!());
        let (e, _) = driver
            .submit(request(w, Operation::Write, vec![1], 0))
            .err()
            .unwrap();
        assert_eq!(e.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(driver.abandon(), 1);
    }
    #[test]
    fn requests_that_need_no_readiness_complete_at_the_next_poll() {
        let mut driver = Readiness::<u32, _>::new(Script::default(), DriverKind::Epoll, 8);
        let (r, w) = pipe();
        let (r2, w2) = (r.try_clone().unwrap(), w.try_clone().unwrap());
        // Zero-length read on an empty pipe whose writer stays open.
        driver
            .submit(request(r, Operation::Read, vec![], 0))
            .unwrap_or_else(|_| panic!());
        // Wrong direction on each end.
        driver
            .submit(request(r2, Operation::Write, vec![1], 0))
            .unwrap_or_else(|_| panic!());
        driver
            .submit(request(w2, Operation::Read, vec![0; 4], 0))
            .unwrap_or_else(|_| panic!());
        assert!(driver.poller.armed.is_empty(), "nothing was registered");
        let done = driver.poll(true).unwrap();
        assert_eq!(done.len(), 3);
        assert_eq!(*done[0].result.as_ref().unwrap(), 0);
        for c in &done[1..] {
            assert_eq!(
                c.result.as_ref().unwrap_err().raw_os_error(),
                Some(libc::EBADF)
            );
        }
        drop(w);
    }
    #[test]
    fn registered_streams_stay_nonblocking_for_duplicates() {
        let mut driver = Readiness::<u32, _>::new(Script::default(), DriverKind::Epoll, 8);
        let (r, _w) = pipe();
        let dup = r.try_clone().unwrap();
        driver
            .submit(request(r, Operation::Read, vec![0; 4], 0))
            .unwrap_or_else(|_| panic!());
        let flags = status_flags(dup.as_raw_fd()).unwrap();
        assert_ne!(flags & libc::O_NONBLOCK, 0);
        driver.poller.fire = driver.poller.armed.iter().copied().collect();
        // A spurious wake-up on an empty pipe must not block.
        assert!(driver.poll(false).unwrap().is_empty());
        assert_ne!(status_flags(dup.as_raw_fd()).unwrap() & libc::O_NONBLOCK, 0);
        driver.cancel_all().unwrap();
        assert_eq!(driver.poll(false).unwrap().len(), 1);
    }
    fn tempfile() -> File {
        let path = std::env::temp_dir().join(format!(
            "vesper-readiness-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        std::fs::remove_file(path).unwrap();
        file
    }
}
