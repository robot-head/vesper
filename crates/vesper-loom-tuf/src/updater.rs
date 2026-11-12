//! Unix owned-worker updater with synced checkpoint gates before subsequent Tough fetches.
//! Dropping a handle detaches its worker; panic/process termination are outside this guarantee.

use crate::{
    Checkpoint, Limits, LocalRequest, ProgressSink, TufError, VerifiedUpdate,
    state::CheckpointStore, verify_target_attempt_with_sink,
};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
};

/// Process-wide admission ceiling; no unbounded background-worker manager is created.
pub const MAX_WORKERS: usize = 2;
static ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// Previous state is deliberately absent: the worker loads it from its owned locked store.
pub struct UpdateRequest {
    pub repository_id: String,
    pub bootstrap_root: Vec<u8>,
    pub metadata_dir: PathBuf,
    pub targets_dir: PathBuf,
    pub target_name: String,
    pub limits: Limits,
}

impl UpdateRequest {
    fn local(self, previous: Option<Checkpoint>) -> LocalRequest {
        LocalRequest {
            repository_id: self.repository_id,
            bootstrap_root: self.bootstrap_root,
            metadata_dir: self.metadata_dir,
            targets_dir: self.targets_dir,
            target_name: self.target_name,
            previous,
            limits: self.limits,
        }
    }
}

/// A rejected start returns ownership of the store, including a fresh uninitialized checkpoint.
pub struct StartError {
    pub error: TufError,
    pub store: CheckpointStore,
}
impl std::fmt::Debug for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartError")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}
impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}
impl std::error::Error for StartError {}

/// Dropping detaches the worker, which retains its OS lock through final persistence.
#[derive(Debug)]
pub struct UpdateHandle {
    worker: JoinHandle<Result<VerifiedUpdate, TufError>>,
}
impl UpdateHandle {
    /// Blocks until verification and final checkpoint persistence complete.
    pub fn wait(self) -> Result<VerifiedUpdate, TufError> {
        self.worker.join().map_err(|_| {
            TufError(
                "owned updater worker panicked; inspect persisted state before recovery".into(),
            )
        })?
    }
    pub fn is_finished(&self) -> bool {
        self.worker.is_finished()
    }
}

struct Admission;
impl Admission {
    fn acquire() -> Result<Self, TufError> {
        ACTIVE
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_WORKERS).then_some(count + 1)
            })
            .map(|_| Self)
            .map_err(|_| TufError("owned updater worker limit reached".into()))
    }
}
impl Drop for Admission {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::AcqRel);
    }
}

type Job = Box<dyn FnOnce() -> Result<VerifiedUpdate, TufError> + Send + 'static>;

/// Starts one admitted, bounded local operation. On rejection the caller retains the store.
/// Once started, even dropping the caller's handle does not cancel the owned verification.
pub fn start_update(
    store: CheckpointStore,
    request: UpdateRequest,
) -> Result<UpdateHandle, StartError> {
    start_with_spawn(store, request, |job| {
        thread::Builder::new()
            .name("vesper-tuf-update".into())
            .spawn(job)
    })
}

fn start_with_spawn(
    store: CheckpointStore,
    request: UpdateRequest,
    spawn: impl FnOnce(Job) -> std::io::Result<JoinHandle<Result<VerifiedUpdate, TufError>>>,
) -> Result<UpdateHandle, StartError> {
    let admission = match Admission::acquire() {
        Ok(permit) => permit,
        Err(error) => return Err(StartError { error, store }),
    };
    // The payload stays recoverable if OS thread creation fails before any work starts.
    let payload = Arc::new(Mutex::new(Some((store, request))));
    let owned = payload.clone();
    let job: Job = Box::new(move || {
        let _admission = admission;
        let (store, request) = owned
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .expect("owned startup payload");
        run_owned(store, request)
    });
    match spawn(job) {
        Ok(worker) => Ok(UpdateHandle { worker }),
        Err(error) => {
            let (store, _) = payload
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
                .expect("unstarted startup payload");
            Err(StartError {
                error: TufError(format!("failed to start owned updater worker: {error}")),
                store,
            })
        }
    }
}

