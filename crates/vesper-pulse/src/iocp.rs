//! Windows I/O completion port driver.
//!
//! Every completion, kernel or synthetic, is delivered through one completion
//! port and identified by its request's heap-pinned `OVERLAPPED`:
//!
//! * Handles opened for overlapped I/O (`FILE_FLAG_OVERLAPPED`) are
//!   associated with the port, with `FILE_SKIP_COMPLETION_PORT_ON_SUCCESS`.
//!   `ReadFile`/`WriteFile` return `ERROR_IO_PENDING` and the kernel posts the
//!   packet, or finish at once, in which case the driver posts the packet
//!   itself. The buffer and `OVERLAPPED` stay owned by the driver until the
//!   packet is dequeued.
//! * Synchronous handles (the default for `std::fs::File`) cannot complete
//!   asynchronously. The driver performs the positional operation in `submit`
//!   and posts its result with `PostQueuedCompletionStatus`, so it is still
//!   delivered by `poll`. A synchronous read of an empty pipe therefore blocks
//!   in `submit`; use overlapped handles (see [`overlapped_pipe`]) for streams.
//!
//! Cancellation calls `CancelIoEx` on the request's `OVERLAPPED`; the request
//! then completes with `ERROR_OPERATION_ABORTED`, or with its real result if
//! it had already finished.
#![allow(unsafe_code)]
use crate::driver::{
    Completion, CompletionDriver, DriverKind, Operation, PollError, Rejected, Request, RequestId,
    check_entries, first_error, queue_full,
};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs::File,
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle},
};
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_MODE_INFORMATION, FILE_SYNCHRONOUS_IO_ALERT, FILE_SYNCHRONOUS_IO_NONALERT,
    FileModeInformation, NtQueryInformationFile,
};
use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_HANDLE_EOF, ERROR_INVALID_PARAMETER, ERROR_IO_PENDING,
    ERROR_NOT_FOUND, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
    PIPE_ACCESS_INBOUND, ReadFile, SetFileCompletionNotificationModes, WriteFile,
};
use windows_sys::Win32::System::IO::{
    CancelIoEx, CreateIoCompletionPort, GetOverlappedResult, GetQueuedCompletionStatusEx,
    IO_STATUS_BLOCK, OVERLAPPED, OVERLAPPED_ENTRY, PostQueuedCompletionStatus,
};
use windows_sys::Win32::System::Pipes::{
    CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::WindowsProgramming::{
    FILE_SKIP_COMPLETION_PORT_ON_SUCCESS, FILE_SKIP_SET_EVENT_ON_HANDLE,
};

const BATCH: usize = 64;
/// Completion key of packets the kernel posts for associated handles.
const KEY_KERNEL: usize = 1;
/// Completion key of packets this driver posts for finished requests.
const KEY_POSTED: usize = 2;
const INFINITE: u32 = u32::MAX;

/// Pinned per-request state; the kernel sees `ov` while the request is
/// pending. `repr(C)` makes a pointer to `ov` a pointer to the slot.
#[repr(C)]
struct Slot {
    ov: OVERLAPPED,
    id: u64,
}

enum State {
    /// The kernel owns the buffer until its packet arrives.
    Kernel,
    /// Finished; the result rides on a posted packet.
    Posted(io::Result<usize>),
}

struct Entry<T> {
    request: Request<T>,
    slot: Box<Slot>,
    state: State,
}

/// The IOCP completion driver.
pub struct IocpDriver<T> {
    port: OwnedHandle,
    capacity: usize,
    next: u64,
    entries: BTreeMap<RequestId, Entry<T>>,
    cancelling: BTreeSet<RequestId>,
    /// Handles this driver associated with its port. A handle can be bound to
    /// one port only, for life, so re-submission of the same handle must not
    /// associate again. (A closed handle's value may be reused by a new file:
    /// that file's association then fails and is reported as a typed error
    /// unless the value is in this set, in which case completions might go to
    /// another port. Submitting a reopened overlapped handle to the same
    /// driver is therefore unsupported.)
    associated: HashSet<usize>,
    events: Vec<OVERLAPPED_ENTRY>,
}

fn last_error() -> io::Error {
    io::Error::last_os_error()
}

fn handle(file: &File) -> HANDLE {
    file.as_raw_handle() as HANDLE
}

/// Whether the handle was opened for synchronous I/O (no FILE_FLAG_OVERLAPPED).
fn synchronous(file: &File) -> io::Result<bool> {
    let mut status: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    let mut info = FILE_MODE_INFORMATION { Mode: 0 };
    // SAFETY: both out-pointers refer to live locals of the declared sizes.
    let rc = unsafe {
        NtQueryInformationFile(
            handle(file),
            &mut status,
            (&mut info as *mut FILE_MODE_INFORMATION).cast(),
            std::mem::size_of::<FILE_MODE_INFORMATION>() as u32,
            FileModeInformation,
        )
    };
    if rc < 0 {
        return Err(io::Error::other(format!(
            "NtQueryInformationFile(FileModeInformation) failed: NTSTATUS {:#x}",
            rc as u32
        )));
    }
    Ok(info.Mode & (FILE_SYNCHRONOUS_IO_ALERT | FILE_SYNCHRONOUS_IO_NONALERT) != 0)
}

/// Map a finished operation's Win32 error: end of file and a closed pipe read
/// as zero bytes, like `read(2)`.
fn result(op: Operation, ok: bool, bytes: u32, code: u32) -> io::Result<usize> {
    if ok {
        return Ok(bytes as usize);
    }
    if op == Operation::Read && (code == ERROR_HANDLE_EOF || code == ERROR_BROKEN_PIPE) {
        return Ok(0);
    }
    Err(io::Error::from_raw_os_error(code as i32))
}

impl<T> IocpDriver<T> {
    /// `entries` bounds the requests the driver owns at once.
    pub fn new(entries: u32) -> io::Result<Self> {
        let capacity = check_entries(entries)?;
        // SAFETY: creating a new port takes no pointers.
        let port =
            unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, std::ptr::null_mut(), 0, 1) };
        if port.is_null() {
            return Err(last_error());
        }
        Ok(Self {
            // SAFETY: the port handle is fresh and owned here.
            port: unsafe { OwnedHandle::from_raw_handle(port as RawHandle) },
            capacity,
            next: 1,
            entries: BTreeMap::new(),
            cancelling: BTreeSet::new(),
            associated: HashSet::new(),
            // SAFETY: OVERLAPPED_ENTRY is plain data; zero is a valid value.
            events: vec![unsafe { std::mem::zeroed() }; BATCH],
        })
    }
    fn port(&self) -> HANDLE {
        self.port.as_raw_handle() as HANDLE
    }
    /// Requests the kernel still owns (pending overlapped operations).
    pub fn in_flight(&self) -> usize {
        self.entries
            .values()
            .filter(|e| matches!(e.state, State::Kernel))
            .count()
    }
    fn associate(&mut self, file: &File) -> io::Result<()> {
        let h = handle(file);
        // SAFETY: associating a live handle with our live port.
        let rc = unsafe { CreateIoCompletionPort(h, self.port(), KEY_KERNEL, 0) };
        if rc.is_null() {
            let e = last_error();
            if e.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32)
                && self.associated.contains(&(h as usize))
            {
                return Ok(());
            }
            return Err(io::Error::new(
                e.kind(),
                format!("cannot associate handle with the completion port: {e}"),
            ));
        }
        self.associated.insert(h as usize);
        let flags = (FILE_SKIP_COMPLETION_PORT_ON_SUCCESS | FILE_SKIP_SET_EVENT_ON_HANDLE) as u8;
        // SAFETY: flags call on a live handle.
        if unsafe { SetFileCompletionNotificationModes(h, flags) } == 0 {
            return Err(last_error());
        }
        Ok(())
    }
    fn post(&self, slot: &Slot) -> io::Result<()> {
        // SAFETY: the slot stays owned by `entries` until this packet is
        // dequeued; the port only stores the pointer.
        if unsafe { PostQueuedCompletionStatus(self.port(), 0, KEY_POSTED, &slot.ov) } == 0 {
            return Err(last_error());
        }
        Ok(())
    }
    /// Issue the operation. Returns `None` when the kernel will post a packet.
    fn start(request: &mut Request<T>, slot: &mut Slot, sync: bool) -> Option<io::Result<usize>> {
        slot.ov.Anonymous.Anonymous.Offset = request.offset as u32;
        slot.ov.Anonymous.Anonymous.OffsetHigh = (request.offset >> 32) as u32;
        let h = handle(&request.file);
        let len = request.buffer.len() as u32;
        let mut bytes = 0u32;
        // A synchronous handle reports its byte count directly; an overlapped
        // one through GetOverlappedResult (the direct count may be wrong).
        let count: *mut u32 = if sync {
            &mut bytes
        } else {
            std::ptr::null_mut()
        };
        // SAFETY: the buffer and OVERLAPPED are owned by the driver's table
        // until the operation's packet is dequeued (or it failed synchronously).
        let ok = unsafe {
            match request.operation {
                Operation::Read => {
                    ReadFile(h, request.buffer.as_mut_ptr(), len, count, &mut slot.ov)
                }
                Operation::Write => WriteFile(h, request.buffer.as_ptr(), len, count, &mut slot.ov),
            }
        } != 0;
        let code = if ok { 0 } else { unsafe { GetLastError() } };
        if !ok && code == ERROR_IO_PENDING {
            return None;
        }
        if ok && !sync {
            // SAFETY: the operation has completed; this only reads the result.
            unsafe { GetOverlappedResult(h, &slot.ov, &mut bytes, 0) };
        }
        Some(result(request.operation, ok, bytes, code))
    }
    fn complete(&mut self, id: RequestId) -> io::Result<Completion<T>> {
        let entry = self
            .entries
            .remove(&id)
            .ok_or_else(|| io::Error::other("unknown or duplicate completion packet"))?;
        self.cancelling.remove(&id);
        let Entry {
            mut request,
            slot,
            state,
        } = entry;
        let result = match state {
            State::Posted(result) => result,
            State::Kernel => {
                let mut bytes = 0u32;
                // SAFETY: the packet was dequeued, so the operation is done.
                let ok =
                    unsafe { GetOverlappedResult(handle(&request.file), &slot.ov, &mut bytes, 0) }
                        != 0;
                let code = if ok { 0 } else { unsafe { GetLastError() } };
                result(request.operation, ok, bytes, code)
            }
        };
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
        Ok(Completion {
            id,
            request,
            result,
        })
    }
}

