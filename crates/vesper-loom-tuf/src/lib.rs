//! Offline TUF target verification, not a complete stateful TUF updater.
//! Checkpoints are trusted caller state, not signed attestations. The attempt API retains
//! authenticated metadata progress on later failures, but only returns it when the call completes.
//! The legacy verify_target API drops error progress. See the README for update-state limits.
#![doc = include_str!("../README.md")]

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};
use tough::{
    IntoVec, RepositoryLoader, TargetName,
    schema::{Root, Signed, Snapshot, Targets, Timestamp},
};
use url::Url;

#[cfg(unix)]
pub mod state;

/// Local verification request. `repository_id` is a stable, caller-configured registry identity.
pub struct LocalRequest {
    pub repository_id: String,
    pub bootstrap_root: Vec<u8>,
    pub metadata_dir: PathBuf,
    pub targets_dir: PathBuf,
    pub target_name: String,
    pub previous: Option<Checkpoint>,
    pub limits: Limits,
}

/// Resource ceilings apply independently of signed metadata lengths.
#[derive(Debug, Clone)]
pub struct Limits {
    pub metadata_file_bytes: u64,
    pub metadata_total_bytes: u64,
    pub target_bytes: u64,
    pub targets_total_bytes: u64,
    pub files: usize,
    pub directory_depth: usize,
    pub json_depth: usize,
    pub roles: usize,
    pub root_updates: usize,
    pub checkpoint_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            metadata_file_bytes: 4 * 1024 * 1024,
            metadata_total_bytes: 32 * 1024 * 1024,
            target_bytes: 64 * 1024 * 1024,
            targets_total_bytes: 256 * 1024 * 1024,
            files: 4096,
            directory_depth: 32,
            json_depth: 32,
            roles: 256,
            root_updates: 64,
            checkpoint_bytes: 16 * 1024 * 1024,
        }
    }
}

/// An opaque rollback checkpoint. Protect this against tampering, deletion, and rewind.
#[derive(Debug, Clone)]
pub struct Checkpoint {
    state: State,
    checksum: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointWire {
    state: State,
    checksum: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    schema_version: u32,
    repository_id: String,
    bootstrap_sha256: String,
    roots: Vec<Vec<u8>>,
    timestamp_version: u64,
    snapshot_version: u64,
    role_versions: BTreeMap<String, u64>,
    verified_at: String,
}

impl Checkpoint {
    /// Decode only the bounded wire structure. Authenticity/identity/floors are checked by verify_target.
    pub fn from_json(bytes: &[u8], limits: &Limits) -> Result<Self, TufError> {
        if bytes.len() > limits.checkpoint_bytes {
            return fail("checkpoint exceeds limit");
        }
        let wire: CheckpointWire = serde_json::from_slice(bytes)
            .map_err(|e| TufError(format!("invalid checkpoint: {e}")))?;
        Ok(Self {
            state: wire.state,
            checksum: wire.checksum,
        })
    }
    pub fn to_json(&self) -> Result<Vec<u8>, TufError> {
        #[derive(Serialize)]
        struct WireRef<'a> {
            state: &'a State,
            checksum: &'a str,
        }
        json_bytes(&WireRef {
            state: &self.state,
            checksum: &self.checksum,
        })
    }
    pub fn root_version(&self) -> Result<u64, TufError> {
        Ok(parse_root(
            self.state
                .roots
                .last()
                .ok_or_else(|| TufError("empty root chain".into()))?,
        )?
        .signed
        .version
        .get())
    }
}

/// Bytes become available only after complete authenticated verification and expiry rechecks.
#[derive(Debug)]
pub struct VerifiedUpdate {
    pub bytes: Vec<u8>,
    pub target_name: String,
    pub length: u64,
    pub sha256: String,
    pub custom: BTreeMap<String, serde_json::Value>,
    pub metadata: MetadataVersions,
    pub checkpoint: Checkpoint,
    pub verified_at: String,
}

