//! The execution layer: snapshot, key, verified lookup, sandboxed run and
//! no-replace publication of raw, untrusted wire output.
//!
//! This layer knows nothing about hygiene or authority. What it caches is a
//! [`RawRecord`]: an untrusted, bounded result that every use re-imports
//! through a fresh Forge session (see [`crate::ForgeSession::expand`]).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use vesper_loom_cas::{
    CasError, DerivationInputs, DerivationKey, Domain, MaterializeOptions, ObjectId, ObjectKind,
    Store, blob_id,
};
use vesper_loom_sandbox::{
    Invocation, SandboxHost, Severity, StepComponent, StepConfig, transport::Token,
};

use crate::error::{RunnerError, io};
use crate::identity::HostIdentity;
use crate::key::{canonical_invocation, derivation_inputs};
use crate::record::RawRecord;
use crate::request::{MacroRequest, RequestError, RunnerLimits};

const RUN_PREFIX: &str = "macro-run-";

/// How the execution cache is used.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CachePolicy {
    /// Reuse a verified hit; otherwise execute and publish.
    #[default]
    ReadWrite,
    /// Always execute; an existing record must name the identical output
    /// (otherwise `DivergentExecution`), and a missing one is published.
    Verify,
    /// Execute without reading or writing records. The only mode available
    /// when the host identity is incomplete.
    Uncached,
}

/// What to do with a corrupt, dangling, stale or inconsistent entry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CorruptionPolicy {
    /// Report `CorruptCache`; execute nothing.
    #[default]
    Fail,
    /// Execute again. Re-storing the true bytes makes the store quarantine
    /// a corrupt object or record file and publish the verified one. A
    /// well-formed record naming different output is never replaced and is
    /// reported as `DivergentExecution`.
    Rerun,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecOptions {
    pub cache: CachePolicy,
    pub on_corrupt: CorruptionPolicy,
}

/// How a raw execution result was obtained.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CacheStatus {
    /// Verified hit; nothing executed.
    Hit,
    /// No record existed; executed and published.
    Miss,
    /// A corrupt entry was found and quarantined; executed again.
    Repaired,
    /// Executed under `Verify` and matched or published.
    Verified,
    /// Executed under `Uncached`; nothing recorded.
    Uncached,
}

/// One raw execution: untrusted output plus how it was obtained.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawExecution {
    pub status: CacheStatus,
    pub executed: bool,
    /// Whether this call created the record.
    pub newly_recorded: bool,
    pub key: DerivationKey,
    /// CAS identity of the raw record (Artifact blob).
    pub record_id: ObjectId,
    pub record: RawRecord,
}

/// A Wasmtime host plus compiled components, shareable between runners.
pub struct MacroEngine {
    host: SandboxHost,
    compiled: Mutex<HashMap<ObjectId, StepComponent>>,
    executions: AtomicU64,
}

impl std::fmt::Debug for MacroEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MacroEngine")
            .field("executions", &self.executions())
            .finish_non_exhaustive()
    }
}

impl MacroEngine {
    pub fn new() -> Result<Self, RunnerError> {
        Ok(Self {
            host: SandboxHost::new()?,
            compiled: Mutex::new(HashMap::new()),
            executions: AtomicU64::new(0),
        })
    }

    /// Number of guest executions started through this engine.
    pub fn executions(&self) -> u64 {
        self.executions.load(Ordering::SeqCst)
    }

    fn component(&self, id: ObjectId, bytes: &[u8]) -> Result<StepComponent, RunnerError> {
        if let Some(c) = self.compiled.lock().expect("compile cache").get(&id) {
            return Ok(c.clone());
        }
        let c = self.host.compile(bytes)?;
        self.compiled
            .lock()
            .expect("compile cache")
            .insert(id, c.clone());
        Ok(c)
    }
}

/// Snapshots, key and resolved host paths of one execution. Nothing has run.
#[derive(Clone, Debug)]
pub struct PreparedExecution {
    request: MacroRequest,
    invocation: Invocation,
    identity: HostIdentity,
    component: ObjectId,
    invocation_id: ObjectId,
    mounts: Vec<(String, ObjectId)>,
    inputs: DerivationInputs,
    key: DerivationKey,
}