#[derive(Default)]
struct Failure {
    message: Option<String>,
    commit_failed: bool,
}
struct DurableSink {
    store: Mutex<CheckpointStore>,
    limits: Limits,
    failure: Mutex<Failure>,
    committed: Mutex<Option<Checkpoint>>,
}
impl DurableSink {
    fn new(store: CheckpointStore, limits: Limits) -> Result<Self, TufError> {
        let committed = store.load_checkpoint(&limits)?;
        Ok(Self {
            store: Mutex::new(store),
            limits,
            failure: Mutex::new(Failure::default()),
            committed: Mutex::new(committed),
        })
    }
}
impl ProgressSink for DurableSink {
    fn commit(&self, checkpoint: &Checkpoint) -> Result<(), TufError> {
        {
            let failed = self
                .failure
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if failed.commit_failed {
                return Err(TufError(
                    failed
                        .message
                        .clone()
                        .unwrap_or_else(|| "checkpoint commit previously failed".into()),
                ));
            }
        }
        let mut committed = self
            .committed
            .lock()
            .map_err(|_| TufError("committed progress lock poisoned".into()))?;
        if let Some(prior) = committed.as_ref() {
            let newer = &checkpoint.state;
            let older = &prior.state;
            let old_time: jiff::Timestamp = older
                .verified_at
                .parse()
                .map_err(|_| TufError("invalid committed time".into()))?;
            let new_time: jiff::Timestamp = newer
                .verified_at
                .parse()
                .map_err(|_| TufError("invalid candidate time".into()))?;
            if newer.repository_id != older.repository_id
                || newer.bootstrap_sha256 != older.bootstrap_sha256
                || !newer.roots.starts_with(&older.roots)
                || newer.timestamp_version < older.timestamp_version
                || newer.snapshot_version < older.snapshot_version
                || new_time < old_time
                || older.role_versions.iter().any(|(role, floor)| {
                    newer
                        .role_versions
                        .get(role)
                        .is_none_or(|version| version < floor)
                })
            {
                let error = TufError(
                    "checkpoint would regress committed root, versions, identity, or time".into(),
                );
                self.latch(error.to_string());
                return Err(error);
            }
        }
        let result = match self.store.lock() {
            Ok(mut store) => store.commit_checkpoint(checkpoint, &self.limits),
            Err(_) => Err(TufError("checkpoint store lock poisoned".into())),
        };
        if let Err(error) = &result {
            let mut failed = self
                .failure
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            failed.commit_failed = true;
            let message = format!("checkpoint persistence failed: {error}");
            failed.message = Some(match failed.message.take() {
                Some(prior) => format!("{prior}; {message}"),
                None => message,
            });
        }
        if result.is_ok() {
            *committed = Some(checkpoint.clone());
        }
        result
    }
    fn latch(&self, message: String) {
        let mut failed = self
            .failure
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if failed.message.is_none() {
            failed.message = Some(message);
        }
    }
    fn failure(&self) -> Option<String> {
        self.failure
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .message
            .clone()
    }
}

fn run_owned(store: CheckpointStore, request: UpdateRequest) -> Result<VerifiedUpdate, TufError> {
    let previous = store.load_checkpoint(&request.limits)?;
    let sink = Arc::new(DurableSink::new(store, request.limits.clone())?);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(2)
        .build()
        .map_err(|error| TufError(format!("failed to start owned updater runtime: {error}")))?;
    let attempt = runtime.block_on(verify_target_attempt_with_sink(
        request.local(previous),
        Some(sink.clone()),
    ));
    finish_attempt(attempt, sink.as_ref())
}

