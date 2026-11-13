//! Bound, fresh external replay of the two fixed Vesper mathematical models.
#![forbid(unsafe_code)]

pub mod artifact;
pub mod fault_peer;
pub mod process;
pub mod protocol;

use serde::{Deserialize, Serialize};
use std::{
    fmt,
    path::{Path, PathBuf},
    time::Instant,
};

pub type Digest = [u8; 32];
pub const PROFILE: &str = "vesper.lean4lean42/1";
pub const LEAN_COMMIT: &str = "797c613eb9b6d4ec95db23e3e00af9ac6657f24b";
pub const UPSTREAM: &str = "4faf51403bc4cb35cf6666d91fc9c938ae46d3aa";
pub const BATTERIES: &str = "8da40b72fece29b7d3fe3d768bac4c8910ce9bee";
pub const PINS: &str = include_str!("../../../tools/lean4lean42/pins.json");
pub const SOURCE_MANIFEST: &str = include_str!("../../../tools/lean4lean42/source-manifest.json");
pub const KNOWN_FIXES: &str = include_str!("../../../tools/lean4lean42/known-fixes.json");
pub const PATCHES: [&[u8]; 3] = [
    include_bytes!("../../../tools/lean4lean42/patches/toolchain.patch"),
    include_bytes!("../../../tools/lean4lean42/patches/kernel.patch"),
    include_bytes!("../../../tools/lean4lean42/patches/replay.patch"),
];
pub const CONTROLS_SOURCE: &[u8] =
    include_bytes!("../../../tools/lean4lean42/controls/VesperControls.lean");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    RewriteWitness,
    ArithmeticOracle,
}
impl Target {
    pub fn tag(self) -> u8 {
        match self {
            Self::RewriteWitness => 0,
            Self::ArithmeticOracle => 1,
        }
    }
    pub fn module(self) -> &'static str {
        match self {
            Self::RewriteWitness => "RewriteWitness",
            Self::ArithmeticOracle => "ArithmeticOracle",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Complete,
    Failed,
    Unsupported,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    EligibleFreshClosure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub setup_timeout_ms: u64,
    pub compile_timeout_ms: u64,
    pub inventory_timeout_ms: u64,
    pub replay_timeout_ms: u64,
    pub shutdown_timeout_ms: u64,
    pub frame_bytes: usize,
    pub stdout_bytes: u64,
    pub stderr_bytes: usize,
    pub declarations: usize,
    pub name_bytes: usize,
    pub inventory_bytes: usize,
    pub artifact_bytes: u64,
    pub artifact_files: usize,
    pub report_bytes: usize,
    pub compiler_runtime_files: usize,
    pub compiler_runtime_bytes: u64,
    pub build_output_files: usize,
    pub build_output_bytes: u64,
}
impl Limits {
    pub const HARD: Self = Self {
        setup_timeout_ms: 3_600_000,
        compile_timeout_ms: 900_000,
        inventory_timeout_ms: 300_000,
        replay_timeout_ms: 1_800_000,
        shutdown_timeout_ms: 10_000,
        frame_bytes: 65536,
        stdout_bytes: 64 * 1024 * 1024,
        stderr_bytes: 256 * 1024,
        declarations: 524288,
        name_bytes: 32 * 1024 * 1024,
        inventory_bytes: 128 * 1024 * 1024,
        artifact_bytes: 4 * 1024 * 1024 * 1024,
        artifact_files: 8192,
        report_bytes: 1024 * 1024,
        compiler_runtime_files: 256,
        compiler_runtime_bytes: 4 * 1024 * 1024 * 1024,
        build_output_files: 16384,
        build_output_bytes: 2 * 1024 * 1024 * 1024,
    };
    pub fn validate(&self) -> Result<(), Error> {
        macro_rules! check {($($field:ident),+)=>{$(if self.$field==0||self.$field>Self::HARD.$field {return Err(Error::new(ErrorKind::InvalidConfig,concat!("invalid limit: ",stringify!($field))));})+};}
        check!(
            setup_timeout_ms,
            compile_timeout_ms,
            inventory_timeout_ms,
            replay_timeout_ms,
            shutdown_timeout_ms,
            frame_bytes,
            stdout_bytes,
            stderr_bytes,
            declarations,
            name_bytes,
            inventory_bytes,
            artifact_bytes,
            artifact_files,
            report_bytes,
            compiler_runtime_files,
            compiler_runtime_bytes,
            build_output_files,
            build_output_bytes
        );
        Ok(())
    }
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            setup_timeout_ms: 1_800_000,
            compile_timeout_ms: 300_000,
            inventory_timeout_ms: 120_000,
            replay_timeout_ms: 600_000,
            shutdown_timeout_ms: 5000,
            frame_bytes: 16384,
            stdout_bytes: 32 * 1024 * 1024,
            stderr_bytes: 64 * 1024,
            declarations: 262144,
            name_bytes: 16 * 1024 * 1024,
            inventory_bytes: 80 * 1024 * 1024,
            artifact_bytes: 2 * 1024 * 1024 * 1024,
            artifact_files: 8192,
            report_bytes: 256 * 1024,
            compiler_runtime_files: 128,
            compiler_runtime_bytes: 2 * 1024 * 1024 * 1024,
            build_output_files: 8192,
            build_output_bytes: 1024 * 1024 * 1024,
        }
    }
}
#[derive(Debug, Clone)]
pub struct Config {
    pub tool_root: PathBuf,
    pub lean: PathBuf,
    pub artifacts: Option<PathBuf>,
    pub limits: Limits,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    InvalidConfig,
    UnsupportedCandidate,
    ToolIdentity,
    Allocation,
    Io,
    Compile,
    Protocol,
    Coverage,
    ExternalRejection,
    Controls,
    Timeout,
    Crash,
    OutputLimit,
    Artifact,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Error {
    pub kind: ErrorKind,
    pub detail: String,
}
impl Error {
    pub fn new(kind: ErrorKind, detail: impl Into<String>) -> Self {
        let mut detail = detail.into();
        if detail.len() > 4096 {
            let mut n = 4096;
            while !detail.is_char_boundary(n) {
                n -= 1
            }
            detail.truncate(n);
        }
        Self { kind, detail }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.detail)
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::new(ErrorKind::Io, e.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileDigest {
    pub path: String,
    pub bytes: u64,
    pub blake3: Digest,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryLink {
    pub path: String,
    pub target: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompilerRuntimeFile {
    pub path: String,
    pub canonical_target: String,
    pub links: Vec<LibraryLink>,
    pub bytes: u64,
    pub blake3: Digest,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlKind {
    RawDeclaration,
    CheckerExpr,
    Condition,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlRecord {
    pub id: String,
    pub kind: ControlKind,
    pub expected_accept: bool,
    pub actual_accept: bool,
    pub expected_reason: String,
    pub actual_reason: String,
    pub matched: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InventoryTotals {
    pub modules: u64,
    pub forms: [u64; 8],
    pub eligibility: [u64; 4],
    pub target_eligibility: [u64; 4],
    pub encoded_name_bytes: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Coverage {
    pub additions: u64,
    pub covered: u64,
    pub constructor_compares: u64,
    pub recursor_compares: u64,
    pub skipped: [u64; 3],
    pub target_covered: u64,
    pub required_roots: u64,
    pub required_roots_covered: u64,
    pub remaining: u64,
    pub pending: u64,
    pub constructor_pending: u64,
    pub recursor_pending: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetStatus {
    NotRun,
    Compiled,
    Inventoried,
    Complete,
    Failed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditStatus {
    NotRun,
    Partial,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetReport {
    pub target: Target,
    pub status: TargetStatus,
    pub source: Option<FileDigest>,
    pub model_artifacts: Vec<FileDigest>,
    pub import_files: u64,
    pub import_bytes: u64,
    pub import_manifest_blake3: Option<Digest>,
    pub inventory: Option<InventoryTotals>,
    pub inventory_blake3: Option<Digest>,
    pub coverage: Option<Coverage>,
    pub coverage_blake3: Option<Digest>,
    pub native_compile_exit: Option<i32>,
    pub external_exit: Option<i32>,
    pub compile_ms: u64,
    pub inventory_ms: u64,
    pub replay_ms: u64,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub stdout_blake3: Option<Digest>,
    pub stderr_blake3: Option<Digest>,
}
impl TargetReport {
    fn new(target: Target) -> Self {
        Self {
            target,
            status: TargetStatus::NotRun,
            source: None,
            model_artifacts: Vec::new(),
            import_files: 0,
            import_bytes: 0,
            import_manifest_blake3: None,
            inventory: None,
            inventory_blake3: None,
            coverage: None,
            coverage_blake3: None,
            native_compile_exit: None,
            external_exit: None,
            compile_ms: 0,
            inventory_ms: 0,
            replay_ms: 0,
            stdout_bytes: 0,
            stderr_bytes: 0,
            stdout_blake3: None,
            stderr_blake3: None,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolInfo {
    pub profile: String,
    pub upstream_commit: String,
    pub batteries_commit: String,
    pub lean_release: String,
    pub lean_commit: String,
    pub checker_blake3: Digest,
    pub compiler_blake3: Digest,
    pub compiler_runtime_manifest_blake3: Digest,
    pub receipt_blake3: Digest,
    pub pins_blake3: Digest,
    pub known_fixes_blake3: Digest,
    pub source_manifest_blake3: Digest,
    pub build_output_manifest_blake3: Digest,
    pub control_inventory_blake3: Digest,
    pub control_import_manifest_blake3: Digest,
    pub os: String,
    pub arch: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub schema: u16,
    pub status: Status,
    pub scope: Scope,
    pub limits: Limits,
    pub tools: Option<ToolInfo>,
    pub controls: Vec<ControlRecord>,
    pub controls_blake3: Option<Digest>,
    pub targets: [TargetReport; 2],
    pub axiom_audit: AuditStatus,
    pub implementation_proof_audit: AuditStatus,
    pub process_spawns: u64,
    pub timeouts: u64,
    pub crashes: u64,
    pub protocol_errors: u64,
    pub elapsed_ms: u64,
    pub artifacts: Vec<FileDigest>,
}
impl Report {
    pub fn new(limits: Limits) -> Self {
        Self {
            schema: 1,
            status: Status::Failed,
            scope: Scope::EligibleFreshClosure,
            limits,
            tools: None,
            controls: Vec::new(),
            controls_blake3: None,
            targets: [
                TargetReport::new(Target::RewriteWitness),
                TargetReport::new(Target::ArithmeticOracle),
            ],
            axiom_audit: AuditStatus::NotRun,
            implementation_proof_audit: AuditStatus::NotRun,
            process_spawns: 0,
            timeouts: 0,
            crashes: 0,
            protocol_errors: 0,
            elapsed_ms: 0,
            artifacts: Vec::new(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Failure {
    pub error: Error,
    pub partial: Box<Report>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchPin {
    pub path: String,
    pub blake3: Digest,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverlayPin {
    pub source: String,
    pub destination: String,
    pub blake3: Digest,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pins {
    pub schema: u16,
    pub profile: String,
    pub upstream_commit: String,
    pub batteries_commit: String,
    pub lean_release: String,
    pub lean_commit: String,
    pub patches: Vec<PatchPin>,
    pub controls_overlay: Vec<OverlayPin>,
    pub known_fixes_blake3: Digest,
    pub source_manifest_blake3: Digest,
    pub build_argv: Vec<String>,
    pub checker_relative_path: String,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifest {
    pub schema: u16,
    pub files: Vec<FileDigest>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupInputs {
    pub schema: u16,
    pub lean: String,
    pub lake: String,
    pub git: String,
    pub build_argv: Vec<String>,
    pub build_exit: i32,
    pub build_stdout: String,
    pub build_stderr: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub schema: u16,
    pub profile: String,
    pub upstream_commit: String,
    pub batteries_commit: String,
    pub lean_release: String,
    pub lean_commit: String,
    pub pins_blake3: Digest,
    pub known_fixes_blake3: Digest,
    pub patches: Vec<PatchPin>,
    pub source_manifest: Vec<FileDigest>,
    pub source_manifest_blake3: Digest,
    pub compiler_path: String,
    pub sysroot: String,
    pub compiler_blake3: Digest,
    pub compiler_runtime_manifest: Vec<CompilerRuntimeFile>,
    pub compiler_runtime_manifest_blake3: Digest,
    pub checker_relative_path: String,
    pub checker_blake3: Digest,
    pub driver_blake3: Digest,
    pub build_argv: Vec<String>,
    pub build_exit: i32,
    pub build_stdout_blake3: Digest,
    pub build_stderr_blake3: Digest,
    pub controls: Vec<ControlRecord>,
    pub controls_blake3: Digest,
    pub control_inventory_blake3: Digest,
    pub control_import_manifest_blake3: Digest,
    pub build_output_manifest: Vec<FileDigest>,
    pub build_output_manifest_blake3: Digest,
}

pub fn run(config: &Config) -> Result<Report, Failure> {
    let started = Instant::now();
    let mut report = Report::new(config.limits.clone());
    let mut artifact_directory = None;
    let mut result = config
        .limits
        .validate()
        .and_then(|()| artifact::execute(config, &mut report, &mut artifact_directory));
    report.elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    report.status = match &result {
        Ok(()) => Status::Complete,
        Err(e) if e.kind == ErrorKind::UnsupportedCandidate => Status::Unsupported,
        Err(_) => Status::Failed,
    };
    if let Err(error) = &result {
        match error.kind {
            ErrorKind::Timeout => report.timeouts = 1,
            ErrorKind::Crash => report.crashes = 1,
            ErrorKind::Protocol => report.protocol_errors = 1,
            _ => {}
        }
    }
    if let Err(e) = artifact::save_report(config, artifact_directory.as_deref(), &report) {
        result = Err(e);
        report.status = Status::Failed;
    }
    match result {
        Ok(()) => {
            report.status = Status::Complete;
            Ok(report)
        }
        Err(error) => {
            report.status = if error.kind == ErrorKind::UnsupportedCandidate {
                Status::Unsupported
            } else {
                Status::Failed
            };
            Err(Failure {
                error,
                partial: Box::new(report),
            })
        }
    }
}
pub fn seal_setup(root: &Path) -> Result<Receipt, Error> {
    artifact::seal(root)
}
pub fn verify_setup_source(root: &Path) -> Result<Digest, Error> {
    artifact::verify_sources(root)
}
pub fn receipt_compiler(root: &Path) -> Result<PathBuf, Error> {
    Ok(PathBuf::from(artifact::read_receipt(root)?.compiler_path))
}
