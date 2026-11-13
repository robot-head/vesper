//! Bounded direct-child supervision shared by probes, compilation and replay.
//! No child reads stdin. The watcher enforces the absolute child deadline even
//! while the consumer is inspecting an inventory or hashing import artifacts.
//! Shutdown grace triggers a direct-child kill and measures actual reap/join
//! completion. Portable std OS wait/read/join calls are not hard-interruptible;
//! an observed grace overrun fails rather than being advertised as bounded.
//! This does not claim cancellation of arbitrary independently spawned descendants.
use super::{Digest, Error, ErrorKind, Limits};
use std::{
    collections::VecDeque,
    io::{Read, Write},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const QUEUED: usize = 4;
const CHUNK: usize = 4096;
// Includes the two hashers, fixed pipe-read buffers, queue slots, Arc/mutex/
// watcher state and small scalar bookkeeping. Thread stacks are OS resources.
const FIXED_BYTES: usize = 65536;

fn error(kind: ErrorKind, detail: &str) -> Error {
    Error::new(kind, detail)
}
fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(|e| e.into_inner())
}
fn elapsed(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Reservation charged alongside the simultaneously retained protocol inventory.
pub fn transport_bytes(limits: &Limits) -> Result<usize, Error> {
    limits.validate()?;
    limits
        .frame_bytes
        .checked_mul(QUEUED + 1)
        .and_then(|n| n.checked_add(limits.stderr_bytes))
        .and_then(|n| n.checked_add(FIXED_BYTES))
        .ok_or_else(|| error(ErrorKind::OutputLimit, "transport reservation overflow"))
}

/// A header-boundary EOF is distinct from either truncated frame component.
pub fn read_frame<R: Read>(reader: &mut R, cap: usize) -> Result<Option<Vec<u8>>, Error> {
    if cap == 0 || cap > Limits::HARD.frame_bytes {
        return Err(error(ErrorKind::InvalidConfig, "frame cap"));
    }
    let mut header = [0u8; 4];
    let mut done = 0;
    while done < header.len() {
        match reader.read(&mut header[done..]) {
            Ok(0) if done == 0 => return Ok(None),
            Ok(0) => return Err(error(ErrorKind::Protocol, "truncated frame header")),
            Ok(n) => done += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    let len = u32::from_le_bytes(header) as usize;
    if len == 0 {
        return Err(error(ErrorKind::Protocol, "frame payload cap"));
    }
    if len > cap {
        return Err(error(ErrorKind::OutputLimit, "frame payload cap"));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(len)
        .map_err(|_| error(ErrorKind::Allocation, "frame allocation"))?;
    bytes.resize(len, 0);
    done = 0;
    while done < len {
        match reader.read(&mut bytes[done..]) {
            Ok(0) => return Err(error(ErrorKind::Protocol, "truncated frame payload")),
            Ok(n) => done += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(Some(bytes))
}

pub fn write_frame<W: Write>(writer: &mut W, payload: &[u8], cap: usize) -> Result<(), Error> {
    if cap == 0 || cap > Limits::HARD.frame_bytes || payload.is_empty() || payload.len() > cap {
        return Err(error(ErrorKind::Protocol, "frame payload cap"));
    }
    writer.write_all(&(payload.len() as u32).to_le_bytes())?;
    writer.write_all(payload)?;
    writer.flush()?;
    Ok(())
}

#[derive(Debug)]
pub struct ProcessResult {
    pub exit: i32,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub stdout_blake3: Digest,
    pub stderr_blake3: Digest,
    pub elapsed_ms: u64,
    pub stderr: Vec<u8>,
    pub child_reaped: bool,
    pub stdout_joined: bool,
    pub stderr_joined: bool,
    pub watcher_joined: bool,
}

#[derive(Debug)]
pub struct Captured {
    pub process: ProcessResult,
    pub stdout: Vec<u8>,
}

/// Actual observations after cleanup, also delivered when a phase failed.
/// An absent exit code denotes a signal or failed OS wait, never a fake code.
#[derive(Debug)]
pub struct ProcessObservation {
    pub exit: Option<i32>,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub stdout_blake3: Digest,
    pub stderr_blake3: Digest,
    pub elapsed_ms: u64,
    pub stderr: Vec<u8>,
    pub child_reaped: bool,
    pub stdout_joined: bool,
    pub stderr_joined: bool,
    pub watcher_joined: bool,
    pub cleanup_elapsed_ms: u64,
    pub cleanup_deadline_exceeded: bool,
}

struct Stream {
    bytes: u64,
    hash: blake3::Hasher,
    retained: Vec<u8>,
}
impl Stream {
    fn new() -> Self {
        Self {
            bytes: 0,
            hash: blake3::Hasher::new(),
            retained: Vec::new(),
        }
    }
    fn record(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| error(ErrorKind::OutputLimit, "stream counter overflow"))?;
        self.hash.update(bytes);
        Ok(())
    }
}

type Fault = Arc<Mutex<Option<Error>>>;
fn fault(state: &Fault, failure: Error) {
    lock(state).get_or_insert(failure);
}

// Hash actual reads, including header bytes and incomplete frames. Retention
// remains separately bounded; an overrun never requires retaining its bytes.
struct Metered<R> {
    inner: R,
    stream: Arc<Mutex<Stream>>,
    fault: Fault,
    cap: u64,
}
impl<R: Read> Read for Metered<R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let remaining = self.cap.saturating_sub(lock(&self.stream).bytes);
        let max = bytes.len().min(
            usize::try_from(remaining)
                .unwrap_or(usize::MAX)
                .saturating_add(1),
        );
        let n = self.inner.read(&mut bytes[..max])?;
        let mut stream = lock(&self.stream);
        if let Err(e) = stream.record(&bytes[..n]) {
            fault(&self.fault, e);
            return Err(std::io::Error::other("stream counter overflow"));
        }
        if stream.bytes > self.cap {
            fault(
                &self.fault,
                error(ErrorKind::OutputLimit, "child stdout byte cap"),
            );
            return Err(std::io::Error::other("child stdout byte cap"));
        }
        Ok(n)
    }
}

enum Event {
    Data(Instant, Vec<u8>),
    Eof(Instant),
    Failed(Instant, Error),
}
impl Event {
    fn at(&self) -> Instant {
        match self {
            Self::Data(at, _) | Self::Eof(at) | Self::Failed(at, _) => *at,
        }
    }
}
struct QueueState {
    entries: VecDeque<Event>,
    closed: bool,
    #[cfg(test)]
    peak: usize,
    #[cfg(test)]
    frame_peak: usize,
}
struct Queue {
    state: Mutex<QueueState>,
    changed: Condvar,
}
impl Queue {
    fn new() -> Result<Self, Error> {
        let mut entries = VecDeque::new();
        entries
            .try_reserve_exact(QUEUED)
            .map_err(|_| error(ErrorKind::Allocation, "stdout queue allocation"))?;
        Ok(Self {
            state: Mutex::new(QueueState {
                entries,
                closed: false,
                #[cfg(test)]
                peak: 0,
                #[cfg(test)]
                frame_peak: 0,
            }),
            changed: Condvar::new(),
        })
    }
    fn send(&self, event: Event) -> bool {
        let mut state = lock(&self.state);
        #[cfg(test)]
        if let Event::Data(_, bytes) = &event {
            state.frame_peak = state.frame_peak.max(bytes.len());
        }
        while state.entries.len() == QUEUED && !state.closed {
            state = self.changed.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        if state.closed {
            return false;
        }
        state.entries.push_back(event);
        #[cfg(test)]
        {
            let frames = state
                .entries
                .iter()
                .filter(|event| matches!(event, Event::Data(..)))
                .count();
            state.peak = state.peak.max(frames);
        }
        self.changed.notify_all();
        true
    }
    fn close(&self) {
        let mut state = lock(&self.state);
        state.closed = true;
        state.entries.clear();
        self.changed.notify_all();
    }
    fn next(&self, until: Instant, faults: &Fault) -> Result<Event, Error> {
        let mut state = lock(&self.state);
        loop {
            if let Some(failure) = lock(faults).clone() {
                return Err(failure);
            }
            if let Some(event) = state.entries.pop_front() {
                self.changed.notify_all();
                if event.at() > until {
                    return Err(error(
                        ErrorKind::Timeout,
                        "child frame completed after deadline",
                    ));
                }
                return Ok(event);
            }
            if state.closed {
                return Err(error(ErrorKind::Io, "closed stdout queue"));
            }
            let now = Instant::now();
            if now >= until {
                return Err(error(ErrorKind::Timeout, "child output deadline"));
            }
            let (next, _) = self
                .changed
                .wait_timeout(state, (until - now).min(Duration::from_millis(10)))
                .unwrap_or_else(|e| e.into_inner());
            state = next;
        }
    }
}

#[derive(Clone, Copy)]
enum Output {
    Framed,
    Raw,
}
struct Exit {
    status: Option<ExitStatus>,
    at: Instant,
}
#[derive(Default)]
struct Cleanup {
    child_reaped: bool,
    stdout_joined: bool,
    stderr_joined: bool,
    watcher_joined: bool,
    elapsed_ms: u64,
    deadline_exceeded: bool,
    #[cfg(test)]
    max_retained_frame: usize,
    #[cfg(test)]
    max_queued_frames: usize,
    #[cfg(test)]
    stderr_retained: usize,
}
struct Session {
    child: Arc<Mutex<Child>>,
    cancel: Arc<AtomicBool>,
    faults: Fault,
    exit: Arc<Mutex<Option<Exit>>>,
    queue: Arc<Queue>,
    stdout: Arc<Mutex<Stream>>,
    stderr: Arc<Mutex<Stream>>,
    reader: Option<JoinHandle<()>>,
    drainer: Option<JoinHandle<()>>,
    watcher: Option<JoinHandle<()>>,
    until: Instant,
    shutdown_until: Arc<Mutex<Option<Instant>>>,
    shutdown_timeout: Duration,
    start: Instant,
    closed: bool,
}
impl Session {
    fn spawn(
        mut command: Command,
        mode: Output,
        timeout_ms: u64,
        limits: &Limits,
    ) -> Result<Self, Error> {
        limits.validate()?;
        if timeout_ms == 0 || timeout_ms > Limits::HARD.setup_timeout_ms {
            return Err(error(ErrorKind::InvalidConfig, "process timeout"));
        }
        if transport_bytes(limits)? > limits.inventory_bytes {
            return Err(error(
                ErrorKind::OutputLimit,
                "transport metadata reservation",
            ));
        }
        let start = Instant::now();
        let until = start
            .checked_add(Duration::from_millis(timeout_ms))
            .ok_or_else(|| error(ErrorKind::InvalidConfig, "process deadline overflow"))?;
        let queue = Arc::new(Queue::new()?);
        let mut err_stream = Stream::new();
        err_stream
            .retained
            .try_reserve_exact(limits.stderr_bytes)
            .map_err(|_| error(ErrorKind::Allocation, "stderr retention allocation"))?;
        let stdout = Arc::new(Mutex::new(Stream::new()));
        let stderr = Arc::new(Mutex::new(err_stream));
        let faults = Arc::new(Mutex::new(None));
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        let out = child.stdout.take();
        let err = child.stderr.take();
        let (Some(out), Some(mut err)) = (out, err) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error(ErrorKind::Io, "missing child pipes"));
        };
        let mut session = Self {
            child: Arc::new(Mutex::new(child)),
            cancel: Arc::new(AtomicBool::new(false)),
            faults,
            exit: Arc::new(Mutex::new(None)),
            queue,
            stdout,
            stderr,
            reader: None,
            drainer: None,
            watcher: None,
            until,
            shutdown_until: Arc::new(Mutex::new(None)),
            shutdown_timeout: Duration::from_millis(limits.shutdown_timeout_ms),
            start,
            closed: false,
        };
        let child = Arc::clone(&session.child);
        let cancel = Arc::clone(&session.cancel);
        let faults = Arc::clone(&session.faults);
        let exit = Arc::clone(&session.exit);
        let shutdown_until = Arc::clone(&session.shutdown_until);
        session.watcher = Some(
            thread::Builder::new()
                .name("lean4lean42-watch".into())
                .spawn(move || {
                    loop {
                        let mut child = lock(&child);
                        match child.try_wait() {
                            Ok(Some(status)) => {
                                let at = Instant::now();
                                if at > until {
                                    fault(
                                        &faults,
                                        error(ErrorKind::Timeout, "child exit deadline"),
                                    );
                                }
                                if lock(&shutdown_until).is_some_and(|limit| at > limit) {
                                    fault(
                                        &faults,
                                        error(ErrorKind::Timeout, "child shutdown deadline"),
                                    );
                                }
                                *lock(&exit) = Some(Exit {
                                    status: Some(status),
                                    at,
                                });
                                break;
                            }
                            Ok(None) => {}
                            Err(e) => fault(&faults, e.into()),
                        }
                        if Instant::now() >= until {
                            fault(&faults, error(ErrorKind::Timeout, "child process deadline"));
                        }
                        if lock(&shutdown_until).is_some_and(|limit| Instant::now() >= limit) {
                            fault(
                                &faults,
                                error(ErrorKind::Timeout, "child shutdown deadline"),
                            );
                        }
                        if cancel.load(Ordering::Acquire) || lock(&faults).is_some() {
                            let _ = child.kill();
                            let status = match child.wait() {
                                Ok(status) => Some(status),
                                Err(e) => {
                                    fault(&faults, e.into());
                                    None
                                }
                            };
                            *lock(&exit) = Some(Exit {
                                status,
                                at: Instant::now(),
                            });
                            break;
                        }
                        drop(child);
                        thread::sleep(Duration::from_millis(2));
                    }
                })?,
        );
        let queue = Arc::clone(&session.queue);
        let faults = Arc::clone(&session.faults);
        let mut out = Metered {
            inner: out,
            stream: Arc::clone(&session.stdout),
            fault: Arc::clone(&faults),
            cap: limits.stdout_bytes,
        };
        let frame_cap = limits.frame_bytes;
        session.reader = Some(
            thread::Builder::new()
                .name("lean4lean42-stdout".into())
                .spawn(move || {
                    loop {
                        let result = match mode {
                            Output::Framed => read_frame(&mut out, frame_cap),
                            Output::Raw => {
                                let mut buffer = [0u8; CHUNK];
                                match out.read(&mut buffer[..CHUNK.min(frame_cap)]) {
                                    Ok(0) => Ok(None),
                                    Ok(n) => {
                                        let mut bytes = Vec::new();
                                        match bytes.try_reserve_exact(n) {
                                            Ok(()) => {
                                                bytes.extend_from_slice(&buffer[..n]);
                                                Ok(Some(bytes))
                                            }
                                            Err(_) => Err(error(
                                                ErrorKind::Allocation,
                                                "stdout chunk allocation",
                                            )),
                                        }
                                    }
                                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                                        continue;
                                    }
                                    Err(e) => Err(e.into()),
                                }
                            }
                        };
                        let at = Instant::now();
                        let terminal = !matches!(&result, Ok(Some(_)));
                        let event = match result {
                            Ok(Some(bytes)) => Event::Data(at, bytes),
                            Ok(None) => Event::Eof(at),
                            Err(e) => {
                                fault(&faults, e.clone());
                                Event::Failed(at, e)
                            }
                        };
                        if !queue.send(event) || terminal {
                            break;
                        }
                    }
                })?,
        );
        let state = Arc::clone(&session.stderr);
        let faults = Arc::clone(&session.faults);
        let stderr_cap = limits.stderr_bytes;
        session.drainer = Some(
            thread::Builder::new()
                .name("lean4lean42-stderr".into())
                .spawn(move || {
                    let mut buffer = [0u8; CHUNK];
                    loop {
                        match err.read(&mut buffer) {
                            Ok(0) => break,
                            Ok(n) => {
                                let mut state = lock(&state);
                                if let Err(e) = state.record(&buffer[..n]) {
                                    fault(&faults, e);
                                }
                                let keep = n.min(stderr_cap.saturating_sub(state.retained.len()));
                                state.retained.extend_from_slice(&buffer[..keep]);
                                if keep != n {
                                    fault(
                                        &faults,
                                        error(ErrorKind::OutputLimit, "child stderr byte cap"),
                                    );
                                }
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                            Err(e) => {
                                fault(&faults, e.into());
                                break;
                            }
                        }
                    }
                })?,
        );
        Ok(session)
    }

    fn cleanup(&mut self, cancel: bool) -> Cleanup {
        let start = Instant::now();
        let until = start + self.shutdown_timeout;
        *lock(&self.shutdown_until) = Some(until);
        if cancel {
            self.cancel.store(true, Ordering::Release);
        }
        let watcher_joined = self.watcher.take().is_some_and(|h| h.join().is_ok());
        // A failed thread creation/panic must not leave the direct child alive.
        if lock(&self.exit).as_ref().is_none_or(|e| e.status.is_none()) {
            let mut child = lock(&self.child);
            let _ = child.kill();
            let status = child.wait().ok();
            *lock(&self.exit) = Some(Exit {
                status,
                at: Instant::now(),
            });
        }
        let child_reaped = lock(&self.exit)
            .as_ref()
            .is_some_and(|e| e.status.is_some());
        // Reap FIRST, cancel the queue SECOND, join pipe threads LAST. Closing
        // wakes a producer blocked on a full four-frame queue.
        self.queue.close();
        let stdout_joined = self.reader.take().is_some_and(|h| h.join().is_ok());
        let stderr_joined = self.drainer.take().is_some_and(|h| h.join().is_ok());
        self.closed = true;
        #[cfg(test)]
        let queue = lock(&self.queue.state);
        Cleanup {
            child_reaped,
            stdout_joined,
            stderr_joined,
            watcher_joined,
            elapsed_ms: elapsed(start),
            deadline_exceeded: Instant::now() > until,
            #[cfg(test)]
            max_retained_frame: queue.frame_peak,
            #[cfg(test)]
            max_queued_frames: queue.peak,
            #[cfg(test)]
            stderr_retained: lock(&self.stderr).retained.len(),
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if !self.closed {
            self.cleanup(true);
        }
    }
}

struct RunOutcome {
    failure: Option<Error>,
    observation: ProcessObservation,
    #[cfg(test)]
    cleanup: Cleanup,
}
fn run(
    command: Command,
    timeout_ms: u64,
    limits: &Limits,
    mode: Output,
    mut receive: impl FnMut(&[u8], Instant) -> Result<(), Error>,
) -> Result<RunOutcome, Error> {
    let mut session = Session::spawn(command, mode, timeout_ms, limits)?;
    let mut failure = None;
    loop {
        // Consuming/inspection work is part of the absolute phase deadline.
        if Instant::now() >= session.until {
            failure = Some(error(ErrorKind::Timeout, "consumer phase deadline"));
            break;
        }
        match session.queue.next(session.until, &session.faults) {
            Ok(Event::Data(at, bytes)) => {
                if let Err(e) = receive(&bytes, at) {
                    failure = Some(e);
                    break;
                }
            }
            Ok(Event::Eof(_)) => break,
            Ok(Event::Failed(_, e)) | Err(e) => {
                failure = Some(e);
                break;
            }
        }
    }
    let cleanup = session.cleanup(failure.is_some());
    if failure.is_none() {
        failure = lock(&session.faults).clone();
    }
    if failure.is_none()
        && (!cleanup.child_reaped
            || !cleanup.stdout_joined
            || !cleanup.stderr_joined
            || !cleanup.watcher_joined)
    {
        failure = Some(error(ErrorKind::Crash, "child wait or pipe thread failed"));
    }
    if failure.is_none() && cleanup.deadline_exceeded {
        failure = Some(error(
            ErrorKind::Timeout,
            "child cleanup exceeded shutdown grace",
        ));
    }
    let exit = lock(&session.exit);
    if failure.is_none() && exit.as_ref().is_some_and(|e| e.at > session.until) {
        failure = Some(error(ErrorKind::Timeout, "child exit deadline"));
    }
    let code = exit
        .as_ref()
        .and_then(|e| e.status.as_ref())
        .and_then(ExitStatus::code);
    if failure.is_none() && code.is_none() {
        failure = Some(error(
            ErrorKind::Crash,
            "child terminated without an exit code",
        ));
    }
    let observation = {
        let stdout = lock(&session.stdout);
        let mut stderr = lock(&session.stderr);
        ProcessObservation {
            exit: code,
            stdout_bytes: stdout.bytes,
            stderr_bytes: stderr.bytes,
            stdout_blake3: *stdout.hash.finalize().as_bytes(),
            stderr_blake3: *stderr.hash.finalize().as_bytes(),
            elapsed_ms: elapsed(session.start),
            stderr: std::mem::take(&mut stderr.retained),
            child_reaped: cleanup.child_reaped,
            stdout_joined: cleanup.stdout_joined,
            stderr_joined: cleanup.stderr_joined,
            watcher_joined: cleanup.watcher_joined,
            cleanup_elapsed_ms: cleanup.elapsed_ms,
            cleanup_deadline_exceeded: cleanup.deadline_exceeded,
        }
    };
    Ok(RunOutcome {
        failure,
        observation,
        #[cfg(test)]
        cleanup,
    })
}

pub fn framed(
    command: Command,
    timeout_ms: u64,
    limits: &Limits,
    receive: impl FnMut(&[u8], Instant) -> Result<(), Error>,
) -> Result<ProcessResult, Error> {
    framed_observed(command, timeout_ms, limits, receive, |_| {})
}

fn observed_result(
    outcome: RunOutcome,
    observe: impl FnOnce(&ProcessObservation),
) -> Result<ProcessResult, Error> {
    observe(&outcome.observation);
    if let Some(failure) = outcome.failure {
        return Err(failure);
    }
    let value = outcome.observation;
    let exit = value
        .exit
        .ok_or_else(|| error(ErrorKind::Crash, "child has no observed exit code"))?;
    Ok(ProcessResult {
        exit,
        stdout_bytes: value.stdout_bytes,
        stderr_bytes: value.stderr_bytes,
        stdout_blake3: value.stdout_blake3,
        stderr_blake3: value.stderr_blake3,
        elapsed_ms: value.elapsed_ms,
        stderr: value.stderr,
        child_reaped: value.child_reaped,
        stdout_joined: value.stdout_joined,
        stderr_joined: value.stderr_joined,
        watcher_joined: value.watcher_joined,
    })
}

pub fn framed_observed(
    command: Command,
    timeout_ms: u64,
    limits: &Limits,
    receive: impl FnMut(&[u8], Instant) -> Result<(), Error>,
    observe: impl FnOnce(&ProcessObservation),
) -> Result<ProcessResult, Error> {
    observed_result(
        run(command, timeout_ms, limits, Output::Framed, receive)?,
        observe,
    )
}

pub fn capture(
    command: Command,
    timeout_ms: u64,
    limits: &Limits,
    cap: usize,
) -> Result<Captured, Error> {
    capture_observed(command, timeout_ms, limits, cap, |_| {})
}

pub fn capture_observed(
    command: Command,
    timeout_ms: u64,
    limits: &Limits,
    cap: usize,
    observe: impl FnOnce(&ProcessObservation),
) -> Result<Captured, Error> {
    let reserved = transport_bytes(limits)?
        .checked_add(cap)
        .ok_or_else(|| error(ErrorKind::OutputLimit, "capture reservation overflow"))?;
    if cap == 0 || cap as u64 > limits.stdout_bytes || reserved > limits.inventory_bytes {
        return Err(error(ErrorKind::OutputLimit, "capture reservation cap"));
    }
    let mut stdout = Vec::new();
    stdout
        .try_reserve_exact(cap)
        .map_err(|_| error(ErrorKind::Allocation, "capture allocation"))?;
    let outcome = run(command, timeout_ms, limits, Output::Raw, |bytes, _| {
        if bytes.len() > cap.saturating_sub(stdout.len()) {
            return Err(error(ErrorKind::OutputLimit, "captured stdout cap"));
        }
        stdout.extend_from_slice(bytes);
        Ok(())
    })?;
    let process = observed_result(outcome, observe)?;
    Ok(Captured { process, stdout })
}

#[cfg(test)]
pub mod testing {
    use super::*;
    const MARKER: &[u8] = b"lean4lean42-transport-marker";
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ProbeKind {
        Malformed,
        Oversize,
        TruncatedHeader,
        TruncatedPayload,
        WrongToken,
        Stall,
        StderrFlood,
        Crash,
        ShortWrites,
        QueuePressure,
        QueueCancel,
    }
    #[derive(Debug, Clone)]
    pub struct ProbeReport {
        pub kind: ProbeKind,
        pub observed: Option<ErrorKind>,
        pub child_reaped: bool,
        pub stdout_joined: bool,
        pub stderr_joined: bool,
        pub watcher_joined: bool,
        pub max_retained_frame: usize,
        pub stderr_retained: usize,
        pub max_queued_frames: usize,
        pub frames: u64,
    }
    pub fn probe(
        command: Command,
        kind: ProbeKind,
        timeout_ms: u64,
        limits: &Limits,
    ) -> Result<ProbeReport, Error> {
        let marker = matches!(
            kind,
            ProbeKind::ShortWrites | ProbeKind::QueuePressure | ProbeKind::QueueCancel
        );
        let token = "0".repeat(32);
        let mut machine = super::super::protocol::Machine::new(3, 2, &token, limits, &[], None)?;
        let mut frames = 0u64;
        let outcome = run(command, timeout_ms, limits, Output::Framed, |bytes, _| {
            frames += 1;
            if marker {
                if bytes != MARKER {
                    return Err(error(ErrorKind::Protocol, "transport marker mismatch"));
                }
                if kind == ProbeKind::QueuePressure {
                    thread::sleep(Duration::from_millis(2));
                }
                if kind == ProbeKind::QueueCancel {
                    thread::sleep(Duration::from_millis(10));
                }
                Ok(())
            } else {
                machine.feed(bytes)
            }
        })?;
        let observed = match outcome.failure {
            Some(e) => Some(e.kind),
            None if outcome.observation.exit != Some(0) => Some(ErrorKind::Crash),
            None if marker => {
                if (kind == ProbeKind::ShortWrites && frames == 1)
                    || (kind == ProbeKind::QueuePressure && frames == 64)
                {
                    None
                } else {
                    Some(ErrorKind::Protocol)
                }
            }
            None => match machine.finish() {
                Err(e) => Some(e.kind),
                Ok(_) => Some(ErrorKind::Protocol),
            },
        };
        Ok(ProbeReport {
            kind,
            observed,
            child_reaped: outcome.cleanup.child_reaped,
            stdout_joined: outcome.cleanup.stdout_joined,
            stderr_joined: outcome.cleanup.stderr_joined,
            watcher_joined: outcome.cleanup.watcher_joined,
            max_retained_frame: outcome.cleanup.max_retained_frame,
            stderr_retained: outcome.cleanup.stderr_retained,
            max_queued_frames: outcome.cleanup.max_queued_frames,
            frames,
        })
    }
}
