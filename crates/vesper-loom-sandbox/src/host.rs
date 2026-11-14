//! The Wasmtime component host.

use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Config, Engine, ResourceLimiter, Store, Trap};
use wasmtime_wasi::clocks::{HostMonotonicClock, HostWallClock};
use wasmtime_wasi::p2::pipe::{ClosedInputStream, MemoryOutputPipe};
use wasmtime_wasi::{FsPerms, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::artifact::{ArtifactKind, classify};
use crate::fs_determinism::{FsNormalizer, HasFsNormalizer};
use crate::policy::{ConfigError, Determinism, OUTPUT_GUEST_PATH, ResourceLimits, StepConfig};
use crate::transport::{self, Direction, PROTOCOL_VERSION, TransportLimits, TransportViolation};

mod bindings {
    wasmtime::component::bindgen!({
        path: "wit",
        world: "sandboxed-step",
        exports: { default: async },
    });
}

use bindings::SandboxedStep;
use bindings::vesper::sandbox::types as wit;

/// Structured failure of a sandboxed step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SandboxError {
    Config(ConfigError),
    /// The binary is not a component; `detected` distinguishes a
    /// `wasm32-wasip1` module from other inputs.
    NotAComponent {
        detected: ArtifactKind,
    },
    Compile {
        message: String,
    },
    /// The component does not export `vesper:sandbox/step@0.1.0` with the
    /// expected types, or imports something the host does not provide.
    Link {
        message: String,
    },
    ProtocolMismatch {
        host: u32,
        guest: u32,
    },
    FuelExhausted {
        limit: u64,
    },
    MemoryLimitExceeded {
        limit: usize,
        requested: usize,
    },
    TableLimitExceeded {
        limit: usize,
        requested: usize,
    },
    WallClockTimeout {
        limit: Duration,
    },
    /// The guest trapped (panic, `unreachable`, stdio overflow, ...).
    Trap {
        message: String,
    },
    Transport {
        direction: Direction,
        violation: TransportViolation,
    },
    Output(OutputViolation),
    Host {
        message: String,
    },
}

impl std::fmt::Display for SandboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for SandboxError {}

impl From<ConfigError> for SandboxError {
    fn from(e: ConfigError) -> Self {
        Self::Config(e)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputViolation {
    /// Symlinks or special files are not accepted as step outputs.
    NonRegularEntry {
        path: PathBuf,
    },
    TooManyEntries {
        count: usize,
        limit: usize,
    },
    TooManyBytes {
        bytes: u64,
        limit: u64,
    },
    Unreadable {
        path: PathBuf,
        reason: String,
    },
}

/// One file the step left in its output directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputFile {
    /// Path relative to the output directory, `/`-separated.
    pub path: String,
    pub bytes: u64,
}

/// Result of a step that ran to completion (successfully or with guest
/// diagnostics).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepReport {
    /// `Ok` with the guest's outcome, or `Err` with its error diagnostics.
    pub result: Result<transport::Outcome, Vec<transport::Diagnostic>>,
    pub fuel_consumed: u64,
    pub peak_memory_bytes: usize,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// Sorted list of regular files in the output directory.
    pub outputs: Vec<OutputFile>,
}

/// A validated, compiled step component bound to a host engine.
#[derive(Clone)]
pub struct StepComponent {
    component: Component,
}

/// Shared execution host. One engine compiles and runs many steps; every
/// run gets a fresh store, WASI context and limiter.
pub struct SandboxHost {
    engine: Engine,
    linker: Linker<StepState>,
}