impl PreparedExecution {
    pub fn key(&self) -> DerivationKey {
        self.key
    }
    pub fn derivation_inputs(&self) -> &DerivationInputs {
        &self.inputs
    }
    pub fn component(&self) -> ObjectId {
        self.component
    }
    pub fn invocation(&self) -> &Invocation {
        &self.invocation
    }
    pub fn invocation_id(&self) -> ObjectId {
        self.invocation_id
    }
    /// `(guest path, snapshot tree)` in guest-path order.
    pub fn snapshots(&self) -> &[(String, ObjectId)] {
        &self.mounts
    }
}

/// Executes macro components against one CAS store and scratch area.
#[derive(Debug)]
pub struct MacroRunner {
    engine: Arc<MacroEngine>,
    store: Store,
    scratch: PathBuf,
    identity: HostIdentity,
    caps: RunnerLimits,
}

impl MacroRunner {
    pub fn open(
        engine: Arc<MacroEngine>,
        store_root: impl AsRef<Path>,
        scratch: impl AsRef<Path>,
    ) -> Result<Self, RunnerError> {
        let store = Store::open(store_root)?;
        let scratch = scratch.as_ref();
        fs::create_dir_all(scratch).map_err(io("create scratch", scratch))?;
        let scratch = scratch
            .canonicalize()
            .map_err(io("canonicalize scratch", scratch))?;
        let root = store.root().to_owned();
        if scratch.starts_with(&root) || root.starts_with(&scratch) {
            return Err(RequestError::LayoutOverlap {
                input: scratch,
                area: root,
            }
            .into());
        }
        Ok(Self {
            engine,
            store,
            scratch,
            identity: HostIdentity::current(),
            caps: RunnerLimits::default(),
        })
    }

    pub fn with_caps(mut self, caps: RunnerLimits) -> Self {
        self.caps = caps;
        self
    }

    /// Replaces the measured host identity. Only for tests and for hosts
    /// that verify an equally complete identity by other means: a wrong
    /// identity lets one implementation reuse another's executions.
    pub fn with_identity_unchecked(mut self, identity: HostIdentity) -> Self {
        self.identity = identity;
        self
    }

    pub fn identity(&self) -> &HostIdentity {
        &self.identity
    }
    pub fn store(&self) -> &Store {
        &self.store
    }
    pub fn engine(&self) -> &Arc<MacroEngine> {
        &self.engine
    }
    pub fn caps(&self) -> &RunnerLimits {
        &self.caps
    }

    /// Validates the request, snapshots the component, the canonical
    /// invocation and every input, and computes the execution key.
    pub fn prepare(
        &self,
        request: &MacroRequest,
        tokens: Vec<Token>,
    ) -> Result<PreparedExecution, RunnerError> {
        request.validate(&self.caps)?;
        let invocation = Invocation::new(request.kind.step(), request.entry.clone())
            .with_args(request.args.iter().cloned())
            .with_tokens(tokens);
        request
            .limits
            .transport
            .check_invocation(&invocation)
            .map_err(|v| {
                RunnerError::Sandbox(vesper_loom_sandbox::SandboxError::Transport {
                    direction: vesper_loom_sandbox::transport::Direction::Input,
                    violation: v,
                })
            })?;
        let mut hosts = Vec::new();
        for i in &request.inputs {
            let host = i
                .host_path
                .canonicalize()
                .map_err(|e| RunnerError::Snapshot {
                    guest_path: i.guest_path.clone(),
                    reason: e.to_string(),
                })?;
            for area in [self.store.root(), self.scratch.as_path()] {
                if host.starts_with(area) || area.starts_with(&host) {
                    return Err(RequestError::LayoutOverlap {
                        input: host,
                        area: area.to_owned(),
                    }
                    .into());
                }
            }
            hosts.push(host);
        }

        let component = self
            .store
            .put_blob(Domain::Artifact, &request.component)?
            .id;
        let invocation_id = self
            .store
            .put_blob(Domain::Source, &canonical_invocation(&invocation))?
            .id;
        let mut mounts = Vec::new();
        for (spec, host) in request.inputs.iter().zip(&hosts) {
            mounts.push((
                spec.guest_path.clone(),
                snapshot_stable(&self.store, host, &spec.guest_path)?,
            ));
        }
        // Preopen order is observable, so mounts are exposed in the key's
        // (sorted) order.
        let mut order: Vec<usize> = (0..mounts.len()).collect();
        order.sort_by(|&a, &b| mounts[a].0.cmp(&mounts[b].0));
        let mounts: Vec<_> = order.iter().map(|&i| mounts[i].clone()).collect();
        let inputs = derivation_inputs(request, &self.identity, component, invocation_id, &mounts);
        let key = inputs
            .key()
            .map_err(|e| RunnerError::Cas(CasError::InvalidDerivation(e)))?;
        Ok(PreparedExecution {
            request: request.clone(),
            invocation,
            identity: self.identity.clone(),
            component,
            invocation_id,
            mounts,
            inputs,
            key,
        })
    }

