//! Bounded supervision of the fixed, original executable Lean resource model.
//! A successful session attests transport and artifacts, not source/native refinement.
use super::protocol::{self, ModelResponse, Request, ServerMessage, Stage};
use serde::Serialize;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
    mpsc::{self, Receiver, SyncSender},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SOURCES: [(&str, &str); 5] = [
    (
        "MetatheoryFull.lean",
        include_str!("../../../lean/MetatheoryFull.lean"),
    ),
    (
        "ResourceModel42/Syntax.lean",
        include_str!("../../../lean/ResourceModel42/Syntax.lean"),
    ),
    (
        "ResourceModel42/Checker.lean",
        include_str!("../../../lean/ResourceModel42/Checker.lean"),
    ),
    (
        "ResourceModel42/Evaluator.lean",
        include_str!("../../../lean/ResourceModel42/Evaluator.lean"),
    ),
    (
        "ResourceOracle42.lean",
        include_str!("../../../lean/ResourceOracle42.lean"),
    ),
];
const MAX_FILES: usize = 128;
const DETAIL_CAP: usize = 1024;
static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);
static ABANDONED: AtomicU64 = AtomicU64::new(0);
static CLEANUP_FAILURES: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessErrorKind {
    InvalidConfig,
    SourceIdentity,
    ToolIdentity,
    ArtifactIdentity,
    ArtifactLimit,
    Io,
    Timeout,
    Protocol,
    Sequence,
    CounterMismatch,
    Quota,
    Crash,
    Abandoned,
    SyntheticPeer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HarnessError {
    pub kind: HarnessErrorKind,
    pub detail: String,
}
impl HarnessError {
    fn new(kind: HarnessErrorKind, detail: impl AsRef<str>) -> Self {
        let mut detail = detail.as_ref().to_owned();
        if detail.len() > DETAIL_CAP {
            let mut boundary = DETAIL_CAP;
            while !detail.is_char_boundary(boundary) {
                boundary -= 1;
            }
            detail.truncate(boundary);
        }
        Self { kind, detail }
    }
}
impl std::fmt::Display for HarnessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.detail)
    }
}
impl std::error::Error for HarnessError {}
impl From<std::io::Error> for HarnessError {
    fn from(error: std::io::Error) -> Self {
        Self::new(HarnessErrorKind::Io, error.to_string())
    }
}
impl From<protocol::WireError> for HarnessError {
    fn from(error: protocol::WireError) -> Self {
        Self::new(HarnessErrorKind::Protocol, error.to_string())
    }
}
fn failure(kind: HarnessErrorKind, detail: &str) -> HarnessError {
    HarnessError::new(kind, detail)
}
fn until(ms: u64) -> Instant {
    Instant::now() + Duration::from_millis(ms)
}

