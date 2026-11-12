//! Owned io_uring requests for the runtime's completion table. No request buffer
//! or file descriptor is released while the kernel can still access it. Request
//! IDs are never reused; cancellation CQEs cannot masquerade as I/O completions.
//!
//! `T` is the scheduler's owned one-shot continuation. `shutdown` returns every
//! outstanding continuation for resumption with its completion/cancellation
//! result. This module is not yet wired into native effect suspension.
use io_uring::{IoUring, opcode, squeue, types};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io,
    os::fd::AsRawFd,
};

const CANCEL_BIT: u64 = 1 << 63;
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RequestId(u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Read,
    Write,
}

/// Moving a request moves its Vec header, never the allocation exposed to the
/// kernel. The table provides exclusive ownership until its matching CQE.
pub struct Request<T> {
    pub file: File,
    pub buffer: Vec<u8>,
    pub continuation: T,
    pub operation: Operation,
    pub offset: u64,
}
pub struct Completion<T> {
    pub id: RequestId,
    pub request: Request<T>,
    pub result: io::Result<usize>,
}
/// A failed poll still returns all completions harvested in that batch.
pub struct PollError<T> {
    pub error: io::Error,
    pub completed: Vec<Completion<T>>,
}
impl<T> std::fmt::Debug for PollError<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PollError")
            .field("error", &self.error)
            .field("completed", &self.completed.len())
            .finish()
    }
}
/// Shutdown failures return the still-owned driver and every harvested token.
pub struct ShutdownError<T> {
    pub error: io::Error,
    pub completed: Vec<Completion<T>>,
    pub driver: Box<IoDriver<T>>,
}
impl<T> std::fmt::Debug for ShutdownError<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShutdownError")
            .field("error", &self.error)
            .field("completed", &self.completed.len())
            .field("pending", &self.driver.pending())
            .finish()
    }
}
struct Ledger<T> {
    next: u64,
    requests: BTreeMap<RequestId, Request<T>>,
    cancellations: BTreeSet<RequestId>,
}
impl<T> Ledger<T> {
    fn new() -> Self {
        Self {
            next: 1,
            requests: BTreeMap::new(),
            cancellations: BTreeSet::new(),
        }
    }
    fn insert(&mut self, request: Request<T>) -> Result<RequestId, (io::Error, Request<T>)> {
        if self.next >= CANCEL_BIT {
            return Err((io::Error::other("I/O request IDs exhausted"), request));
        }
        let id = RequestId(self.next);
        self.next += 1;
        self.requests.insert(id, request);
        Ok(id)
    }
    fn harvest(
        &mut self,
        events: impl IntoIterator<Item = (u64, i32)>,
    ) -> Result<Vec<Completion<T>>, PollError<T>> {
        let mut completed = vec![];
        let mut error = None;
        for (id, result) in events {
            match self.complete(id, result) {
                Ok(Some(completion)) => completed.push(completion),
                Ok(None) => {}
                Err(e) => {
                    if error.is_none() {
                        error = Some(e);
                    }
                }
            }
        }
        if let Some(error) = error {
            Err(PollError { error, completed })
        } else {
            Ok(completed)
        }
    }
    fn complete(&mut self, user_data: u64, result: i32) -> io::Result<Option<Completion<T>>> {
        if user_data & CANCEL_BIT != 0 {
            let id = RequestId(user_data & !CANCEL_BIT);
            if !self.cancellations.remove(&id) {
                return Err(io::Error::other("unexpected cancellation completion"));
            }
            return Ok(None);
        }
        let id = RequestId(user_data);
        let mut request = self
            .requests
            .remove(&id)
            .ok_or_else(|| io::Error::other("unknown or duplicate I/O completion"))?;
        let result = if result < 0 {
            Err(io::Error::from_raw_os_error(-result))
        } else if result as usize > request.buffer.len() {
            Err(io::Error::other("kernel returned an invalid I/O length"))
        } else {
            Ok(result as usize)
        };
        if request.operation == Operation::Read
            && let Ok(n) = result
        {
            request.buffer.truncate(n);
        }
        Ok(Some(Completion {
            id,
            request,
            result,
        }))
    }
}
pub struct IoDriver<T> {
    ring: IoUring,
    ledger: Ledger<T>,
}
impl<T> IoDriver<T> {
    pub fn new(entries: u32) -> io::Result<Self> {
        if entries == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "I/O queue must have entries",
            ));
        }
        let ring = IoUring::new(entries)?;
        if !ring.params().is_feature_nodrop() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "exact-once I/O requires IORING_FEAT_NODROP",
            ));
        }
        Ok(Self {
            ring,
            ledger: Ledger::new(),
        })
    }
    pub fn pending(&self) -> usize {
        self.ledger.requests.len()
    }
    pub fn submit(&mut self, request: Request<T>) -> Result<RequestId, (io::Error, Request<T>)> {
        if request.buffer.len() > u32::MAX as usize {
            return Err((
                io::Error::new(io::ErrorKind::InvalidInput, "I/O buffer exceeds u32 length"),
                request,
            ));
        }
        // Check/flush the queue before transferring ownership. On failure the
        // caller receives its file, allocation and continuation unchanged.
        if self.ring.submission().is_full() {
            if let Err(e) = self.ring.submit() {
                return Err((e, request));
            }
            if self.ring.submission().is_full() {
                return Err((
                    io::Error::new(io::ErrorKind::WouldBlock, "I/O submission queue is full"),
                    request,
                ));
            }
        }
        let id = self.ledger.insert(request)?;
        let owned = self.ledger.requests.get_mut(&id).unwrap();
        let len = owned.buffer.len() as u32;
        let entry = match owned.operation {
            Operation::Read => opcode::Read::new(
                types::Fd(owned.file.as_raw_fd()),
                owned.buffer.as_mut_ptr(),
                len,
            )
            .offset(owned.offset)
            .build(),
            Operation::Write => opcode::Write::new(
                types::Fd(owned.file.as_raw_fd()),
                owned.buffer.as_ptr(),
                len,
            )
            .offset(owned.offset)
            .build(),
        }
        .user_data(id.0);
        // SAFETY: owned is installed in the table before publication. Its file
        // and Vec allocation remain alive and inaccessible to users until CQE.
        if unsafe { self.ring.submission().push(&entry) }.is_err() {
            let request = self.ledger.requests.remove(&id).unwrap();
            return Err((
                io::Error::new(io::ErrorKind::WouldBlock, "I/O submission queue is full"),
                request,
            ));
        }
        Ok(id)
    }
    fn push(&mut self, entry: &squeue::Entry) -> io::Result<()> {
        if self.ring.submission().is_full() {
            self.ring.submit()?;
        }
        // SAFETY: cancellation entries contain only IDs, no borrowed pointers.
        unsafe { self.ring.submission().push(entry) }
            .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "I/O submission queue is full"))
    }
    pub fn cancel(&mut self, id: RequestId) -> io::Result<bool> {
        if !self.ledger.requests.contains_key(&id) || self.ledger.cancellations.contains(&id) {
            return Ok(false);
        }
        let entry = opcode::AsyncCancel::new(id.0)
            .build()
            .user_data(id.0 | CANCEL_BIT);
        self.push(&entry)?;
        self.ledger.cancellations.insert(id);
        Ok(true)
    }
    pub fn cancel_all(&mut self) -> io::Result<()> {
        for id in self.ledger.requests.keys().copied().collect::<Vec<_>>() {
            self.cancel(id)?;
        }
        Ok(())
    }
    pub fn poll(&mut self, wait: bool) -> Result<Vec<Completion<T>>, PollError<T>> {
        let wait = usize::from(
            wait && (!self.ledger.requests.is_empty() || !self.ledger.cancellations.is_empty()),
        );
        loop {
            match self.ring.submit_and_wait(wait) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                result => {
                    result.map_err(|error| PollError {
                        error,
                        completed: vec![],
                    })?;
                    break;
                }
            }
        }
        let events = self
            .ring
            .completion()
            .map(|e| (e.user_data(), e.result()))
            .collect::<Vec<_>>();
        self.ledger.harvest(events)
    }
    /// Consume the driver after cancelling and harvesting every request. The
    /// scheduler must resume each returned continuation, including ECANCELED.
    pub fn shutdown(mut self) -> Result<Vec<Completion<T>>, ShutdownError<T>> {
        let ids = self.ledger.requests.keys().copied().collect::<Vec<_>>();
        let mut completed = vec![];
        for id in ids {
            if let Err(error) = self.cancel(id) {
                return Err(ShutdownError {
                    error,
                    completed,
                    driver: Box::new(self),
                });
            }
        }
        while !self.ledger.requests.is_empty() || !self.ledger.cancellations.is_empty() {
            match self.poll(true) {
                Ok(mut batch) => completed.append(&mut batch),
                Err(mut e) => {
                    completed.append(&mut e.completed);
                    return Err(ShutdownError {
                        error: e.error,
                        completed,
                        driver: Box::new(self),
                    });
                }
            }
        }
        Ok(completed)
    }
}
impl<T> Drop for IoDriver<T> {
    fn drop(&mut self) {
        // Native typed owners will call shutdown explicitly. This defensive
        // path also makes ordinary Rust embedding memory safe on early errors.
        let ids = self.ledger.requests.keys().copied().collect::<Vec<_>>();
        for id in ids {
            let _ = self.cancel(id);
        }
        while !self.ledger.requests.is_empty() {
            if self.poll(true).is_err() {
                // The kernel may still hold these addresses. Retain allocations
                // on an irrecoverable driver error rather than freeing them.
                std::mem::forget(std::mem::take(&mut self.ledger.requests));
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(token: u64) -> Request<u64> {
        Request {
            file: File::open("/dev/null").unwrap(),
            buffer: vec![0; 16],
            continuation: token,
            operation: Operation::Read,
            offset: 0,
        }
    }
    #[test]
    fn completion_owns_resources_exactly_once() {
        let mut ledger = Ledger::new();
        let id = ledger
            .insert(request(42))
            .unwrap_or_else(|_| panic!("insert failed"));
        let completed = ledger.complete(id.0, 3).unwrap().unwrap();
        assert_eq!(completed.request.continuation, 42);
        assert_eq!(completed.request.buffer.len(), 3);
        assert_eq!(completed.result.unwrap(), 3);
        assert!(ledger.complete(id.0, 3).is_err());
    }
    #[test]
    fn cancellation_completion_cannot_release_a_buffer() {
        let mut ledger = Ledger::new();
        let id = ledger
            .insert(request(42))
            .unwrap_or_else(|_| panic!("insert failed"));
        ledger.cancellations.insert(id);
        assert!(ledger.complete(id.0 | CANCEL_BIT, 0).unwrap().is_none());
        assert!(ledger.requests.contains_key(&id));
        let completed = ledger.complete(id.0, -125).unwrap().unwrap();
        assert_eq!(completed.request.continuation, 42);
        assert_eq!(completed.result.unwrap_err().raw_os_error(), Some(125));
    }
    #[test]
    fn ids_are_not_reused_after_completion() {
        let mut ledger = Ledger::new();
        let a = ledger.insert(request(1)).unwrap_or_else(|_| panic!());
        ledger.complete(a.0, 0).unwrap();
        let b = ledger.insert(request(2)).unwrap_or_else(|_| panic!());
        assert_ne!(a, b);
        assert!(ledger.complete(a.0, 0).is_err());
        assert!(ledger.requests.contains_key(&b));
    }
    #[test]
    fn failed_batch_keeps_all_harvested_continuations() {
        let mut ledger = Ledger::new();
        let id = ledger.insert(request(42)).unwrap_or_else(|_| panic!());
        let failed = match ledger.harvest([(u64::MAX, 0), (id.0, 3)]) {
            Err(e) => e,
            Ok(_) => panic!("expected batch error"),
        };
        assert_eq!(failed.completed.len(), 1);
        assert_eq!(failed.completed[0].request.continuation, 42);
        assert!(ledger.requests.is_empty());
    }
    #[test]
    fn kernel_roundtrip_when_available() {
        let mut driver = match IoDriver::new(8) {
            Ok(driver) => driver,
            Err(e) if matches!(e.raw_os_error(), Some(1 | 38 | 95)) => {
                eprintln!("kernel io_uring validation unavailable: {e}");
                return;
            }
            Err(e) => panic!("io_uring setup failed: {e}"),
        };
        let path = std::env::temp_dir().join(format!("vesper-uring-{}", std::process::id()));
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        driver
            .submit(Request {
                file,
                buffer: b"vesper".to_vec(),
                continuation: 42,
                operation: Operation::Write,
                offset: 0,
            })
            .unwrap_or_else(|_| panic!("write submission failed"));
        let mut writes = driver.poll(true).unwrap();
        let written = writes.pop().unwrap();
        assert_eq!(written.result.unwrap(), 6);
        driver
            .submit(Request {
                file: written.request.file,
                buffer: vec![0; 16],
                continuation: written.request.continuation,
                operation: Operation::Read,
                offset: 0,
            })
            .unwrap_or_else(|_| panic!("read submission failed"));
        let mut reads = driver.poll(true).unwrap();
        let read = reads.pop().unwrap();
        assert_eq!(read.request.buffer, b"vesper");
        assert_eq!(read.request.continuation, 42);
        assert_eq!(driver.pending(), 0);
        assert!(driver.shutdown().unwrap().is_empty());
        std::fs::remove_file(path).unwrap();
    }
}