impl SandboxHost {
    pub fn new() -> Result<Self, SandboxError> {
        let mut cfg = Config::new();
        cfg.wasm_component_model(true)
            .consume_fuel(true)
            // Determinism: canonical NaNs and relaxed SIMD pinned to its
            // deterministic lowering. The `threads` cargo feature is not
            // enabled, so shared memories and atomics.wait are unavailable.
            .cranelift_nan_canonicalization(true)
            .relaxed_simd_deterministic(true)
            .max_wasm_stack(1 << 20);
        let engine = Engine::new(&cfg).map_err(host_err)?;
        let mut linker = Linker::new(&engine);
        // All of WASI p2 is linked so that std-built components instantiate;
        // authority is withheld by the per-run `WasiCtx`, not by missing
        // imports. Every denial is therefore observable by the guest.
        wasmtime_wasi::p2::add_to_linker_async(&mut linker).map_err(host_err)?;
        crate::fs_determinism::add_to_linker(&mut linker).map_err(host_err)?;
        Ok(Self { engine, linker })
    }

    fn start_validated(
        &self,
        config: &StepConfig,
        layout: crate::policy::ValidatedLayout,
    ) -> Result<RunContext, SandboxError> {
        let limits = &config.limits;
        let stdout = MemoryOutputPipe::new(limits.max_stdio_bytes);
        let stderr = MemoryOutputPipe::new(limits.max_stdio_bytes);
        let mut b = WasiCtxBuilder::new();
        b.stdin(ClosedInputStream)
            .stdout(stdout.clone())
            .stderr(stderr.clone())
            .allow_tcp(false)
            .allow_udp(false)
            .allow_ip_name_lookup(false)
            .socket_addr_check(|_, _| Box::pin(async { false }));
        apply_determinism(&mut b, &config.determinism);
        for (host, guest) in &layout.inputs {
            b.preopened_dir(host, guest, FsPerms::ReadOnly)
                .map_err(host_err)?;
        }
        b.preopened_dir(&layout.output_dir, OUTPUT_GUEST_PATH, FsPerms::ReadWrite)
            .map_err(host_err)?;

        let state = StepState {
            ctx: b.build(),
            table: ResourceTable::new(),
            limiter: Limiter::new(limits),
            fs: FsNormalizer::new(config.determinism.wall_clock_ns),
        };
        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| &mut s.limiter);
        store.set_fuel(limits.fuel).map_err(host_err)?;

        Ok(RunContext {
            store,
            stdout,
            stderr,
            layout,
        })
    }

    /// Classifies and compiles a component. Core modules are rejected before
    /// compilation.
    pub fn compile(&self, bytes: &[u8]) -> Result<StepComponent, SandboxError> {
        match classify(bytes) {
            ArtifactKind::Component => {}
            detected => return Err(SandboxError::NotAComponent { detected }),
        }
        let component =
            Component::from_binary(&self.engine, bytes).map_err(|e| SandboxError::Compile {
                message: format!("{e:#}"),
            })?;
        Ok(StepComponent { component })
    }

    /// Runs one invocation synchronously on a private current-thread runtime.
    pub fn run(
        &self,
        component: &StepComponent,
        config: &StepConfig,
        invocation: &transport::Invocation,
    ) -> Result<StepReport, SandboxError> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .enable_io()
            .build()
            .map_err(host_err)?;
        rt.block_on(self.run_async(component, config, invocation))
    }

    pub async fn run_async(
        &self,
        component: &StepComponent,
        config: &StepConfig,
        invocation: &transport::Invocation,
    ) -> Result<StepReport, SandboxError> {
        let layout = config.validate()?;
        let limits = &config.limits;
        limits
            .transport
            .check_invocation(invocation)
            .map_err(|violation| SandboxError::Transport {
                direction: Direction::Input,
                violation,
            })?;

        let RunContext {
            mut store,
            stdout,
            stderr,
            layout,
        } = self.start_validated(config, layout)?;

        let wit_inv = to_wit_invocation(invocation);
        let call = async {
            let step =
                SandboxedStep::instantiate_async(&mut store, &component.component, &self.linker)
                    .await
                    .map_err(Failure::Link)?;
            let api = step.vesper_sandbox_step();
            let guest = api
                .call_protocol_version(&mut store)
                .await
                .map_err(Failure::Run)?;
            if guest != PROTOCOL_VERSION {
                return Err(Failure::Protocol(guest));
            }
            api.call_run(&mut store, &wit_inv)
                .await
                .map_err(Failure::Run)
        };
        let outcome = tokio::time::timeout(limits.wall_clock_backstop, call).await;

        let fuel_left = store.get_fuel().unwrap_or(0);
        let fuel_consumed = limits.fuel - fuel_left;
        let limiter = &store.data().limiter;
        let result = complete_call(outcome, limits, limiter)?;

        let (failed, packed) = match result {
            Ok(p) => (false, p),
            Err(p) => (true, p),
        };
        let (tokens, diagnostics) =
            unpack(packed, &limits.transport, failed).map_err(|violation| {
                SandboxError::Transport {
                    direction: Direction::Output,
                    violation,
                }
            })?;
        let result = if failed {
            Err(diagnostics)
        } else {
            Ok(transport::Outcome {
                tokens,
                diagnostics,
            })
        };
        let peak_memory_bytes = limiter.peak_memory;
        drop(store);
        let outputs = scan_outputs(&layout.output_dir, limits).map_err(SandboxError::Output)?;
        Ok(StepReport {
            result,
            fuel_consumed,
            peak_memory_bytes,
            stdout: stdout.contents().to_vec(),
            stderr: stderr.contents().to_vec(),
            outputs,
        })
    }
}

