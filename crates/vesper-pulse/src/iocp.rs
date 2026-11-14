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
//! * Winsock sockets use AFD readiness (see the private `afd` module): the
//!   driver polls the socket through its own AFD device handle and performs
//!   a non-blocking `recv`/`send` when the poll fires. Sockets are never
//!   bound to the port.
//! * Synchronous handles (the default for `std::fs::File`) cannot complete
//!   asynchronously. Regular disk files are transferred positionally in
//!   `submit`, which cannot block indefinitely. Every other synchronous
//!   handle (anonymous pipes, character devices) goes to an offload thread,
//!   one per in-flight operation, reused once idle; so an empty synchronous
//!   pipe no longer blocks the scheduler. Both post their result with
//!   `PostQueuedCompletionStatus`, so it is still delivered by `poll`.
//!
//! Cancellation calls `CancelIoEx` on the request's `OVERLAPPED` (or on its
//! AFD poll), or `CancelSynchronousIo` on the offload thread running it; the
//! request then completes with `ERROR_OPERATION_ABORTED`, or with its real
//! result if it had already finished.
//!
//! Packets are matched to requests through a table of the driver's own
//! `OVERLAPPED` addresses, never by dereferencing an unknown pointer, so a
//! stray packet is a typed error. A handle can be bound to one port for
//! life: a handle value this driver bound before (possibly closed and reused
//! by a new file that another port now owns) is rebound to this port with
//! `FileReplaceCompletionInformation`, so its packets can only arrive here;
//! a handle another port owns that this driver never bound is rejected.
#![allow(unsafe_code)]
use crate::afd;
use crate::driver::{
    Completion, CompletionDriver, DriverKind, Operation, PollError, Rejected, Request, RequestId,
    cancelled_error, check_entries, first_error, queue_full,
};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs::File,
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle},
    sync::{Arc, Mutex, mpsc},
    thread::JoinHandle,
};
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_COMPLETION_INFORMATION, FILE_MODE_INFORMATION, FILE_SYNCHRONOUS_IO_ALERT,
    FILE_SYNCHRONOUS_IO_NONALERT, FileModeInformation, FileReplaceCompletionInformation,
    NtQueryInformationFile, NtSetInformationFile,
};
use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_HANDLE_EOF, ERROR_INVALID_HANDLE, ERROR_INVALID_PARAMETER,
    ERROR_IO_PENDING, ERROR_NOT_FOUND, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
    STATUS_INVALID_DEVICE_REQUEST, STATUS_INVALID_INFO_CLASS, STATUS_NOT_IMPLEMENTED, WAIT_TIMEOUT,
};
use windows_sys::Win32::Networking::WinSock::SOCKET;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_FLAGS_AND_ATTRIBUTES,
    FILE_TYPE_DISK, GetFileType, OPEN_EXISTING, PIPE_ACCESS_INBOUND, ReadFile,
    SetFileCompletionNotificationModes, WriteFile,
};
use windows_sys::Win32::System::IO::{
    CancelIoEx, CancelSynchronousIo, CreateIoCompletionPort, GetOverlappedResult,
    GetQueuedCompletionStatusEx, IO_STATUS_BLOCK, OVERLAPPED, OVERLAPPED_ENTRY,
    PostQueuedCompletionStatus,
};
use windows_sys::Win32::System::Pipes::{
    CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::WindowsProgramming::{
    FILE_SKIP_COMPLETION_PORT_ON_SUCCESS, FILE_SKIP_SET_EVENT_ON_HANDLE,
};

const BATCH: usize = 64;
/// Completion key of packets the kernel posts for associated handles
/// (overlapped files and the AFD poll handle).
const KEY_KERNEL: usize = 1;
/// Completion key of packets this driver (or an offload thread) posts.
const KEY_POSTED: usize = 2;
const INFINITE: u32 = u32::MAX;
/// While a synchronous offloaded operation is being cancelled, `poll(true)`
/// wakes this often to re-issue `CancelSynchronousIo`, which cannot reach an
/// operation that has not entered the kernel yet.
const CANCEL_RETRY_MS: u32 = 5;
const ERROR_OPERATION_ABORTED: i32 = 995;

/// Pinned per-request state; the kernel sees `ov` (and `poll`) while the
/// request is pending. `repr(C)` keeps `ov` first.
#[repr(C)]
struct Slot {
    ov: OVERLAPPED,
    id: u64,
    poll: afd::PollInfo,
}

enum State {
    /// The kernel owns the buffer until its packet arrives.
    Kernel,
    /// Finished; the result rides on a posted packet.
    Posted(io::Result<usize>),
    /// An AFD readiness poll for this socket is pending.
    Socket(SOCKET, SOCKET),
    /// A synchronous operation runs on offload thread `.0`; it owns the
    /// buffer until it posts.
    Offloaded(usize),
}

struct Entry<T> {
    request: Request<T>,
    slot: Box<Slot>,
    state: State,
    /// Counted in the handle's [`Binding::in_flight`].
    bound: bool,
}

/// Which Pulse IOCP port a handle value was last bound to, and how many
/// requests are in flight on it, process-wide. A handle can be bound to one
/// port at a time; this table lets a pool worker take over a handle that a
/// sibling worker's driver bound (work stealing moves fibers, and their
/// files, between drivers) once that driver has nothing in flight on it,
/// and lets a driver detect values it bound that may have been reused.
struct Binding {
    port: usize,
    in_flight: usize,
}

fn bindings() -> std::sync::MutexGuard<'static, HashMap<usize, Binding>> {
    static TABLE: std::sync::OnceLock<Mutex<HashMap<usize, Binding>>> = std::sync::OnceLock::new();
    TABLE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn release(h: HANDLE) {
    if let Some(b) = bindings().get_mut(&(h as usize)) {
        b.in_flight = b.in_flight.saturating_sub(1);
    }
}

/// The port handle, shared with offload threads so it outlives them.
struct Port(OwnedHandle);
impl Port {
    fn raw(&self) -> HANDLE {
        self.0.as_raw_handle() as HANDLE
    }
}

struct Job {
    id: u64,
    handle: usize,
    buffer: Vec<u8>,
    op: Operation,
    offset: u64,
    ov: usize,
}

struct Done {
    id: u64,
    buffer: Vec<u8>,
    result: io::Result<usize>,
}

#[derive(Default)]
struct ThreadState {
    /// The job whose blocking call is (about to be) in the kernel.
    current: Option<u64>,
    /// A job cancelled before it started.
    cancelled: Option<u64>,
}

struct Offloader {
    jobs: mpsc::Sender<Job>,
    thread: JoinHandle<()>,
    state: Arc<Mutex<ThreadState>>,
    busy: Option<RequestId>,
}

fn lock(m: &Mutex<ThreadState>) -> std::sync::MutexGuard<'_, ThreadState> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The IOCP completion driver.
pub struct IocpDriver<T> {
    port: Arc<Port>,
    capacity: usize,
    next: u64,
    entries: BTreeMap<RequestId, Entry<T>>,
    /// The driver's live `OVERLAPPED` addresses.
    slots: HashMap<usize, RequestId>,
    cancelling: BTreeSet<RequestId>,
    /// The AFD device handle for socket polls, opened on first use.
    afd: Option<OwnedHandle>,
    afd_polls: u64,
    offloaders: Vec<Offloader>,
    done_tx: mpsc::Sender<Done>,
    done_rx: mpsc::Receiver<Done>,
    finished: HashMap<u64, Done>,
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

/// Regular files and directories: synchronous transfers on them finish
/// without waiting for a peer.
fn is_disk(file: &File) -> bool {
    // SAFETY: GetFileType only inspects a live handle.
    unsafe { GetFileType(handle(file)) == FILE_TYPE_DISK }
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

/// A positional transfer on a synchronous handle; blocks until it finishes.
fn sync_transfer(h: HANDLE, op: Operation, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
    // SAFETY: OVERLAPPED is plain data; it only carries the offset here.
    let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
    ov.Anonymous.Anonymous.Offset = offset as u32;
    ov.Anonymous.Anonymous.OffsetHigh = (offset >> 32) as u32;
    let len = buffer.len().min(u32::MAX as usize) as u32;
    let mut bytes = 0u32;
    // SAFETY: the buffer and locals outlive this blocking call.
    let ok = unsafe {
        match op {
            Operation::Read => ReadFile(h, buffer.as_mut_ptr(), len, &mut bytes, &mut ov),
            Operation::Write => WriteFile(h, buffer.as_ptr(), len, &mut bytes, &mut ov),
        }
    } != 0;
    let code = if ok { 0 } else { unsafe { GetLastError() } };
    result(op, ok, bytes, code)
}

fn offload_thread(
    port: Arc<Port>,
    jobs: mpsc::Receiver<Job>,
    state: Arc<Mutex<ThreadState>>,
    done: mpsc::Sender<Done>,
) {
    while let Ok(mut job) = jobs.recv() {
        let skip = {
            let mut s = lock(&state);
            if s.cancelled == Some(job.id) {
                true
            } else {
                s.current = Some(job.id);
                false
            }
        };
        let result = if skip {
            Err(io::Error::from_raw_os_error(ERROR_OPERATION_ABORTED))
        } else {
            sync_transfer(job.handle as HANDLE, job.op, &mut job.buffer, job.offset)
        };
        {
            let mut s = lock(&state);
            s.current = None;
            s.cancelled = None;
        }
        // The result is queued before its packet, so `poll` always finds it.
        if done
            .send(Done {
                id: job.id,
                buffer: job.buffer,
                result,
            })
            .is_err()
        {
            return;
        }
        // SAFETY: `ov` is the request's slot, owned by the driver's table
        // until this packet is dequeued; the port outlives this thread.
        unsafe {
            PostQueuedCompletionStatus(port.raw(), 0, KEY_POSTED, job.ov as *const OVERLAPPED)
        };
    }
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
        let (done_tx, done_rx) = mpsc::channel();
        Ok(Self {
            // SAFETY: the port handle is fresh and owned here.
            port: Arc::new(Port(unsafe {
                OwnedHandle::from_raw_handle(port as RawHandle)
            })),
            capacity,
            next: 1,
            entries: BTreeMap::new(),
            slots: HashMap::new(),
            cancelling: BTreeSet::new(),
            afd: None,
            afd_polls: 0,
            offloaders: vec![],
            done_tx,
            done_rx,
            finished: HashMap::new(),
            // SAFETY: OVERLAPPED_ENTRY is plain data; zero is a valid value.
            events: vec![unsafe { std::mem::zeroed() }; BATCH],
        })
    }
    fn port(&self) -> HANDLE {
        self.port.raw()
    }
    /// Requests still running: pending overlapped operations, socket polls
    /// and offloaded synchronous operations.
    pub fn in_flight(&self) -> usize {
        self.entries
            .values()
            .filter(|e| !matches!(e.state, State::Posted(_)))
            .count()
    }
    /// AFD readiness polls this driver has armed for sockets.
    pub fn socket_polls(&self) -> u64 {
        self.afd_polls
    }
    /// Offload threads started so far (each runs one synchronous operation
    /// at a time and is reused once idle).
    pub fn offload_threads(&self) -> usize {
        self.offloaders.len()
    }
    /// Bind `file` to this port (or confirm it is bound here) and count one
    /// more request of this driver on it. See [`Binding`].
    fn associate(&mut self, file: &File) -> io::Result<()> {
        let h = handle(file);
        let me = self.port() as usize;
        let mut table = bindings();
        let known = table.get(&(h as usize)).map(|b| (b.port, b.in_flight));
        match known {
            // A request this driver owns holds this very handle open, so it
            // is the file bound here; nothing can have reused the value.
            Some((port, n)) if port == me && n > 0 => {}
            Some((_, n)) if n > 0 => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "handle has I/O in flight on another Pulse completion port",
                ));
            }
            _ => {
                // SAFETY: associating a live handle with our live port.
                let rc = unsafe { CreateIoCompletionPort(h, self.port(), KEY_KERNEL, 0) };
                if rc.is_null() {
                    let e = last_error();
                    if e.raw_os_error() != Some(ERROR_INVALID_PARAMETER as i32) {
                        return Err(io::Error::new(
                            e.kind(),
                            format!("cannot associate handle with the completion port: {e}"),
                        ));
                    }
                    // Already bound to some port. Only a value a Pulse
                    // driver bound, with nothing in flight on it, may move:
                    // it is either a sibling's file (a stolen fiber's) or a
                    // closed value reused by a file another port owns.
                    let Some((port, _)) = known else {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "handle is bound to another completion port",
                        ));
                    };
                    if rebinds_reused_handles() {
                        self.rebind(h)?;
                    } else if port != me {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "handle is bound to another Pulse completion port and this OS cannot rebind it",
                        ));
                    }
                    // Without rebinding, a value bound here before is assumed
                    // to still be this driver's file (see
                    // `rebinds_reused_handles`).
                }
                table.insert(
                    h as usize,
                    Binding {
                        port: me,
                        in_flight: 0,
                    },
                );
            }
        }
        table.get_mut(&(h as usize)).expect("bound above").in_flight += 1;
        drop(table);
        let flags = (FILE_SKIP_COMPLETION_PORT_ON_SUCCESS | FILE_SKIP_SET_EVENT_ON_HANDLE) as u8;
        // SAFETY: flags call on a live handle.
        if unsafe { SetFileCompletionNotificationModes(h, flags) } == 0 {
            let e = last_error();
            release(h);
            return Err(e);
        }
        Ok(())
    }
    fn rebind(&mut self, h: HANDLE) -> io::Result<()> {
        if !rebinds_reused_handles() {
            return Ok(());
        }
        let rc = replace_completion(h, self.port());
        if rc < 0 {
            let e = afd::ntstatus_error(rc);
            return Err(io::Error::new(
                e.kind(),
                format!("cannot rebind a reused handle value to this completion port: {e}"),
            ));
        }
        Ok(())
    }
    fn afd(&mut self) -> io::Result<HANDLE> {
        if let Some(h) = &self.afd {
            return Ok(h.as_raw_handle() as HANDLE);
        }
        let afd = afd::open()?;
        let h = afd.as_raw_handle() as HANDLE;
        // SAFETY: associating the fresh AFD handle with our live port.
        if unsafe { CreateIoCompletionPort(h, self.port(), KEY_KERNEL, 0) }.is_null() {
            return Err(last_error());
        }
        // Keep packets on success (a poll can finish at once); skip only
        // the handle's event.
        // SAFETY: flags call on a live handle.
        if unsafe { SetFileCompletionNotificationModes(h, FILE_SKIP_SET_EVENT_ON_HANDLE as u8) }
            == 0
        {
            return Err(last_error());
        }
        self.afd = Some(afd);
        Ok(h)
    }
    fn post(&self, slot: &Slot) -> io::Result<()> {
        // SAFETY: the slot stays owned by `entries` until this packet is
        // dequeued; the port only stores the pointer.
        if unsafe { PostQueuedCompletionStatus(self.port(), 0, KEY_POSTED, &slot.ov) } == 0 {
            return Err(last_error());
        }
        Ok(())
    }
    /// Issue an overlapped operation. Returns `None` when the kernel will
    /// post a packet.
    fn start(request: &mut Request<T>, slot: &mut Slot) -> Option<io::Result<usize>> {
        slot.ov.Anonymous.Anonymous.Offset = request.offset as u32;
        slot.ov.Anonymous.Anonymous.OffsetHigh = (request.offset >> 32) as u32;
        let h = handle(&request.file);
        let len = request.buffer.len() as u32;
        // SAFETY: the buffer and OVERLAPPED are owned by the driver's table
        // until the operation's packet is dequeued (or it failed at once).
        let ok = unsafe {
            match request.operation {
                Operation::Read => ReadFile(
                    h,
                    request.buffer.as_mut_ptr(),
                    len,
                    std::ptr::null_mut(),
                    &mut slot.ov,
                ),
                Operation::Write => WriteFile(
                    h,
                    request.buffer.as_ptr(),
                    len,
                    std::ptr::null_mut(),
                    &mut slot.ov,
                ),
            }
        } != 0;
        let code = if ok { 0 } else { unsafe { GetLastError() } };
        if !ok && code == ERROR_IO_PENDING {
            return None;
        }
        let mut bytes = 0u32;
        if ok {
            // SAFETY: the operation has completed; this only reads the result.
            unsafe { GetOverlappedResult(h, &slot.ov, &mut bytes, 0) };
        }
        Some(result(request.operation, ok, bytes, code))
    }
    /// Try a socket transfer now; arm an AFD poll if it would block.
    fn start_socket(
        &mut self,
        request: &mut Request<T>,
        slot: &mut Slot,
        socket: SOCKET,
        base: SOCKET,
    ) -> io::Result<State> {
        afd::set_nonblocking(socket)?;
        match afd::transfer(socket, request.operation, &mut request.buffer) {
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            done => return Ok(State::Posted(done)),
        }
        let afd = self.afd()?;
        // SAFETY: the slot is boxed and owned by the table until the poll's
        // packet is dequeued.
        unsafe {
            afd::arm(
                afd,
                &mut slot.ov,
                &mut slot.poll,
                base,
                afd::interest(request.operation),
            )?
        };
        self.afd_polls += 1;
        Ok(State::Socket(socket, base))
    }
    fn offload(&mut self, id: RequestId, request: &mut Request<T>, slot: &Slot) -> usize {
        let index = match self.offloaders.iter().position(|o| o.busy.is_none()) {
            Some(i) => i,
            None => {
                let (jobs, rx) = mpsc::channel();
                let state = Arc::new(Mutex::new(ThreadState::default()));
                let port = self.port.clone();
                let done = self.done_tx.clone();
                let thread_state = state.clone();
                let thread = std::thread::Builder::new()
                    .name("pulse-iocp-offload".into())
                    .spawn(move || offload_thread(port, rx, thread_state, done))
                    .expect("spawn an IOCP offload thread");
                self.offloaders.push(Offloader {
                    jobs,
                    thread,
                    state,
                    busy: None,
                });
                self.offloaders.len() - 1
            }
        };
        let o = &mut self.offloaders[index];
        o.busy = Some(id);
        let job = Job {
            id: id.0,
            handle: handle(&request.file) as usize,
            buffer: std::mem::take(&mut request.buffer),
            op: request.operation,
            offset: request.offset,
            ov: &slot.ov as *const OVERLAPPED as usize,
        };
        o.jobs
            .send(job)
            .expect("an offload thread lives as long as its sender");
        index
    }
    fn complete_offloaded(&mut self, id: RequestId, thread: usize) -> io::Result<Completion<T>> {
        while let Ok(done) = self.done_rx.try_recv() {
            self.finished.insert(done.id, done);
        }
        let done = self
            .finished
            .remove(&id.0)
            .ok_or_else(|| io::Error::other("offload packet without its result"))?;
        if let Some(o) = self.offloaders.get_mut(thread) {
            o.busy = None;
        }
        let mut entry = self.take(id)?;
        entry.request.buffer = done.buffer;
        Ok(self.finish(id, entry.request, done.result))
    }
    fn take(&mut self, id: RequestId) -> io::Result<Entry<T>> {
        let entry = self
            .entries
            .remove(&id)
            .ok_or_else(|| io::Error::other("unknown or duplicate completion packet"))?;
        self.slots
            .remove(&(&entry.slot.ov as *const OVERLAPPED as usize));
        self.cancelling.remove(&id);
        if entry.bound {
            release(handle(&entry.request.file));
        }
        Ok(entry)
    }
    fn finish(
        &mut self,
        id: RequestId,
        mut request: Request<T>,
        result: io::Result<usize>,
    ) -> Completion<T> {
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
    /// An AFD poll fired: transfer, or re-arm when the socket is not ready
    /// after all. `Ok(None)` means the request stays pending.
    fn socket_ready(
        &mut self,
        id: RequestId,
        socket: SOCKET,
        base: SOCKET,
    ) -> io::Result<Option<Completion<T>>> {
        let cancelling = self.cancelling.contains(&id);
        let entry = self.entries.get_mut(&id).expect("caller checked");
        let status = entry.slot.ov.Internal as i32;
        let outcome = if status < 0 {
            Some(Err(afd::ntstatus_error(status)))
        } else if entry.slot.poll.number_of_handles == 1
            && entry.slot.poll.handles[0].events & afd::AFD_POLL_LOCAL_CLOSE != 0
        {
            Some(Err(io::Error::from_raw_os_error(
                ERROR_INVALID_HANDLE as i32,
            )))
        } else {
            match afd::transfer(socket, entry.request.operation, &mut entry.request.buffer) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if cancelling {
                        Some(Err(cancelled_error()))
                    } else {
                        None
                    }
                }
                done => Some(done),
            }
        };
        let Some(result) = outcome else {
            let afd = self.afd()?;
            let entry = self.entries.get_mut(&id).expect("caller checked");
            let op = entry.request.operation;
            // SAFETY: as in `start_socket`; the slot is still owned here.
            if let Err(e) = unsafe {
                afd::arm(
                    afd,
                    &mut entry.slot.ov,
                    &mut entry.slot.poll,
                    base,
                    afd::interest(op),
                )
            } {
                let entry = self.take(id)?;
                return Ok(Some(self.finish(id, entry.request, Err(e))));
            }
            self.afd_polls += 1;
            return Ok(None);
        };
        let entry = self.take(id)?;
        Ok(Some(self.finish(id, entry.request, result)))
    }
    fn complete(&mut self, id: RequestId) -> io::Result<Option<Completion<T>>> {
        let state = match self.entries.get(&id) {
            None => return Err(io::Error::other("unknown or duplicate completion packet")),
            Some(e) => match &e.state {
                State::Socket(s, b) => Some(Ok((*s, *b))),
                State::Offloaded(t) => Some(Err(*t)),
                _ => None,
            },
        };
        match state {
            Some(Ok((socket, base))) => return self.socket_ready(id, socket, base),
            Some(Err(thread)) => return self.complete_offloaded(id, thread).map(Some),
            None => {}
        }
        let Entry {
            request,
            slot,
            state,
            ..
        } = self.take(id)?;
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
            State::Socket(..) | State::Offloaded(_) => unreachable!("handled above"),
        };
        Ok(Some(self.finish(id, request, result)))
    }
    /// Re-issue `CancelSynchronousIo` for offloaded operations still being
    /// cancelled.
    fn retry_sync_cancels(&self) {
        for id in &self.cancelling {
            if let Some(Entry {
                state: State::Offloaded(t),
                ..
            }) = self.entries.get(id)
            {
                self.cancel_offloaded(*id, *t);
            }
        }
    }
    fn cancel_offloaded(&self, id: RequestId, thread: usize) {
        let o = &self.offloaders[thread];
        let mut s = lock(&o.state);
        if s.current == Some(id.0) {
            // SAFETY: the thread handle is live (the JoinHandle owns it); the
            // call only cancels that thread's pending synchronous I/O, which
            // is this request's (checked under the state lock).
            unsafe { CancelSynchronousIo(o.thread.as_raw_handle() as HANDLE) };
        } else {
            s.cancelled = Some(id.0);
        }
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
        let id = RequestId(self.next);
        let mut slot = Box::new(Slot {
            // SAFETY: OVERLAPPED is plain data; zero is its initial state.
            ov: unsafe { std::mem::zeroed() },
            id: id.0,
            poll: afd::PollInfo::zeroed(),
        });
        let mut bound = false;
        let state = if let Some((socket, base)) = afd::socket_of(&request.file) {
            match self.start_socket(&mut request, &mut slot, socket, base) {
                Ok(state) => state,
                Err(e) => return Err((e, request)),
            }
        } else {
            let sync = match synchronous(&request.file) {
                Ok(sync) => sync,
                Err(e) => return Err((e, request)),
            };
            if sync && is_disk(&request.file) {
                let op = request.operation;
                let h = handle(&request.file);
                State::Posted(sync_transfer(h, op, &mut request.buffer, request.offset))
            } else if sync {
                State::Offloaded(usize::MAX)
            } else {
                if let Err(e) = self.associate(&request.file) {
                    return Err((e, request));
                }
                bound = true;
                match Self::start(&mut request, &mut slot) {
                    None => State::Kernel,
                    Some(result) => State::Posted(result),
                }
            }
        };
        self.next += 1;
        let state = match state {
            State::Offloaded(_) => State::Offloaded(self.offload(id, &mut request, &slot)),
            other => other,
        };
        let posted = matches!(state, State::Posted(_));
        self.slots
            .insert(&slot.ov as *const OVERLAPPED as usize, id);
        self.entries.insert(
            id,
            Entry {
                request,
                slot,
                state,
                bound,
            },
        );
        if posted {
            let entry = &self.entries[&id];
            if let Err(e) = self.post(&entry.slot) {
                // No packet is queued and the operation already finished:
                // hand the request back untouched by the table.
                let entry = self.take(id).expect("just inserted");
                return Err((e, entry.request));
            }
        }
        Ok(id)
    }
    fn cancel(&mut self, id: RequestId) -> io::Result<bool> {
        let Some(entry) = self.entries.get(&id) else {
            return Ok(false);
        };
        if self.cancelling.contains(&id) {
            return Ok(false);
        }
        match entry.state {
            State::Posted(_) => Ok(false),
            State::Offloaded(t) => {
                self.cancel_offloaded(id, t);
                self.cancelling.insert(id);
                Ok(true)
            }
            State::Socket(..) => {
                let afd = self.afd()?;
                let entry = &self.entries[&id];
                // Marked first: a poll that already fired and finds the
                // socket not ready completes cancelled instead of re-arming.
                self.cancelling.insert(id);
                // SAFETY: cancels only the poll identified by this OVERLAPPED.
                if unsafe { CancelIoEx(afd, &entry.slot.ov) } == 0 {
                    let e = last_error();
                    if e.raw_os_error() == Some(ERROR_NOT_FOUND as i32) {
                        return Ok(false);
                    }
                    self.cancelling.remove(&id);
                    return Err(e);
                }
                Ok(true)
            }
            State::Kernel => {
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
        }
    }
    fn cancel_all(&mut self) -> io::Result<()> {
        let ids: Vec<_> = self.entries.keys().copied().collect();
        first_error(ids, |id| self.cancel(id).map(|_| ()))
    }
    fn poll(&mut self, wait: bool) -> Result<Vec<Completion<T>>, PollError<T>> {
        loop {
            if self.entries.is_empty() {
                return Ok(vec![]);
            }
            let retrying = self.cancelling.iter().any(|id| {
                matches!(
                    self.entries.get(id),
                    Some(Entry {
                        state: State::Offloaded(_),
                        ..
                    })
                )
            });
            // A CancelSynchronousIo that raced ahead of its ReadFile missed
            // it: re-issue on every poll, blocking or not.
            if retrying {
                self.retry_sync_cancels();
            }
            let timeout = match (wait, retrying) {
                (false, _) => 0,
                (true, false) => INFINITE,
                (true, true) => CANCEL_RETRY_MS,
            };
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
                    if wait {
                        continue;
                    }
                    return Ok(vec![]);
                }
                return Err(PollError {
                    error: e,
                    completed: vec![],
                });
            }
            let ids: Vec<_> = self.events[..n as usize]
                .iter()
                .map(|e| self.slots.get(&(e.lpOverlapped as usize)).copied())
                .collect();
            let mut completed = vec![];
            let mut error = None;
            for id in ids {
                let done = match id {
                    Some(id) => self.complete(id),
                    None => Err(io::Error::other(
                        "completion packet for an OVERLAPPED this driver does not own",
                    )),
                };
                match done {
                    Ok(Some(c)) => completed.push(c),
                    Ok(None) => {}
                    Err(e) => {
                        error.get_or_insert(e);
                    }
                }
            }
            if let Some(error) = error {
                return Err(PollError { error, completed });
            }
            // Only re-armed socket polls were dequeued: keep waiting.
            if completed.is_empty() && wait {
                continue;
            }
            return Ok(completed);
        }
    }
}