#[derive(Debug, Clone, Serialize)]
pub struct HarnessLimits {
    pub compile_timeout_ms: u64,
    pub request_timeout_ms: u64,
    pub shutdown_timeout_ms: u64,
    pub max_requests: u64,
    pub session_wire_bytes: u64,
    pub stderr_bytes: usize,
    pub compiler_stdout_bytes: usize,
    pub artifact_file_bytes: u64,
    pub artifact_total_bytes: u64,
}
impl Default for HarnessLimits {
    fn default() -> Self {
        Self {
            compile_timeout_ms: 120_000,
            request_timeout_ms: 30_000,
            shutdown_timeout_ms: 5_000,
            max_requests: 64,
            session_wire_bytes: protocol::MAX_SESSION_BYTES,
            stderr_bytes: 262_144,
            compiler_stdout_bytes: 262_144,
            artifact_file_bytes: 67_108_864,
            artifact_total_bytes: 268_435_456,
        }
    }
}
impl HarnessLimits {
    fn validate(&self) -> Result<(), HarnessError> {
        if self.compile_timeout_ms == 0
            || self.compile_timeout_ms > 180_000
            || self.request_timeout_ms == 0
            || self.request_timeout_ms > 60_000
            || self.shutdown_timeout_ms == 0
            || self.shutdown_timeout_ms > 15_000
            || self.max_requests > protocol::MAX_REQUESTS
            || self.session_wire_bytes > protocol::MAX_SESSION_BYTES
            || self.stderr_bytes > 1_048_576
            || self.compiler_stdout_bytes > 1_048_576
            || self.artifact_file_bytes > 268_435_456
            || self.artifact_total_bytes > 1_073_741_824
        {
            return Err(failure(
                HarnessErrorKind::InvalidConfig,
                "outer limit exceeds fixed hard bound",
            ));
        }
        Ok(())
    }
}
#[derive(Debug, Clone)]
pub struct Config {
    pub lean: PathBuf,
    pub repo_root: PathBuf,
    pub artifact_dir: Option<PathBuf>,
    pub limits: HarnessLimits,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileIdentity {
    pub path: PathBuf,
    pub bytes: u64,
    pub blake3: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct ToolIdentity {
    pub path: PathBuf,
    pub prefix: PathBuf,
    pub bytes: u64,
    pub blake3: String,
    pub version: String,
    pub commit: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct CommandEvidence {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub lean_path: Option<OsString>,
    pub exit_code: Option<i32>,
    pub stdout_bytes: usize,
    pub stdout_blake3: String,
    pub stderr_bytes: usize,
    pub stderr_blake3: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct SessionEvidence {
    pub completed: bool,
    pub attestation_scope: &'static str,
    pub toolchain_dependencies: &'static str,
    pub artifact_root: PathBuf,
    pub artifacts: Vec<FileIdentity>,
    pub tool: ToolIdentity,
    pub commands: Vec<CommandEvidence>,
    pub requests: u64,
    pub counters: [u64; 7],
    pub framed_bytes: u64,
    pub transcript_blake3: String,
}

/// A drop without consuming `finish` or `abort` is recorded, never a completion.
pub fn abandoned_sessions() -> u64 {
    ABANDONED.load(Ordering::Relaxed)
}
/// Failed destructor cleanup attempts remain observable; none are completions.
pub fn cleanup_failures() -> u64 {
    CLEANUP_FAILURES.load(Ordering::Relaxed)
}

struct Root {
    path: PathBuf,
    retain: bool,
}
impl Root {
    fn create(parent: &Path, retain: bool) -> Result<Self, HarnessError> {
        fs::create_dir_all(parent)?;
        for _ in 0..32 {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = parent.join(format!(
                "resource-oracle42-{}-{stamp}-{}",
                std::process::id(),
                NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
            ));
            #[cfg(unix)]
            let builder = {
                use std::os::unix::fs::DirBuilderExt;
                let mut builder = fs::DirBuilder::new();
                builder.mode(0o700);
                builder
            };
            #[cfg(not(unix))]
            let builder = fs::DirBuilder::new();
            match builder.create(&path) {
                Ok(()) => return Ok(Self { path, retain }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(failure(
            HarnessErrorKind::Io,
            "private directory allocation exhausted",
        ))
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        if !self.retain {
            make_removable(&self.path);
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn make_removable(root: &Path) {
    let mut directories = vec![root.to_path_buf()];
    let mut visited = 0usize;
    while let Some(directory) = directories.pop() {
        visited += 1;
        if visited > MAX_FILES {
            break;
        }
        if let Ok(metadata) = fs::symlink_metadata(&directory) {
            if !metadata.is_dir() {
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&directory, fs::Permissions::from_mode(0o700));
            }
            #[cfg(windows)]
            {
                let mut permissions = metadata.permissions();
                #[expect(
                    clippy::permissions_set_readonly_false,
                    reason = "On Windows this clears only the read-only attribute; ACL permissions are unchanged."
                )]
                permissions.set_readonly(false);
                let _ = fs::set_permissions(&directory, permissions);
            }
            if let Ok(entries) = fs::read_dir(&directory) {
                for entry in entries.take(MAX_FILES).flatten() {
                    if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                        directories.push(entry.path());
                    }
                    #[cfg(windows)]
                    if let Ok(metadata) = entry.metadata() {
                        let mut permissions = metadata.permissions();
                        #[expect(
                            clippy::permissions_set_readonly_false,
                            reason = "On Windows this clears only the read-only attribute; ACL permissions are unchanged."
                        )]
                        permissions.set_readonly(false);
                        let _ = fs::set_permissions(entry.path(), permissions);
                    }
                }
            }
        }
    }
}

fn digest_file(path: &Path, cap: u64) -> Result<FileIdentity, HarnessError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(failure(
            HarnessErrorKind::ArtifactIdentity,
            "artifact is not a regular file",
        ));
    }
    if metadata.len() > cap {
        return Err(failure(
            HarnessErrorKind::ArtifactLimit,
            "artifact file limit",
        ));
    }
    let mut file = File::open(path)?;
    let mut hash = blake3::Hasher::new();
    let mut count = 0u64;
    let mut buffer = [0u8; 16_384];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        count = count
            .checked_add(n as u64)
            .ok_or_else(|| failure(HarnessErrorKind::ArtifactLimit, "artifact size overflow"))?;
        if count > cap {
            return Err(failure(
                HarnessErrorKind::ArtifactLimit,
                "artifact grew beyond limit",
            ));
        }
        hash.update(&buffer[..n]);
    }
    if count != metadata.len() {
        return Err(failure(
            HarnessErrorKind::ArtifactIdentity,
            "artifact changed while hashing",
        ));
    }
    Ok(FileIdentity {
        path: path.to_path_buf(),
        bytes: count,
        blake3: hash.finalize().to_hex().to_string(),
    })
}
fn verify_fixed_sources(directory: &Path) -> Result<(), HarnessError> {
    for (relative, embedded) in SOURCES {
        let path = directory.join(relative);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|_| failure(HarnessErrorKind::SourceIdentity, "fixed source missing"))?;
        if !metadata.file_type().is_file() || metadata.len() != embedded.len() as u64 {
            return Err(failure(
                HarnessErrorKind::SourceIdentity,
                "fixed source must be regular and exact length",
            ));
        }
        let mut bytes = Vec::new();
        File::open(path)?
            .take(embedded.len() as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes != embedded.as_bytes() {
            return Err(failure(
                HarnessErrorKind::SourceIdentity,
                "fixed source differs from embedded bytes",
            ));
        }
    }
    Ok(())
}
fn verify_sources(repo: &Path) -> Result<(), HarnessError> {
    verify_fixed_sources(&repo.join("lean"))
}
fn inventory(root: &Path, limits: &HarnessLimits) -> Result<Vec<FileIdentity>, HarnessError> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    let mut total = 0u64;
    let mut entries = 0usize;
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            entries += 1;
            if entries > MAX_FILES {
                return Err(failure(
                    HarnessErrorKind::ArtifactLimit,
                    "artifact entry count",
                ));
            }
            let ty = entry.file_type()?;
            if ty.is_dir() {
                pending.push(entry.path());
            } else if ty.is_file() {
                let mut identity = digest_file(&entry.path(), limits.artifact_file_bytes)?;
                total = total.checked_add(identity.bytes).ok_or_else(|| {
                    failure(HarnessErrorKind::ArtifactLimit, "artifact total overflow")
                })?;
                if total > limits.artifact_total_bytes {
                    return Err(failure(
                        HarnessErrorKind::ArtifactLimit,
                        "artifact total limit",
                    ));
                }
                identity.path = identity
                    .path
                    .strip_prefix(root)
                    .map_err(|_| {
                        failure(
                            HarnessErrorKind::ArtifactIdentity,
                            "artifact escaped private root",
                        )
                    })?
                    .to_path_buf();
                files.push(identity);
            } else {
                return Err(failure(
                    HarnessErrorKind::ArtifactIdentity,
                    "nonregular private artifact",
                ));
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}
fn check_artifact_sizes(root: &Path, limits: &HarnessLimits) -> Result<(), HarnessError> {
    let mut pending = vec![root.to_path_buf()];
    let mut total = 0u64;
    let mut entries = 0usize;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            entries += 1;
            if entries > MAX_FILES {
                return Err(failure(
                    HarnessErrorKind::ArtifactLimit,
                    "artifact entry count during compilation",
                ));
            }
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            let ty = match entry.file_type() {
                Ok(ty) => ty,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            if ty.is_dir() {
                pending.push(entry.path());
            } else if ty.is_file() {
                if metadata.len() > limits.artifact_file_bytes {
                    return Err(failure(
                        HarnessErrorKind::ArtifactLimit,
                        "artifact file limit during compilation",
                    ));
                }
                total = total.checked_add(metadata.len()).ok_or_else(|| {
                    failure(HarnessErrorKind::ArtifactLimit, "artifact total overflow")
                })?;
                if total > limits.artifact_total_bytes {
                    return Err(failure(
                        HarnessErrorKind::ArtifactLimit,
                        "artifact total limit during compilation",
                    ));
                }
            } else {
                return Err(failure(
                    HarnessErrorKind::ArtifactIdentity,
                    "nonregular compilation artifact",
                ));
            }
        }
    }
    Ok(())
}
fn clean_command(program: &Path, cwd: Option<&Path>) -> Command {
    let mut command = Command::new(program);
    command.env_clear();
    for name in [
        "PATH",
        "SystemRoot",
        "SYSTEMROOT",
        "WINDIR",
        "COMSPEC",
        "PATHEXT",
        "TEMP",
        "TMP",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command.env("LEAN_NUM_THREADS", "1");
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    command
}
fn invocation_path(path: &Path) -> Result<PathBuf, HarnessError> {
    let candidates = if path.components().count() == 1 && !path.is_absolute() {
        let search = std::env::var_os("PATH")
            .ok_or_else(|| failure(HarnessErrorKind::ToolIdentity, "PATH missing"))?;
        std::env::split_paths(&search)
            .map(|dir| dir.join(path))
            .collect::<Vec<_>>()
    } else {
        vec![path.to_path_buf()]
    };
    for candidate in candidates {
        #[cfg(windows)]
        let candidate = if candidate.extension().is_none() {
            candidate.with_extension("exe")
        } else {
            candidate
        };
        if candidate.is_file() {
            return Ok(if candidate.is_absolute() {
                candidate
            } else {
                std::env::current_dir()?.join(candidate)
            });
        }
    }
    Err(failure(
        HarnessErrorKind::ToolIdentity,
        "Lean executable unavailable",
    ))
}

enum Event {
    Frame(Instant, Result<Option<Vec<u8>>, HarnessError>),
    Written(Instant, Result<(), HarnessError>),
}
#[derive(Default)]
struct Stderr {
    bytes: Vec<u8>,
    overflow: bool,
    error: bool,
}
struct Transport {
    child: Child,
    tx: Option<SyncSender<Vec<u8>>>,
    rx: Option<Receiver<Event>>,
    threads: Vec<JoinHandle<()>>,
    stderr: Arc<Mutex<Stderr>>,
    closed: bool,
    shutdown_ms: u64,
    artifact_watch: Option<(PathBuf, HarnessLimits)>,
}
impl Transport {
    fn spawn(
        mut command: Command,
        framed: bool,
        stdout_cap: usize,
        limits: &HarnessLimits,
    ) -> Result<Self, HarnessError> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| failure(HarnessErrorKind::Io, "stdout pipe unavailable"))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| failure(HarnessErrorKind::Io, "stdin pipe unavailable"))?;
        let mut stderr_pipe = child
            .stderr
            .take()
            .ok_or_else(|| failure(HarnessErrorKind::Io, "stderr pipe unavailable"))?;
        let (events, rx) = mpsc::sync_channel(4);
        let (tx, writes) = mpsc::sync_channel::<Vec<u8>>(1);
        let reader_events = events.clone();
        let reader = thread::spawn(move || {
            if framed {
                loop {
                    let frame =
                        protocol::read_frame(&mut stdout, stdout_cap).map_err(HarnessError::from);
                    let terminal = !matches!(frame, Ok(Some(_)));
                    if reader_events
                        .send(Event::Frame(Instant::now(), frame))
                        .is_err()
                        || terminal
                    {
                        break;
                    }
                }
            } else {
                let mut bytes = Vec::new();
                let mut buffer = [0u8; 4096];
                let answer = loop {
                    match stdout.read(&mut buffer) {
                        Ok(0) => break Ok(Some(bytes)),
                        Ok(n) if n <= stdout_cap.saturating_sub(bytes.len()) => {
                            bytes.extend_from_slice(&buffer[..n])
                        }
                        Ok(_) => {
                            break Err(failure(HarnessErrorKind::Quota, "compiler stdout limit"));
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(error) => break Err(error.into()),
                    }
                };
                let _ = reader_events.send(Event::Frame(Instant::now(), answer));
            }
        });
        let writer = thread::spawn(move || {
            while let Ok(bytes) = writes.recv() {
                let answer = protocol::write_frame(&mut stdin, &bytes, protocol::REQUEST_CAP)
                    .map_err(HarnessError::from);
                let terminal = answer.is_err();
                if events.send(Event::Written(Instant::now(), answer)).is_err() || terminal {
                    break;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(Stderr::default()));
        let state = Arc::clone(&stderr);
        let stderr_cap = limits.stderr_bytes;
        let error_reader = thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            loop {
                match stderr_pipe.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                        let keep = n.min(stderr_cap.saturating_sub(state.bytes.len()));
                        state.bytes.extend_from_slice(&buffer[..keep]);
                        if keep != n {
                            state.overflow = true;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => {
                        state.lock().unwrap_or_else(|e| e.into_inner()).error = true;
                        break;
                    }
                }
            }
        });
        Ok(Self {
            child,
            tx: Some(tx),
            rx: Some(rx),
            threads: vec![reader, writer, error_reader],
            stderr,
            closed: false,
            shutdown_ms: limits.shutdown_timeout_ms,
            artifact_watch: None,
        })
    }
    fn check_stderr(&self) -> Result<(), HarnessError> {
        let state = self.stderr.lock().unwrap_or_else(|e| e.into_inner());
        if state.overflow {
            return Err(failure(HarnessErrorKind::Quota, "stderr limit"));
        }
        if state.error {
            return Err(failure(HarnessErrorKind::Io, "stderr read failure"));
        }
        Ok(())
    }
    fn event(&mut self, deadline: Instant) -> Result<Event, HarnessError> {
        loop {
            self.check_stderr()?;
            if let Some((root, limits)) = &self.artifact_watch {
                check_artifact_sizes(root, limits)?;
            }
            let receiver = self
                .rx
                .as_ref()
                .ok_or_else(|| failure(HarnessErrorKind::Io, "transport closed"))?;
            let event = match receiver.try_recv() {
                Ok(event) => Some(event),
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(failure(
                        HarnessErrorKind::Crash,
                        "worker pipes disconnected",
                    ));
                }
                Err(mpsc::TryRecvError::Empty) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return Err(failure(HarnessErrorKind::Timeout, "worker deadline"));
                    }
                    match receiver.recv_timeout((deadline - now).min(Duration::from_millis(10))) {
                        Ok(event) => Some(event),
                        Err(mpsc::RecvTimeoutError::Timeout) => None,
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            return Err(failure(
                                HarnessErrorKind::Crash,
                                "worker pipes disconnected",
                            ));
                        }
                    }
                }
            };
            if let Some(event) = event {
                let at = match &event {
                    Event::Frame(at, _) | Event::Written(at, _) => *at,
                };
                if at > deadline {
                    return Err(failure(
                        HarnessErrorKind::Timeout,
                        "worker event completed after deadline",
                    ));
                }
                return Ok(event);
            }
        }
    }
    fn receive(&mut self, deadline: Instant) -> Result<Option<Vec<u8>>, HarnessError> {
        match self.event(deadline)? {
            Event::Frame(_, bytes) => bytes,
            Event::Written(_, _) => Err(failure(
                HarnessErrorKind::Protocol,
                "unexpected write acknowledgement",
            )),
        }
    }
    fn exchange(&mut self, bytes: Vec<u8>, deadline: Instant) -> Result<Vec<u8>, HarnessError> {
        match self
            .rx
            .as_ref()
            .ok_or_else(|| failure(HarnessErrorKind::Io, "transport closed"))?
            .try_recv()
        {
            Ok(_) => {
                return Err(failure(
                    HarnessErrorKind::Protocol,
                    "unsolicited worker output",
                ));
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err(failure(
                    HarnessErrorKind::Crash,
                    "worker ended before request",
                ));
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        self.tx
            .as_ref()
            .ok_or_else(|| failure(HarnessErrorKind::Io, "worker input closed"))?
            .try_send(bytes)
            .map_err(|_| {
                failure(
                    HarnessErrorKind::Protocol,
                    "bounded write queue unavailable",
                )
            })?;
        let mut response = None;
        let mut written = false;
        while !written || response.is_none() {
            match self.event(deadline)? {
                Event::Written(_, result) => {
                    result?;
                    if written {
                        return Err(failure(
                            HarnessErrorKind::Protocol,
                            "duplicate write acknowledgement",
                        ));
                    }
                    written = true;
                }
                Event::Frame(_, result) => {
                    let frame = result?
                        .ok_or_else(|| failure(HarnessErrorKind::Crash, "EOF during request"))?;
                    if response.replace(frame).is_some() {
                        return Err(failure(HarnessErrorKind::Protocol, "extra worker response"));
                    }
                }
            }
        }
        response.ok_or_else(|| failure(HarnessErrorKind::Protocol, "missing worker response"))
    }
    fn close_input(&mut self) {
        self.tx.take();
    }
    fn cleanup(&mut self, deadline: Instant, force: bool) -> Result<ExitStatus, HarnessError> {
        if self.closed {
            return Err(failure(HarnessErrorKind::Io, "transport already closed"));
        }
        self.close_input();
        // Graceful exit and forced cleanup share one deadline. Reserve half
        // the remaining budget for killing, polling/reaping, and pipe joins;
        // waiting until the hard deadline to send the kill leaves no time to
        // perform that hygiene.
        let now = Instant::now();
        let kill_at = now + deadline.saturating_duration_since(now) / 2;
        if force {
            let _ = self.child.kill();
        }
        let mut killed = force;
        let status = loop {
            if let Some(status) = self.child.try_wait()? {
                break Some(status);
            }
            let now = Instant::now();
            if !killed && now >= kill_at {
                let _ = self.child.kill();
                killed = true;
                continue;
            }
            if now >= deadline {
                break None;
            }
            thread::sleep((deadline - now).min(Duration::from_millis(2)));
        };
        self.rx.take();
        let mut joined = true;
        for handle in self.threads.drain(..) {
            while !handle.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(2));
            }
            if handle.is_finished() {
                joined &= handle.join().is_ok();
            } else {
                joined = false;
            }
        }
        self.closed = true;
        if !joined || status.is_none() {
            return Err(failure(
                HarnessErrorKind::Timeout,
                "kill/reap/pipe-join did not complete within shutdown deadline",
            ));
        }
        let status =
            status.ok_or_else(|| failure(HarnessErrorKind::Crash, "worker was not reaped"))?;
        if !force && killed {
            return Err(failure(
                HarnessErrorKind::Timeout,
                "worker shutdown deadline",
            ));
        }
        Ok(status)
    }
}
impl Drop for Transport {
    fn drop(&mut self) {
        if !self.closed && self.cleanup(until(self.shutdown_ms), true).is_err() {
            CLEANUP_FAILURES.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn execute_tool(
    program: &Path,
    args: &[OsString],
    cwd: Option<&Path>,
    search: Option<&OsString>,
    limits: &HarnessLimits,
) -> Result<(Vec<u8>, CommandEvidence), HarnessError> {
    let mut command = clean_command(program, cwd);
    command.args(args);
    if let Some(search) = search {
        command.env("LEAN_PATH", search);
    }
    let mut transport = Transport::spawn(command, false, limits.compiler_stdout_bytes, limits)?;
    if let Some(cwd) = cwd {
        transport.artifact_watch = Some((cwd.to_path_buf(), limits.clone()));
    }
    transport.close_input();
    let deadline = until(limits.compile_timeout_ms);
    let output = transport
        .receive(deadline)?
        .ok_or_else(|| failure(HarnessErrorKind::ToolIdentity, "tool output absent"))?;
    let status = transport.cleanup(deadline.min(until(limits.shutdown_timeout_ms)), false)?;
    transport.check_stderr()?;
    let stderr = transport.stderr.lock().unwrap_or_else(|e| e.into_inner());
    let evidence = CommandEvidence {
        program: program.to_path_buf(),
        args: args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect(),
        cwd: cwd.map(Path::to_path_buf),
        lean_path: search.cloned(),
        exit_code: status.code(),
        stdout_bytes: output.len(),
        stdout_blake3: blake3::hash(&output).to_hex().to_string(),
        stderr_bytes: stderr.bytes.len(),
        stderr_blake3: blake3::hash(&stderr.bytes).to_hex().to_string(),
    };
    if !status.success() {
        return Err(HarnessError::new(
            HarnessErrorKind::Crash,
            format!(
                "Lean tool {:?} {:?} exited {:?}: {}",
                program,
                args,
                status.code(),
                String::from_utf8_lossy(&stderr.bytes)
            ),
        ));
    }
    Ok((output, evidence))
}
fn text_output(bytes: &[u8]) -> Result<&str, HarnessError> {
    let value = std::str::from_utf8(bytes)
        .map_err(|_| failure(HarnessErrorKind::ToolIdentity, "tool output is not UTF-8"))?
        .trim();
    if value.is_empty() || value.len() > 4096 {
        return Err(failure(
            HarnessErrorKind::ToolIdentity,
            "tool output length",
        ));
    }
    Ok(value)
}

struct Conversation {
    transport: Transport,
    sequence: u64,
    counters: [u64; 7],
    bytes: u64,
    transcript: blake3::Hasher,
    output_bytes: u64,
    output_hash: blake3::Hasher,
    limits: HarnessLimits,
    healthy: bool,
}
impl Conversation {
    fn frame(&mut self, direction: u8, bytes: &[u8]) -> Result<(), HarnessError> {
        self.bytes = self
            .bytes
            .checked_add(4 + bytes.len() as u64)
            .ok_or_else(|| failure(HarnessErrorKind::Quota, "wire byte overflow"))?;
        if self.bytes > self.limits.session_wire_bytes {
            return Err(failure(HarnessErrorKind::Quota, "session wire limit"));
        }
        self.transcript.update(&[direction]);
        self.transcript.update(&(bytes.len() as u32).to_le_bytes());
        self.transcript.update(bytes);
        if direction == 1 {
            self.output_bytes += 4 + bytes.len() as u64;
            self.output_hash.update(&(bytes.len() as u32).to_le_bytes());
            self.output_hash.update(bytes);
        }
        Ok(())
    }
    fn attach(transport: Transport, limits: HarnessLimits) -> Result<Self, HarnessError> {
        let mut result = Self {
            transport,
            sequence: 0,
            counters: [0; 7],
            bytes: 0,
            transcript: blake3::Hasher::new(),
            output_bytes: 0,
            output_hash: blake3::Hasher::new(),
            limits,
            healthy: true,
        };
        result
            .transcript
            .update(b"vesper.resource-model42.transcript/1\0");
        let ready = result
            .transport
            .receive(until(result.limits.request_timeout_ms))?
            .ok_or_else(|| failure(HarnessErrorKind::Protocol, "READY missing"))?;
        result.frame(1, &ready)?;
        match protocol::decode_message(&ready)? {
            ServerMessage::Ready(ready)
                if ready.protocol == protocol::PROTOCOL_ID
                    && ready.lean_version == protocol::LEAN_VERSION
                    && ready.lean_commit == protocol::LEAN_COMMIT
                    && ready.bounds == protocol::BOUNDS => {}
            _ => {
                return Err(failure(
                    HarnessErrorKind::Protocol,
                    "READY identity or bounds mismatch",
                ));
            }
        }
        Ok(result)
    }
    fn evaluate(&mut self, request: &Request) -> Result<ModelResponse, HarnessError> {
        if !self.healthy {
            return Err(failure(
                HarnessErrorKind::Abandoned,
                "session already failed",
            ));
        }
        if self.sequence >= self.limits.max_requests {
            return Err(failure(HarnessErrorKind::Quota, "session request limit"));
        }
        let bytes = protocol::encode_request(self.sequence, request)?;
        let prepaid = self
            .bytes
            .checked_add(4 + bytes.len() as u64)
            .and_then(|n| n.checked_add(4 + protocol::RESPONSE_CAP as u64))
            .and_then(|n| n.checked_add(protocol::BYE_FRAME_RESERVE))
            .ok_or_else(|| failure(HarnessErrorKind::Quota, "wire prepayment overflow"))?;
        if prepaid > self.limits.session_wire_bytes {
            return Err(failure(
                HarnessErrorKind::Quota,
                "request/maximum-response/BYE prepayment exceeds remaining wire budget",
            ));
        }
        let response = self
            .transport
            .exchange(bytes.clone(), until(self.limits.request_timeout_ms))?;
        self.frame(0, &bytes)?;
        self.frame(1, &response)?;
        let ServerMessage::Result(response) = protocol::decode_message(&response)? else {
            return Err(failure(HarnessErrorKind::Protocol, "expected one result"));
        };
        if response.sequence != self.sequence {
            return Err(failure(
                HarnessErrorKind::Sequence,
                "response sequence mismatch",
            ));
        }
        let index = match response.stage() {
            Stage::ValidationError => 1,
            Stage::CheckSemantic => 2,
            Stage::CheckProfile => 3,
            Stage::InvocationError => 4,
            Stage::EvalAdmissionError => 5,
            Stage::Evaluation => 6,
        };
        self.counters[0] += 1;
        self.counters[index] += 1;
        self.sequence += 1;
        Ok(response)
    }
    fn finish(&mut self) -> Result<ExitStatus, HarnessError> {
        if !self.healthy {
            return Err(failure(
                HarnessErrorKind::Abandoned,
                "cannot retire failed session",
            ));
        }
        let deadline = until(self.limits.shutdown_timeout_ms);
        self.transport.close_input();
        let bye = self
            .transport
            .receive(deadline)?
            .ok_or_else(|| failure(HarnessErrorKind::Protocol, "BYE missing"))?;
        if bye.len() as u64 + 4 > protocol::BYE_FRAME_RESERVE {
            return Err(failure(
                HarnessErrorKind::Protocol,
                "BYE exceeds reserved complete-frame bound",
            ));
        }
        self.frame(1, &bye)?;
        match protocol::decode_message(&bye)? {
            ServerMessage::Bye(bye)
                if bye.next_sequence == self.sequence && bye.counters == self.counters => {}
            ServerMessage::Bye(_) => {
                return Err(failure(
                    HarnessErrorKind::CounterMismatch,
                    "BYE sequence/counter partition mismatch",
                ));
            }
            _ => return Err(failure(HarnessErrorKind::Protocol, "expected BYE")),
        }
        if self.transport.receive(deadline)?.is_some() {
            return Err(failure(
                HarnessErrorKind::Protocol,
                "trailing frame after BYE",
            ));
        }
        let status = self.transport.cleanup(deadline, false)?;
        self.transport.check_stderr()?;
        if !status.success() {
            return Err(failure(
                HarnessErrorKind::Crash,
                "worker nonzero exit after BYE",
            ));
        }
        Ok(status)
    }
}

/// A session must be explicitly retired; dropping it cannot produce evidence.
#[must_use = "call finish for verified BYE/EOF/exit, or abort to record abandonment"]
pub struct Session {
    conversation: Conversation,
    root: Root,
    repo: PathBuf,
    artifacts: Vec<FileIdentity>,
    tool: ToolIdentity,
    commands: Vec<CommandEvidence>,
    settled: bool,
}
impl Session {
    pub fn artifact_root(&self) -> &Path {
        &self.root.path
    }
    fn verify(&self) -> Result<(), HarnessError> {
        verify_sources(&self.repo)?;
        verify_fixed_sources(&self.root.path)?;
        if inventory(&self.root.path, &self.conversation.limits)? != self.artifacts {
            return Err(failure(
                HarnessErrorKind::ArtifactIdentity,
                "private source/object/companion identity changed",
            ));
        }
        let tool = digest_file(
            &self.tool.path,
            self.conversation.limits.artifact_file_bytes,
        )?;
        if tool.bytes != self.tool.bytes || tool.blake3 != self.tool.blake3 {
            return Err(failure(
                HarnessErrorKind::ToolIdentity,
                "Lean binary identity changed",
            ));
        }
        Ok(())
    }
    pub fn evaluate(&mut self, request: &Request) -> Result<ModelResponse, HarnessError> {
        let result = (|| {
            self.verify()?;
            let response = self.conversation.evaluate(request)?;
            self.verify()?;
            Ok(response)
        })();
        if result.is_err() {
            self.conversation.healthy = false;
            let _ = self
                .conversation
                .transport
                .cleanup(until(self.conversation.limits.shutdown_timeout_ms), true);
        }
        result
    }
    pub fn finish(mut self) -> Result<SessionEvidence, HarnessError> {
        self.settled = true;
        self.verify()?;
        let status = self.conversation.finish()?;
        self.verify()?;
        let stderr = self
            .conversation
            .transport
            .stderr
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.commands.push(CommandEvidence {
            program: self.tool.path.clone(),
            args: vec![
                "-j1".into(),
                "-M4096".into(),
                "--run".into(),
                "ResourceOracle42.lean".into(),
            ],
            cwd: Some(self.root.path.clone()),
            lean_path: std::env::join_paths([
                self.root.path.clone(),
                self.tool.prefix.join("lib/lean"),
            ])
            .ok(),
            exit_code: status.code(),
            stdout_bytes: self.conversation.output_bytes as usize,
            stdout_blake3: self
                .conversation
                .output_hash
                .finalize()
                .to_hex()
                .to_string(),
            stderr_bytes: stderr.bytes.len(),
            stderr_blake3: blake3::hash(&stderr.bytes).to_hex().to_string(),
        });
        Ok(SessionEvidence {
            completed: true,
            attestation_scope: "project_artifacts_and_compiler_executable",
            toolchain_dependencies: "standard-library objects and shared libraries supplied by pinned toolchain prefix; not individually attested",
            artifact_root: self.root.path.clone(),
            artifacts: self.artifacts.clone(),
            tool: self.tool.clone(),
            commands: self.commands.clone(),
            requests: self.conversation.sequence,
            counters: self.conversation.counters,
            framed_bytes: self.conversation.bytes,
            transcript_blake3: self.conversation.transcript.finalize().to_hex().to_string(),
        })
    }
    pub fn abort(mut self) -> Result<(), HarnessError> {
        self.settled = true;
        self.conversation.healthy = false;
        if !self.conversation.transport.closed {
            self.conversation
                .transport
                .cleanup(until(self.conversation.limits.shutdown_timeout_ms), true)?;
        }
        Err(failure(
            HarnessErrorKind::Abandoned,
            "session explicitly aborted without model retirement",
        ))
    }
}
impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("artifact_root", &self.root.path)
            .field("requests", &self.conversation.sequence)
            .field("settled", &self.settled)
            .finish()
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if !self.settled {
            ABANDONED.fetch_add(1, Ordering::Relaxed);
        }
        if !self.conversation.transport.closed
            && self
                .conversation
                .transport
                .cleanup(until(self.conversation.limits.shutdown_timeout_ms), true)
                .is_err()
        {
            CLEANUP_FAILURES.fetch_add(1, Ordering::Relaxed);
        }
    }
}

pub fn start(config: &Config) -> Result<Session, HarnessError> {
    config.limits.validate()?;
    verify_sources(&config.repo_root)?;
    if SOURCES
        .iter()
        .any(|(_, source)| source.len() as u64 > config.limits.artifact_file_bytes)
        || SOURCES
            .iter()
            .map(|(_, source)| source.len() as u64)
            .sum::<u64>()
            > config.limits.artifact_total_bytes
    {
        return Err(failure(
            HarnessErrorKind::ArtifactLimit,
            "fixed source copy exceeds artifact limits",
        ));
    }
    let mut commands = Vec::new();
    let configured = invocation_path(&config.lean)?;
    let (prefix, record) = execute_tool(
        &configured,
        &["--print-prefix".into()],
        None,
        None,
        &config.limits,
    )?;
    commands.push(record);
    let prefix = fs::canonicalize(Path::new(text_output(&prefix)?))?;
    let real = fs::canonicalize(invocation_path(&prefix.join("bin").join("lean"))?)?;
    let binary = digest_file(&real, config.limits.artifact_file_bytes)?;
    let (version, record) = execute_tool(&real, &["--version".into()], None, None, &config.limits)?;
    commands.push(record);
    let version = text_output(&version)?.to_owned();
    if !version
        .split([' ', ',', '(', ')'])
        .any(|part| part == protocol::LEAN_VERSION)
    {
        return Err(failure(
            HarnessErrorKind::ToolIdentity,
            "pinned Lean version mismatch",
        ));
    }
    let (commit, record) = execute_tool(&real, &["--githash".into()], None, None, &config.limits)?;
    commands.push(record);
    let commit = text_output(&commit)?.to_owned();
    if commit != protocol::LEAN_COMMIT {
        return Err(failure(
            HarnessErrorKind::ToolIdentity,
            "pinned Lean commit mismatch",
        ));
    }
    let tool = ToolIdentity {
        path: real.clone(),
        prefix: prefix.clone(),
        bytes: binary.bytes,
        blake3: binary.blake3,
        version,
        commit,
    };
    let root = Root::create(
        config
            .artifact_dir
            .as_deref()
            .unwrap_or(&std::env::temp_dir()),
        config.artifact_dir.is_some(),
    )?;
    for (name, source) in SOURCES {
        let path = root.path.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(source.as_bytes())?;
    }
    let search = std::env::join_paths([root.path.clone(), prefix.join("lib/lean")])
        .map_err(|_| failure(HarnessErrorKind::InvalidConfig, "private Lean search path"))?;
    for (name, _) in SOURCES {
        let output = Path::new(name).with_extension("olean");
        let args = [
            OsString::from("-j1"),
            OsString::from("-M4096"),
            OsString::from("-o"),
            output.into_os_string(),
            OsString::from(name),
        ];
        let (_, record) = execute_tool(
            &real,
            &args,
            Some(&root.path),
            Some(&search),
            &config.limits,
        )?;
        commands.push(record);
        verify_fixed_sources(&root.path)?;
        inventory(&root.path, &config.limits)?;
    }
    for (name, _) in SOURCES {
        if !root
            .path
            .join(Path::new(name).with_extension("olean"))
            .is_file()
        {
            return Err(failure(
                HarnessErrorKind::ArtifactIdentity,
                "original executable object missing",
            ));
        }
    }
    let artifacts = inventory(&root.path, &config.limits)?;
    for artifact in &artifacts {
        let path = root.path.join(&artifact.path);
        let mut permissions = fs::metadata(&path)?.permissions();
        permissions.set_readonly(true);
        fs::set_permissions(path, permissions)?;
    }
    for directory in [root.path.join("ResourceModel42"), root.path.clone()] {
        let mut permissions = fs::metadata(&directory)?.permissions();
        permissions.set_readonly(true);
        fs::set_permissions(directory, permissions)?;
    }
    let mut command = clean_command(&real, Some(&root.path));
    command
        .env("LEAN_PATH", &search)
        .args(["-j1", "-M4096", "--run", "ResourceOracle42.lean"]);
    let transport = Transport::spawn(command, true, protocol::RESPONSE_CAP, &config.limits)?;
    let conversation = Conversation::attach(transport, config.limits.clone())?;
    let session = Session {
        conversation,
        root,
        repo: config.repo_root.clone(),
        artifacts,
        tool,
        commands,
        settled: false,
    };
    session.verify()?;
    Ok(session)
}

/// Negative-only seam: even a fully cooperative synthetic peer cannot return
/// a model response or successful mathematical session evidence to the caller.
#[cfg(test)]
pub fn test_transport_failure(
    command: Command,
    requests: &[Request],
    limits: HarnessLimits,
) -> HarnessError {
    let result = (|| {
        limits.validate()?;
        let transport = Transport::spawn(command, true, protocol::RESPONSE_CAP, &limits)?;
        let mut conversation = Conversation::attach(transport, limits)?;
        for request in requests {
            conversation.evaluate(request)?;
        }
        conversation.finish()?;
        Err::<(), _>(failure(
            HarnessErrorKind::SyntheticPeer,
            "synthetic transport cannot attest a model run",
        ))
    })();
    result.err().unwrap_or_else(|| {
        failure(
            HarnessErrorKind::SyntheticPeer,
            "synthetic transport cannot attest a model run",
        )
    })
}