/// Complete-call progress, including authenticated metadata accepted before a later failure.
/// Persist `checkpoint` on both success and failure. Cancellation/process failure returns no progress.
#[derive(Debug)]
pub struct VerificationAttempt {
    pub checkpoint: Option<Checkpoint>,
    pub result: Result<VerifiedUpdate, TufError>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MetadataVersions {
    pub root: u64,
    pub timestamp: u64,
    pub snapshot: u64,
    pub roles: BTreeMap<String, u64>,
}

#[derive(Debug)]
pub struct TufError(String);
impl std::fmt::Display for TufError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for TufError {}
impl From<std::io::Error> for TufError {
    fn from(e: std::io::Error) -> Self {
        Self(e.to_string())
    }
}
fn fail<T>(s: impl Into<String>) -> Result<T, TufError> {
    Err(TufError(s.into()))
}
fn json_bytes(value: &impl Serialize) -> Result<Vec<u8>, TufError> {
    serde_json::to_vec(value).map_err(|e| TufError(e.to_string()))
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn state_checksum(state: &State) -> Result<String, TufError> {
    let mut bytes = Vec::new();
    state
        .serialize(&mut serde_json::Serializer::with_formatter(
            &mut bytes,
            olpc_cjson::CanonicalFormatter::new(),
        ))
        .map_err(|e| TufError(e.to_string()))?;
    Ok(hash(&bytes))
}
fn parse_root(bytes: &[u8]) -> Result<Signed<Root>, TufError> {
    serde_json::from_slice(bytes).map_err(|e| TufError(format!("invalid root: {e}")))
}
fn bounded_root(bytes: &[u8], limits: &Limits) -> Result<Signed<Root>, TufError> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| TufError(format!("invalid root JSON: {e}")))?;
    bound_json(&value, limits, 0, &mut 0)?;
    // Parse original bytes again so Tough's duplicate key checks are preserved.
    parse_root(bytes)
}
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['\\', ':', '?', '#', '%', '\0'])
        && name.split('/').all(safe_component)
}
fn safe_component(part: &str) -> bool {
    let device = part.split('.').next().unwrap_or("").to_ascii_uppercase();
    let numbered_device = ["COM", "LPT"].iter().any(|prefix| {
        device.strip_prefix(prefix).is_some_and(|n| {
            matches!(
                n,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    });
    !part.is_empty()
        && part.len() <= 255
        && part != "."
        && part != ".."
        && !part.ends_with(['.', ' '])
        && !part.chars().any(char::is_control)
        && !part.contains(['\\', ':', '<', '>', '"', '|', '*', '?'])
        && !matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$")
        && !numbered_device
}

/// Verify using real host time, safe expiry enforcement, and no network transport.
/// Failed calls return no target bytes and no checkpoint advancement; persist successful checkpoints
/// before using their bytes. Authenticated metadata preceding a later failure is NOT retained.
/// This permits rollback to the previous successful checkpoint on retry, a security limitation
/// beyond crash durability. This API does not implement the complete per-step TUF update protocol.
pub async fn verify_target(request: LocalRequest) -> Result<VerifiedUpdate, TufError> {
    verify_target_attempt(request).await.result
}

/// Preserve authenticated metadata advancement even when later metadata/target verification fails.
/// This returns progress only when the call completes; it is not durable per-step state or a
/// cancellation/crash-safe complete TUF updater. No failed result contains target bytes.
pub async fn verify_target_attempt(request: LocalRequest) -> VerificationAttempt {
    let mut checkpoint = None;
    let result = verify_target_inner(request, &mut checkpoint).await;
    if let Ok(update) = &result {
        checkpoint = Some(update.checkpoint.clone());
    }
    VerificationAttempt { checkpoint, result }
}

async fn verify_target_inner(
    request: LocalRequest,
    progress: &mut Option<Checkpoint>,
) -> Result<VerifiedUpdate, TufError> {
    let LocalRequest {
        repository_id,
        bootstrap_root,
        metadata_dir,
        targets_dir,
        target_name,
        previous,
        limits,
    } = request;
    if repository_id.is_empty()
        || repository_id.len() > 256
        || repository_id.chars().any(char::is_control)
    {
        return fail("invalid repository identity");
    }
    if !valid_name(&target_name) {
        return fail("target name must be a canonical relative path");
    }
    validate_limits(&limits)?;
    if bootstrap_root.len() as u64 > limits.metadata_file_bytes {
        return fail("bootstrap root exceeds limit");
    }
    let bootstrap = bounded_root(&bootstrap_root, &limits)?;
    bootstrap
        .signed
        .verify_role(&bootstrap)
        .map_err(|e| TufError(format!("bootstrap root verification: {e}")))?;
    let bootstrap_sha256 = hash(&bootstrap_root);
    let mut roots = vec![bootstrap_root.clone()];
    let started = jiff::Timestamp::now();
    if let Some(old) = &previous {
        validate_checkpoint(
            old,
            &repository_id,
            &bootstrap_sha256,
            &bootstrap_root,
            &limits,
        )?;
        let old_time: jiff::Timestamp = old
            .state
            .verified_at
            .parse()
            .map_err(|_| TufError("invalid checkpoint time".into()))?;
        if started < old_time {
            return fail("system time precedes checkpoint");
        }
        roots = old.state.roots.clone();
    }
    let initial = previous
        .as_ref()
        .map(|checkpoint| checkpoint.state.clone())
        .unwrap_or_else(|| State {
            schema_version: 2,
            repository_id: repository_id.clone(),
            bootstrap_sha256: bootstrap_sha256.clone(),
            roots: roots.clone(),
            timestamp_version: 0,
            snapshot_version: 0,
            role_versions: BTreeMap::new(),
            verified_at: started.to_string(),
        });
    *progress = Some(make_checkpoint(initial, &limits)?);

    let staging = tempfile::tempdir()?;
    let metadata = staging.path().join("metadata");
    let targets = staging.path().join("targets");
    let mut files = 0;
    snapshot_tree(&metadata_dir, &metadata, &limits, true, &mut files)?;
    snapshot_tree(&targets_dir, &targets, &limits, false, &mut files)?;
    preflight_metadata(&metadata, &limits)?;
    reserve_checkpoint_budget(
        &metadata,
        &progress.as_ref().expect("initialized progress").state,
        &limits,
    )?;
    let current = roots.last().expect("root chain is nonempty");
    if parse_root(current)?
        .signed
        .version
        .get()
        .checked_add(limits.root_updates as u64)
        .is_none()
    {
        return fail("root update version overflow");
    }
    let datastore = staging.path().join("state");
    fs::create_dir(&datastore)?;
    let loaded =
        RepositoryLoader::new(current, directory_url(&metadata)?, directory_url(&targets)?)
            .transport(tough::FilesystemTransport)
            .expiration_enforcement(tough::ExpirationEnforcement::Safe)
            .limits(tough::Limits {
                max_root_size: limits.metadata_file_bytes,
                max_targets_size: limits.metadata_file_bytes,
                max_timestamp_size: limits.metadata_file_bytes,
                max_snapshot_size: limits.metadata_file_bytes,
                max_root_updates: limits.root_updates as u64,
            })
            .datastore(&datastore)
            .load()
            .await;
    // Tough writes each successfully authenticated step before attempting the next one.
    // Inspect that private datastore even when load() failed, and reauthenticate before promotion.
    harvest_progress(
        &datastore,
        &metadata,
        &bootstrap_root,
        started,
        &limits,
        progress,
    )?;
    let repo = loaded.map_err(|e| TufError(format!("TUF metadata verification: {e}")))?;
    let role_versions: BTreeMap<_, _> = repo
        .snapshot()
        .signed
        .meta
        .iter()
        .map(|(name, value)| (name.clone(), value.version.get()))
        .collect();
    if role_versions.len() > limits.roles {
        return fail("snapshot role count exceeds limit");
    }
    if let Some(old) = &previous {
        if repo.timestamp().signed.version.get() < old.state.timestamp_version
            || repo.snapshot().signed.version.get() < old.state.snapshot_version
        {
            return fail("timestamp or snapshot rollback");
        }
        for (role, floor) in &old.state.role_versions {
            if role_versions
                .get(role)
                .is_none_or(|version| version < floor)
            {
                return fail(format!("role rollback or removal: {role}"));
            }
        }
    }
    let start_root = parse_root(current)?.signed.version.get();
    let final_root = repo.root().signed.version.get();
    for version in start_root + 1..=final_root {
        let bytes = fs::read(metadata.join(format!("{version}.root.json")))?;
        roots.push(bytes);
    }
    validate_root_chain(&roots, &bootstrap_root, &limits)?;
    let name = TargetName::new(&target_name).map_err(|e| TufError(e.to_string()))?;
    let target = repo
        .targets()
        .signed
        .find_target(&name, false)
        .map_err(|e| TufError(format!("target authorization: {e}")))?;
    if target.length > limits.target_bytes {
        return fail("signed target length exceeds limit");
    }
    let stream = repo.read_target(&name).await;
    refresh_progress_clock(&datastore, &limits, progress)?;
    let bytes = stream
        .map_err(|e| TufError(e.to_string()))?
        .ok_or_else(|| TufError("target not found".into()))?
        .into_vec()
        .await;
    refresh_progress_clock(&datastore, &limits, progress)?;
    let bytes = bytes.map_err(|e| TufError(format!("target verification: {e}")))?;
    if bytes.len() as u64 != target.length {
        return fail("target length differs from signed length");
    }
    let now = jiff::Timestamp::now();
    let last_clock: jiff::Timestamp =
        serde_json::from_slice(&fs::read(datastore.join("latest_known_time.json"))?)
            .map_err(|e| TufError(format!("invalid private clock sample: {e}")))?;
    let progress_time: jiff::Timestamp = progress
        .as_ref()
        .expect("initialized progress")
        .state
        .verified_at
        .parse()
        .map_err(|_| TufError("invalid progress time".into()))?;
    if now < started || now < last_clock || now < progress_time {
        return fail("system clock moved backward during verification");
    }
    for expiry in [
        repo.root().signed.expires,
        repo.timestamp().signed.expires,
        repo.snapshot().signed.expires,
    ] {
        if now >= expiry {
            return fail("metadata expired during target verification");
        }
    }
    check_targets_expiry(&repo.targets().signed, now, 0, &mut 0, &limits)?;
    let verified_at = now.to_string();
    let versions = MetadataVersions {
        root: final_root,
        timestamp: repo.timestamp().signed.version.get(),
        snapshot: repo.snapshot().signed.version.get(),
        roles: role_versions.clone(),
    };
    let state = State {
        schema_version: 2,
        repository_id,
        bootstrap_sha256,
        roots,
        timestamp_version: repo.timestamp().signed.version.get(),
        snapshot_version: repo.snapshot().signed.version.get(),
        role_versions,
        verified_at: verified_at.clone(),
    };
    let checkpoint = make_checkpoint(state, &limits)?;
    Ok(VerifiedUpdate {
        length: target.length,
        sha256: hash(&bytes),
        bytes,
        target_name,
        custom: target
            .custom
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        metadata: versions,
        checkpoint,
        verified_at,
    })
}

fn validate_limits(l: &Limits) -> Result<(), TufError> {
    if l.metadata_file_bytes == 0
        || l.metadata_total_bytes == 0
        || l.target_bytes == 0
        || l.targets_total_bytes == 0
        || l.files == 0
        || l.directory_depth == 0
        || l.directory_depth > 128
        || l.json_depth == 0
        || l.json_depth > 128
        || l.roles == 0
        || l.roles > 4096
        || l.root_updates == 0
        || l.root_updates > 4096
        || l.checkpoint_bytes == 0
    {
        return fail("invalid resource limits");
    }
    Ok(())
}

fn make_checkpoint(mut state: State, limits: &Limits) -> Result<Checkpoint, TufError> {
    state.schema_version = 2;
    let checkpoint = Checkpoint {
        checksum: state_checksum(&state)?,
        state,
    };
    if checkpoint.to_json()?.len() > limits.checkpoint_bytes {
        return fail("result checkpoint exceeds limit");
    }
    Ok(checkpoint)
}

/// Reserve enough wire space for every root/floor this immutable snapshot could authenticate.
/// This runs before Tough, so a later accepted step cannot exceed the configured return budget.
fn reserve_checkpoint_budget(
    metadata: &Path,
    previous: &State,
    limits: &Limits,
) -> Result<(), TufError> {
    let mut upper = previous.clone();
    let current_root = parse_root(upper.roots.last().expect("validated root chain"))?
        .signed
        .version
        .get();
    for entry in fs::read_dir(metadata)? {
        let path = entry?.path();
        if let Some(version) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".root.json"))
            .and_then(|n| n.parse::<u64>().ok())
            && version > current_root
        {
            upper.roots.push(fs::read(&path)?);
        }
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path)?).map_err(|e| TufError(e.to_string()))?;
        if value["signed"]["_type"] == "snapshot"
            && let Some(meta) = value["signed"]["meta"].as_object()
        {
            for name in meta.keys() {
                upper.role_versions.insert(name.clone(), u64::MAX);
            }
        }
    }
    if upper.roots.len() > limits.root_updates || upper.role_versions.len() > limits.roles {
        return fail("prospective checkpoint collection limit exceeded");
    }
    for value in upper.role_versions.values_mut() {
        *value = u64::MAX;
    }
    upper.timestamp_version = u64::MAX;
    upper.snapshot_version = u64::MAX;
    upper.verified_at = "9999-12-31T23:59:59.999999999Z".into();
    make_checkpoint(upper, limits).map(|_| ())
}