impl<T> CompletionDriver<T> for IocpDriver<T> {
    fn kind(&self) -> DriverKind {
        DriverKind::Iocp
    }
    fn pending(&self) -> usize {
        self.entries.len()
    }
    fn submit(&mut self, mut request: Request<T>) -> Result<RequestId, Rejected<T>> {
        if self.entries.len() >= self.capacity {
            return Err((queue_full(), request));
        }
        if request.buffer.len() > u32::MAX as usize {
            return Err((
                io::Error::new(io::ErrorKind::InvalidInput, "I/O buffer exceeds u32 length"),
                request,
            ));
        }
        if self.next == u64::MAX {
            return Err((io::Error::other("I/O request IDs exhausted"), request));
        }
        let sync = match synchronous(&request.file) {
            Ok(sync) => sync,
            Err(e) => return Err((e, request)),
        };
        if !sync && let Err(e) = self.associate(&request.file) {
            return Err((e, request));
        }
        let id = RequestId(self.next);
        let mut slot = Box::new(Slot {
            // SAFETY: OVERLAPPED is plain data; zero is its initial state.
            ov: unsafe { std::mem::zeroed() },
            id: id.0,
        });
        let state = match Self::start(&mut request, &mut slot, sync) {
            None => State::Kernel,
            Some(result) => State::Posted(result),
        };
        self.next += 1;
        let posted = matches!(state, State::Posted(_));
        self.entries.insert(
            id,
            Entry {
                request,
                slot,
                state,
            },
        );
        if posted {
            let entry = &self.entries[&id];
            if let Err(e) = self.post(&entry.slot) {
                // No packet is queued and the operation already finished:
                // hand the request back untouched by the table.
                let entry = self.entries.remove(&id).expect("just inserted");
                return Err((e, entry.request));
            }
        }
        Ok(id)
    }
    fn cancel(&mut self, id: RequestId) -> io::Result<bool> {
        let Some(entry) = self.entries.get(&id) else {
            return Ok(false);
        };
        if !matches!(entry.state, State::Kernel) || self.cancelling.contains(&id) {
            return Ok(false);
        }
        // SAFETY: cancels only the operation identified by this OVERLAPPED.
        if unsafe { CancelIoEx(handle(&entry.request.file), &entry.slot.ov) } == 0 {
            let e = last_error();
            if e.raw_os_error() == Some(ERROR_NOT_FOUND as i32) {
                // Already finished; its packet is on the way.
                return Ok(false);
            }
            return Err(e);
        }
        self.cancelling.insert(id);
        Ok(true)
    }
    fn cancel_all(&mut self) -> io::Result<()> {
        let ids: Vec<_> = self.entries.keys().copied().collect();
        first_error(ids, |id| self.cancel(id).map(|_| ()))
    }
    fn poll(&mut self, wait: bool) -> Result<Vec<Completion<T>>, PollError<T>> {
        if self.entries.is_empty() {
            return Ok(vec![]);
        }
        let timeout = if wait { INFINITE } else { 0 };
        let mut n = 0u32;
        // SAFETY: events has BATCH entries for the kernel to fill.
        let ok = unsafe {
            GetQueuedCompletionStatusEx(
                self.port(),
                self.events.as_mut_ptr(),
                BATCH as u32,
                &mut n,
                timeout,
                0,
            )
        } != 0;
        if !ok {
            let e = last_error();
            if e.raw_os_error() == Some(WAIT_TIMEOUT as i32) {
                return Ok(vec![]);
            }
            return Err(PollError {
                error: e,
                completed: vec![],
            });
        }
        let ids: Vec<_> = self.events[..n as usize]
            .iter()
            .map(|e| {
                if e.lpOverlapped.is_null() {
                    None
                } else {
                    // SAFETY: every OVERLAPPED we hand out is the first field
                    // of a live Slot owned by `entries`.
                    Some(RequestId(unsafe { (*(e.lpOverlapped as *const Slot)).id }))
                }
            })
            .collect();
        let mut completed = vec![];
        let mut error = None;
        for id in ids {
            let done = match id {
                Some(id) => self.complete(id),
                None => Err(io::Error::other("completion packet without an OVERLAPPED")),
            };
            match done {
                Ok(c) => completed.push(c),
                Err(e) => {
                    error.get_or_insert(e);
                }
            }
        }
        match error {
            Some(error) => Err(PollError { error, completed }),
            None => Ok(completed),
        }
    }
}