struct RunContext {
    store: Store<StepState>,
    stdout: MemoryOutputPipe,
    stderr: MemoryOutputPipe,
    layout: crate::policy::ValidatedLayout,
}
fn complete_call<T>(
    outcome: Result<Result<T, Failure>, tokio::time::error::Elapsed>,
    limits: &ResourceLimits,
    limiter: &Limiter,
) -> Result<T, SandboxError> {
    Ok(match outcome {
        Err(_) => {
            return Err(SandboxError::WallClockTimeout {
                limit: limits.wall_clock_backstop,
            });
        }
        Ok(Err(Failure::Link(e))) => {
            // Instantiation runs guest start code, which is metered too.
            if let Some(Trap::OutOfFuel) = e.downcast_ref::<Trap>() {
                return Err(SandboxError::FuelExhausted { limit: limits.fuel });
            }
            if let Some(err) = limiter.denial() {
                return Err(err);
            }
            return Err(SandboxError::Link {
                message: format!("{e:#}"),
            });
        }
        Ok(Err(Failure::Protocol(guest))) => {
            return Err(SandboxError::ProtocolMismatch {
                host: PROTOCOL_VERSION,
                guest,
            });
        }
        Ok(Err(Failure::Run(e))) => {
            return Err(classify_trap(&e, limits, limiter));
        }
        Ok(Ok(r)) => r,
    })
}
#[path = "quote_host.rs"]
pub mod quote_runtime;

enum Failure {
    Link(wasmtime::Error),
    Protocol(u32),
    Run(wasmtime::Error),
}

fn host_err(e: impl std::fmt::Display) -> SandboxError {
    SandboxError::Host {
        message: e.to_string(),
    }
}

fn classify_trap(e: &wasmtime::Error, limits: &ResourceLimits, limiter: &Limiter) -> SandboxError {
    if let Some(Trap::OutOfFuel) = e.downcast_ref::<Trap>() {
        return SandboxError::FuelExhausted { limit: limits.fuel };
    }
    // A denied grow returns -1 to the guest; Rust's allocator then aborts
    // with `unreachable`. Attribute such traps to the limit.
    if let Some(err) = limiter.denial() {
        return err;
    }
    SandboxError::Trap {
        message: format!("{e:#}"),
    }
}

struct StepState {
    ctx: WasiCtx,
    table: ResourceTable,
    limiter: Limiter,
    fs: FsNormalizer,
}

impl HasFsNormalizer for StepState {
    fn normalizer(&mut self) -> &mut FsNormalizer {
        &mut self.fs
    }
}

impl WasiView for StepState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.ctx,
            table: &mut self.table,
        }
    }
}

