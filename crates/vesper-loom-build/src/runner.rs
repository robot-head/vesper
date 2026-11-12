//! The offline build-step runner.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use vesper_loom_cas::{
    CasError, DerivationInputs, DerivationKey, Domain, EntryKind, MaterializeOptions, ObjectId,
    ObjectKind, Store,
};
use vesper_loom_sandbox::transport::Outcome;
use vesper_loom_sandbox::{
    OutputFile, SandboxHost, Severity, StepComponent, StepConfig, StepReport,
};

use crate::error::{BuildError, RequestError, io};
use crate::fsx::{ScratchDir, remove_tree, replace_dir, unique_name};
use crate::identity::ExecutionIdentity;
use crate::key::{canonical_invocation, derivation_inputs};
use crate::manifest::{RESULT_MANIFEST_SCHEMA, ResultManifest};
use crate::request::BuildRequest;

/// Version of [`BuildOutcome`].
pub const BUILD_OUTCOME_VERSION: u32 = 1;

const RUN_PREFIX: &str = "run-";
const DEST_STAGE_PREFIX: &str = ".vlb-stage-";

/// How the derivation cache is used.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CachePolicy {
    /// Reuse a verified hit; otherwise execute and publish.
    #[default]
    ReadWrite,
    /// Always execute. If a result is recorded, the new one must be
    /// identical (otherwise `DivergentOutput`); if not, publish it.
    Verify,
    /// Execute without reading or writing derivation records. The only
    /// mode available when the execution fingerprint is incomplete.
    Uncached,
}

/// What to do with a corrupt, dangling or inconsistent cache entry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CorruptionPolicy {
    /// Report `CorruptCache` and do nothing else.
    #[default]
    Fail,
    /// Execute again. Re-ingesting the true bytes repairs corrupt objects
    /// and records (the store quarantines the bad files); a different
    /// result is still reported as `DivergentOutput`.
    Rebuild,
}

/// What may happen to an existing destination directory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DestinationPolicy {
    /// The destination must not exist; checked before execution.
    #[default]
    CreateNew,
    /// An existing directory is replaced only after a result is accepted.
    Replace,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunOptions {
    pub cache: CachePolicy,
    pub on_corrupt: CorruptionPolicy,
    pub destination: DestinationPolicy,
}

/// How a result was obtained.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CacheStatus {
    /// Verified hit; nothing executed.
    Hit,
    /// No record existed; executed.
    Miss,
    /// A corrupt entry was found and the step executed again.
    Repaired,
    /// Executed under `CachePolicy::Verify` and matched or recorded.
    Verified,
    /// Executed under `CachePolicy::Uncached`; nothing recorded.
    Uncached,
}

/// An accepted build result, materialized at `destination`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildOutcome {
    pub version: u32,
    pub key: String,
    pub cache: CacheStatus,
    /// Whether this call created the derivation record.
    pub newly_recorded: bool,
    pub executed: bool,
    pub manifest: String,
    pub output_tree: String,
    pub destination: PathBuf,
    pub outputs: Vec<OutputFile>,
    pub outcome: Outcome,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub fuel_consumed: u64,
    pub peak_memory_bytes: usize,
}

/// A Wasmtime host plus a cache of compiled components, shareable between
/// runners and threads.
pub struct BuildEngine {
    host: SandboxHost,
    compiled: Mutex<HashMap<ObjectId, StepComponent>>,
    executions: AtomicU64,
}

impl std::fmt::Debug for BuildEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuildEngine")
            .field("executions", &self.executions())
            .finish_non_exhaustive()
    }
}

impl BuildEngine {
    pub fn new() -> Result<Self, BuildError> {
        Ok(BuildEngine {
            host: SandboxHost::new()?,
            compiled: Mutex::new(HashMap::new()),
            executions: AtomicU64::new(0),
        })
    }

    /// Number of sandbox executions started through this engine.
    pub fn executions(&self) -> u64 {
        self.executions.load(Ordering::SeqCst)
    }

