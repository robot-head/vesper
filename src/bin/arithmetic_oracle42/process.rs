//! Bounded subprocess supervision. Numeric authority remains in the one Lean server.
use super::protocol::{self, Counters};
use super::*;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, AtomicUsize, Ordering},
    mpsc::{self, Receiver, SyncSender},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MODEL: &str = include_str!("../../../lean/RewriteWitness.lean");
const DRIVER: &str = include_str!("../../../lean/ArithmeticOracle.lean");
#[cfg(test)]
const MARKER: &[u8] = b"oracle42-transport-marker";
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
fn err(kind: FailureKind, message: impl Into<String>) -> HarnessError {
    HarnessError::new(kind, message)
}
fn ms(time: Instant) -> u64 {
    u64::try_from(time.elapsed().as_millis()).unwrap_or(u64::MAX)
}
fn deadline(timeout: u64) -> Instant {
    Instant::now() + Duration::from_millis(timeout)
}

/// PATH-aware lookup also works with Bazel's bare CARGO_BIN_EXE names.
pub fn resolve_executable(path: &Path) -> Result<PathBuf, HarnessError> {
    let candidates = |p: PathBuf| -> Vec<PathBuf> {
        #[cfg(windows)]
        {
            if p.extension().is_none() {
                return vec![p.with_extension("exe"), p];
            }
        }
        vec![p]
    };
    let bare = path.components().count() == 1 && !path.is_absolute();
    let paths = if bare {
        let value =
            std::env::var_os("PATH").ok_or_else(|| err(FailureKind::Io, "PATH unavailable"))?;
        std::env::split_paths(&value)
            .flat_map(|dir| candidates(dir.join(path)))
            .collect::<Vec<_>>()
    } else {
        candidates(path.to_path_buf())
    };
    for candidate in paths {
        if !candidate.is_file() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if fs::metadata(&candidate)?.permissions().mode() & 0o111 == 0 {
                continue;
            }
        }
        return fs::canonicalize(candidate).map_err(Into::into);
    }
    Err(err(FailureKind::Io, "executable not found"))
}
fn file_hash(path: &Path) -> Result<[u8; 32], HarnessError> {
    let mut file = File::open(path)?;
    let mut hash = blake3::Hasher::new();
    let mut buffer = [0; 16384];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(*hash.finalize().as_bytes())
}
struct TempRoot(PathBuf);
impl TempRoot {
    fn new(parent: &Path, prefix: &str) -> Result<Self, HarnessError> {
        fs::create_dir_all(parent)?;
        for _ in 0..32 {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = parent.join(format!(
                "{prefix}-{}-{stamp}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err(err(FailureKind::Io, "unique directory exhausted"))
    }
}
impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

enum Event {
    Frame(Instant, Result<Option<Vec<u8>>, HarnessError>),
    Written(Instant, Result<(), HarnessError>),
}
#[derive(Default)]
struct StderrState {
    bytes: Vec<u8>,
    overflow: bool,
    io_error: bool,
}
enum ReadMode {
    Framed,
    Raw,
}
struct Cleanup {
    status: Option<ExitStatus>,
    killed: bool,
    reader: bool,
    writer: bool,
    stderr: bool,
}
struct Session {
    child: Child,
    tx: Option<SyncSender<Vec<u8>>>,
    rx: Option<Receiver<Event>>,
    reader: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
    stderr: Option<JoinHandle<()>>,
    stderr_state: Arc<Mutex<StderrState>>,
    #[cfg(test)]
    peak: Arc<AtomicUsize>,
    closed: bool,
}
impl Session {
    fn spawn(
        mut command: Command,
        cap: usize,
        stderr_cap: usize,
        mode: ReadMode,
    ) -> Result<Self, HarnessError> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| err(FailureKind::Io, "child stdout"))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| err(FailureKind::Io, "child stdin"))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| err(FailureKind::Io, "child stderr"))?;
        let (events, rx) = mpsc::sync_channel(4);
        let (write_tx, write_rx) = mpsc::sync_channel::<Vec<u8>>(1);
        let peak = Arc::new(AtomicUsize::new(0));
        let reader_peak = Arc::clone(&peak);
        let read_events = events.clone();
        let reader = thread::spawn(move || match mode {
            ReadMode::Framed => loop {
                let event = protocol::read_frame(&mut stdout, cap);
                let terminal = !matches!(&event, Ok(Some(_)));
                if let Ok(Some(bytes)) = &event {
                    reader_peak.fetch_max(bytes.len(), Ordering::Relaxed);
                }
                if read_events
                    .send(Event::Frame(Instant::now(), event))
                    .is_err()
                    || terminal
                {
                    break;
                }
            },
            ReadMode::Raw => {
                let mut bytes = Vec::new();
                let mut buffer = [0u8; 4096];
                let result = loop {
                    match stdout.read(&mut buffer) {
                        Ok(0) => break Ok(Some(bytes)),
                        Ok(n) => {
                            if n > cap.saturating_sub(bytes.len()) {
                                break Err(err(FailureKind::Protocol, "tool stdout limit"));
                            }
                            bytes.extend_from_slice(&buffer[..n]);
                            reader_peak.fetch_max(bytes.len(), Ordering::Relaxed);
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(e) => break Err(e.into()),
                    }
                };
                let _ = read_events.send(Event::Frame(Instant::now(), result));
            }
        });
        let writer = thread::spawn(move || {
            while let Ok(bytes) = write_rx.recv() {
                let answer = protocol::write_frame(&mut stdin, &bytes, FRAME_CAP);
                let terminal = answer.is_err();
                if events.send(Event::Written(Instant::now(), answer)).is_err() || terminal {
                    break;
                }
            }
        });
        let stderr_state = Arc::new(Mutex::new(StderrState::default()));
        let state = Arc::clone(&stderr_state);
        let stderr = thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            loop {
                match stderr.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                        let keep = n.min(stderr_cap.saturating_sub(state.bytes.len()));
                        state.bytes.extend_from_slice(&buffer[..keep]);
                        if keep < n {
                            state.overflow = true;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => {
                        state.lock().unwrap_or_else(|e| e.into_inner()).io_error = true;
                        break;
                    }
                }
            }
        });
        Ok(Self {
            child,
            tx: Some(write_tx),
            rx: Some(rx),
            reader: Some(reader),
            writer: Some(writer),
            stderr: Some(stderr),
            stderr_state,
            #[cfg(test)]
            peak,
            closed: false,
        })
    }
    fn stderr_fault(&self) -> Result<(), HarnessError> {
        let state = self.stderr_state.lock().unwrap_or_else(|e| e.into_inner());
        if state.overflow {
            return Err(err(FailureKind::Protocol, "child stderr limit"));
        }
        if state.io_error {
            return Err(err(FailureKind::Io, "child stderr read"));
        }
        Ok(())
    }
    fn event(&mut self, until: Instant) -> Result<Event, HarnessError> {
        let check = |event: Event| {
            let at = match &event {
                Event::Frame(at, _) | Event::Written(at, _) => *at,
            };
            if at > until {
                Err(err(FailureKind::Timeout, "child completed after deadline"))
            } else {
                Ok(event)
            }
        };
        loop {
            self.stderr_fault()?;
            let rx = self
                .rx
                .as_ref()
                .ok_or_else(|| err(FailureKind::Io, "closed child receiver"))?;
            // A timely event remains timely while another slot is being serviced.
            match rx.try_recv() {
                Ok(event) => return check(event),
                Err(mpsc::TryRecvError::Disconnected) => {
                    let status = self.child.try_wait()?;
                    return Err(err(
                        if status.is_some_and(|s| !s.success()) {
                            FailureKind::Crash
                        } else {
                            FailureKind::Protocol
                        },
                        "child output ended",
                    ));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            let now = Instant::now();
            if now >= until {
                return Err(err(FailureKind::Timeout, "child response deadline"));
            }
            match rx.recv_timeout((until - now).min(Duration::from_millis(10))) {
                Ok(event) => return check(event),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => continue,
            }
        }
    }
    fn send(&mut self, bytes: Vec<u8>) -> Result<(), HarnessError> {
        self.tx
            .as_ref()
            .ok_or_else(|| err(FailureKind::Io, "closed child input"))?
            .try_send(bytes)
            .map_err(|_| err(FailureKind::Protocol, "child write queue"))
    }
    fn receive(&mut self, until: Instant) -> Result<Option<Vec<u8>>, HarnessError> {
        loop {
            match self.event(until)? {
                Event::Frame(_, value) => return value,
                Event::Written(_, value) => value?,
            }
        }
    }
    fn exchange_sent_observed(
        &mut self,
        until: Instant,
        mut on_written: impl FnMut(),
        mut on_frame: impl FnMut(&[u8]),
    ) -> Result<Vec<u8>, HarnessError> {
        let mut answer = None;
        let mut written = false;
        while answer.is_none() || !written {
            match self.event(until)? {
                Event::Written(_, value) => {
                    value?;
                    if written {
                        return Err(err(
                            FailureKind::Protocol,
                            "duplicate write acknowledgement",
                        ));
                    }
                    written = true;
                    on_written();
                }
                Event::Frame(_, value) => {
                    let bytes = value?
                        .ok_or_else(|| err(FailureKind::Crash, "child closed during request"))?;
                    on_frame(&bytes);
                    if answer.replace(bytes).is_some() {
                        return Err(err(FailureKind::Protocol, "extra child response"));
                    }
                }
            }
        }
        Ok(answer.expect("response present after loop"))
    }
    fn exchange_sent(&mut self, until: Instant) -> Result<Vec<u8>, HarnessError> {
        self.exchange_sent_observed(until, || {}, |_| {})
    }
    fn exchange(&mut self, bytes: Vec<u8>, until: Instant) -> Result<Vec<u8>, HarnessError> {
        self.send(bytes)?;
        self.exchange_sent(until)
    }
    fn close_input(&mut self) {
        self.tx.take();
    }
    fn cleanup(&mut self, timeout: u64, force: bool) -> Result<Cleanup, HarnessError> {
        if self.closed {
            return Err(err(FailureKind::Io, "duplicate cleanup"));
        }
        self.close_input();
        let mut killed = force;
        if force {
            let _ = self.child.kill();
        }
        let until = deadline(timeout);
        let status = loop {
            if let Some(status) = self.child.try_wait()? {
                break Some(status);
            }
            if Instant::now() >= until {
                killed = true;
                let _ = self.child.kill();
                break Some(self.child.wait()?);
            }
            thread::sleep(Duration::from_millis(5));
        };
        // Disconnect event consumers before joins, releasing bounded senders.
        self.rx.take();
        let reader = self.reader.take().is_none_or(|h| h.join().is_ok());
        let writer = self.writer.take().is_none_or(|h| h.join().is_ok());
        let stderr = self.stderr.take().is_none_or(|h| h.join().is_ok());
        self.closed = true;
        Ok(Cleanup {
            status,
            killed,
            reader,
            writer,
            stderr,
        })
    }
    fn finish(&mut self, timeout: u64) -> Result<(), HarnessError> {
        let result = self.cleanup(timeout, false)?;
        self.stderr_fault()?;
        if result.killed {
            return Err(err(FailureKind::Timeout, "child shutdown deadline"));
        }
        if !result.reader || !result.writer || !result.stderr {
            return Err(err(FailureKind::Crash, "pipe thread panic"));
        }
        if !result.status.is_some_and(|s| s.success()) {
            return Err(err(FailureKind::Crash, "child nonzero exit"));
        }
        Ok(())
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if !self.closed {
            let _ = self.cleanup(5000, true);
        }
    }
}

fn tool(mut command: Command, limits: &HarnessLimits) -> Result<Vec<u8>, HarnessError> {
    command.env_remove("LEAN_PATH");
    let mut process = Session::spawn(command, FRAME_CAP, limits.stderr_bytes, ReadMode::Raw)?;
    process.close_input();
    let output = process
        .receive(deadline(limits.startup_timeout_ms))?
        .ok_or_else(|| err(FailureKind::ToolVersion, "missing tool output"))?;
    process.finish(limits.shutdown_timeout_ms)?;
    Ok(output)
}
fn command(program: &Path, root: Option<&Path>) -> Command {
    let mut cmd = Command::new(program);
    if let Some(root) = root {
        cmd.current_dir(root);
    }
    cmd
}
fn version(bytes: &[u8]) -> Result<String, HarnessError> {
    let s = std::str::from_utf8(bytes)
        .map_err(|_| err(FailureKind::ToolVersion, "tool version UTF8"))?
        .trim();
    if s.len() > 1024
        || !s
            .split_whitespace()
            .any(|part| part.trim_matches([',', '(', ')']) == "4.24.0")
    {
        return Err(err(FailureKind::ToolVersion, "Lean 4.24.0 required"));
    }
    Ok(s.to_owned())
}
fn lean_command(program: &Path, root: &Path, search: &std::ffi::OsStr) -> Command {
    let mut cmd = command(program, Some(root));
    cmd.env("LEAN_PATH", search);
    cmd
}

struct WorkerSlot {
    session: Session,
    sequence: u64,
    handled: u16,
}
struct Evidence {
    case: Case,
    request: Vec<u8>,
    response: Vec<u8>,
    rust: Option<RustAnswer>,
    error: HarnessError,
    shrunken: Option<(Case, Vec<u8>, Vec<u8>)>,
    shrink_log: Vec<serde_json::Value>,
}
struct Pending {
    case: Case,
    oracle: OracleAnswer,
    request: Vec<u8>,
    response: Vec<u8>,
    fingerprint: [u8; 32],
    slot: usize,
    until: Instant,
}
struct Engine {
    lean: Session,
    _root: TempRoot,
    worker_path: PathBuf,
    workers: Vec<Option<WorkerSlot>>,
    started: Vec<bool>,
    requests: blake3::Hasher,
    responses: blake3::Hasher,
    corpus: blake3::Hasher,
    wire_seq: u64,
    counters: Counters,
    healthy: bool,
}
impl Engine {
    fn new(config: &RunConfig, report: &mut RunReport) -> Result<Self, HarnessError> {
        let start = Instant::now();
        config.limits.validate()?;
        if !(1..=4).contains(&config.jobs) {
            return Err(err(FailureKind::InvalidConfig, "jobs must be 1..4"));
        }
        let worker_path = resolve_executable(&config.worker_exe)?;
        report.worker_blake3 = file_hash(&worker_path)?;
        let configured = resolve_executable(&config.lean)?;
        let mut cmd = command(&configured, None);
        cmd.arg("--version");
        report.tools.lean_version = version(&tool(cmd, &config.limits)?)?;
        let mut cmd = command(&configured, None);
        cmd.arg("--print-prefix");
        let prefix_bytes = tool(cmd, &config.limits)?;
        let prefix = std::str::from_utf8(&prefix_bytes)
            .map_err(|_| err(FailureKind::ToolVersion, "Lean prefix UTF8"))?
            .trim();
        if prefix.is_empty() || prefix.len() > 4096 {
            return Err(err(FailureKind::ToolVersion, "Lean prefix"));
        }
        let lean = resolve_executable(&Path::new(prefix).join("bin").join("lean"))?;
        let mut cmd = command(&lean, None);
        cmd.arg("--version");
        report.tools.lean_version = version(&tool(cmd, &config.limits)?)?;
        report.tools.rustc_version = resolve_executable(Path::new("rustc"))
            .ok()
            .and_then(|p| {
                let mut cmd = command(&p, None);
                cmd.arg("--version");
                tool(cmd, &config.limits).ok()
            })
            .and_then(|b| String::from_utf8(b).ok())
            .map(|s| s.trim().chars().take(1024).collect())
            .unwrap_or_else(|| "unavailable".into());
        let root = TempRoot::new(&std::env::temp_dir(), "vesper-oracle42")?;
        for (name, source) in [
            ("RewriteWitness.lean", MODEL),
            ("ArithmeticOracle.lean", DRIVER),
        ] {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(root.0.join(name))?;
            file.write_all(source.as_bytes())?;
        }
        report.model_blake3 = *blake3::hash(MODEL.as_bytes()).as_bytes();
        report.driver_blake3 = *blake3::hash(DRIVER.as_bytes()).as_bytes();
        let search = std::env::join_paths([root.0.clone(), Path::new(prefix).join("lib/lean")])
            .map_err(|_| err(FailureKind::ToolVersion, "Lean search path"))?;
        for name in ["RewriteWitness", "ArithmeticOracle"] {
            let mut cmd = lean_command(&lean, &root.0, &search);
            cmd.arg("-o")
                .arg(root.0.join(format!("{name}.olean")))
                .arg(root.0.join(format!("{name}.lean"))); // preserve explicit private LEAN_PATH for import compilation
            let mut child =
                Session::spawn(cmd, FRAME_CAP, config.limits.stderr_bytes, ReadMode::Raw)?;
            child.close_input();
            child.receive(deadline(config.limits.startup_timeout_ms))?;
            child.finish(config.limits.shutdown_timeout_ms)?;
        }
        let mut cmd = lean_command(&lean, &root.0, &search);
        cmd.arg("--run").arg(root.0.join("ArithmeticOracle.lean"));
        let mut server = Session::spawn(
            cmd,
            RESPONSE_CAP,
            config.limits.stderr_bytes,
            ReadMode::Framed,
        )?;
        report.lean_spawns = 1;
        let ready = server
            .receive(deadline(config.limits.startup_timeout_ms))?
            .ok_or_else(|| err(FailureKind::ToolVersion, "Lean READY absent"))?;
        let mut responses = blake3::Hasher::new();
        protocol::hash_frame(&mut responses, &ready);
        // A rejected but fully observed READY is still transcript evidence.
        report.responses_blake3 = *responses.finalize().as_bytes();
        protocol::ready(&ready)?;
        let mut corpus = blake3::Hasher::new();
        corpus.update(b"vesper.oracle42.corpus\0");
        corpus.update(&1u16.to_le_bytes());
        corpus.update(&config.suite.count().to_le_bytes());
        corpus.update(&config.seed.to_le_bytes());
        report.phase_times.setup_ms = ms(start);
        Ok(Self {
            lean: server,
            _root: root,
            worker_path,
            workers: (0..config.jobs).map(|_| None).collect(),
            started: vec![false; usize::from(config.jobs)],
            requests: blake3::Hasher::new(),
            responses,
            corpus,
            wire_seq: 0,
            counters: Counters::default(),
            healthy: true,
        })
    }
    fn oracle(
        &mut self,
        case: &Case,
        config: &RunConfig,
        report: &mut RunReport,
        shrink: bool,
    ) -> Result<(OracleAnswer, Vec<u8>, Vec<u8>), HarnessError> {
        let start = Instant::now();
        let request = protocol::encode_request(
            self.wire_seq,
            &case.program,
            config.limits.lean_fuel,
            config.limits.request_bytes,
        )?;
        self.lean.send(request.clone())?;
        let requests = &mut self.requests;
        let responses = &mut self.responses;
        let (request_count, response_count) = if shrink {
            (
                &mut report.shrink_calls.lean_requests,
                &mut report.shrink_calls.lean_responses,
            )
        } else {
            (&mut report.lean_requests, &mut report.lean_responses)
        };
        let response = match self.lean.exchange_sent_observed(
            deadline(config.limits.request_timeout_ms),
            || {
                protocol::hash_frame(requests, &request);
                *request_count += 1;
            },
            |bytes| {
                protocol::hash_frame(responses, bytes);
                *response_count += 1;
            },
        ) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.healthy = false;
                return Err(error);
            }
        };
        let answer = match protocol::decode_answer(self.wire_seq, &response) {
            Ok(a) => a,
            Err(e) => {
                self.healthy = false;
                return Err(e);
            }
        };
        self.counters.record(&answer)?;
        self.wire_seq = self
            .wire_seq
            .checked_add(1)
            .filter(|v| *v <= SEQUENCE_CAP)
            .ok_or_else(|| err(FailureKind::Protocol, "sequence limit"))?;
        if !shrink {
            match &answer {
                OracleAnswer::Outcome {
                    outcome: Outcome::Value(_),
                    ..
                } => report.lean_values += 1,
                OracleAnswer::Outcome {
                    outcome: Outcome::Overflow,
                    ..
                } => report.lean_overflows += 1,
                OracleAnswer::Diagnostic { class, .. } => {
                    report.diagnostic_counts[class.index()] += 1
                }
            }
        }
        report.phase_times.oracle_ms += ms(start);
        Ok((answer, request, response))
    }
    fn start_worker(
        &mut self,
        index: usize,
        case: &Case,
        config: &RunConfig,
        report: &mut RunReport,
    ) -> Result<(), HarnessError> {
        if self.workers[index].is_none() {
            let mut cmd = command(&self.worker_path, None);
            cmd.arg("--worker");
            let session =
                Session::spawn(cmd, FRAME_CAP, config.limits.stderr_bytes, ReadMode::Framed)?;
            report.rust_worker_spawns += 1;
            if self.started[index] {
                report.rust_worker_recycles += 1;
            }
            self.started[index] = true;
            self.workers[index] = Some(WorkerSlot {
                session,
                sequence: 0,
                handled: 0,
            });
        }
        let worker = self.workers[index].as_mut().expect("worker created");
        let request = WorkerRequest {
            worker_seq: worker.sequence,
            case: case.clone(),
            evaluator_fuel: config.limits.evaluator_fuel,
        };
        let bytes =
            serde_json::to_vec(&request).map_err(|e| err(FailureKind::Protocol, e.to_string()))?;
        if bytes.len() > FRAME_CAP {
            return Err(err(FailureKind::Protocol, "worker request cap"));
        }
        worker.session.send(bytes)
    }
    fn receive_worker(
        &mut self,
        index: usize,
        id: u64,
        until: Instant,
        config: &RunConfig,
        report: &mut RunReport,
    ) -> Result<RustAnswer, HarnessError> {
        let start = Instant::now();
        let worker = self.workers[index]
            .as_mut()
            .ok_or_else(|| err(FailureKind::Invariant, "missing worker"))?;
        let bytes = match worker.session.exchange_sent(until) {
            Ok(bytes) => bytes,
            Err(e) => {
                report.interrupted_worker_cases += 1;
                return Err(e);
            }
        };
        let response: WorkerResponse = serde_json::from_slice(&bytes).map_err(|e| {
            report.interrupted_worker_cases += 1;
            err(FailureKind::Protocol, e.to_string())
        })?;
        if response.worker_seq != worker.sequence || response.case_id != id {
            report.interrupted_worker_cases += 1;
            return Err(err(FailureKind::Protocol, "worker response identity"));
        }
        let answer = match response.answer {
            Ok(answer) => answer,
            Err(e) => {
                report.interrupted_worker_cases += 1;
                return Err(e);
            }
        };
        validate_rust_answer(&answer).inspect_err(|_| {
            report.interrupted_worker_cases += 1;
        })?;
        // This response already confirms completed work, even if retirement fails.
        record_rust(&answer, report, false);
        worker.sequence += 1;
        worker.handled += 1;
        report.max_worker_cases = report.max_worker_cases.max(worker.handled);
        if worker.handled == config.limits.worker_cases {
            worker.session.close_input();
            let eof = worker
                .session
                .receive(deadline(config.limits.shutdown_timeout_ms))?;
            if eof.is_some() {
                return Err(err(
                    FailureKind::Protocol,
                    "extra worker response on retirement",
                ));
            }
            worker.session.finish(config.limits.shutdown_timeout_ms)?;
            self.workers[index] = None;
        }
        report.phase_times.workers_ms += ms(start);
        Ok(answer)
    }
    fn commit(&mut self, case: &Case, fingerprint: [u8; 32]) {
        self.corpus.update(&case.id.to_le_bytes());
        self.corpus
            .update(&[u8::from(case.intent == Intent::StaticNegative)]);
        self.corpus.update(&fingerprint);
    }
    fn retire_workers(&mut self, config: &RunConfig) -> Result<(), HarnessError> {
        let mut failure = None;
        for worker in &mut self.workers {
            if let Some(mut slot) = worker.take() {
                slot.session.close_input();
                let result = (|| {
                    if slot
                        .session
                        .receive(deadline(config.limits.shutdown_timeout_ms))?
                        .is_some()
                    {
                        return Err(err(FailureKind::Protocol, "extra worker terminal frame"));
                    }
                    slot.session.finish(config.limits.shutdown_timeout_ms)
                })();
                if let Err(e) = result {
                    failure.get_or_insert(e);
                }
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
    fn finish(&mut self, config: &RunConfig, report: &mut RunReport) -> Result<(), HarnessError> {
        let start = Instant::now();
        let mut failure = self.retire_workers(config).err();
        if self.healthy {
            self.lean.close_input();
            let result = (|| {
                let bytes = self
                    .lean
                    .receive(deadline(config.limits.shutdown_timeout_ms))?
                    .ok_or_else(|| err(FailureKind::Protocol, "missing BYE"))?;
                protocol::hash_frame(&mut self.responses, &bytes);
                protocol::bye(&bytes, self.counters)?;
                if let Some(extra) = self
                    .lean
                    .receive(deadline(config.limits.shutdown_timeout_ms))?
                {
                    protocol::hash_frame(&mut self.responses, &extra);
                    return Err(err(FailureKind::Protocol, "extra Lean terminal frame"));
                }
                self.lean.finish(config.limits.shutdown_timeout_ms)
            })();
            if let Err(e) = result {
                failure.get_or_insert(e);
            }
        } else {
            let _ = self.lean.cleanup(config.limits.shutdown_timeout_ms, true);
        }
        report.phase_times.shutdown_ms = ms(start);
        if let Some(e) = failure {
            Err(e)
        } else {
            Ok(())
        }
    }
    fn hashes(&self, report: &mut RunReport) {
        report.corpus_blake3 = *self.corpus.finalize().as_bytes();
        report.requests_blake3 = *self.requests.finalize().as_bytes();
        report.responses_blake3 = *self.responses.finalize().as_bytes();
    }
}

fn validate_rust_answer(answer: &RustAnswer) -> Result<(), HarnessError> {
    if let RustAnswer::Diagnostic { codes, .. } = answer
        && (codes.is_empty()
            || codes.len() > 16
            || codes.iter().any(|s| s.len() > 16 || !s.is_ascii()))
    {
        return Err(err(FailureKind::Protocol, "worker diagnostic envelope"));
    }
    Ok(())
}
fn record_rust(answer: &RustAnswer, report: &mut RunReport, shrink: bool) {
    let (parser, checker, opt, eval, jit) = if shrink {
        (
            &mut report.shrink_calls.parser_calls,
            &mut report.shrink_calls.checker_calls,
            &mut report.shrink_calls.ast_optimizer_calls,
            &mut report.shrink_calls.evaluator_calls,
            &mut report.shrink_calls.jit_calls,
        )
    } else {
        (
            &mut report.parser_calls,
            &mut report.checker_calls,
            &mut report.ast_optimizer_calls,
            &mut report.evaluator_calls,
            &mut report.jit_calls,
        )
    };
    *parser += 1;
    match answer {
        RustAnswer::Outcomes { .. } => {
            *checker += 2;
            *opt += 1;
            for n in eval.iter_mut().chain(jit.iter_mut()) {
                *n += 1;
            }
        }
        RustAnswer::Diagnostic {
            phase: DiagnosticPhase::Check,
            ..
        } => *checker += 1,
        RustAnswer::Diagnostic {
            phase: DiagnosticPhase::Parse,
            ..
        } => {}
    }
}
fn compare(case: &Case, oracle: &OracleAnswer, rust: &RustAnswer) -> Result<(), HarnessError> {
    match (case.intent, oracle, rust) {
        (
            Intent::Valid,
            OracleAnswer::Outcome { outcome, .. },
            RustAnswer::Outcomes { evaluator, jit },
        ) => {
            if let Outcome::Value(n) = outcome
                && !case.program.kind.fits(*n)
            {
                return Err(err(FailureKind::Protocol, "Lean value kind range"));
            }
            if evaluator.iter().chain(jit.iter()).any(|a| a != outcome) {
                return Err(err(
                    FailureKind::OutcomeMismatch,
                    "backend differs from Lean",
                ));
            }
            Ok(())
        }
        (
            Intent::StaticNegative,
            OracleAnswer::Diagnostic { class, .. },
            RustAnswer::Diagnostic { phase, codes },
        ) => {
            let expected = if *class == DiagnosticClass::LiteralParseRange {
                DiagnosticPhase::Parse
            } else {
                DiagnosticPhase::Check
            };
            if *phase != expected || codes.as_slice() != [class.code()] {
                return Err(err(
                    FailureKind::DiagnosticMismatch,
                    "isolated diagnostic differs from Lean",
                ));
            }
            Ok(())
        }
        _ => Err(err(
            FailureKind::DiagnosticMismatch,
            "case intent or phase disagrees with Lean",
        )),
    }
}
fn coverage(case: &Case, report: &mut RunReport) {
    if case.intent == Intent::Valid {
        report.per_kind_valid[usize::from(case.program.kind.tag())] += 1;
        for n in case
            .program
            .locals
            .iter()
            .chain(std::iter::once(&case.program.result))
            .flatten()
        {
            match n {
                Node::Checked(op) => report.per_op_nodes[usize::from(op.tag())] += 1,
                Node::Call(Callee::Intrinsic(op), _) => {
                    report.per_op_nodes[3 + usize::from(op.tag())] += 1
                }
                _ => {}
            }
        }
    }
}
fn count_failure(error: &HarnessError, report: &mut RunReport) {
    match error.kind {
        FailureKind::Timeout => report.timeouts += 1,
        FailureKind::Crash => report.crashes += 1,
        FailureKind::Protocol | FailureKind::CounterMismatch => report.protocol_errors += 1,
        FailureKind::OutcomeMismatch => report.outcome_mismatches += 1,
        FailureKind::DiagnosticMismatch => report.diagnostic_mismatches += 1,
        _ => {}
    }
}

fn drive(
    config: &RunConfig,
    engine: &mut Engine,
    report: &mut RunReport,
    evidence: &mut Option<Evidence>,
) -> Result<(), HarnessError> {
    let total = config.suite.count();
    let negatives = corpus::negative_cases(total)?;
    if negatives.len() != 15 {
        return Err(err(FailureKind::Invariant, "negative matrix size"));
    }
    let mut fingerprints = Vec::new();
    fingerprints
        .try_reserve_exact(total as usize)
        .map_err(|_| err(FailureKind::Allocation, "fingerprints"))?;
    let mut next = 0u64;
    let mut failure = None;
    while next < total + 15 && failure.is_none() {
        let mut batch = Vec::with_capacity(usize::from(config.jobs));
        for slot in 0..usize::from(config.jobs) {
            if next >= total + 15 {
                break;
            }
            let id = next;
            let case = if id < total {
                corpus::case_at(config.suite, config.seed, id)
            } else {
                Ok(negatives[(id - total) as usize].clone())
            };
            let result = (|| {
                let case = case?;
                if case.id != id {
                    return Err(err(FailureKind::Invariant, "generator identity"));
                }
                case.program.validate()?;
                if corpus::render_case(&case)?.len() > config.limits.source_bytes {
                    return Err(err(
                        FailureKind::UnsupportedProfile,
                        "configured source byte cap",
                    ));
                }
                let fingerprint = corpus::semantic_fingerprint(&case)?;
                if id < total {
                    report.generated_valid += 1;
                    fingerprints.push(fingerprint);
                } else {
                    report.generated_negative += 1;
                }
                let (oracle, request, response) = engine.oracle(&case, config, report, false)?;
                engine.start_worker(slot, &case, config, report)?;
                Ok(Pending {
                    case,
                    oracle,
                    request,
                    response,
                    fingerprint,
                    slot,
                    until: deadline(config.limits.worker_timeout_ms),
                })
            })();
            match result {
                Ok(p) => {
                    batch.push(p);
                    next += 1;
                }
                Err(e) => {
                    report.first_failed_case = Some(id);
                    failure = Some(e);
                    break;
                }
            }
        }
        report.max_in_flight = report.max_in_flight.max(batch.len() as u16);
        for pending in batch {
            if report
                .first_failed_case
                .is_some_and(|id| pending.case.id > id)
            {
                // The lower failure is settled: do not wait on higher JIT work.
                report.cancelled_cases += 1;
                report.interrupted_worker_cases += 1;
                if let Some(mut worker) = engine.workers[pending.slot].take() {
                    let _ = worker
                        .session
                        .cleanup(config.limits.shutdown_timeout_ms, true);
                }
                continue;
            }
            // Always settle lower IDs before selecting the first failure.
            let result =
                engine.receive_worker(pending.slot, pending.case.id, pending.until, config, report);
            let (rust, error) = match result {
                Ok(rust) => {
                    let error = compare(&pending.case, &pending.oracle, &rust).err();
                    (Some(rust), error)
                }
                Err(e) => (None, Some(e)),
            };
            if let Some(error) = error {
                if report
                    .first_failed_case
                    .is_none_or(|id| pending.case.id < id)
                {
                    report.first_failed_case = Some(pending.case.id);
                    failure = Some(error.clone());
                    *evidence = Some(Evidence {
                        case: pending.case,
                        request: pending.request,
                        response: pending.response,
                        rust,
                        error,
                        shrunken: None,
                        shrink_log: Vec::new(),
                    });
                }
            } else if report
                .first_failed_case
                .is_none_or(|id| pending.case.id < id)
            {
                engine.commit(&pending.case, pending.fingerprint);
                coverage(&pending.case, report);
                if pending.case.intent == Intent::Valid {
                    report.committed_valid += 1;
                } else {
                    report.matched_negative += 1;
                }
            } else {
                report.cancelled_cases += 1;
            }
        }
    }
    fingerprints.sort_unstable();
    report.unique_valid = fingerprints
        .iter()
        .enumerate()
        .filter(|(i, h)| *i == 0 || fingerprints[*i - 1] != **h)
        .count() as u64;
    if report.unique_valid != report.generated_valid && failure.is_none() {
        failure = Some(err(FailureKind::Invariant, "duplicate corpus fingerprint"));
    }
    if let Some(e) = failure {
        Err(e)
    } else {
        Ok(())
    }
}

fn shrink(
    config: &RunConfig,
    engine: &mut Engine,
    report: &mut RunReport,
    evidence: &mut Evidence,
) -> Result<(), HarnessError> {
    if config.limits.shrink_attempts == 0
        || !engine.healthy
        || !matches!(
            evidence.error.kind,
            FailureKind::OutcomeMismatch | FailureKind::DiagnosticMismatch
        )
    {
        return Ok(());
    }
    let start = Instant::now();
    let candidates = corpus::shrink_candidates(&evidence.case)?;
    if candidates.len() > 64 {
        return Err(err(FailureKind::Invariant, "shrink candidate cap"));
    }
    // No fresh shrink child can exceed the configured live-worker bound.
    engine.retire_workers(config)?;
    for mut case in candidates
        .into_iter()
        .take(usize::from(config.limits.shrink_attempts))
    {
        case.id = evidence.case.id;
        case.program.validate()?;
        if corpus::render_case(&case)?.len() > config.limits.source_bytes {
            return Err(err(
                FailureKind::UnsupportedProfile,
                "configured shrink source byte cap",
            ));
        }
        report.shrink_attempts += 1;
        evidence
            .shrink_log
            .push(serde_json::json!({"attempt":report.shrink_attempts,
            "fingerprint":corpus::semantic_fingerprint(&case)?,"status":"started",
            "worker_spawned":false}));
        let index = evidence.shrink_log.len() - 1;
        let mut spawned = false;
        let mut confirmed = false;
        let result = (|| {
            let (oracle, request, response) = engine.oracle(&case, config, report, true)?;
            evidence.shrink_log[index]["status"] = serde_json::json!("oracle_answered");
            let mut cmd = command(&engine.worker_path, None);
            cmd.arg("--worker");
            let mut child =
                Session::spawn(cmd, FRAME_CAP, config.limits.stderr_bytes, ReadMode::Framed)?;
            spawned = true;
            report.rust_worker_spawns += 1;
            evidence.shrink_log[index]["worker_spawned"] = serde_json::json!(true);
            let payload = serde_json::to_vec(&WorkerRequest {
                worker_seq: 0,
                case: case.clone(),
                evaluator_fuel: config.limits.evaluator_fuel,
            })
            .map_err(|e| err(FailureKind::Protocol, e.to_string()))?;
            let bytes = child.exchange(payload, deadline(config.limits.worker_timeout_ms))?;
            let reply: WorkerResponse = serde_json::from_slice(&bytes)
                .map_err(|e| err(FailureKind::Protocol, e.to_string()))?;
            if reply.worker_seq != 0 || reply.case_id != case.id {
                return Err(err(FailureKind::Protocol, "shrink worker identity"));
            }
            let rust = reply.answer?;
            validate_rust_answer(&rust)?;
            record_rust(&rust, report, true);
            confirmed = true;
            report.max_worker_cases = report.max_worker_cases.max(1);
            child.close_input();
            if child
                .receive(deadline(config.limits.shutdown_timeout_ms))?
                .is_some()
            {
                return Err(err(FailureKind::Protocol, "shrink extra response"));
            }
            child.finish(config.limits.shutdown_timeout_ms)?;
            let mismatch = compare(&case, &oracle, &rust).err();
            evidence.shrink_log[index]["status"] = serde_json::json!("completed");
            evidence.shrink_log[index]["mismatch"] =
                serde_json::json!(mismatch.as_ref().map(|e| e.kind));
            Ok((request, response, mismatch))
        })();
        match result {
            Ok((request, response, mismatch)) => {
                if mismatch.is_some_and(|e| e.kind == evidence.error.kind) {
                    evidence.shrunken = Some((case, request, response));
                    break;
                }
            }
            Err(error) => {
                if spawned && !confirmed {
                    report.interrupted_worker_cases += 1;
                }
                evidence.shrink_log[index]["status"] = serde_json::json!("failed");
                evidence.shrink_log[index]["error"] = serde_json::to_value(&error)
                    .map_err(|e| err(FailureKind::Artifact, e.to_string()))?;
                report.phase_times.shrink_ms = ms(start);
                return Err(error);
            }
        }
    }
    report.phase_times.shrink_ms = ms(start);
    Ok(())
}

fn artifact_bytes(
    config: &RunConfig,
    report: &mut RunReport,
    evidence: Option<&Evidence>,
    failure: Option<&HarnessError>,
) -> Result<(), HarnessError> {
    let Some(parent) = &config.artifacts else {
        return Ok(());
    };
    // Build the complete bounded publication plan before touching the destination.
    let mut plan: Vec<(&str, Vec<u8>)> = Vec::with_capacity(12);
    let mut used = 0usize;
    let mut metadata = Vec::with_capacity(12);
    let mut put = |name: &'static str, bytes: Vec<u8>| -> Result<(), HarnessError> {
        if bytes.len() > config.limits.artifact_bytes.saturating_sub(used)
            || name.len() > 256
            || metadata.len() >= 16
        {
            return Err(err(FailureKind::Artifact, "artifact quota"));
        }
        used += bytes.len();
        metadata.push(ArtifactInfo {
            relative_path: name.into(),
            bytes: bytes.len() as u64,
            blake3: *blake3::hash(&bytes).as_bytes(),
        });
        plan.push((name, bytes));
        Ok(())
    };
    if let Some(evidence) = evidence {
        put(
            "original.vs",
            corpus::render_case(&evidence.case)?.into_bytes(),
        )?;
        put(
            "case.json",
            serde_json::to_vec(&evidence.case)
                .map_err(|e| err(FailureKind::Artifact, e.to_string()))?,
        )?;
        let framed = |bytes: &[u8]| {
            let mut framed = Vec::with_capacity(4 + bytes.len());
            framed.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            framed.extend_from_slice(bytes);
            framed
        };
        put("request.frame", framed(&evidence.request))?;
        put("response.frame", framed(&evidence.response))?;
        put(
            "difference.json",
            serde_json::to_vec(&(&evidence.error, &evidence.rust))
                .map_err(|e| err(FailureKind::Artifact, e.to_string()))?,
        )?;
        if let Some((case, request, response)) = &evidence.shrunken {
            put("shrunken.vs", corpus::render_case(case)?.into_bytes())?;
            put("shrunken-request.frame", framed(request))?;
            put("shrunken-response.frame", framed(response))?;
        }
        if !evidence.shrink_log.is_empty() {
            put(
                "shrink-log.json",
                serde_json::to_vec(&evidence.shrink_log)
                    .map_err(|e| err(FailureKind::Artifact, e.to_string()))?,
            )?;
        }
    }
    put(
        "config.json",
        serde_json::to_vec(&(config.suite, config.seed, config.jobs, &config.limits))
            .map_err(|e| err(FailureKind::Artifact, e.to_string()))?,
    )?;
    // Snapshot includes content inventory, but excludes the summary's own hash.
    let mut snapshot = report.clone();
    snapshot.artifacts = metadata.clone();
    let (name, summary) = if let Some(error) = failure {
        (
            "failure.json",
            serde_json::to_vec(&RunFailure {
                error: error.clone(),
                case_id: snapshot.first_failed_case,
                partial: Box::new(snapshot),
            }),
        )
    } else {
        ("report.json", serde_json::to_vec(&snapshot))
    };
    let summary = summary.map_err(|e| err(FailureKind::Artifact, e.to_string()))?;
    if summary.len() > FRAME_CAP
        || summary.len() > config.limits.artifact_bytes.saturating_sub(used)
    {
        return Err(err(FailureKind::Artifact, "complete artifact quota"));
    }
    plan.push((name, summary));
    let root = TempRoot::new(parent, "oracle42-run")?;
    // The unique output directory is intentionally retained, including on an I/O failure.
    let path = root.0.clone();
    std::mem::forget(root);
    for (name, bytes) in plan {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path.join(name))
            .map_err(|e| err(FailureKind::Artifact, e.to_string()))?;
        file.write_all(&bytes)
            .map_err(|e| err(FailureKind::Artifact, e.to_string()))?;
    }
    report.artifacts = metadata;
    Ok(())
}