fn private_file(path: &Path, limits: &Limits) -> Result<Option<Vec<u8>>, TufError> {
    match fs::File::open(path) {
        Ok(file) => {
            let mut bytes = Vec::new();
            // Tough reserializes accepted metadata. Allow bounded encoding overhead here;
            // original repository files already passed the strict source-file ceiling.
            let ceiling = limits
                .metadata_file_bytes
                .saturating_mul(4)
                .saturating_add(1024);
            file.take(ceiling.saturating_add(1))
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 > ceiling {
                return fail("private metadata exceeds limit");
            }
            Ok(Some(bytes))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn refresh_progress_clock(
    datastore: &Path,
    limits: &Limits,
    progress: &mut Option<Checkpoint>,
) -> Result<(), TufError> {
    let bytes = private_file(&datastore.join("latest_known_time.json"), limits)?
        .ok_or_else(|| TufError("missing private clock sample".into()))?;
    let sampled: jiff::Timestamp = serde_json::from_slice(&bytes)
        .map_err(|e| TufError(format!("invalid private clock sample: {e}")))?;
    let mut state = progress
        .as_ref()
        .expect("initialized progress")
        .state
        .clone();
    let old: jiff::Timestamp = state
        .verified_at
        .parse()
        .map_err(|_| TufError("invalid progress time".into()))?;
    let now = jiff::Timestamp::now();
    state.verified_at = old.max(sampled).max(now).to_string();
    *progress = Some(make_checkpoint(state, limits)?);
    if now < old || now < sampled {
        return fail("system clock moved backward during verification");
    }
    Ok(())
}

fn harvest_progress(
    datastore: &Path,
    metadata: &Path,
    bootstrap: &[u8],
    started: jiff::Timestamp,
    limits: &Limits,
    progress: &mut Option<Checkpoint>,
) -> Result<(), TufError> {
    let clock_result = refresh_progress_clock(datastore, limits, progress);
    let Some(root_bytes) = private_file(&datastore.join("root.json"), limits)? else {
        return clock_result;
    };
    let persisted_root = bounded_root(&root_bytes, limits)?;
    let mut state = progress
        .as_ref()
        .expect("initialized progress")
        .state
        .clone();
    let old_root = parse_root(state.roots.last().expect("validated root chain"))?;
    if persisted_root.signed.version < old_root.signed.version {
        return fail("private root version rollback");
    }
    for version in old_root.signed.version.get() + 1..=persisted_root.signed.version.get() {
        let bytes = private_file(&metadata.join(format!("{version}.root.json")), limits)?
            .ok_or_else(|| TufError("missing authenticated root chain member".into()))?;
        state.roots.push(bytes);
    }
    validate_root_chain(&state.roots, bootstrap, limits)?;
    if parse_root(state.roots.last().expect("validated root chain"))? != persisted_root {
        return fail("private root does not match authenticated chain");
    }
    // Root authentication survives expiry or any subsequent metadata failure.
    *progress = Some(make_checkpoint(state.clone(), limits)?);
    let Some(timestamp_bytes) = private_file(&datastore.join("timestamp.json"), limits)? else {
        return clock_result;
    };
    let timestamp: Signed<Timestamp> = serde_json::from_slice(&timestamp_bytes)
        .map_err(|e| TufError(format!("invalid private timestamp: {e}")))?;
    persisted_root
        .signed
        .verify_role(&timestamp)
        .map_err(|e| TufError(format!("private timestamp authentication: {e}")))?;
    let pin = timestamp
        .signed
        .meta
        .get("snapshot.json")
        .ok_or_else(|| TufError("private timestamp lacks snapshot pin".into()))?;
    if timestamp.signed.meta.len() != 1 || timestamp.signed.expires < started {
        return fail("invalid accepted timestamp metadata");
    }
    if timestamp.signed.version.get() < state.timestamp_version
        || pin.version.get() < state.snapshot_version
    {
        return fail("timestamp or pinned snapshot rollback");
    }
    state.timestamp_version = timestamp.signed.version.get();
    // A verified timestamp already authenticates this snapshot version floor even when its body
    // cannot be retrieved or verified. Do not wait for a complete snapshot before retaining it.
    state.snapshot_version = pin.version.get();
    *progress = Some(make_checkpoint(state.clone(), limits)?);
    let Some(snapshot_bytes) = private_file(&datastore.join("snapshot.json"), limits)? else {
        return clock_result;
    };
    let snapshot: Signed<Snapshot> = serde_json::from_slice(&snapshot_bytes)
        .map_err(|e| TufError(format!("invalid private snapshot: {e}")))?;
    persisted_root
        .signed
        .verify_role(&snapshot)
        .map_err(|e| TufError(format!("private snapshot authentication: {e}")))?;
    if snapshot.signed.version != pin.version
        || snapshot.signed.expires < started
        || !snapshot.signed.meta.contains_key("targets.json")
    {
        return fail("invalid accepted snapshot metadata");
    }
    let name = if persisted_root.signed.consistent_snapshot {
        format!("{}.snapshot.json", pin.version)
    } else {
        "snapshot.json".into()
    };
    let original = private_file(&metadata.join(name), limits)?
        .ok_or_else(|| TufError("missing pinned snapshot source".into()))?;
    let original_snapshot: Signed<Snapshot> = serde_json::from_slice(&original)
        .map_err(|e| TufError(format!("invalid pinned snapshot source: {e}")))?;
    if original_snapshot != snapshot
        || pin
            .length
            .is_some_and(|length| original.len() as u64 > length)
        || pin
            .hashes
            .as_ref()
            .is_some_and(|hashes| hash(&original) != hex_digest(hashes.sha256.as_ref()))
    {
        return fail("private snapshot source pin mismatch");
    }
    let floors: BTreeMap<_, _> = snapshot
        .signed
        .meta
        .iter()
        .map(|(name, meta)| (name.clone(), meta.version.get()))
        .collect();
    if floors.len() > limits.roles {
        return fail("snapshot role count exceeds limit");
    }
    for (name, floor) in &state.role_versions {
        if floors.get(name).is_none_or(|version| version < floor) {
            return fail(format!("role rollback or removal: {name}"));
        }
    }
    state.role_versions = floors;
    *progress = Some(make_checkpoint(state, limits)?);
    clock_result
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn validate_checkpoint(
    old: &Checkpoint,
    repository: &str,
    bootstrap_hash: &str,
    bootstrap: &[u8],
    limits: &Limits,
) -> Result<(), TufError> {
    if old.to_json()?.len() > limits.checkpoint_bytes
        || !matches!(old.state.schema_version, 1 | 2)
        || old.state.repository_id != repository
        || old.state.bootstrap_sha256 != bootstrap_hash
        || old.checksum != state_checksum(&old.state)?
        || (old.state.schema_version == 1
            && (old.state.timestamp_version == 0
                || old.state.snapshot_version == 0
                || old.state.role_versions.is_empty()))
        || ((old.state.timestamp_version == 0) != (old.state.snapshot_version == 0))
        || (old.state.snapshot_version == 0 && !old.state.role_versions.is_empty())
        || old.state.role_versions.len() > limits.roles
        || old.state.role_versions.values().any(|x| *x == 0)
        || (!old.state.role_versions.is_empty()
            && !old.state.role_versions.contains_key("targets.json"))
    {
        return fail("corrupt or mismatched checkpoint");
    }
    validate_root_chain(&old.state.roots, bootstrap, limits)
}
fn validate_root_chain(
    roots: &[Vec<u8>],
    bootstrap: &[u8],
    limits: &Limits,
) -> Result<(), TufError> {
    if roots.is_empty() || roots.len() > limits.root_updates || roots[0] != bootstrap {
        return fail("invalid checkpoint root chain");
    }
    let mut old = bounded_root(bootstrap, limits)?;
    old.signed
        .verify_role(&old)
        .map_err(|e| TufError(e.to_string()))?;
    for bytes in &roots[1..] {
        if bytes.len() as u64 > limits.metadata_file_bytes {
            return fail("checkpoint root exceeds limit");
        }
        let new = bounded_root(bytes, limits)?;
        if old.signed.version.get().checked_add(1) != Some(new.signed.version.get()) {
            return fail("nonconsecutive checkpoint roots");
        }
        old.signed
            .verify_role(&new)
            .map_err(|e| TufError(format!("root chain old threshold: {e}")))?;
        new.signed
            .verify_role(&new)
            .map_err(|e| TufError(format!("root chain new threshold: {e}")))?;
        old = new;
    }
    Ok(())
}
fn directory_url(path: &Path) -> Result<Url, TufError> {
    use tough::SafeUrlPath;
    let url =
        Url::from_directory_path(path).map_err(|_| TufError("invalid directory URL".into()))?;
    if url.safe_url_filepath() != path {
        return fail(
            "unsupported temporary staging path: Tough file transport preserves URL escapes; configure an ASCII temporary directory without spaces or percent characters",
        );
    }
    Ok(url)
}

fn check_targets_expiry(
    targets: &Targets,
    now: jiff::Timestamp,
    depth: usize,
    count: &mut usize,
    limits: &Limits,
) -> Result<(), TufError> {
    *count += 1;
    if depth > limits.directory_depth || *count > limits.roles {
        return fail("delegation traversal limit exceeded");
    }
    if now >= targets.expires {
        return fail("targets or delegation expired during verification");
    }
    if let Some(delegations) = &targets.delegations {
        for role in &delegations.roles {
            if let Some(child) = &role.targets {
                check_targets_expiry(&child.signed, now, depth + 1, count, limits)?;
            }
        }
    }
    Ok(())
}

fn snapshot_tree(
    source: &Path,
    output: &Path,
    limits: &Limits,
    metadata: bool,
    files: &mut usize,
) -> Result<(), TufError> {
    // Reject existing symlink ancestors as well as leaf links. Callers must prevent concurrent
    // replacement of directory ancestors while the private snapshot is being constructed.
    let source = if source.is_absolute() {
        source.to_path_buf()
    } else {
        std::env::current_dir()?.join(source)
    };
    if source
        .components()
        .any(|component| component == std::path::Component::ParentDir)
    {
        return fail("repository directory contains parent traversal");
    }
    for ancestor in source.ancestors() {
        if is_link(&fs::symlink_metadata(ancestor)?) {
            return fail("symlink directory ancestor");
        }
    }
    let mut total = 0;
    copy_directory(&source, output, limits, metadata, files, &mut total, 0)
}
fn is_link(info: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if info.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    info.file_type().is_symlink()
}
fn copy_directory(
    source: &Path,
    output: &Path,
    limits: &Limits,
    metadata: bool,
    files: &mut usize,
    total: &mut u64,
    depth: usize,
) -> Result<(), TufError> {
    if depth > limits.directory_depth {
        return fail("directory depth exceeds limit");
    }
    let info = fs::symlink_metadata(source)?;
    if !info.is_dir() || is_link(&info) {
        return fail("snapshot source must be a real directory");
    }
    fs::create_dir(output)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        *files += 1;
        if *files > limits.files {
            return fail("repository entry count exceeds limit");
        }
        if is_link(&fs::symlink_metadata(entry.path())?) {
            return fail("symlink in repository snapshot");
        }
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| TufError("non-UTF8 repository filename".into()))?;
        if !safe_component(name) {
            return fail("unsafe repository filename");
        }
        if kind.is_dir() {
            if metadata {
                return fail("metadata directory must be flat");
            }
            copy_directory(
                &entry.path(),
                &output.join(name),
                limits,
                metadata,
                files,
                total,
                depth + 1,
            )?;
        } else if kind.is_file() {
            let ceiling = if metadata {
                limits.metadata_file_bytes
            } else {
                limits.target_bytes
            };
            let mut options = fs::OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                options.custom_flags(0x00200000);
            }
            let file = options.open(entry.path())?;
            let opened = file.metadata()?;
            if !opened.is_file() || is_link(&opened) {
                return fail("repository entry is not a regular file");
            }
            let mut bytes = Vec::new();
            file.take(ceiling.saturating_add(1))
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 > ceiling {
                return fail("repository file exceeds limit");
            }
            *total = total
                .checked_add(bytes.len() as u64)
                .ok_or_else(|| TufError("snapshot size overflow".into()))?;
            if *total
                > if metadata {
                    limits.metadata_total_bytes
                } else {
                    limits.targets_total_bytes
                }
            {
                return fail("repository total bytes exceed limit");
            }
            fs::write(output.join(name), bytes)?;
        } else {
            return fail("nonregular repository entry");
        }
    }
    Ok(())
}

fn preflight_metadata(directory: &Path, limits: &Limits) -> Result<(), TufError> {
    let mut roles = 0;
    let mut graph: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            return fail("metadata contains a non-JSON file");
        }
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)
            .map_err(|e| TufError(format!("invalid metadata JSON: {e}")))?;
        bound_json(&value, limits, 0, &mut 0)?;
        let signed = value
            .get("signed")
            .ok_or_else(|| TufError("metadata missing signed object".into()))?;
        if let Some(delegations) = signed
            .get("delegations")
            .and_then(|v| v.get("roles"))
            .and_then(|v| v.as_array())
        {
            roles += delegations.len();
            if roles > limits.roles {
                return fail("delegation count exceeds limit");
            }
            let stem = path
                .file_stem()
                .and_then(|name| name.to_str())
                .ok_or_else(|| TufError("invalid metadata filename".into()))?;
            // A numeric prefix may be part of a role name (non-consistent snapshots),
            // or a consistent-snapshot version. Conservatively attach both aliases;
            // dropping the raw name would disconnect genuine outgoing graph edges.
            let mut stems = vec![stem];
            if let Some((version, name)) = stem.split_once('.')
                && version.parse::<u64>().is_ok()
            {
                stems.push(name);
            }
            let names: Result<Vec<_>, _> = stems
                .into_iter()
                .map(|stem| {
                    percent_encoding::percent_decode_str(stem)
                        .decode_utf8()
                        .map(|name| name.into_owned())
                })
                .collect();
            let names = names.map_err(|_| TufError("invalid encoded metadata filename".into()))?;
            for role in delegations {
                let child = role
                    .get("name")
                    .and_then(|name| name.as_str())
                    .ok_or_else(|| TufError("invalid delegated role name".into()))?;
                for name in &names {
                    graph
                        .entry(name.clone())
                        .or_default()
                        .push(child.to_owned());
                }
            }
        }
        for key in ["targets", "meta", "keys"] {
            if signed
                .get(key)
                .and_then(|v| v.as_object())
                .is_some_and(|map| map.len() > limits.files)
            {
                return fail("metadata collection exceeds limit");
            }
        }
    }
    let mut budget = limits.roles.saturating_mul(limits.roles);
    for name in graph.keys() {
        check_graph_depth(name, &graph, &mut Vec::new(), &mut budget, limits)?;
    }
    Ok(())
}