    /// Compiles `bytes`, whose identity is `id` (the caller hashed exactly
    /// these bytes). Compiled components are cached by identity.
    fn component(&self, id: ObjectId, bytes: &[u8]) -> Result<StepComponent, BuildError> {
        if let Some(c) = self.compiled.lock().unwrap().get(&id) {
            return Ok(c.clone());
        }
        let c = self.host.compile(bytes)?;
        self.compiled.lock().unwrap().insert(id, c.clone());
        Ok(c)
    }

    fn run(
        &self,
        step: &StepComponent,
        config: &StepConfig,
        request: &BuildRequest,
    ) -> Result<StepReport, BuildError> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(self.host.run(step, config, &request.invocation)?)
    }
}

/// A validated request whose inputs are snapshotted in the store.
#[derive(Clone, Debug)]
pub struct PreparedBuild {
    request: BuildRequest,
    component: ObjectId,
    invocation: ObjectId,
    mounts: Vec<(String, ObjectId)>,
    inputs: DerivationInputs,
    key: DerivationKey,
}

impl PreparedBuild {
    pub fn key(&self) -> DerivationKey {
        self.key
    }

    /// The exact inputs the key is computed from.
    pub fn derivation_inputs(&self) -> &DerivationInputs {
        &self.inputs
    }

    pub fn component(&self) -> ObjectId {
        self.component
    }

    /// `Source` blob of the canonical invocation encoding.
    pub fn invocation(&self) -> ObjectId {
        self.invocation
    }

    /// `(guest path, Source tree)` of every input snapshot.
    pub fn snapshots(&self) -> &[(String, ObjectId)] {
        &self.mounts
    }
}

/// Runs build steps against one store with one scratch area.
#[derive(Debug)]
pub struct Runner {
    engine: Arc<BuildEngine>,
    store: Store,
    scratch: PathBuf,
    identity: ExecutionIdentity,
}