/// Aggregate store limits. Wasmtime calls `memory_growing` for initial
/// allocation at instantiation and for every `memory.grow`.
struct Limiter {
    max_memory: usize,
    max_table: usize,
    max_instances: usize,
    max_memories: usize,
    max_tables: usize,
    memory: usize,
    table: usize,
    peak_memory: usize,
    memory_denied: Option<usize>,
    table_denied: Option<usize>,
    /// Aggregate before the most recent approved grow, restored if Wasmtime
    /// then fails to perform it.
    memory_before_grow: Option<(usize, usize)>,
    table_before_grow: Option<usize>,
}

impl Limiter {
    fn new(l: &ResourceLimits) -> Self {
        Self {
            max_memory: l.max_memory_bytes,
            max_table: l.max_table_elements,
            max_instances: l.max_instances,
            max_memories: l.max_memories,
            max_tables: l.max_tables,
            memory: 0,
            table: 0,
            peak_memory: 0,
            memory_denied: None,
            table_denied: None,
            memory_before_grow: None,
            table_before_grow: None,
        }
    }

    fn denial(&self) -> Option<SandboxError> {
        if let Some(requested) = self.memory_denied {
            return Some(SandboxError::MemoryLimitExceeded {
                limit: self.max_memory,
                requested,
            });
        }
        self.table_denied
            .map(|requested| SandboxError::TableLimitExceeded {
                limit: self.max_table,
                requested,
            })
    }
}

impl ResourceLimiter for Limiter {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        // Beyond the memory's own declared maximum: Wasmtime fails the grow
        // anyway. Neither charge it nor attribute it to the sandbox limit.
        if maximum.is_some_and(|m| desired > m) {
            return Ok(false);
        }
        let total = self.memory - current.min(self.memory) + desired;
        if total > self.max_memory {
            self.memory_denied = Some(total);
            return Ok(false);
        }
        self.memory_before_grow = Some((self.memory, self.peak_memory));
        self.memory = total;
        self.peak_memory = self.peak_memory.max(total);
        Ok(true)
    }

    fn memory_grow_failed(&mut self, _error: wasmtime::Error) -> wasmtime::Result<()> {
        if let Some((memory, peak)) = self.memory_before_grow.take() {
            self.memory = memory;
            self.peak_memory = peak;
        }
        Ok(())
    }

    fn table_grow_failed(&mut self, _error: wasmtime::Error) -> wasmtime::Result<()> {
        if let Some(before) = self.table_before_grow.take() {
            self.table = before;
        }
        Ok(())
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        if maximum.is_some_and(|m| desired > m) {
            return Ok(false);
        }
        let total = self.table - current.min(self.table) + desired;
        if total > self.max_table {
            self.table_denied = Some(total);
            return Ok(false);
        }
        self.table_before_grow = Some(self.table);
        self.table = total;
        Ok(true)
    }

    fn instances(&self) -> usize {
        self.max_instances
    }

    fn memories(&self) -> usize {
        self.max_memories
    }

    fn tables(&self) -> usize {
        self.max_tables
    }
}

struct FixedWallClock(Duration);

impl HostWallClock for FixedWallClock {
    fn resolution(&self) -> Duration {
        Duration::from_nanos(1)
    }
    fn now(&self) -> Duration {
        self.0
    }
}

struct FixedMonotonicClock(u64);

impl HostMonotonicClock for FixedMonotonicClock {
    fn resolution(&self) -> u64 {
        1
    }
    fn now(&self) -> u64 {
        self.0
    }
}

/// SplitMix64: a small generator with a stable, documented output sequence,
/// so recorded randomness does not depend on `rand`'s version choices.
/// Deliberately not cryptographic.
pub(crate) struct SplitMix64(u64);

impl SplitMix64 {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

impl rand_core::TryRng for SplitMix64 {
    type Error = Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Infallible> {
        Ok((self.next() >> 32) as u32)
    }

