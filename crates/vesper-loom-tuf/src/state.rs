//! Unix checkpoint persistence for a caller-protected directory.
//!
//! Serializes cooperating processes and atomically replaces one opaque checkpoint.
//! This does not authenticate decoded checkpoints, protect against a hostile directory
//! owner, or make the verifier's internal TUF steps durable.

use crate::{Checkpoint, Limits, TufError};
use std::{
    fs::{self, File, Metadata, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const LOCK: &str = "checkpoint.lock";
const STATE: &str = "checkpoint.json";

/// Exclusive transaction over a private Unix state directory.
///
/// Parents must be trusted against concurrent replacement. Retain this guard across
/// loading, verification and persistence. Process termination releases the OS lock;
/// the lock file itself must never be deleted. State deletion/rewind is outside this boundary.
pub struct CheckpointStore {
    directory: PathBuf,
    _lock: File,
    fresh: bool,
    usable: bool,
}

impl Drop for CheckpointStore {
    fn drop(&mut self) {
        // A concurrent process spawn can briefly inherit the open description
        // before CLOEXEC closes it. End this transaction explicitly rather than
        // waiting for every such duplicate descriptor to close.
        let _ = File::unlock(&self._lock);
    }
}

fn error(message: impl std::fmt::Display) -> TufError {
    TufError(format!("checkpoint store: {message}"))
}

fn absolute(path: &Path) -> Result<PathBuf, TufError> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir().map_err(error)?.join(path))
    }
}

fn private_owner(info: &Metadata) -> Result<(), TufError> {
    // SAFETY: geteuid has no arguments or memory preconditions.
    let uid = unsafe { libc::geteuid() };
    if info.uid() != uid || info.permissions().mode() & 0o077 != 0 {
        return Err(error(
            "state must be owned by this user with no group/other permissions",
        ));
    }
    Ok(())
}

fn open_private(path: &Path, create: bool) -> Result<File, TufError> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(create)
        .create_new(create)
        .mode(0o600);
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = options.open(path).map_err(error)?;
    let info = file.metadata().map_err(error)?;
    if !info.is_file() {
        return Err(error("state entry is not a regular file"));
    }
    private_owner(&info)?;
    Ok(file)
}

impl CheckpointStore {
    /// Explicit initialization; fails if the directory already exists, even if empty.
    /// An interrupted initialization never silently becomes fresh state through `open`.
    pub fn create(directory: impl AsRef<Path>) -> Result<Self, TufError> {
        let directory = absolute(directory.as_ref())?;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(error)?;
        let store = Self::acquire(&directory, true)?;
        store._lock.sync_all().map_err(error)?;
        File::open(&directory)
            .and_then(|f| f.sync_all())
            .map_err(error)?;
        let parent = directory
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(error)?;
        Ok(store)
    }

    /// Missing lock/checkpoint files are errors, never initialization.
    pub fn open(directory: impl AsRef<Path>) -> Result<Self, TufError> {
        let store = Self::acquire(&absolute(directory.as_ref())?, false)?;
        open_private(&store.directory.join(STATE), false)?;
        Ok(store)
    }

    fn acquire(directory: &Path, create: bool) -> Result<Self, TufError> {
        let info = fs::symlink_metadata(directory).map_err(error)?;
        if !info.is_dir() || info.file_type().is_symlink() {
            return Err(error("state path must be a real directory"));
        }
        private_owner(&info)?;
        let lock = open_private(&directory.join(LOCK), create)?;
        lock.try_lock()
            .map_err(|e| error(format!("cannot acquire exclusive lock: {e}")))?;
        if !create {
            let mut removed = false;
            for (index, entry) in fs::read_dir(directory).map_err(error)?.enumerate() {
                if index >= 4096 {
                    return Err(error("state directory entry limit exceeded"));
                }
                let entry = entry.map_err(error)?;
                let name = entry.file_name();
                let temporary = name
                    .to_str()
                    .and_then(|n| n.strip_prefix(".checkpoint-tmp-"))
                    .and_then(|n| n.split_once('-'))
                    .is_some_and(|(pid, counter)| {
                        !pid.is_empty()
                            && !counter.is_empty()
                            && pid.bytes().all(|b| b.is_ascii_digit())
                            && counter.bytes().all(|b| b.is_ascii_digit())
                    });
                if temporary {
                    // Only a dead prior writer can leave staging files while we hold the lock.
                    // Never follow a link or remove a directory through the cleanup path.
                    open_private(&entry.path(), false)?;
                    fs::remove_file(entry.path()).map_err(error)?;
                    removed = true;
                }
            }
            if removed {
                File::open(directory)
                    .and_then(|f| f.sync_all())
                    .map_err(error)?;
            }
        }
        Ok(Self {
            directory: directory.to_path_buf(),
            _lock: lock,
            fresh: create,
            usable: true,
        })
    }