/// A verified cached result.
struct Verified {
    manifest_id: ObjectId,
    manifest: ResultManifest,
    tree: ObjectId,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Runner {
    /// Opens (or creates) the store and scratch directories.
    pub fn open(
        engine: Arc<BuildEngine>,
        store_root: impl AsRef<Path>,
        scratch_root: impl AsRef<Path>,
    ) -> Result<Self, BuildError> {
        let store = Store::open(store_root.as_ref())?;
        let scratch = scratch_root.as_ref();
        fs::create_dir_all(scratch).map_err(io("create scratch", scratch))?;
        let scratch = scratch
            .canonicalize()
            .map_err(io("canonicalize scratch", scratch))?;
        Ok(Runner {
            engine,
            store,
            scratch,
            identity: ExecutionIdentity::current(),
        })
    }

    /// Replaces the execution identity bound into keys.
    ///
    /// For embedders that run a differently configured host and for tests.
    /// The identity is not checked against the real engine: claiming the
    /// identity of another configuration makes its cached results reusable
    /// here, which is unsound if they differ.
    pub fn with_identity_unchecked(mut self, identity: ExecutionIdentity) -> Self {
        self.identity = identity;
        self
    }

    pub fn identity(&self) -> &ExecutionIdentity {
        &self.identity
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn engine(&self) -> &Arc<BuildEngine> {
        &self.engine
    }

    /// Validates, then snapshots the component, invocation and every input
    /// into the store, and computes the derivation key. Nothing executes.
    pub fn prepare(&self, request: &BuildRequest) -> Result<PreparedBuild, BuildError> {
        request.validate()?;
        let hosts = request.canonical_inputs()?;
        let store_root = self
            .store
            .root()
            .canonicalize()
            .map_err(io("canonicalize store", self.store.root()))?;
        for input in &hosts {
            for (what, path) in [("store", &store_root), ("scratch", &self.scratch)] {
                if path.starts_with(input) || input.starts_with(path) {
                    return Err(RequestError::LayoutOverlap {
                        what,
                        path: path.clone(),
                        input: input.clone(),
                    }
                    .into());
                }
            }
        }

        let component = self
            .store
            .put_blob(Domain::Artifact, &request.component)?
            .id;
        let invocation = self
            .store
            .put_blob(Domain::Source, &canonical_invocation(&request.invocation))?
            .id;
        let mut mounts = Vec::new();
        for (spec, host) in request.inputs.iter().zip(&hosts) {
            let tree = snapshot_stable(&self.store, host, &spec.guest_path, &mut || {})?;
            mounts.push((spec.guest_path.clone(), tree));
        }
        let inputs = derivation_inputs(request, &self.identity, component, invocation, &mounts);
        let key = inputs
            .key()
            .map_err(|e| BuildError::from(CasError::InvalidDerivation(e)))?;
        Ok(PreparedBuild {
            request: request.clone(),
            component,
            invocation,
            mounts,
            inputs,
            key,
        })
    }

    /// `prepare` followed by `execute`.
    pub fn build(
        &self,
        request: &BuildRequest,
        destination: impl AsRef<Path>,
        options: RunOptions,
    ) -> Result<BuildOutcome, BuildError> {
        let prepared = self.prepare(request)?;
        self.execute(&prepared, destination, options)
    }

    /// Produces the result for `prepared` at `destination`, from a verified
    /// cache hit or by executing the step against fresh materializations of
    /// the snapshots. On any failure no derivation is published and an
    /// existing destination is left untouched.
    pub fn execute(
        &self,
        prepared: &PreparedBuild,
        destination: impl AsRef<Path>,
        options: RunOptions,
    ) -> Result<BuildOutcome, BuildError> {
        let dest = destination.as_ref();
        self.check_destination(prepared, dest, options.destination)?;
        if options.cache != CachePolicy::Uncached && !self.identity.is_complete() {
            return Err(BuildError::UnknownFingerprint {
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
                    CorruptionPolicy::Fail => return Err(BuildError::CorruptCache { key, reason }),
                    CorruptionPolicy::Rebuild => repaired = true,
                },
            }
        }
        if options.cache == CachePolicy::ReadWrite
            && let Some(hit) = existing.take()
        {
            self.place(&hit.tree, dest, options.destination)?;
            return Ok(outcome(&hit, key, CacheStatus::Hit, false, false, dest));
        }

        let fresh = self.run_step(prepared)?;
        let mut newly_recorded = false;
        if options.cache != CachePolicy::Uncached {
            if let Some(prev) = &existing
                && prev.manifest_id != fresh.manifest_id
            {
                return Err(BuildError::DivergentOutput {
                    key,
                    existing: prev.manifest_id,
                    proposed: fresh.manifest_id,
                });
            }
            match self
                .store
                .publish_derivation(&prepared.inputs, fresh.manifest_id)
            {
                Ok(o) => newly_recorded = o.newly_recorded,
                Err(CasError::DerivationConflict {
                    existing, proposed, ..
                }) => {
                    return Err(BuildError::DivergentOutput {
                        key,
                        existing,
                        proposed,
                    });
                }
                Err(e) => return Err(e.into()),
            }
        }
        self.place(&fresh.tree, dest, options.destination)?;
        let status = match (options.cache, repaired) {
            (CachePolicy::Uncached, _) => CacheStatus::Uncached,
            (_, true) => CacheStatus::Repaired,
            (CachePolicy::Verify, false) => CacheStatus::Verified,
            (CachePolicy::ReadWrite, false) => CacheStatus::Miss,
        };
        Ok(outcome(&fresh, key, status, newly_recorded, true, dest))
    }

    /// Removes scratch runs and destination staging directories left by
    /// killed processes, plus the store's stale temporaries. Only entries
    /// older than `min_age` are touched, so live runs are not disturbed
    /// when `min_age` exceeds the longest run.
    pub fn sweep_stale(&self, min_age: Duration) -> Result<usize, BuildError> {
        let mut removed = self.store.sweep_temporaries(min_age)?;
        let entries = fs::read_dir(&self.scratch).map_err(io("read scratch", &self.scratch))?;
        for entry in entries.flatten() {
            let name = entry.file_name();
            if !name.to_string_lossy().starts_with(RUN_PREFIX) {
                continue;
            }
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age >= min_age);
            if old {
                remove_tree(&entry.path()).map_err(io("remove stale run", entry.path()))?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    // ------------------------------------------------------------------

    fn check_destination(
        &self,
        prepared: &PreparedBuild,
        dest: &Path,
        policy: DestinationPolicy,
    ) -> Result<(), BuildError> {
        let bad = |reason: &str| BuildError::Destination {
            path: dest.to_owned(),
            reason: reason.to_owned(),
        };
        let name = dest
            .file_name()
            .ok_or_else(|| bad("has no final component"))?;
        let parent = match dest.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        let parent = parent
            .canonicalize()
            .map_err(|e| bad(&format!("parent unavailable: {e}")))?;
        let full = parent.join(name);
        let store_root = self.store.root().canonicalize().unwrap_or_default();
        for host in prepared.request.canonical_inputs()? {
            if full.starts_with(&host) || host.starts_with(&full) {
                return Err(RequestError::LayoutOverlap {
                    what: "destination",
                    path: full,
                    input: host,
                }
                .into());
            }
        }
        if full.starts_with(&store_root) || full.starts_with(&self.scratch) {
            return Err(bad("lies inside the store or scratch area"));
        }
        match fs::symlink_metadata(dest) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(bad(&e.to_string())),
            Ok(_) if policy == DestinationPolicy::CreateNew => Err(bad("already exists")),
            Ok(m) if m.is_dir() => Ok(()),
            Ok(_) => Err(bad("exists and is not a directory")),
        }
    }

    /// `Ok(None)` on a miss, `Ok(Some)` for a fully verified hit, `Err` with
    /// a reason for anything corrupt, dangling or inconsistent.
    fn lookup_verified(&self, prepared: &PreparedBuild) -> Result<Option<Verified>, String> {
        let id = match self.store.lookup_derivation(&prepared.inputs) {
            Ok(None) => return Ok(None),
            Ok(Some(id)) => id,
            Err(e) => return Err(format!("derivation record: {e}")),
        };
        if id.domain != Domain::Artifact || id.kind != ObjectKind::Blob {
            return Err(format!("record names {id}, not an artifact blob"));
        }
        let bytes = self
            .store
            .read_blob(&id)
            .map_err(|e| format!("manifest {id}: {e}"))?;
        let manifest = ResultManifest::decode(&bytes).map_err(|e| format!("manifest {id}: {e}"))?;
        if manifest.key != prepared.key.to_string() {
            return Err(format!("manifest {id} belongs to {}", manifest.key));
        }
        let tree = parse_id(&manifest.output_tree, ObjectKind::Tree)?;
        let mut listing = Vec::new();
        self.verify_tree(&tree, "", &mut listing)?;
        if listing != manifest.outputs {
            return Err("output tree does not match the manifest's file list".into());
        }
        let blob = |text: &str| -> Result<Vec<u8>, String> {
            let id = parse_id(text, ObjectKind::Blob)?;
            self.store.read_blob(&id).map_err(|e| format!("{id}: {e}"))
        };
        let stdout = blob(&manifest.stdout)?;
        let stderr = blob(&manifest.stderr)?;
        Ok(Some(Verified {
            manifest_id: id,
            manifest,
            tree,
            stdout,
            stderr,
        }))
    }

    /// Rehashes every object under `tree` and lists its files.
    fn verify_tree(
        &self,
        tree: &ObjectId,
        prefix: &str,
        out: &mut Vec<OutputFile>,
    ) -> Result<(), String> {
        let t = self
            .store
            .read_tree(tree)
            .map_err(|e| format!("{tree}: {e}"))?;
        for (name, entry) in t.entries() {
            let child = t.child_id(entry);
            let path = format!("{prefix}{name}");
            match entry.kind {
                EntryKind::Directory => self.verify_tree(&child, &format!("{path}/"), out)?,
                EntryKind::File { .. } => {
                    let size = self
                        .store
                        .verify_object(&child)
                        .map_err(|e| format!("{child}: {e}"))?;
                    if size != entry.size {
                        return Err(format!("{child}: size {size} != recorded {}", entry.size));
                    }
                    out.push(OutputFile { path, bytes: size });
                }
            }
        }
        Ok(())
    }

    /// Executes the step in a fresh scratch run and stores the accepted
    /// result (objects and manifest, but no derivation record).
    fn run_step(&self, prepared: &PreparedBuild) -> Result<Verified, BuildError> {
        let req = &prepared.request;
        let run = ScratchDir::create(&self.scratch, RUN_PREFIX)
            .map_err(io("create run directory", &self.scratch))?;
        let mut config = StepConfig::new(run.path().join("out"));
        fs::create_dir(&config.output_dir).map_err(io("create output", &config.output_dir))?;
        let mut materialized = Vec::new();
        for (i, (guest, tree)) in prepared.mounts.iter().enumerate() {
            let dir = run.path().join(format!("in-{i}"));
            self.store
                .materialize(tree, &dir, MaterializeOptions::default())?;
            config = config.input(&dir, guest);
            materialized.push((guest, tree, dir));
        }
        config.limits = req.limits.clone();
        config.determinism = req.determinism;

        let step = self.engine.component(prepared.component, &req.component)?;
        let report = self.engine.run(&step, &config, req)?;
        let outcome = match report.result {
            Err(diagnostics) => return Err(BuildError::GuestFailed { diagnostics }),
            Ok(o) if o.diagnostics.iter().any(|d| d.severity == Severity::Error) => {
                return Err(BuildError::GuestFailed {
                    diagnostics: o.diagnostics,
                });
            }
            Ok(o) => o,
        };
        for (guest, tree, dir) in &materialized {
            let after = self.store.ingest_dir(Domain::Source, dir).map_err(|_| {
                BuildError::InputMutatedDuringRun {
                    guest_path: (*guest).clone(),
                }
            })?;
            if after.id != **tree {
                return Err(BuildError::InputMutatedDuringRun {
                    guest_path: (*guest).clone(),
                });
            }
        }

        let tree = self
            .store
            .ingest_dir(Domain::Artifact, &config.output_dir)
            .map_err(|e| BuildError::OutputRejected(e.to_string()))?
            .id;
        let mut listing = Vec::new();
        self.verify_tree(&tree, "", &mut listing)
            .map_err(BuildError::OutputRejected)?;
        let mut scanned = report.outputs.clone();
        scanned.sort_by(|a, b| a.path.cmp(&b.path));
        if listing != scanned {
            return Err(BuildError::OutputRejected(
                "stored output tree differs from the sandbox's output scan".into(),
            ));
        }
        let stdout = self.store.put_blob(Domain::Artifact, &report.stdout)?.id;
        let stderr = self.store.put_blob(Domain::Artifact, &report.stderr)?.id;
        let manifest = ResultManifest {
            schema: RESULT_MANIFEST_SCHEMA.to_owned(),
            key: prepared.key.to_string(),
            output_tree: tree.to_string(),
            outputs: listing,
            outcome,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            fuel_consumed: report.fuel_consumed,
            peak_memory_bytes: report.peak_memory_bytes,
        };
        let manifest_id = self
            .store
            .put_blob(Domain::Artifact, &manifest.encode())?
            .id;
        Ok(Verified {
            manifest_id,
            manifest,
            tree,
            stdout: report.stdout,
            stderr: report.stderr,
        })
    }

    /// Materializes `tree` at `dest` from the store, never from scratch.
    fn place(
        &self,
        tree: &ObjectId,
        dest: &Path,
        policy: DestinationPolicy,
    ) -> Result<(), BuildError> {
        let opts = MaterializeOptions::default();
        let exists = fs::symlink_metadata(dest).is_ok();
        if !exists {
            return match self.store.materialize(tree, dest, opts) {
                Ok(_) => Ok(()),
                Err(CasError::DestinationExists(p)) => Err(BuildError::Destination {
                    path: p,
                    reason: "appeared during the build".into(),
                }),
                Err(e) => Err(e.into()),
            };
        }
        if policy == DestinationPolicy::CreateNew {
            return Err(BuildError::Destination {
                path: dest.to_owned(),
                reason: "appeared during the build".into(),
            });
        }
        let parent = match dest.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        let staged = parent.join(unique_name(DEST_STAGE_PREFIX));
        if let Err(e) = self.store.materialize(tree, &staged, opts) {
            let _ = remove_tree(&staged);
            return Err(e.into());
        }
        replace_dir(&staged, dest).map_err(|e| {
            let _ = remove_tree(&staged);
            BuildError::Io {
                op: "replace destination",
                path: dest.to_owned(),
                source: e,
            }
        })
    }
}

/// Ingests `host` twice; the two passes must produce the same tree, or the
/// input was changing while it was read and no snapshot is labeled
/// immutable. `between` runs between the passes (a test seam).
fn snapshot_stable(
    store: &Store,
    host: &Path,
    guest_path: &str,
    between: &mut dyn FnMut(),
) -> Result<ObjectId, BuildError> {
    let snap = || {
        store
            .ingest_dir(Domain::Source, host)
            .map(|i| i.id)
            .map_err(|source| BuildError::Snapshot {
                guest_path: guest_path.to_owned(),
                source: Box::new(source),
            })
    };
    let first = snap()?;
    between();
    let second = snap()?;
    if first != second {
        return Err(BuildError::InputChangedDuringSnapshot {
            guest_path: guest_path.to_owned(),
            first,
            second,
        });
    }
    Ok(first)
}

fn parse_id(text: &str, kind: ObjectKind) -> Result<ObjectId, String> {
    let id: ObjectId = text
        .parse()
        .map_err(|_| format!("invalid object id `{text}`"))?;
    if id.domain != Domain::Artifact || id.kind != kind {
        return Err(format!("{id} is not an artifact {}", kind.as_str()));
    }
    Ok(id)
}

fn outcome(
    v: &Verified,
    key: DerivationKey,
    cache: CacheStatus,
    newly_recorded: bool,
    executed: bool,
    dest: &Path,
) -> BuildOutcome {
    BuildOutcome {
        version: BUILD_OUTCOME_VERSION,
        key: key.to_string(),
        cache,
        newly_recorded,
        executed,
        manifest: v.manifest_id.to_string(),
        output_tree: v.tree.to_string(),
        destination: dest.to_owned(),
        outputs: v.manifest.outputs.clone(),
        outcome: v.manifest.outcome.clone(),
        stdout: v.stdout.clone(),
        stderr: v.stderr.clone(),
        fuel_consumed: v.manifest.fuel_consumed,
        peak_memory_bytes: v.manifest.peak_memory_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_refuses_inputs_that_change_between_passes() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store")).unwrap();
        let input = dir.path().join("in");
        fs::create_dir(&input).unwrap();
        fs::write(input.join("a"), b"one").unwrap();

        let stable = snapshot_stable(&store, &input, "/in", &mut || {}).unwrap();
        assert_eq!(stable, store.ingest_dir(Domain::Source, &input).unwrap().id);

        let err = snapshot_stable(&store, &input, "/in", &mut || {
            fs::write(input.join("a"), b"two").unwrap();
        })
        .unwrap_err();
        match err {
            BuildError::InputChangedDuringSnapshot {
                guest_path,
                first,
                second,
            } => {
                assert_eq!(guest_path, "/in");
                assert_eq!(first, stable);
                assert_ne!(first, second);
            }
            other => panic!("{other:?}"),
        }
        let err = snapshot_stable(&store, &input, "/in", &mut || {
            fs::write(input.join("b"), b"new file").unwrap();
        })
        .unwrap_err();
        assert!(matches!(err, BuildError::InputChangedDuringSnapshot { .. }));
    }

    #[test]
    fn manifest_decoding_is_strict() {
        let m = ResultManifest {
            schema: RESULT_MANIFEST_SCHEMA.into(),
            key: "drv:x".into(),
            output_tree: "t".into(),
            outputs: vec![],
            outcome: Outcome::default(),
            stdout: "o".into(),
            stderr: "e".into(),
            fuel_consumed: 1,
            peak_memory_bytes: 2,
        };
        let bytes = m.encode();
        assert_eq!(ResultManifest::decode(&bytes).unwrap(), m);
        // Whitespace makes it non-canonical; an unknown field or schema fails.
        let mut spaced = b" ".to_vec();
        spaced.extend_from_slice(&bytes);
        assert!(ResultManifest::decode(&spaced).is_err());
        let text = String::from_utf8(bytes).unwrap();
        let extra = text.replacen('{', "{\"extra\":1,", 1);
        assert!(ResultManifest::decode(extra.as_bytes()).is_err());
        let other = text.replace(RESULT_MANIFEST_SCHEMA, "vesper-loom-build/result/v2");
        assert!(ResultManifest::decode(other.as_bytes()).is_err());
    }
}