pub fn run(config: &RunConfig) -> Result<RunReport, RunFailure> {
    let start = Instant::now();
    let mut report = RunReport::new(config.suite, config.seed, config.jobs);
    let mut evidence = None;
    let mut engine = match Engine::new(config, &mut report) {
        Ok(engine) => engine,
        Err(error) => {
            report.status = RunStatus::Failed;
            report.elapsed_ms = ms(start);
            count_failure(&error, &mut report);
            return Err(RunFailure {
                error,
                case_id: report.first_failed_case,
                partial: Box::new(report),
            });
        }
    };
    let mut failure = drive(config, &mut engine, &mut report, &mut evidence).err();
    if let Some(item) = &mut evidence
        && let Err(error) = shrink(config, &mut engine, &mut report, item)
        && failure.is_none()
    {
        failure = Some(error);
    }
    if let Err(error) = engine.finish(config, &mut report)
        && failure.is_none()
    {
        failure = Some(error);
    }
    engine.hashes(&mut report);
    report.elapsed_ms = ms(start);
    if failure.is_none()
        && (report.committed_valid != config.suite.count()
            || report.unique_valid != config.suite.count()
            || report.matched_negative != 15
            || report.evaluator_calls != [config.suite.count(); 4]
            || report.jit_calls != [config.suite.count(); 4]
            || report.lean_spawns != 1)
    {
        failure = Some(err(FailureKind::CounterMismatch, "completion counts"));
    }
    report.status = if failure.is_some() {
        RunStatus::Failed
    } else {
        RunStatus::Complete
    };
    if let Some(error) = &failure {
        count_failure(error, &mut report);
    }
    if let Err(error) = artifact_bytes(config, &mut report, evidence.as_ref(), failure.as_ref()) {
        failure = Some(error);
        report.status = RunStatus::Failed;
    }
    match failure {
        Some(error) => Err(RunFailure {
            error,
            case_id: report.first_failed_case,
            partial: Box::new(report),
        }),
        None => Ok(report),
    }
}