    /// Decode bounded state; the verifier must validate its binding and authenticated chain.
    pub fn load_checkpoint(&self, limits: &Limits) -> Result<Option<Checkpoint>, TufError> {
        self.read_bytes(limits.checkpoint_bytes)?
            .map(|bytes| Checkpoint::from_json(&bytes, limits))
            .transpose()
    }

    /// Atomically replace and sync a verified checkpoint before releasing target bytes.
    /// A storage-write failure makes this guard unusable; reopen to inspect recovered state.
    /// Serialization and size preflight errors do not start a storage write. The byte limit
    /// bounds wire size, not the transient serialization allocation.
    pub fn commit_checkpoint(
        &mut self,
        checkpoint: &Checkpoint,
        limits: &Limits,
    ) -> Result<(), TufError> {
        self.write_bytes(&checkpoint.to_json()?, limits.checkpoint_bytes)
    }

    fn read_bytes(&self, limit: usize) -> Result<Option<Vec<u8>>, TufError> {
        if !self.usable {
            return Err(error("transaction failed; reopen the store"));
        }
        if self.fresh {
            return Ok(None);
        }
        let file = open_private(&self.directory.join(STATE), false)?;
        let mut bytes = Vec::new();
        file.take((limit as u64).saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(error)?;
        if bytes.len() > limit {
            return Err(error("checkpoint exceeds limit"));
        }
        Ok(Some(bytes))
    }

    fn write_bytes(&mut self, bytes: &[u8], limit: usize) -> Result<(), TufError> {
        if !self.usable {
            return Err(error("transaction failed; reopen the store"));
        }
        if bytes.len() > limit {
            return Err(error("checkpoint exceeds limit"));
        }
        self.usable = false;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let (mut file, temporary) = (0..128)
            .find_map(|_| {
                let path = self.directory.join(format!(
                    ".checkpoint-tmp-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match open_private(&path, true) {
                    Ok(file) => Some(Ok((file, Temporary(path)))),
                    Err(_) if path.exists() => None,
                    Err(e) => Some(Err(e)),
                }
            })
            .unwrap_or_else(|| Err(error("temporary file collision limit exceeded")))?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(error)?;
        fs::rename(&temporary.0, self.directory.join(STATE)).map_err(error)?;
        File::open(&self.directory)
            .and_then(|f| f.sync_all())
            .map_err(error)?;
        self.fresh = false;
        self.usable = true;
        Ok(())
    }
}

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::fs::symlink, process::Command};

    #[test]
    fn exclusive_lock_survives_replacement_and_releases_on_drop() {
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("state");
        let mut store = CheckpointStore::create(&path).unwrap();
        assert_eq!(store.read_bytes(100).unwrap(), None);
        store.write_bytes(b"old", 100).unwrap();
        assert!(CheckpointStore::open(&path).is_err());
        store.write_bytes(b"new", 100).unwrap();
        assert!(CheckpointStore::open(&path).is_err());
        assert!(store.write_bytes(b"too large", 2).is_err());
        assert_eq!(store.read_bytes(100).unwrap().unwrap(), b"new");
        drop(store);
        let store = CheckpointStore::open(&path).unwrap();
        assert_eq!(store.read_bytes(100).unwrap().unwrap(), b"new");
        assert!(store.read_bytes(2).is_err());
        assert!(store.load_checkpoint(&Limits::default()).is_err());
        assert!(CheckpointStore::create(&path).is_err());
        assert_eq!(fs::read_dir(&path).unwrap().count(), 2);
    }

    #[test]
    fn ending_transaction_unlocks_even_with_an_inherited_description() {
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("state");
        let mut store = CheckpointStore::create(&path).unwrap();
        store.write_bytes(b"durable", 100).unwrap();
        let inherited = store._lock.try_clone().unwrap();
        drop(store);
        let reopened = CheckpointStore::open(&path).unwrap();
        assert_eq!(reopened.read_bytes(100).unwrap().unwrap(), b"durable");
        drop(inherited);
    }