fn finish_attempt(
    attempt: crate::VerificationAttempt,
    sink: &dyn ProgressSink,
) -> Result<VerifiedUpdate, TufError> {
    // This includes ordinary errors, no-next-fetch paths, and partial harvest failures.
    if let Some(checkpoint) = &attempt.checkpoint
        && let Err(error) = sink.commit(checkpoint)
    {
        sink.latch(error.to_string());
    }
    if let Some(message) = sink.failure() {
        return Err(TufError(format!("durable updater: {message}")));
    }
    attempt.result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FetchGate, GatedFilesystem};
    use aws_lc_rs::{rand::SystemRandom, signature::Ed25519KeyPair};
    use serde::Serialize;
    use serde_json::{Value, json};
    use std::{
        collections::BTreeMap,
        fs,
        path::Path,
        sync::Barrier,
        time::{Duration, Instant},
    };
    use tempfile::TempDir;
    use tough::sign::Sign;
    use url::Url;

    static SERIAL: Mutex<()> = Mutex::new(());
    const BODY: &[u8] = b"owned-worker signed fixture";
    struct Repo {
        dir: TempDir,
        bootstrap: Vec<u8>,
    }
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(2)
            .build()
            .unwrap()
    }
    fn id(key: &Ed25519KeyPair) -> String {
        hex::encode(key.tuf_key().key_id().unwrap().as_ref())
    }
    async fn signed(value: Value, keys: &[&Ed25519KeyPair]) -> Vec<u8> {
        let mut bytes = Vec::new();
        value
            .serialize(&mut serde_json::Serializer::with_formatter(
                &mut bytes,
                olpc_cjson::CanonicalFormatter::new(),
            ))
            .unwrap();
        let mut signatures = Vec::new();
        for key in keys {
            signatures.push(json!({"keyid":id(key),"sig":hex::encode(Sign::sign(*key,&bytes,&SystemRandom::new()).await.unwrap())}));
        }
        serde_json::to_vec(&json!({"signed":value,"signatures":signatures})).unwrap()
    }
    fn root(version: u64, key: &Ed25519KeyPair) -> Value {
        let id = id(key);
        let roles: BTreeMap<_, _> = ["root", "timestamp", "snapshot", "targets"]
            .into_iter()
            .map(|name| (name, json!({"keyids":[id],"threshold":1})))
            .collect();
        json!({"_type":"root","spec_version":"1.0.0","version":version,"expires":"2999-01-01T00:00:00Z","consistent_snapshot":false,"keys":{id:key.tuf_key()},"roles":roles})
    }
    fn pin(bytes: &[u8], version: u64) -> Value {
        json!({"length":bytes.len(),"version":version,"hashes":{"sha256":crate::hash(bytes)}})
    }
    impl Repo {
        fn new(version: u64, rotated: bool, target_version: u64) -> Self {
            let runtime = runtime();
            let key1 = Ed25519KeyPair::from_seed_unchecked(&[11; 32]).unwrap();
            let key2 = Ed25519KeyPair::from_seed_unchecked(&[12; 32]).unwrap();
            let bootstrap = runtime.block_on(signed(root(1, &key1), &[&key1]));
            let dir = tempfile::tempdir().unwrap();
            fs::create_dir(dir.path().join("metadata")).unwrap();
            fs::create_dir(dir.path().join("targets")).unwrap();
            fs::write(dir.path().join("metadata/1.root.json"), &bootstrap).unwrap();
            let active = if rotated {
                fs::write(
                    dir.path().join("metadata/2.root.json"),
                    runtime.block_on(signed(root(2, &key2), &[&key1, &key2])),
                )
                .unwrap();
                &key2
            } else {
                &key1
            };
            let targets=runtime.block_on(signed(json!({"_type":"targets","spec_version":"1.0.0","version":target_version,"expires":"2999-01-01T00:00:00Z","targets":{"top.bin":{"length":BODY.len(),"hashes":{"sha256":crate::hash(BODY)}}}}),&[active]));
            let snapshot=runtime.block_on(signed(json!({"_type":"snapshot","spec_version":"1.0.0","version":version,"expires":"2999-01-01T00:00:00Z","meta":{"targets.json":pin(&targets,target_version)}}),&[active]));
            let timestamp=runtime.block_on(signed(json!({"_type":"timestamp","spec_version":"1.0.0","version":version,"expires":"2999-01-01T00:00:00Z","meta":{"snapshot.json":pin(&snapshot,version)}}),&[active]));
            for (name, bytes) in [
                ("timestamp.json", timestamp),
                ("snapshot.json", snapshot),
                ("targets.json", targets),
            ] {
                fs::write(dir.path().join("metadata").join(name), bytes).unwrap();
            }
            fs::write(dir.path().join("targets/top.bin"), BODY).unwrap();
            Self { dir, bootstrap }
        }
        fn request(&self) -> UpdateRequest {
            UpdateRequest {
                repository_id: "owned-test-registry".into(),
                bootstrap_root: self.bootstrap.clone(),
                metadata_dir: self.dir.path().join("metadata"),
                targets_dir: self.dir.path().join("targets"),
                target_name: "top.bin".into(),
                limits: Limits::default(),
            }
        }
        fn checkpoint(&self) -> Checkpoint {
            runtime()
                .block_on(crate::verify_target(self.request().local(None)))
                .unwrap()
                .checkpoint
        }
    }

    struct RecordingSink {
        durable: DurableSink,
        path: PathBuf,
        fetches: Mutex<Vec<(String, Checkpoint)>>,
        fail_root_two: bool,
        fail_final: bool,
        target_seen: std::sync::atomic::AtomicBool,
        break_private: Option<bool>,
    }
    impl RecordingSink {
        fn new(store: CheckpointStore, path: PathBuf) -> Self {
            Self {
                durable: DurableSink::new(store, Limits::default()).unwrap(),
                path,
                fetches: Mutex::new(Vec::new()),
                fail_root_two: false,
                fail_final: false,
                target_seen: std::sync::atomic::AtomicBool::new(false),
                break_private: None,
            }
        }
    }
    impl ProgressSink for RecordingSink {
        fn commit(&self, checkpoint: &Checkpoint) -> Result<(), TufError> {
            if (self.fail_root_two && checkpoint.root_version()? == 2)
                || (self.fail_final && self.target_seen.load(Ordering::Acquire))
            {
                let file = self.path.join("checkpoint.json");
                let backup = self.path.join("last-good.json");
                if file.is_file() && !backup.exists() {
                    fs::rename(&file, backup)?;
                    fs::create_dir(file)?;
                }
            }
            self.durable.commit(checkpoint)
        }
        fn latch(&self, message: String) {
            self.durable.latch(message)
        }
        fn failure(&self) -> Option<String> {
            self.durable.failure()
        }
        fn observe_fetch(&self, url: &Url) {
            // Read the actual synced store immediately before the backend file open.
            let checkpoint = Checkpoint::from_json(
                &fs::read(self.path.join("checkpoint.json")).unwrap(),
                &Limits::default(),
            )
            .unwrap();
            self.fetches
                .lock()
                .unwrap()
                .push((url.path().to_owned(), checkpoint));
            if url.path().ends_with("/top.bin") {
                self.target_seen.store(true, Ordering::Release);
            }
            if url.path().ends_with("/3.root.json")
                && let Some(remove_state) = self.break_private
            {
                let private = Path::new(url.path())
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("state");
                if remove_state {
                    fs::rename(&private, private.with_file_name("removed-state")).unwrap();
                    fs::write(private, b"replaced private datastore").unwrap();
                } else {
                    let root = private.join("root.json");
                    fs::remove_file(&root).unwrap();
                    fs::create_dir(root).unwrap();
                }
            }
        }
    }
    fn attempt(
        repo: &Repo,
        sink: Arc<RecordingSink>,
        previous: Option<Checkpoint>,
    ) -> Result<VerifiedUpdate, TufError> {
        let result = runtime().block_on(verify_target_attempt_with_sink(
            repo.request().local(previous),
            Some(sink.clone()),
        ));
        finish_attempt(result, sink.as_ref())
    }
    fn store(path: &Path) -> Checkpoint {
        Checkpoint::from_json(
            &fs::read(path.join("checkpoint.json")).unwrap(),
            &Limits::default(),
        )
        .unwrap()
    }

    #[test]
    fn signed_stages_are_synced_before_subsequent_backend_reads() {
        let _serial = SERIAL.lock().unwrap();
        let repo = Repo::new(2, true, 2);
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        let sink = Arc::new(RecordingSink::new(
            CheckpointStore::create(&path).unwrap(),
            path.clone(),
        ));
        let update = attempt(&repo, sink.clone(), None).unwrap();
        assert_eq!(update.bytes, BODY);
        let events = sink.fetches.lock().unwrap();
        let before = |suffix: &str| {
            &events
                .iter()
                .find(|(path, _)| path.ends_with(suffix))
                .unwrap()
                .1
        };
        assert_eq!(before("/3.root.json").root_version().unwrap(), 2);
        assert_eq!(before("/snapshot.json").state.timestamp_version, 2);
        assert_eq!(before("/snapshot.json").state.snapshot_version, 2);
        assert_eq!(
            before("/targets.json").state.role_versions["targets.json"],
            2
        );
        assert_eq!(before("/top.bin").state.role_versions["targets.json"], 2);
        assert_eq!(
            store(&path).to_json().unwrap(),
            update.checkpoint.to_json().unwrap()
        );
    }

    #[test]
    fn commit_failure_latches_despite_tough_swallowing_root_fetch_errors() {
        let _serial = SERIAL.lock().unwrap();
        let repo = Repo::new(2, true, 2);
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        let mut sink = RecordingSink::new(CheckpointStore::create(&path).unwrap(), path.clone());
        sink.fail_root_two = true;
        let sink = Arc::new(sink);
        let error = attempt(&repo, sink.clone(), None).unwrap_err().to_string();
        assert!(error.contains("persistence failed"), "{error}");
        let events = sink.fetches.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].0.ends_with("/2.root.json"));
        let persisted = Checkpoint::from_json(
            &fs::read(path.join("last-good.json")).unwrap(),
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(persisted.root_version().unwrap(), 1);
        assert!(sink.failure().is_some());
    }

    #[test]
    fn final_commit_failure_never_returns_verified_target_bytes() {
        let _serial = SERIAL.lock().unwrap();
        let repo = Repo::new(2, true, 2);
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        let mut sink = RecordingSink::new(CheckpointStore::create(&path).unwrap(), path);
        sink.fail_final = true;
        let sink = Arc::new(sink);
        let error = attempt(&repo, sink.clone(), None).unwrap_err().to_string();
        assert!(error.contains("persistence failed"), "{error}");
        assert!(sink.target_seen.load(Ordering::Acquire));
    }

    #[test]
    fn private_datastore_failure_cannot_rewind_already_synced_gate_progress() {
        let _serial = SERIAL.lock().unwrap();
        for remove_state in [false, true] {
            let repo = Repo::new(2, true, 2);
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("state");
            let mut sink =
                RecordingSink::new(CheckpointStore::create(&path).unwrap(), path.clone());
            sink.break_private = Some(remove_state);
            let sink = Arc::new(sink);
            assert!(attempt(&repo, sink.clone(), None).is_err());
            let before = sink
                .fetches
                .lock()
                .unwrap()
                .iter()
                .find(|(name, _)| name.ends_with("/3.root.json"))
                .unwrap()
                .1
                .clone();
            let after = store(&path);
            assert_eq!(before.root_version().unwrap(), 2);
            assert_eq!(after.root_version().unwrap(), 2);
            let before_time: jiff::Timestamp = before.state.verified_at.parse().unwrap();
            let after_time: jiff::Timestamp = after.state.verified_at.parse().unwrap();
            assert!(after_time >= before_time);
            assert!(
                !sink
                    .fetches
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(name, _)| name.ends_with("/timestamp.json"))
            );
        }
    }

    #[test]
    fn stale_final_checkpoint_cannot_lower_committed_root_versions_or_time() {
        let _serial = SERIAL.lock().unwrap();
        let repo = Repo::new(2, true, 2);
        let old = Repo::new(1, false, 1);
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        let sink = Arc::new(RecordingSink::new(
            CheckpointStore::create(&path).unwrap(),
            path.clone(),
        ));
        let update = attempt(&repo, sink.clone(), None).unwrap();
        let bytes = store(&path).to_json().unwrap();
        assert!(
            sink.commit(&old.checkpoint())
                .unwrap_err()
                .to_string()
                .contains("regress")
        );
        assert_eq!(store(&path).to_json().unwrap(), bytes);
        let mut state = update.checkpoint.state.clone();
        state.verified_at = "2000-01-01T00:00:00Z".into();
        let stale = crate::make_checkpoint(state, &Limits::default()).unwrap();
        assert!(
            sink.commit(&stale)
                .unwrap_err()
                .to_string()
                .contains("regress")
        );
        assert_eq!(store(&path).to_json().unwrap(), bytes);
        assert!(sink.failure().is_some());
    }

    #[test]
    fn harvest_failure_commits_valid_prefix_and_blocks_all_later_fetches() {
        let _serial = SERIAL.lock().unwrap();
        let old = Repo::new(2, false, 2);
        let previous = old.checkpoint();
        let repo = Repo::new(3, true, 1);
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        let mut owned = CheckpointStore::create(&path).unwrap();
        owned
            .commit_checkpoint(&previous, &Limits::default())
            .unwrap();
        let sink = Arc::new(RecordingSink::new(owned, path.clone()));
        let result = attempt(&repo, sink.clone(), Some(previous));
        assert!(result.unwrap_err().to_string().contains("role rollback"));
        let checkpoint = store(&path);
        assert_eq!(checkpoint.root_version().unwrap(), 2);
        assert_eq!(checkpoint.state.timestamp_version, 3);
        assert_eq!(checkpoint.state.snapshot_version, 3);
        assert_eq!(checkpoint.state.role_versions["targets.json"], 2);
        assert!(
            !sink
                .fetches
                .lock()
                .unwrap()
                .iter()
                .any(|(name, _)| name.ends_with("/targets.json"))
        );
    }

    #[test]
    fn prepopulated_harvest_failure_is_latched_inside_root_fetch_path() {
        let _serial = SERIAL.lock().unwrap();
        let old = Repo::new(2, false, 2);
        let previous = old.checkpoint();
        let repo = Repo::new(3, true, 1);
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        let private = root.path().join("private");
        fs::create_dir(&private).unwrap();
        // Let Tough itself authenticate/write these metadata steps without the prior role floor.
        let metadata = repo.dir.path().join("metadata");
        let targets = repo.dir.path().join("targets");
        runtime()
            .block_on(
                tough::RepositoryLoader::new(
                    &repo.bootstrap,
                    Url::from_directory_path(&metadata).unwrap(),
                    Url::from_directory_path(&targets).unwrap(),
                )
                .transport(tough::FilesystemTransport)
                .datastore(&private)
                .load(),
            )
            .unwrap();
        let mut owned = CheckpointStore::create(&path).unwrap();
        owned
            .commit_checkpoint(&previous, &Limits::default())
            .unwrap();
        let sink = Arc::new(RecordingSink::new(owned, path.clone()));
        let transport = GatedFilesystem {
            gate: Arc::new(Mutex::new(FetchGate {
                datastore: private.clone(),
                metadata: metadata.clone(),
                bootstrap: repo.bootstrap.clone(),
                started: jiff::Timestamp::now(),
                limits: Limits::default(),
                progress: Arc::new(Mutex::new(Some(previous))),
            })),
            sink: sink.clone(),
        };
        let current = fs::read(metadata.join("2.root.json")).unwrap();
        let result = runtime().block_on(
            tough::RepositoryLoader::new(
                &current,
                Url::from_directory_path(&metadata).unwrap(),
                Url::from_directory_path(&targets).unwrap(),
            )
            .transport(transport)
            .datastore(&private)
            .load(),
        );
        assert!(result.is_err());
        assert!(sink.failure().unwrap().contains("role rollback"));
        assert!(sink.fetches.lock().unwrap().is_empty());
        let checkpoint = store(&path);
        assert_eq!(checkpoint.root_version().unwrap(), 2);
        assert_eq!(checkpoint.state.timestamp_version, 3);
        assert_eq!(checkpoint.state.role_versions["targets.json"], 2);
    }

    #[test]
    fn rejected_admission_and_spawn_return_fresh_store_ownership() {
        let _serial = SERIAL.lock().unwrap();
        let repo = Repo::new(1, false, 1);
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        let owned = CheckpointStore::create(&path).unwrap();
        let permits: Vec<_> = (0..MAX_WORKERS)
            .map(|_| Admission::acquire().unwrap())
            .collect();
        let error = start_update(owned, repo.request()).unwrap_err();
        assert!(error.error.to_string().contains("limit"));
        assert!(
            error
                .store
                .load_checkpoint(&Limits::default())
                .unwrap()
                .is_none()
        );
        assert!(CheckpointStore::open(&path).is_err());
        drop(permits);
        let error = start_with_spawn(error.store, repo.request(), |job| {
            drop(job);
            Err(std::io::Error::other("injected spawn failure"))
        })
        .unwrap_err();
        assert!(error.error.to_string().contains("start owned"));
        assert!(
            error
                .store
                .load_checkpoint(&Limits::default())
                .unwrap()
                .is_none()
        );
        assert_eq!(ACTIVE.load(Ordering::Acquire), 0);
        assert_eq!(
            start_update(error.store, repo.request())
                .unwrap()
                .wait()
                .unwrap()
                .bytes,
            BODY
        );
        assert_eq!(ACTIVE.load(Ordering::Acquire), 0);
    }

    #[test]
    fn dropped_handle_detaches_worker_which_persists_and_releases_lock() {
        let _serial = SERIAL.lock().unwrap();
        let repo = Repo::new(2, true, 2);
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        let owned = CheckpointStore::create(&path).unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let worker_barrier = barrier.clone();
        let handle = start_with_spawn(owned, repo.request(), move |job| {
            Ok(thread::spawn(move || {
                worker_barrier.wait();
                job()
            }))
        })
        .unwrap();
        assert!(!handle.is_finished());
        assert!(CheckpointStore::open(&path).is_err());
        drop(handle);
        barrier.wait();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match CheckpointStore::open(&path) {
                Ok(store) => {
                    let checkpoint = store.load_checkpoint(&Limits::default()).unwrap().unwrap();
                    assert_eq!(checkpoint.root_version().unwrap(), 2);
                    assert_eq!(checkpoint.state.timestamp_version, 2);
                    break;
                }
                Err(error) => {
                    assert!(
                        Instant::now() < deadline,
                        "worker did not persist/release: {error}"
                    );
                    thread::sleep(Duration::from_millis(2));
                }
            }
        }
        // The store drops just before the outer job's admission guard; reopening alone
        // does not prove the detached thread has reached that final guard drop yet.
        while ACTIVE.load(Ordering::Acquire) != 0 {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn panicking_worker_releases_admission_and_returns_no_bytes() {
        let _serial = SERIAL.lock().unwrap();
        let repo = Repo::new(1, false, 1);
        let root = tempfile::tempdir().unwrap();
        let owned = CheckpointStore::create(root.path().join("state")).unwrap();
        let handle = start_with_spawn(owned, repo.request(), |job| {
            Ok(thread::spawn(move || {
                drop(job);
                panic!("injected worker panic")
            }))
        })
        .unwrap();
        assert!(handle.wait().unwrap_err().to_string().contains("panicked"));
        assert_eq!(ACTIVE.load(Ordering::Acquire), 0);
    }
}