    /// Produces the raw result for `prepared` from a verified record or by
    /// executing it. On any failure nothing is published.
    pub fn execute(
        &self,
        prepared: &PreparedExecution,
        options: ExecOptions,
    ) -> Result<RawExecution, RunnerError> {
        if prepared.identity != self.identity {
            return Err(RunnerError::IdentityMismatch);
        }
        if options.cache != CachePolicy::Uncached && !self.identity.is_complete() {
            return Err(RunnerError::IncompleteIdentity {
                missing: self.identity.missing(),
            });
        }
        let key = prepared.key;
        let mut existing = None;
        let mut repaired = false;
        if options.cache != CachePolicy::Uncached {
            match self.lookup_verified(prepared) {
                Ok(found) => existing = found,
                Err(reason) => match options.on_corrupt {
                    CorruptionPolicy::Fail => {
                        return Err(RunnerError::CorruptCache { key, reason });
                    }
                    CorruptionPolicy::Rerun => repaired = true,
                },
            }
        }
        if options.cache == CachePolicy::ReadWrite
            && let Some((record_id, record)) = existing.take()
        {
            return Ok(RawExecution {
                status: CacheStatus::Hit,
                executed: false,
                newly_recorded: false,
                key,
                record_id,
                record,
            });
        }

        let (record, bytes) = self.run(prepared)?;
        let record_id = blob_id(Domain::Artifact, &bytes);
        let mut newly_recorded = false;
        if options.cache != CachePolicy::Uncached {
            if let Some((prev, _)) = &existing
                && *prev != record_id
            {
                return Err(RunnerError::DivergentExecution {
                    key,
                    existing: *prev,
                    proposed: record_id,
                });
            }
            let stored = self.store.put_blob(Domain::Artifact, &bytes)?;
            debug_assert_eq!(stored.id, record_id);
            match self.store.publish_derivation(&prepared.inputs, record_id) {
                Ok(o) => newly_recorded = o.newly_recorded,
                Err(CasError::DerivationConflict {
                    existing, proposed, ..
                }) => {
                    return Err(RunnerError::DivergentExecution {
                        key,
                        existing,
                        proposed,
                    });
                }
                Err(e) => return Err(e.into()),
            }
        }
        let status = match (options.cache, repaired) {
            (CachePolicy::Uncached, _) => CacheStatus::Uncached,
            (_, true) => CacheStatus::Repaired,
            (CachePolicy::Verify, false) => CacheStatus::Verified,
            (CachePolicy::ReadWrite, false) => CacheStatus::Miss,
        };
        Ok(RawExecution {
            status,
            executed: true,
            newly_recorded,
            key,
            record_id,
            record,
        })
    }

    /// `Ok(None)` on a miss; `Ok(Some)` for a fully verified record;
    /// `Err(reason)` for anything corrupt, dangling, stale or inconsistent.
    fn lookup_verified(
        &self,
        prepared: &PreparedExecution,
    ) -> Result<Option<(ObjectId, RawRecord)>, String> {
        let id = match self.store.lookup_derivation(&prepared.inputs) {
            Ok(None) => return Ok(None),
            Ok(Some(id)) => id,
            Err(e) => return Err(format!("execution record: {e}")),
        };
        if id.domain != Domain::Artifact || id.kind != ObjectKind::Blob {
            return Err(format!("record names {id}, not an artifact blob"));
        }
        // Rehash while streaming (no retention) before reading the bytes,
        // so an oversized object is rejected without being loaded.
        let size = self
            .store
            .verify_object(&id)
            .map_err(|e| format!("raw output {id}: {e}"))?;
        if size > self.caps.max_record_bytes as u64 {
            return Err(format!(
                "raw output {id}: {size} bytes exceeds {}",
                self.caps.max_record_bytes
            ));
        }
        let bytes = self
            .store
            .read_blob(&id)
            .map_err(|e| format!("raw output {id}: {e}"))?;
        let limits = prepared.request.record_limits(&self.caps);
        let record =
            RawRecord::decode(&bytes, &limits).map_err(|e| format!("raw output {id}: {e}"))?;
        if record.key != prepared.key.0.0 {
            return Err(format!(
                "raw output {id} belongs to another execution, not {}",
                prepared.key
            ));
        }
        Ok(Some((id, record)))
    }