impl<T> Drop for IocpDriver<T> {
    fn drop(&mut self) {
        // The kernel may still write into pending buffers: cancel and wait for
        // every packet before releasing them.
        let _ = self.cancel_all();
        let mut abandoned = 0;
        while !self.entries.is_empty() {
            match self.poll(true) {
                Ok(done) => abandoned += done.len(),
                Err(e) => {
                    abandoned += e.completed.len() + self.entries.len();
                    // Retain allocations the kernel may still reference.
                    std::mem::forget(std::mem::take(&mut self.entries));
                    break;
                }
            }
        }
        crate::driver::note_abandoned(abandoned);
    }
}

/// A connected, overlapped, unidirectional named pipe `(reader, writer)` with
/// a unique name, both ends opened with `FILE_FLAG_OVERLAPPED`.
pub fn overlapped_pipe() -> io::Result<(File, File)> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let name = format!(
        r"\\.\pipe\vesper-pulse-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    // SAFETY: `wide` is a NUL-terminated name alive for both calls.
    let server = unsafe {
        CreateNamedPipeW(
            wide.as_ptr(),
            PIPE_ACCESS_INBOUND | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            4096,
            4096,
            0,
            std::ptr::null(),
        )
    };
    if server == INVALID_HANDLE_VALUE {
        return Err(last_error());
    }
    // SAFETY: fresh handle owned here.
    let reader = unsafe { File::from_raw_handle(server as RawHandle) };
    let client = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_WRITE,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            std::ptr::null_mut(),
        )
    };
    if client == INVALID_HANDLE_VALUE {
        return Err(last_error());
    }
    // SAFETY: fresh handle owned here. The client connected by opening, so the
    // server end needs no ConnectNamedPipe.
    let writer = unsafe { File::from_raw_handle(client as RawHandle) };
    Ok((reader, writer))
}