fn check_graph_depth(
    name: &str,
    graph: &BTreeMap<String, Vec<String>>,
    path: &mut Vec<String>,
    budget: &mut usize,
    limits: &Limits,
) -> Result<(), TufError> {
    if *budget == 0 {
        return fail("delegation graph work limit exceeded");
    }
    *budget -= 1;
    if path.len() > limits.directory_depth || path.iter().any(|seen| seen == name) {
        return fail("delegation graph depth or cycle exceeds policy");
    }
    path.push(name.to_owned());
    if let Some(children) = graph.get(name) {
        for child in children {
            check_graph_depth(child, graph, path, budget, limits)?;
        }
    }
    path.pop();
    Ok(())
}

fn bound_json(
    value: &serde_json::Value,
    limits: &Limits,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), TufError> {
    *nodes = nodes.saturating_add(1);
    if depth > limits.json_depth || *nodes > limits.files.saturating_mul(limits.roles) {
        return fail("metadata JSON traversal exceeds limit");
    }
    match value {
        serde_json::Value::Object(map) => {
            if map.len() > limits.files {
                return fail("metadata collection exceeds limit");
            }
            for child in map.values() {
                bound_json(child, limits, depth + 1, nodes)?;
            }
        }
        serde_json::Value::Array(array) => {
            if array.len() > limits.files {
                return fail("metadata collection exceeds limit");
            }
            for child in array {
                bound_json(child, limits, depth + 1, nodes)?;
            }
        }
        _ => (),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU64;
    use tough::schema::DelegatedRole;

    #[test]
    fn expiry_recheck_detects_delegation_expiring_after_initial_metadata_verification() {
        let expiry: jiff::Timestamp = "2030-01-01T00:00:00Z".parse().unwrap();
        let mut top = Targets::new(
            "1.0.0".into(),
            NonZeroU64::new(1).unwrap(),
            "2999-01-01T00:00:00Z".parse().unwrap(),
        );
        let mut role: DelegatedRole = serde_json::from_value(serde_json::json!({"name":"packages","keyids":[],"threshold":1,"paths":["*"],"terminating":true})).unwrap();
        role.targets = Some(Signed {
            signed: Targets::new("1.0.0".into(), NonZeroU64::new(1).unwrap(), expiry),
            signatures: vec![],
        });
        top.delegations.as_mut().unwrap().roles.push(role);
        let before = expiry - jiff::SignedDuration::from_secs(1);
        assert!(check_targets_expiry(&top, before, 0, &mut 0, &Limits::default()).is_ok());
        assert!(check_targets_expiry(&top, expiry, 0, &mut 0, &Limits::default()).is_err());
    }

    #[test]
    fn escaped_private_staging_paths_fail_with_explicit_transport_limit() {
        for prefix in ["space in staging-", "nonASCII-é-", "percent%-"] {
            let directory = tempfile::Builder::new().prefix(prefix).tempdir().unwrap();
            let error = directory_url(directory.path()).unwrap_err().to_string();
            assert!(
                error.contains("unsupported temporary staging path"),
                "{error}"
            );
        }
    }
}