    /// Runs the component in a fresh scratch directory against fresh
    /// materializations of the snapshots and a fresh, empty `/out`.
    fn run(&self, prepared: &PreparedExecution) -> Result<(RawRecord, Vec<u8>), RunnerError> {
        let req = &prepared.request;
        let run = ScratchDir::create(&self.scratch)?;
        let out = run.path.join("out");
        fs::create_dir(&out).map_err(io("create output", &out))?;
        let mut config = StepConfig::new(&out);
        let mut materialized = Vec::new();
        for (i, (guest, tree)) in prepared.mounts.iter().enumerate() {
            let dir = run.path.join(format!("in-{i}"));
            self.store
                .materialize(tree, &dir, MaterializeOptions::default())?;
            config = config.input(&dir, guest);
            materialized.push((guest, tree, dir));
        }
        config.limits = req.limits.clone();
        config.determinism = req.determinism;

        let step = self.engine.component(prepared.component, &req.component)?;
        self.engine.executions.fetch_add(1, Ordering::SeqCst);
        let report = self.engine.host.run(&step, &config, &prepared.invocation)?;
        let outcome = match report.result {
            Err(diagnostics) => return Err(RunnerError::GuestFailed { diagnostics }),
            Ok(o) if o.diagnostics.iter().any(|d| d.severity == Severity::Error) => {
                return Err(RunnerError::GuestFailed {
                    diagnostics: o.diagnostics,
                });
            }
            Ok(o) => o,
        };
        if !report.outputs.is_empty() {
            return Err(RunnerError::OutputFilesWritten {
                files: report.outputs,
            });
        }
        for (guest, tree, dir) in &materialized {
            let after = self.store.ingest_dir(Domain::Source, dir).map_err(|_| {
                RunnerError::InputMutatedDuringRun {
                    guest_path: (*guest).clone(),
                }
            })?;
            if after.id != **tree {
                return Err(RunnerError::InputMutatedDuringRun {
                    guest_path: (*guest).clone(),
                });
            }
        }
        let record = RawRecord {
            key: prepared.key.0.0,
            fuel_consumed: report.fuel_consumed,
            peak_memory_bytes: report.peak_memory_bytes as u64,
            outcome,
            stdout: report.stdout,
            stderr: report.stderr,
        };
        let len = record.encoded_len();
        if len > self.caps.max_record_bytes {
            return Err(RunnerError::RecordTooLarge {
                bytes: len,
                limit: self.caps.max_record_bytes,
            });
        }
        let bytes = record.encode();
        // What is published must be exactly what a later hit accepts.
        let back = RawRecord::decode(&bytes, &req.record_limits(&self.caps)).map_err(|e| {
            RunnerError::Sandbox(vesper_loom_sandbox::SandboxError::Host {
                message: format!("raw record does not round-trip: {e}"),
            })
        })?;
        debug_assert_eq!(back, record);
        Ok((record, bytes))
    }
}

/// Ingests `host` twice; the passes must agree, or the input was changing
/// while it was read and no snapshot is labeled immutable.
fn snapshot_stable(store: &Store, host: &Path, guest_path: &str) -> Result<ObjectId, RunnerError> {
    let snap = || {
        store
            .ingest_dir(Domain::Source, host)
            .map(|i| i.id)
            .map_err(|e| RunnerError::Snapshot {
                guest_path: guest_path.to_owned(),
                reason: e.to_string(),
            })
    };
    let first = snap()?;
    let second = snap()?;
    if first != second {
        return Err(RunnerError::Snapshot {
            guest_path: guest_path.to_owned(),
            reason: "input changed while it was snapshotted".into(),
        });
    }
    Ok(first)
}

/// A uniquely named run directory, removed on drop.
struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn create(parent: &Path) -> Result<Self, RunnerError> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..16 {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let name = format!(
                "{RUN_PREFIX}{}-{}-{nanos}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            let path = parent.join(name);
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(io("create run directory", &path)(e)),
            }
        }
        Err(RunnerError::Io {
            op: "create run directory",
            path: parent.to_owned(),
            source: std::io::Error::from(std::io::ErrorKind::AlreadyExists),
        })
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = make_writable(&self.path);
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Materialized inputs may be read-only; restore write permission so the
/// run directory can be removed.
fn make_writable(path: &Path) -> std::io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        let mut perm = meta.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        let _ = fs::set_permissions(path, perm);
        for e in fs::read_dir(path)? {
            make_writable(&e?.path())?;
        }
    }
    Ok(())
}