    fn try_next_u64(&mut self) -> Result<u64, Infallible> {
        Ok(self.next())
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Infallible> {
        for chunk in dst.chunks_mut(8) {
            let v = self.next().to_le_bytes();
            chunk.copy_from_slice(&v[..chunk.len()]);
        }
        Ok(())
    }
}

fn apply_determinism(b: &mut WasiCtxBuilder, d: &Determinism) {
    // Distinct streams per interface so that one API's consumption does not
    // shift another's sequence.
    let mut seeds = SplitMix64::new(d.random_seed);
    let secure = seeds.next();
    let insecure = seeds.next();
    let seed = (u128::from(seeds.next()) << 64) | u128::from(seeds.next());
    b.wall_clock(FixedWallClock(Duration::from_nanos(d.wall_clock_ns)))
        .monotonic_clock(FixedMonotonicClock(d.monotonic_ns))
        .secure_random(SplitMix64::new(secure))
        .insecure_random(SplitMix64::new(insecure))
        .insecure_random_seed(seed);
}

fn scan_outputs(root: &Path, limits: &ResourceLimits) -> Result<Vec<OutputFile>, OutputViolation> {
    let mut files = Vec::new();
    let mut entries = 0usize;
    let mut bytes = 0u64;
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let dir = root.join(&rel);
        let unreadable = |e: std::io::Error| OutputViolation::Unreadable {
            path: dir.clone(),
            reason: e.to_string(),
        };
        for entry in std::fs::read_dir(&dir).map_err(unreadable)? {
            let entry = entry.map_err(unreadable)?;
            entries += 1;
            if entries > limits.max_output_entries {
                return Err(OutputViolation::TooManyEntries {
                    count: entries,
                    limit: limits.max_output_entries,
                });
            }
            let child = rel.join(entry.file_name());
            // `file_type` does not follow symlinks.
            let ty = entry.file_type().map_err(unreadable)?;
            if ty.is_dir() {
                stack.push(child);
            } else if ty.is_file() {
                let len = entry.metadata().map_err(unreadable)?.len();
                bytes = bytes.saturating_add(len);
                if bytes > limits.max_output_bytes {
                    return Err(OutputViolation::TooManyBytes {
                        bytes,
                        limit: limits.max_output_bytes,
                    });
                }
                let path = child
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                files.push(OutputFile { path, bytes: len });
            } else {
                return Err(OutputViolation::NonRegularEntry { path: child });
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

// ---- WIT <-> transport DTO conversion ------------------------------------

fn to_wit_span(s: transport::Span) -> wit::Span {
    wit::Span {
        file: s.file,
        start: s.start,
        end: s.end,
    }
}

fn from_wit_span(s: wit::Span) -> transport::Span {
    transport::Span {
        file: s.file,
        start: s.start,
        end: s.end,
    }
}

fn to_wit_delim(d: transport::Delimiter) -> wit::Delimiter {
    match d {
        transport::Delimiter::Paren => wit::Delimiter::Paren,
        transport::Delimiter::Bracket => wit::Delimiter::Bracket,
        transport::Delimiter::Brace => wit::Delimiter::Brace,
    }
}

fn from_wit_delim(d: wit::Delimiter) -> transport::Delimiter {
    match d {
        wit::Delimiter::Paren => transport::Delimiter::Paren,
        wit::Delimiter::Bracket => transport::Delimiter::Bracket,
        wit::Delimiter::Brace => transport::Delimiter::Brace,
    }
}

fn to_wit_token(t: &transport::Token) -> wit::Token {
    use transport::TokenKind as K;
    wit::Token {
        kind: match t.kind {
            K::Ident => wit::TokenKind::Ident,
            K::Punct => wit::TokenKind::Punct,
            K::IntLiteral => wit::TokenKind::IntLiteral,
            K::StringLiteral => wit::TokenKind::StringLiteral,
            K::Open(d) => wit::TokenKind::Open(to_wit_delim(d)),
            K::Close(d) => wit::TokenKind::Close(to_wit_delim(d)),
        },
        text: t.text.clone(),
        span: to_wit_span(t.span),
        scopes: t.scopes.clone(),
        pre_resolved: t.pre_resolved.map(|d| wit::DefId {
            package: d.package,
            item: d.item,
        }),
    }
}

fn to_wit_invocation(i: &transport::Invocation) -> wit::Invocation {
    wit::Invocation {
        protocol_version: i.protocol_version,
        step: match i.step {
            transport::StepKind::BuildScript => wit::StepKind::BuildScript,
            transport::StepKind::ProcMacro => wit::StepKind::ProcMacro,
            transport::StepKind::DeriveMacro => wit::StepKind::DeriveMacro,
        },
        entry: i.entry.clone(),
        args: i.args.clone(),
        tokens: i.tokens.iter().map(to_wit_token).collect(),
    }
}

fn from_wit_kind(k: wit::TokenKind) -> transport::TokenKind {
    use transport::TokenKind as K;
    match k {
        wit::TokenKind::Ident => K::Ident,
        wit::TokenKind::Punct => K::Punct,
        wit::TokenKind::IntLiteral => K::IntLiteral,
        wit::TokenKind::StringLiteral => K::StringLiteral,
        wit::TokenKind::Open(d) => K::Open(from_wit_delim(d)),
        wit::TokenKind::Close(d) => K::Close(from_wit_delim(d)),
    }
}

fn from_wit_severity(s: wit::Severity) -> transport::Severity {
    match s {
        wit::Severity::Error => transport::Severity::Error,
        wit::Severity::Warning => transport::Severity::Warning,
        wit::Severity::Note => transport::Severity::Note,
    }
}

fn slice_range(s: wit::Slice, table_len: usize) -> Option<std::ops::Range<usize>> {
    let start = s.offset as usize;
    let end = start.checked_add(s.len as usize)?;
    (end <= table_len).then_some(start..end)
}

/// Decodes a packed guest result. All slice bounds and size budgets are
/// checked before any per-token string or scope vector is allocated, so
/// aliasing slices cannot amplify host allocation beyond the limits.
fn unpack(
    p: wit::PackedResult,
    limits: &TransportLimits,
    failed: bool,
) -> Result<(Vec<transport::Token>, Vec<transport::Diagnostic>), TransportViolation> {
    use transport::PackedTable as Tbl;
    if failed && !p.tokens.is_empty() {
        return Err(TransportViolation::TokensInFailure {
            count: p.tokens.len(),
        });
    }
    if p.tokens.len() > limits.max_tokens {
        return Err(TransportViolation::TooManyTokens {
            count: p.tokens.len(),
            limit: limits.max_tokens,
        });
    }
    if p.diagnostics.len() > limits.max_diagnostics {
        return Err(TransportViolation::TooManyDiagnostics {
            count: p.diagnostics.len(),
            limit: limits.max_diagnostics,
        });
    }

    let mut text_bytes = 0usize;
    let mut scope_count = 0usize;
    for (index, t) in p.tokens.iter().enumerate() {
        let text =
            slice_range(t.text, p.text.len()).ok_or(TransportViolation::SliceOutOfBounds {
                table: Tbl::TokenText,
                index,
            })?;
        let scopes =
            slice_range(t.scopes, p.scopes.len()).ok_or(TransportViolation::SliceOutOfBounds {
                table: Tbl::TokenScopes,
                index,
            })?;
        if scopes.len() > limits.max_scopes_per_token {
            return Err(TransportViolation::TooManyScopes {
                token: index,
                count: scopes.len(),
                limit: limits.max_scopes_per_token,
            });
        }
        text_bytes = text_bytes.saturating_add(text.len());
        scope_count = scope_count.saturating_add(scopes.len());
    }
    for (index, d) in p.diagnostics.iter().enumerate() {
        let msg =
            slice_range(d.message, p.text.len()).ok_or(TransportViolation::SliceOutOfBounds {
                table: Tbl::DiagnosticText,
                index,
            })?;
        text_bytes = text_bytes.saturating_add(msg.len());
    }
    if text_bytes > limits.max_text_bytes {
        return Err(TransportViolation::TooMuchText {
            bytes: text_bytes,
            limit: limits.max_text_bytes,
        });
    }
    if scope_count > limits.max_total_scopes {
        return Err(TransportViolation::TooManyScopesTotal {
            count: scope_count,
            limit: limits.max_total_scopes,
        });
    }

    let text_at = |s: wit::Slice, table: Tbl, index: usize| {
        let range = slice_range(s, p.text.len()).expect("checked above");
        std::str::from_utf8(&p.text[range])
            .map(str::to_owned)
            .map_err(|_| TransportViolation::InvalidUtf8 { table, index })
    };
    let mut tokens = Vec::with_capacity(p.tokens.len());
    for (index, t) in p.tokens.iter().enumerate() {
        let scopes = slice_range(t.scopes, p.scopes.len()).expect("checked above");
        tokens.push(transport::Token {
            kind: from_wit_kind(t.kind),
            text: text_at(t.text, Tbl::TokenText, index)?,
            span: from_wit_span(t.span),
            scopes: p.scopes[scopes].to_vec(),
            pre_resolved: t.pre_resolved.map(|d| transport::DefId {
                package: d.package,
                item: d.item,
            }),
        });
    }
    let mut diagnostics = Vec::with_capacity(p.diagnostics.len());
    for (index, d) in p.diagnostics.iter().enumerate() {
        diagnostics.push(transport::Diagnostic {
            severity: from_wit_severity(d.severity),
            message: text_at(d.message, Tbl::DiagnosticText, index)?,
            span: d.span.map(from_wit_span),
        });
    }
    // Structural checks (delimiter balance) on the decoded stream.
    limits.check_tokens(&tokens)?;
    Ok((tokens, diagnostics))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitmix_sequence_is_stable() {
        // Reference values for seed 0 (Vigna's splitmix64.c).
        let mut r = SplitMix64::new(0);
        assert_eq!(r.next(), 0xE220_A839_7B1D_CDAF);
        assert_eq!(r.next(), 0x6E78_9E6A_A1B9_65F4);
    }

    #[test]
    fn limiter_aggregates_across_memories() {
        let limits = ResourceLimits {
            max_memory_bytes: 100,
            ..ResourceLimits::default()
        };
        let mut l = Limiter::new(&limits);
        assert!(l.memory_growing(0, 60, None).unwrap());
        // A second memory pushing the aggregate above the limit is denied.
        assert!(!l.memory_growing(0, 50, None).unwrap());
        assert_eq!(l.memory_denied, Some(110));
        assert_eq!(l.peak_memory, 60);
    }

    #[test]
    fn limiter_ignores_grows_past_declared_maximum_and_rolls_back_failures() {
        let limits = ResourceLimits {
            max_memory_bytes: 100,
            max_table_elements: 10,
            ..ResourceLimits::default()
        };
        let mut l = Limiter::new(&limits);
        assert!(l.memory_growing(0, 40, Some(50)).unwrap());
        // Past the memory's own maximum: refused, uncharged, not our denial.
        for _ in 0..10 {
            assert!(!l.memory_growing(40, 60, Some(50)).unwrap());
        }
        assert_eq!((l.memory, l.memory_denied), (40, None));
        // Approved, then failed inside Wasmtime: the charge is restored.
        assert!(l.memory_growing(40, 90, None).unwrap());
        l.memory_grow_failed(wasmtime::format_err!("mmap failed"))
            .unwrap();
        assert_eq!((l.memory, l.peak_memory), (40, 40));
        assert!(l.memory_growing(40, 100, None).unwrap());
        assert_eq!(l.peak_memory, 100);

        assert!(l.table_growing(0, 5, None).unwrap());
        assert!(!l.table_growing(5, 8, Some(6)).unwrap());
        assert!(l.table_growing(5, 9, None).unwrap());
        l.table_grow_failed(wasmtime::format_err!("oom")).unwrap();
        assert_eq!((l.table, l.table_denied), (5, None));
    }
}