    #[test]
    fn missing_and_unsafe_state_never_reset_trust() {
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("state");
        let store = CheckpointStore::create(&path).unwrap();
        drop(store);
        assert!(CheckpointStore::open(&path).is_err());
        assert!(CheckpointStore::create(&path).is_err());
        fs::write(path.join(STATE), b"{}").unwrap();
        fs::set_permissions(path.join(STATE), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(CheckpointStore::open(&path).is_err());
        fs::remove_file(path.join(STATE)).unwrap();
        let outside = parent.path().join("outside");
        fs::write(&outside, b"secret").unwrap();
        symlink(&outside, path.join(STATE)).unwrap();
        assert!(CheckpointStore::open(&path).is_err());
        assert_eq!(fs::read(outside).unwrap(), b"secret");
        let link = parent.path().join("link");
        symlink(&path, &link).unwrap();
        assert!(CheckpointStore::open(link).is_err());
    }

    #[test]
    fn write_failure_poisons_guard_and_removes_temporary() {
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("state");
        let mut store = CheckpointStore::create(&path).unwrap();
        store.write_bytes(b"old", 100).unwrap();
        // Force rename failure without relying on root/user permission behavior.
        fs::remove_file(path.join(STATE)).unwrap();
        fs::create_dir(path.join(STATE)).unwrap();
        assert!(store.write_bytes(b"new", 100).is_err());
        assert!(store.read_bytes(100).is_err());
        assert!(store.write_bytes(b"later", 100).is_err());
        assert_eq!(fs::read_dir(&path).unwrap().count(), 2);
    }

    #[test]
    fn process_exit_releases_lock_and_retains_last_checkpoint() {
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("state");
        let mut store = CheckpointStore::create(&path).unwrap();
        store.write_bytes(b"durable", 100).unwrap();
        let blocked = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "state::tests::child_exits_without_dropping_lock"])
            .env("VESPER_TUF_LOCK_CRASH_FIXTURE", &path)
            .env("VESPER_TUF_EXPECT_LOCKED", "1")
            .output()
            .unwrap();
        assert_eq!(blocked.status.code(), Some(24), "{blocked:?}");
        drop(store);
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "state::tests::child_exits_without_dropping_lock"])
            .env("VESPER_TUF_LOCK_CRASH_FIXTURE", &path)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(23), "{output:?}");
        let store = CheckpointStore::open(&path).unwrap();
        assert_eq!(store.read_bytes(100).unwrap().unwrap(), b"durable");
        assert_eq!(fs::read_dir(&path).unwrap().count(), 2);
    }

    #[test]
    fn child_exits_without_dropping_lock() {
        let Some(path) = std::env::var_os("VESPER_TUF_LOCK_CRASH_FIXTURE") else {
            return;
        };
        if std::env::var_os("VESPER_TUF_EXPECT_LOCKED").is_some() {
            assert!(CheckpointStore::open(path).is_err());
            std::process::exit(24);
        }
        let store = CheckpointStore::open(path).unwrap();
        let mut temporary =
            open_private(&store.directory.join(".checkpoint-tmp-999-1"), true).unwrap();
        temporary.write_all(b"unfinished checkpoint").unwrap();
        std::process::exit(23);
    }

    #[test]
    fn relative_store_stays_bound_to_locked_directory_after_cwd_change() {
        let parent = tempfile::tempdir().unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "state::tests::child_changes_current_directory"])
            .env("VESPER_TUF_CWD_FIXTURE", parent.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            fs::read(parent.path().join("a/state/checkpoint.json")).unwrap(),
            b"new original"
        );
        assert_eq!(
            fs::read(parent.path().join("b/state/checkpoint.json")).unwrap(),
            b"other"
        );
    }

    #[test]
    fn child_changes_current_directory() {
        let Some(parent) = std::env::var_os("VESPER_TUF_CWD_FIXTURE") else {
            return;
        };
        let parent = PathBuf::from(parent);
        fs::create_dir(parent.join("a")).unwrap();
        fs::create_dir(parent.join("b")).unwrap();
        let mut other = CheckpointStore::create(parent.join("b/state")).unwrap();
        other.write_bytes(b"other", 100).unwrap();
        std::env::set_current_dir(parent.join("a")).unwrap();
        let mut store = CheckpointStore::create("state").unwrap();
        store.write_bytes(b"original", 100).unwrap();
        std::env::set_current_dir(parent.join("b")).unwrap();
        assert_eq!(store.read_bytes(100).unwrap().unwrap(), b"original");
        store.write_bytes(b"new original", 100).unwrap();
    }
}