#[cfg(test)]
pub mod testing {
    use super::*;
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ProbeKind {
        Malformed,
        Oversize,
        TruncatedHeader,
        TruncatedPayload,
        Stall,
        StderrFlood,
        Reordered,
        BadBye,
        ShortWrites,
        Crash,
    }
    #[derive(Debug, Clone)]
    pub struct ProbeLimits {
        pub frame_bytes: usize,
        pub stderr_bytes: usize,
        pub timeout_ms: u64,
        pub shutdown_timeout_ms: u64,
    }
    #[derive(Debug, Clone)]
    pub struct TransportProbeReport {
        pub kind: ProbeKind,
        pub observed: Option<FailureKind>,
        pub child_reaped: bool,
        pub reader_joined: bool,
        pub writer_joined: bool,
        pub stderr_joined: bool,
        pub max_frame_bytes: usize,
        pub stderr_bytes: usize,
    }
    pub fn probe(
        command: Command,
        kind: ProbeKind,
        limits: ProbeLimits,
    ) -> Result<TransportProbeReport, HarnessError> {
        if limits.frame_bytes == 0
            || limits.frame_bytes > FRAME_CAP
            || limits.stderr_bytes == 0
            || limits.stderr_bytes > FRAME_CAP
            || limits.timeout_ms == 0
            || limits.timeout_ms > 120000
            || limits.shutdown_timeout_ms == 0
            || limits.shutdown_timeout_ms > 5000
        {
            return Err(err(FailureKind::InvalidConfig, "probe limits"));
        }
        let mut session = Session::spawn(
            command,
            limits.frame_bytes,
            limits.stderr_bytes,
            ReadMode::Framed,
        )?;
        let result = (|| {
            let bytes = session
                .receive(deadline(limits.timeout_ms))?
                .ok_or_else(|| err(FailureKind::Crash, "fault peer EOF"))?;
            match kind {
                ProbeKind::ShortWrites => {
                    if bytes != MARKER {
                        return Err(err(FailureKind::Protocol, "transport marker mismatch"));
                    }
                }
                ProbeKind::BadBye => protocol::bye(&bytes, Counters::default())?,
                _ => {
                    protocol::decode_answer(0, &bytes)?;
                    return Err(err(
                        FailureKind::Invariant,
                        "fault peer emitted accepted answer",
                    ));
                }
            }
            Ok(())
        })();
        let mut observed = result.err().map(|e| e.kind);
        let cleanup = session.cleanup(limits.shutdown_timeout_ms, observed.is_some())?;
        if observed.is_none() {
            if cleanup.killed {
                observed = Some(FailureKind::Timeout);
            } else if !cleanup.status.is_some_and(|s| s.success()) {
                observed = Some(FailureKind::Crash);
            } else if let Err(e) = session.stderr_fault() {
                observed = Some(e.kind);
            }
        }
        if observed.is_none() && kind != ProbeKind::ShortWrites {
            return Err(err(FailureKind::Invariant, "non-marker probe succeeded"));
        }
        let stderr_bytes = session
            .stderr_state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .bytes
            .len();
        Ok(TransportProbeReport {
            kind,
            observed,
            child_reaped: cleanup.status.is_some(),
            reader_joined: cleanup.reader,
            writer_joined: cleanup.writer,
            stderr_joined: cleanup.stderr,
            max_frame_bytes: session.peak.load(Ordering::Relaxed),
            stderr_bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_artifact_quota_is_checked_before_destination_initialization() {
        let root = TempRoot::new(&std::env::temp_dir(), "oracle42-artifact-test").unwrap();
        let output = root.0.join("must-not-be-created");
        let mut config = RunConfig {
            suite: Suite::Ci128,
            seed: 0,
            jobs: 1,
            lean: "unused".into(),
            worker_exe: "unused".into(),
            artifacts: Some(output.clone()),
            limits: HarnessLimits::default(),
        };
        // This permits config.json alone, but excludes the complete summary.
        config.limits.artifact_bytes = 1024;
        let mut report = RunReport::default();
        let failure = artifact_bytes(&config, &mut report, None, None).unwrap_err();
        assert_eq!(failure.kind, FailureKind::Artifact);
        assert!(!output.exists());
        assert!(report.artifacts.is_empty());
    }
}
