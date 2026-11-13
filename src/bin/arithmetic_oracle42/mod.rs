//! Bounded developer arithmetic oracle. The reference is the static Lean process.
#![forbid(unsafe_code)]

pub mod corpus;
pub mod process;
pub mod protocol;
pub mod worker;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub use process::run;

pub const MAX_NODES: usize = 63;
pub const MAX_DEPTH: usize = 8;
pub const MAX_INPUTS: usize = 3;
pub const MAX_LOCALS: usize = 3;
pub const FRAME_CAP: usize = 65536;
pub const RESPONSE_CAP: usize = 512;
pub const SEQUENCE_CAP: u64 = 9_999_999_999;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    U8,
    U32,
    U64,
    I64,
}
impl Kind {
    pub fn tag(self) -> u8 {
        match self {
            Self::U8 => 0,
            Self::U32 => 1,
            Self::U64 => 2,
            Self::I64 => 3,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::U8 => "u8",
            Self::U32 => "u32",
            Self::U64 => "u64",
            Self::I64 => "i64",
        }
    }
    pub fn bounds(self) -> (i128, i128) {
        match self {
            Self::U8 => (0, 255),
            Self::U32 => (0, u32::MAX.into()),
            Self::U64 => (0, u64::MAX.into()),
            Self::I64 => (i64::MIN.into(), i64::MAX.into()),
        }
    }
    pub fn fits(self, n: i128) -> bool {
        let (lo, hi) = self.bounds();
        (lo..=hi).contains(&n)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Add,
    Sub,
    Mul,
}
impl Op {
    pub fn tag(self) -> u8 {
        match self {
            Self::Add => 0,
            Self::Sub => 1,
            Self::Mul => 2,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Input,
    Local,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Callee {
    Intrinsic(Op),
    Variable(Scope, u16),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Node {
    Int(#[serde(with = "decimal_i128")] i128),
    Bool(bool),
    Variable(Scope, u16),
    Checked(Op),
    Call(Callee, u8),
}
pub type Code = Vec<Node>;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub kind: Kind,
    #[serde(with = "decimal_i128")]
    pub value: i128,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Program {
    pub kind: Kind,
    pub inputs: Vec<Input>,
    pub locals: Vec<Code>,
    pub result: Code,
    pub allow_fail: bool,
}
impl Program {
    /// Structural admission only: unknown slots and language negatives remain checker inputs.
    pub fn validate(&self) -> Result<(), HarnessError> {
        if self.inputs.len() > MAX_INPUTS
            || self.locals.len() > MAX_LOCALS
            || self.inputs.iter().any(|v| !v.kind.fits(v.value))
        {
            return Err(HarnessError::new(
                FailureKind::UnsupportedProfile,
                "input/local profile",
            ));
        }
        let mut count = 0usize;
        for code in self.locals.iter().chain(std::iter::once(&self.result)) {
            count = count
                .checked_add(code.len())
                .ok_or_else(|| HarnessError::new(FailureKind::UnsupportedProfile, "node count"))?;
            if count > MAX_NODES || code.is_empty() {
                return Err(HarnessError::new(
                    FailureKind::UnsupportedProfile,
                    "node count",
                ));
            }
            let mut depths = [0usize; MAX_NODES];
            let mut len = 0;
            for node in code {
                let arity = match node {
                    Node::Checked(_) => 2,
                    Node::Call(_, n) if *n <= 3 => usize::from(*n),
                    Node::Call(_, _) => {
                        return Err(HarnessError::new(
                            FailureKind::UnsupportedProfile,
                            "call arity cap",
                        ));
                    }
                    _ => 0,
                };
                if len < arity {
                    return Err(HarnessError::new(
                        FailureKind::UnsupportedProfile,
                        "postfix underflow",
                    ));
                }
                let depth = depths[len - arity..len].iter().copied().max().unwrap_or(0) + 1;
                if depth > MAX_DEPTH {
                    return Err(HarnessError::new(
                        FailureKind::UnsupportedProfile,
                        "expression depth",
                    ));
                }
                len -= arity;
                depths[len] = depth;
                len += 1;
            }
            if len != 1 {
                return Err(HarnessError::new(
                    FailureKind::UnsupportedProfile,
                    "postfix roots",
                ));
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Suite {
    Ci128,
    Full100000,
}
impl Suite {
    pub fn count(self) -> u64 {
        match self {
            Self::Ci128 => 128,
            Self::Full100000 => 100000,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    Valid,
    StaticNegative,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: u64,
    pub program: Program,
    pub intent: Intent,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Value(#[serde(with = "decimal_i128")] i128),
    Overflow,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticClass {
    UnknownVariable,
    TypeMismatch,
    LiteralRange,
    LiteralParseRange,
    MissingFail,
    CallArity,
    NotCallable,
}
impl DiagnosticClass {
    pub fn index(self) -> usize {
        match self {
            Self::UnknownVariable => 0,
            Self::TypeMismatch => 1,
            Self::LiteralRange => 2,
            Self::LiteralParseRange => 3,
            Self::MissingFail => 4,
            Self::CallArity => 5,
            Self::NotCallable => 6,
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            Self::UnknownVariable => "E0201",
            Self::TypeMismatch => "E0200",
            Self::LiteralRange => "E0209",
            Self::LiteralParseRange => "E0002",
            Self::MissingFail => "E0401",
            Self::CallArity => "E0207",
            Self::NotCallable => "E0206",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::UnknownVariable => "unknown_variable",
            Self::TypeMismatch => "type_mismatch",
            Self::LiteralRange => "literal_range",
            Self::LiteralParseRange => "literal_parse_range",
            Self::MissingFail => "missing_fail",
            Self::CallArity => "call_arity",
            Self::NotCallable => "not_callable",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blame {
    pub scope: u8,
    pub slot: u16,
    pub node: u16,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OracleAnswer {
    Outcome {
        outcome: Outcome,
        steps: u16,
    },
    Diagnostic {
        class: DiagnosticClass,
        blame: Blame,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mode {
    pub ast_optimized: bool,
    pub ir_optimized: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RustAnswer {
    Outcomes {
        evaluator: [Outcome; 4],
        jit: [Outcome; 4],
    },
    Diagnostic {
        phase: DiagnosticPhase,
        codes: Vec<String>,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticPhase {
    Parse,
    Check,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessLimits {
    pub request_bytes: usize,
    pub source_bytes: usize,
    pub worker_cases: u16,
    pub lean_fuel: u16,
    pub evaluator_fuel: u64,
    pub startup_timeout_ms: u64,
    pub request_timeout_ms: u64,
    pub worker_timeout_ms: u64,
    pub shutdown_timeout_ms: u64,
    pub stderr_bytes: usize,
    pub artifact_bytes: usize,
    pub shrink_attempts: u16,
}
impl HarnessLimits {
    pub const HARD: Self = Self {
        request_bytes: 65536,
        source_bytes: 16384,
        worker_cases: 128,
        lean_fuel: 4096,
        evaluator_fuel: 100000,
        startup_timeout_ms: 300000,
        request_timeout_ms: 30000,
        worker_timeout_ms: 120000,
        shutdown_timeout_ms: 5000,
        stderr_bytes: 65536,
        artifact_bytes: 4194304,
        shrink_attempts: 64,
    };
    pub fn validate(&self) -> Result<(), HarnessError> {
        macro_rules! bound {($($f:ident),+)=>{$(if self.$f==0 || self.$f>Self::HARD.$f {return Err(HarnessError::new(FailureKind::InvalidConfig,concat!("limit: ",stringify!($f))));})+};}
        bound!(
            request_bytes,
            source_bytes,
            worker_cases,
            lean_fuel,
            evaluator_fuel,
            startup_timeout_ms,
            request_timeout_ms,
            worker_timeout_ms,
            shutdown_timeout_ms,
            stderr_bytes,
            artifact_bytes
        );
        if self.shrink_attempts > 64 {
            return Err(HarnessError::new(
                FailureKind::InvalidConfig,
                "shrink attempts",
            ));
        }
        Ok(())
    }
}
impl Default for HarnessLimits {
    fn default() -> Self {
        Self {
            request_bytes: 16384,
            source_bytes: 16384,
            worker_cases: 32,
            lean_fuel: 128,
            evaluator_fuel: 100000,
            startup_timeout_ms: 300000,
            request_timeout_ms: 5000,
            worker_timeout_ms: 30000,
            shutdown_timeout_ms: 5000,
            stderr_bytes: 65536,
            artifact_bytes: 1048576,
            shrink_attempts: 0,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunConfig {
    pub suite: Suite,
    pub seed: u64,
    pub jobs: u8,
    pub lean: PathBuf,
    pub worker_exe: PathBuf,
    pub artifacts: Option<PathBuf>,
    pub limits: HarnessLimits,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    InvalidConfig,
    UnsupportedProfile,
    Allocation,
    Io,
    ToolVersion,
    Protocol,
    Timeout,
    Crash,
    Fuel,
    Invariant,
    DiagnosticMismatch,
    OutcomeMismatch,
    CounterMismatch,
    Artifact,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessError {
    pub kind: FailureKind,
    pub detail: String,
}
impl HarnessError {
    pub fn new(kind: FailureKind, detail: impl Into<String>) -> Self {
        let mut detail = detail.into();
        if detail.len() > 4096 {
            let mut end = 4096;
            while !detail.is_char_boundary(end) {
                end -= 1;
            }
            detail.truncate(end);
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
    fn from(e: std::io::Error) -> Self {
        Self::new(FailureKind::Io, e.to_string())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunFailure {
    pub error: HarnessError,
    pub case_id: Option<u64>,
    pub partial: RunReport,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Complete,
    Failed,
    Incomplete,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseTimes {
    pub setup_ms: u64,
    pub oracle_ms: u64,
    pub workers_ms: u64,
    pub shutdown_ms: u64,
    pub shrink_ms: u64,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInfo {
    pub lean_version: String,
    pub rustc_version: String,
    pub os: String,
    pub arch: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactInfo {
    pub relative_path: String,
    pub bytes: u64,
    pub blake3: [u8; 32],
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShrinkCounts {
    pub lean_requests: u64,
    pub lean_responses: u64,
    pub parser_calls: u64,
    pub checker_calls: u64,
    pub ast_optimizer_calls: u64,
    pub evaluator_calls: [u64; 4],
    pub jit_calls: [u64; 4],
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunReport {
    pub schema: u16,
    pub status: RunStatus,
    pub suite: Suite,
    pub seed: u64,
    pub requested_valid: u64,
    pub generated_valid: u64,
    pub unique_valid: u64,
    pub committed_valid: u64,
    pub generated_negative: u64,
    pub matched_negative: u64,
    pub parser_calls: u64,
    pub checker_calls: u64,
    pub ast_optimizer_calls: u64,
    pub evaluator_calls: [u64; 4],
    pub jit_calls: [u64; 4],
    pub lean_requests: u64,
    pub lean_responses: u64,
    pub lean_spawns: u64,
    pub rust_worker_spawns: u64,
    pub rust_worker_recycles: u64,
    pub max_worker_cases: u16,
    pub max_in_flight: u16,
    pub jobs: u8,
    pub timeouts: u64,
    pub crashes: u64,
    pub protocol_errors: u64,
    pub outcome_mismatches: u64,
    pub diagnostic_mismatches: u64,
    pub cancelled_cases: u64,
    pub shrink_attempts: u64,
    pub interrupted_worker_cases: u64,
    pub shrink_calls: ShrinkCounts,
    pub per_kind_valid: [u64; 4],
    pub per_op_nodes: [u64; 6],
    pub lean_values: u64,
    pub lean_overflows: u64,
    pub diagnostic_counts: [u64; 7],
    pub elapsed_ms: u64,
    pub phase_times: PhaseTimes,
    pub tools: ToolInfo,
    pub model_blake3: [u8; 32],
    pub driver_blake3: [u8; 32],
    pub worker_blake3: [u8; 32],
    pub corpus_blake3: [u8; 32],
    pub requests_blake3: [u8; 32],
    pub responses_blake3: [u8; 32],
    pub first_failed_case: Option<u64>,
    pub artifacts: Vec<ArtifactInfo>,
}
impl RunReport {
    pub fn new(suite: Suite, seed: u64, jobs: u8) -> Self {
        Self {
            schema: 1,
            status: RunStatus::Incomplete,
            suite,
            seed,
            requested_valid: suite.count(),
            generated_valid: 0,
            unique_valid: 0,
            committed_valid: 0,
            generated_negative: 0,
            matched_negative: 0,
            parser_calls: 0,
            checker_calls: 0,
            ast_optimizer_calls: 0,
            evaluator_calls: [0; 4],
            jit_calls: [0; 4],
            lean_requests: 0,
            lean_responses: 0,
            lean_spawns: 0,
            rust_worker_spawns: 0,
            rust_worker_recycles: 0,
            max_worker_cases: 0,
            max_in_flight: 0,
            jobs,
            timeouts: 0,
            crashes: 0,
            protocol_errors: 0,
            outcome_mismatches: 0,
            diagnostic_mismatches: 0,
            cancelled_cases: 0,
            shrink_attempts: 0,
            interrupted_worker_cases: 0,
            shrink_calls: ShrinkCounts::default(),
            per_kind_valid: [0; 4],
            per_op_nodes: [0; 6],
            lean_values: 0,
            lean_overflows: 0,
            diagnostic_counts: [0; 7],
            elapsed_ms: 0,
            phase_times: PhaseTimes::default(),
            tools: ToolInfo {
                os: std::env::consts::OS.into(),
                arch: std::env::consts::ARCH.into(),
                ..ToolInfo::default()
            },
            model_blake3: [0; 32],
            driver_blake3: [0; 32],
            worker_blake3: [0; 32],
            corpus_blake3: [0; 32],
            requests_blake3: [0; 32],
            responses_blake3: [0; 32],
            first_failed_case: None,
            artifacts: Vec::new(),
        }
    }
}
impl Default for RunReport {
    fn default() -> Self {
        Self::new(Suite::Ci128, 0, 1)
    }
}

/// Exact string adapter shared by worker data and structured artifacts.
pub mod decimal_i128 {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};
    pub fn parse(s: &str) -> Result<i128, &'static str> {
        let bytes = s.as_bytes();
        let digits = if bytes.first() == Some(&b'-') {
            &bytes[1..]
        } else {
            bytes
        };
        if bytes.len() > 40
            || digits.is_empty()
            || !digits.iter().all(u8::is_ascii_digit)
            || (digits.len() > 1 && digits[0] == b'0')
            || s == "-0"
        {
            return Err("canonical i128 decimal required");
        }
        s.parse().map_err(|_| "i128 range")
    }
    pub fn serialize<S: Serializer>(value: &i128, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&value.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<i128, D::Error> {
        let value = String::deserialize(d)?;
        parse(&value).map_err(D::Error::custom)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkerRequest {
    pub worker_seq: u64,
    pub case: Case,
    pub evaluator_fuel: u64,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkerResponse {
    pub worker_seq: u64,
    pub case_id: u64,
    pub answer: Result<RustAnswer, HarnessError>,
}