impl<T> Drop for IocpDriver<T> {
    fn drop(&mut self) {
        // The kernel (or an offload thread) may still write into pending
        // buffers: cancel and wait for every packet before releasing them.
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
        // This port is about to close: handles bound to it must be rebound
        // (or rejected) by the next driver, never trusted as its own when a
        // new port reuses this one's handle value.
        let me = self.port() as usize;
        for b in bindings().values_mut() {
            if b.port == me {
                b.port = 0;
            }
        }
        // Idle offload threads exit when their job senders drop; they hold
        // the port alive through their `Arc` until then.
    }
}

fn replace_completion(h: HANDLE, port: HANDLE) -> i32 {
    let info = FILE_COMPLETION_INFORMATION {
        Port: port,
        Key: KEY_KERNEL as *mut _,
    };
    // SAFETY: plain data out-parameter.
    let mut status: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    // SAFETY: `info` and `status` are live locals of the declared sizes.
    unsafe {
        NtSetInformationFile(
            h,
            &mut status,
            (&info as *const FILE_COMPLETION_INFORMATION).cast(),
            std::mem::size_of::<FILE_COMPLETION_INFORMATION>() as u32,
            FileReplaceCompletionInformation,
        )
    }
}

/// Whether this OS can rebind a handle to another completion port
/// (`FileReplaceCompletionInformation`, Windows 8.1 and later). Probed once,
/// on a private pipe and two private ports. Where it is missing (Wine 9),
/// the driver assumes a handle value it bound before, and that no request
/// of its own still holds, is still its own, so reusing such a value for a
/// file another port owns is then unsupported.
pub fn rebinds_reused_handles() -> bool {
    static PROBE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *PROBE.get_or_init(|| {
        let Ok((reader, _writer)) = overlapped_pipe() else {
            return false;
        };
        let port = || {
            // SAFETY: creating a new port takes no pointers.
            let p =
                unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, std::ptr::null_mut(), 0, 1) };
            // SAFETY: a fresh handle owned from here on.
            (!p.is_null()).then(|| unsafe { OwnedHandle::from_raw_handle(p as RawHandle) })
        };
        let (Some(a), Some(b)) = (port(), port()) else {
            return false;
        };
        let h = handle(&reader);
        // SAFETY: binding a live private pipe to a live private port.
        if unsafe { CreateIoCompletionPort(h, a.as_raw_handle() as HANDLE, KEY_KERNEL, 0) }
            .is_null()
        {
            return false;
        }
        let rc = replace_completion(h, b.as_raw_handle() as HANDLE);
        rc >= 0
            && ![
                STATUS_INVALID_INFO_CLASS,
                STATUS_NOT_IMPLEMENTED,
                STATUS_INVALID_DEVICE_REQUEST,
            ]
            .contains(&rc)
    })
}

fn pipe_pair(writer_flags: FILE_FLAGS_AND_ATTRIBUTES) -> io::Result<(File, File)> {
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
            writer_flags,
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

/// A connected, overlapped, unidirectional named pipe `(reader, writer)` with
/// a unique name, both ends opened with `FILE_FLAG_OVERLAPPED`.
pub fn overlapped_pipe() -> io::Result<(File, File)> {
    pipe_pair(FILE_FLAG_OVERLAPPED)
}

/// A pipe whose reader is overlapped (for the driver) and whose writer is
/// synchronous, so any thread can `write` to it with `std::io::Write`.
pub fn wake_pipe() -> io::Result<(File, File)> {
    pipe_pair(0)
}
