//! The on-disk store.
//!
//! Layout (schema v1) below the store root:
//!
//! ```text
//! LAYOUT                                  layout marker, "vesper-loom-cas layout 1\n"
//! objects/v1/<domain>/<kind>/<aa>/<hex>   immutable, read-only objects
//! derivations/v1/<aa>/<hex>               derivation key -> output records
//! tmp/                                    private staging files
//! quarantine/                             objects/records that failed verification
//! locks/                                  per-key lock files used only while repairing
//! ```
//!
//! Writers stage into `tmp/`, hash while writing, `fsync`, and then publish
//! with a no-replace rename. A crash before publication leaves only a stray
//! staging file that no reader ever looks at. Readers always rehash, so a
//! truncated or bit-flipped object is rejected rather than returned.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::derivation::{DerivationInputs, DerivationKey};
use crate::error::{CasError, NameError, Result, TreeError};
use crate::fsutil::{self, Publish};
use crate::id::{Digest, Domain, OBJECT_ID_BYTES, ObjectHasher, ObjectId, ObjectKind};
use crate::tree::{EntryKind, MAX_TREE_DEPTH, Tree, validate_name};

const LAYOUT_MARKER: &str = "vesper-loom-cas layout 1\n";
const RECORD_MAGIC: &[u8; 4] = b"VLDR";
const RECORD_VERSION: u32 = 1;
const BUF: usize = 64 * 1024;
const PUBLISH_ATTEMPTS: usize = 4;

/// Result of ingesting one object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ingested {
    pub id: ObjectId,
    pub size: u64,
    /// `false` when an identical, verified object was already present.
    pub newly_stored: bool,
}

/// Result of recording a derivation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DerivationOutcome {
    pub key: DerivationKey,
    pub output: ObjectId,
    /// `false` when the same mapping had already been recorded.
    pub newly_recorded: bool,
}

/// How file contents are placed into a workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyStrategy {
    /// Try a copy-on-write clone (`FICLONE`, `clonefile`), fall back to a
    /// verified byte copy when the file system cannot clone.
    ReflinkOrCopy,
    /// Always stream-copy and verify.
    CopyOnly,
    /// Fail with [`CasError::ReflinkUnavailable`] instead of copying.
    ReflinkOnly,
}

/// Materialization options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaterializeOptions {
    pub strategy: CopyStrategy,
    /// Rehash each reflinked file after cloning. Copies are always hashed
    /// while they are written; this only controls the extra read for clones.
    pub verify_reflinks: bool,
}

impl Default for MaterializeOptions {
    fn default() -> Self {
        MaterializeOptions {
            strategy: CopyStrategy::ReflinkOrCopy,
            verify_reflinks: true,
        }
    }
}

/// What a materialization did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MaterializeReport {
    pub directories: u64,
    pub files: u64,
    pub bytes: u64,
    pub reflinked: u64,
    pub copied: u64,
}

/// Result of a full store scan.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoreReport {
    pub verified_objects: u64,
    pub corrupt_objects: Vec<ObjectId>,
    pub verified_derivations: u64,
    pub corrupt_derivations: Vec<PathBuf>,
    /// Files under `objects/` or `derivations/` whose names are not valid
    /// identities. They are never read as objects.
    pub stray_paths: Vec<PathBuf>,
}

impl StoreReport {
    pub fn is_clean(&self) -> bool {
        self.corrupt_objects.is_empty()
            && self.corrupt_derivations.is_empty()
            && self.stray_paths.is_empty()
    }
}

/// A content-addressed store rooted at one directory. Any number of `Store`
/// values, threads and processes may use the same root concurrently.
#[derive(Clone, Debug)]
pub struct Store {
    root: PathBuf,
    objects: PathBuf,
    derivations: PathBuf,
    tmp: PathBuf,
    quarantine: PathBuf,
    locks: PathBuf,
}

/// Removes a staging path on drop unless disarmed.
struct StagedFile {
    path: PathBuf,
    armed: bool,
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Removes a staging directory tree on drop unless disarmed.
struct StagedDir {
    path: PathBuf,
    armed: bool,
}

impl Drop for StagedDir {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

enum Existing {
    Valid,
    Invalid,
    Gone,
}

impl Store {
    /// Open (creating if necessary) a store at `root`.
    pub fn open(root: impl AsRef<Path>) -> Result<Store> {
        let root = root.as_ref();
        fs::create_dir_all(root).map_err(|e| CasError::io("create store root", root, e))?;
        let root = root
            .canonicalize()
            .map_err(|e| CasError::io("resolve store root", root, e))?;
        let store = Store {
            objects: root.join("objects").join("v1"),
            derivations: root.join("derivations").join("v1"),
            tmp: root.join("tmp"),
            quarantine: root.join("quarantine"),
            locks: root.join("locks"),
            root,
        };
        for dir in [
            store.root.join("objects"),
            store.objects.clone(),
            store.root.join("derivations"),
            store.derivations.clone(),
            store.tmp.clone(),
            store.quarantine.clone(),
            store.locks.clone(),
        ] {
            ensure_plain_dir(&dir)?;
        }
        store.ensure_marker()?;
        Ok(store)
    }

    fn ensure_marker(&self) -> Result<()> {
        let marker = self.root.join("LAYOUT");
        match fs::symlink_metadata(&marker) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let staged = self.stage_bytes(LAYOUT_MARKER.as_bytes())?;
                fsutil::publish_noreplace(&staged.path, &marker)
                    .map_err(|e| CasError::io("publish layout marker", &marker, e))?;
            }
            Err(e) => return Err(CasError::io("inspect layout marker", &marker, e)),
        }
        let mut file =
            fsutil::open_regular_nofollow(&marker).map_err(|_| CasError::UnsafeStoreLayout {
                path: marker.clone(),
                reason: "layout marker is not a regular file".into(),
            })?;
        let mut text = String::new();
        Read::by_ref(&mut file)
            .take(1024)
            .read_to_string(&mut text)
            .map_err(|e| CasError::io("read layout marker", &marker, e))?;
        if text != LAYOUT_MARKER {
            return Err(CasError::UnsafeStoreLayout {
                path: marker,
                reason: format!("unknown layout marker {text:?}"),
            });
        }
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The path an object is stored at. The path is derived only from the
    /// validated id, never from caller-supplied text.
    pub fn object_path(&self, id: &ObjectId) -> PathBuf {
        let hex = id.digest.to_hex();
        self.objects
            .join(id.domain.as_str())
            .join(id.kind.as_str())
            .join(&hex[..2])
            .join(hex)
    }

    fn record_path(&self, key: &DerivationKey) -> PathBuf {
        let hex = key.0.to_hex();
        self.derivations.join(&hex[..2]).join(hex)
    }

    /// Whether an object file is present (contents are not rehashed).
    pub fn contains(&self, id: &ObjectId) -> Result<bool> {
        let path = self.object_path(id);
        match fs::symlink_metadata(&path) {
            Ok(meta) => Ok(meta.is_file()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(CasError::io("inspect object", path, e)),
        }
    }

    // ----------------------------------------------------------------------
    // Ingestion
    // ----------------------------------------------------------------------

    /// Store `bytes` as a blob.
    pub fn put_blob(&self, domain: Domain, bytes: &[u8]) -> Result<Ingested> {
        self.put_reader(domain, ObjectKind::Blob, bytes, None)
    }

    /// Stream a blob into the store. If the reader fails, nothing is
    /// published and the staging file is removed.
    pub fn put_blob_reader(&self, domain: Domain, reader: impl Read) -> Result<Ingested> {
        self.put_reader(domain, ObjectKind::Blob, reader, None)
    }

    /// Stream a blob and require it to hash to `expected` (e.g. a digest from
    /// a lockfile). On mismatch nothing is published.
    pub fn put_blob_expected(&self, expected: ObjectId, reader: impl Read) -> Result<Ingested> {
        if expected.kind != ObjectKind::Blob {
            return Err(CasError::WrongKind {
                id: expected,
                expected: ObjectKind::Blob,
            });
        }
        self.put_reader(expected.domain, ObjectKind::Blob, reader, Some(expected))
    }

    /// Ingest a regular file. Symlinks and special files are rejected.
    pub fn put_file(&self, domain: Domain, path: impl AsRef<Path>) -> Result<Ingested> {
        let path = path.as_ref();
        let file = open_input_file(path)?;
        self.put_reader(domain, ObjectKind::Blob, file, None)
    }

    /// Store a tree after checking that every child is present with the
    /// recorded size and kind.
    pub fn put_tree(&self, tree: &Tree) -> Result<Ingested> {
        for (_, entry) in tree.entries() {
            let child = tree.child_id(entry);
            let path = self.object_path(&child);
            let meta = match fs::symlink_metadata(&path) {
                Ok(m) if m.is_file() => m,
                Ok(_) => {
                    return Err(CasError::UnsafeStoreLayout {
                        path,
                        reason: "object is not a regular file".into(),
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    return Err(CasError::NotFound(child));
                }
                Err(e) => return Err(CasError::io("inspect tree child", path, e)),
            };
            if let EntryKind::File { .. } = entry.kind
                && meta.len() != entry.size
            {
                return Err(CasError::SizeMismatch {
                    id: child,
                    expected: entry.size,
                    actual: meta.len(),
                });
            }
        }
        let bytes = tree.encode();
        self.put_reader(tree.domain(), ObjectKind::Tree, &bytes[..], None)
    }

    /// Recursively ingest a directory as a tree of blobs in `domain`.
    ///
    /// Only regular files and directories are accepted. Symlinks (to files,
    /// directories or nowhere), FIFOs, sockets and devices are rejected, as
    /// are names that fail the portable naming policy. Hard-linked inputs are
    /// read like any other file; the store never hard-links them.
    pub fn ingest_dir(&self, domain: Domain, path: impl AsRef<Path>) -> Result<Ingested> {
        let path = path.as_ref();
        let meta =
            fs::symlink_metadata(path).map_err(|e| CasError::io("inspect directory", path, e))?;
        if !meta.is_dir() {
            return Err(CasError::UnsupportedFileType {
                path: path.to_owned(),
                kind: file_kind(&meta),
            });
        }
        self.ingest_dir_at(domain, path, 0)
    }

    fn ingest_dir_at(&self, domain: Domain, dir: &Path, depth: usize) -> Result<Ingested> {
        if depth >= MAX_TREE_DEPTH {
            return Err(TreeError::TooDeep {
                limit: MAX_TREE_DEPTH,
            }
            .into());
        }
        let mut tree = Tree::new(domain);
        let entries = fs::read_dir(dir).map_err(|e| CasError::io("read directory", dir, e))?;
        for entry in entries {
            let entry = entry.map_err(|e| CasError::io("read directory", dir, e))?;
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_str().ok_or_else(|| CasError::InvalidName {
                path: path.clone(),
                reason: NameError::NotUtf8,
            })?;
            validate_name(name).map_err(|reason| CasError::InvalidName {
                path: path.clone(),
                reason,
            })?;
            let meta =
                fs::symlink_metadata(&path).map_err(|e| CasError::io("inspect entry", &path, e))?;
            if meta.is_dir() {
                let child = self.ingest_dir_at(domain, &path, depth + 1)?;
                tree.insert_dir(name, child.id)?;
            } else if meta.is_file() {
                let file = open_input_file(&path)?;
                let executable = fsutil::is_executable(
                    &file
                        .metadata()
                        .map_err(|e| CasError::io("inspect file", &path, e))?,
                );
                let blob = self.put_reader(domain, ObjectKind::Blob, file, None)?;
                tree.insert_file(name, blob.id, blob.size, executable)?;
            } else {
                return Err(CasError::UnsupportedFileType {
                    path,
                    kind: file_kind(&meta),
                });
            }
        }
        self.put_tree(&tree)
    }

    fn stage_bytes(&self, bytes: &[u8]) -> Result<StagedFile> {
        let (staged, mut file) = self.new_staging_file()?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| CasError::io("write staging file", &staged.path, e))?;
        Ok(staged)
    }

    fn new_staging_file(&self) -> Result<(StagedFile, fs::File)> {
        loop {
            let path = self.tmp.join(fsutil::unique_name("stage-"));
            match fsutil::create_new(&path) {
                Ok(file) => return Ok((StagedFile { path, armed: true }, file)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(CasError::io("create staging file", path, e)),
            }
        }
    }

    fn put_reader(
        &self,
        domain: Domain,
        kind: ObjectKind,
        mut reader: impl Read,
        expected: Option<ObjectId>,
    ) -> Result<Ingested> {
        let (staged, mut file) = self.new_staging_file()?;
        let mut hasher = ObjectHasher::new(domain, kind);
        let mut buf = vec![0u8; BUF];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(CasError::io("read ingestion input", &staged.path, e)),
            };
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n])
                .map_err(|e| CasError::io("write staging file", &staged.path, e))?;
        }
        let id = hasher.finish();
        let size = hasher.len();
        if let Some(expected) = expected
            && expected != id
        {
            return Err(CasError::DigestMismatch {
                expected,
                actual: id,
            });
        }
        file.sync_all()
            .and_then(|()| fsutil::make_readonly(&file))
            .map_err(|e| CasError::io("flush staging file", &staged.path, e))?;
        drop(file);
        let newly_stored = self.publish_object(staged, &id)?;
        Ok(Ingested {
            id,
            size,
            newly_stored,
        })
    }

    fn publish_object(&self, mut staged: StagedFile, id: &ObjectId) -> Result<bool> {
        let dest = self.object_path(id);
        let parent = dest.parent().expect("object paths have a parent");
        fs::create_dir_all(parent).map_err(|e| CasError::io("create shard", parent, e))?;
        for _ in 0..PUBLISH_ATTEMPTS {
            match fsutil::publish_noreplace(&staged.path, &dest)
                .map_err(|e| CasError::io("publish object", &dest, e))?
            {
                Publish::Published => {
                    staged.armed = false;
                    fsutil::sync_dir(parent).map_err(|e| CasError::io("sync shard", parent, e))?;
                    return Ok(true);
                }
                Publish::Exists => match self.check_existing_object(id, &dest)? {
                    // Identical verified contents: deduplicate. The staged
                    // copy is removed by the guard.
                    Existing::Valid => return Ok(false),
                    Existing::Invalid => self.repair(&dest, &object_lock_name(id), || {
                        Ok(matches!(
                            self.check_existing_object(id, &dest)?,
                            Existing::Invalid
                        ))
                    })?,
                    Existing::Gone => {}
                },
            }
        }
        Err(CasError::PublishContention(*id))
    }

    fn check_existing_object(&self, id: &ObjectId, path: &Path) -> Result<Existing> {
        match self.verify_at(id, path) {
            Ok(_) => Ok(Existing::Valid),
            Err(CasError::NotFound(_)) => Ok(Existing::Gone),
            Err(CasError::Corrupt { .. }) | Err(CasError::UnsafeStoreLayout { .. }) => {
                Ok(Existing::Invalid)
            }
            Err(e) => Err(e),
        }
    }

    /// Move a failed object or record out of the addressable namespace.
    ///
    /// Repairs are serialized per key with an exclusive lock, and the entry
    /// is re-validated while the lock is held. Without this, two writers that
    /// both saw the same corrupt entry could race: the first quarantines it
    /// and publishes a valid replacement, and the second, acting on its stale
    /// check, would quarantine that valid replacement. Ordinary publication
    /// needs no lock: it can only succeed while the path is empty, and only a
    /// lock holder ever empties an occupied path.
    fn repair(
        &self,
        path: &Path,
        lock_name: &str,
        still_invalid: impl FnOnce() -> Result<bool>,
    ) -> Result<()> {
        let lock_path = self.locks.join(lock_name);
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(|e| CasError::io("open repair lock", &lock_path, e))?;
        lock.lock()
            .map_err(|e| CasError::io("acquire repair lock", &lock_path, e))?;
        if !still_invalid()? {
            // Already repaired (or removed) by another writer.
            return Ok(());
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("object");
        let target = self
            .quarantine
            .join(format!("{name}.{}", fsutil::unique_name("q-")));
        match fs::rename(path, &target) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(CasError::io("quarantine", path, e)),
        }
        // `lock` is released when dropped here.
    }

    // ----------------------------------------------------------------------
    // Readback
    // ----------------------------------------------------------------------

    /// Rehash an object and return its size.
    pub fn verify_object(&self, id: &ObjectId) -> Result<u64> {
        self.verify_at(id, &self.object_path(id))
    }

    fn verify_at(&self, id: &ObjectId, path: &Path) -> Result<u64> {
        let mut file = self.open_object_at(id, path)?;
        let mut hasher = ObjectHasher::new(id.domain, id.kind);
        let mut buf = vec![0u8; BUF];
        loop {
            let n = read_retry(&mut file, &mut buf).map_err(|e| CasError::io("read", path, e))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        let actual = hasher.finish();
        if actual != *id {
            return Err(CasError::Corrupt {
                id: *id,
                actual,
                path: path.to_owned(),
            });
        }
        Ok(hasher.len())
    }

    fn open_object_at(&self, id: &ObjectId, path: &Path) -> Result<fs::File> {
        match fs::symlink_metadata(path) {
            Ok(meta) if meta.is_file() => {}
            Ok(_) => {
                return Err(CasError::UnsafeStoreLayout {
                    path: path.to_owned(),
                    reason: "object is not a regular file".into(),
                });
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(CasError::NotFound(*id)),
            Err(e) => return Err(CasError::io("inspect object", path, e)),
        }
        fsutil::open_regular_nofollow(path).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => CasError::NotFound(*id),
            _ => CasError::io("open object", path, e),
        })
    }

    /// Read a blob and verify it hashes to `id`.
    pub fn read_blob(&self, id: &ObjectId) -> Result<Vec<u8>> {
        if id.kind != ObjectKind::Blob {
            return Err(CasError::WrongKind {
                id: *id,
                expected: ObjectKind::Blob,
            });
        }
        self.read_verified(id)
    }

    fn read_verified(&self, id: &ObjectId) -> Result<Vec<u8>> {
        let path = self.object_path(id);
        let mut file = self.open_object_at(id, &path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|e| CasError::io("read object", &path, e))?;
        let mut hasher = ObjectHasher::new(id.domain, id.kind);
        hasher.update(&bytes);
        let actual = hasher.finish();
        if actual != *id {
            return Err(CasError::Corrupt {
                id: *id,
                actual,
                path,
            });
        }
        Ok(bytes)
    }

    /// Read, verify and strictly decode a tree.
    pub fn read_tree(&self, id: &ObjectId) -> Result<Tree> {
        if id.kind != ObjectKind::Tree {
            return Err(CasError::WrongKind {
                id: *id,
                expected: ObjectKind::Tree,
            });
        }
        let bytes = self.read_verified(id)?;
        Ok(Tree::decode(id.domain, &bytes)?)
    }

    // ----------------------------------------------------------------------
    // Derivations
    // ----------------------------------------------------------------------

    /// Record that `inputs` produce `output`. Every input and the output must
    /// already be stored, and the output must be in the declared output
    /// domain. An existing record for the same key is never replaced: a
    /// different output is reported as [`CasError::DerivationConflict`].
    ///
    /// A record states only what a local builder produced. It is not a
    /// provenance, signature or reproducibility attestation.
    pub fn publish_derivation(
        &self,
        inputs: &DerivationInputs,
        output: ObjectId,
    ) -> Result<DerivationOutcome> {
        let encoding = inputs.encode()?;
        let key = DerivationKey::of_encoding(&encoding);
        if output.domain != inputs.output_domain {
            return Err(CasError::DomainMismatch {
                expected: inputs.output_domain,
                found: output.domain,
            });
        }
        for id in inputs.inputs.values().chain(std::iter::once(&output)) {
            if !self.contains(id)? {
                return Err(CasError::NotFound(*id));
            }
        }
        let mut record = Vec::with_capacity(8 + OBJECT_ID_BYTES + encoding.len());
        record.extend_from_slice(RECORD_MAGIC);
        record.extend_from_slice(&RECORD_VERSION.to_le_bytes());
        output.encode_into(&mut record);
        record.extend_from_slice(&encoding);

        let dest = self.record_path(&key);
        let parent = dest.parent().expect("record paths have a parent");
        fs::create_dir_all(parent).map_err(|e| CasError::io("create shard", parent, e))?;
        for _ in 0..PUBLISH_ATTEMPTS {
            let mut staged = self.stage_bytes(&record)?;
            match fsutil::publish_noreplace(&staged.path, &dest)
                .map_err(|e| CasError::io("publish derivation", &dest, e))?
            {
                Publish::Published => {
                    staged.armed = false;
                    fsutil::sync_dir(parent).map_err(|e| CasError::io("sync shard", parent, e))?;
                    return Ok(DerivationOutcome {
                        key,
                        output,
                        newly_recorded: true,
                    });
                }
                Publish::Exists => match self.read_record(&key, &dest) {
                    Ok(Some((existing, _))) if existing == output => {
                        return Ok(DerivationOutcome {
                            key,
                            output,
                            newly_recorded: false,
                        });
                    }
                    Ok(Some((existing, _))) => {
                        return Err(CasError::DerivationConflict {
                            key,
                            existing,
                            proposed: output,
                        });
                    }
                    Ok(None) => {}
                    Err(CasError::CorruptDerivationRecord { .. }) => {
                        let lock = format!("drv-{}", key.0.to_hex());
                        self.repair(&dest, &lock, || match self.read_record(&key, &dest) {
                            Err(CasError::CorruptDerivationRecord { .. }) => Ok(true),
                            Ok(_) => Ok(false),
                            Err(e) => Err(e),
                        })?
                    }
                    Err(e) => return Err(e),
                },
            }
        }
        Err(CasError::PublishContention(output))
    }

    /// Look up the recorded output for `inputs`. Returns `Ok(None)` on a
    /// miss. A record whose output object has been removed is an error, not a
    /// silent miss. The output's contents are verified when it is read.
    pub fn lookup_derivation(&self, inputs: &DerivationInputs) -> Result<Option<ObjectId>> {
        let encoding = inputs.encode()?;
        let key = DerivationKey::of_encoding(&encoding);
        let path = self.record_path(&key);
        match self.read_record(&key, &path)? {
            None => Ok(None),
            Some((output, stored)) => {
                if stored != encoding {
                    return Err(CasError::CorruptDerivationRecord { key, path });
                }
                if !self.contains(&output)? {
                    return Err(CasError::NotFound(output));
                }
                Ok(Some(output))
            }
        }
    }

    /// Read and validate a record. The stored inputs must re-encode
    /// canonically and hash to `key`.
    fn read_record(&self, key: &DerivationKey, path: &Path) -> Result<Option<(ObjectId, Vec<u8>)>> {
        let corrupt = || CasError::CorruptDerivationRecord {
            key: *key,
            path: path.to_owned(),
        };
        match fs::symlink_metadata(path) {
            Ok(meta) if meta.is_file() => {}
            Ok(_) => return Err(corrupt()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(CasError::io("inspect derivation", path, e)),
        }
        let mut file = match fsutil::open_regular_nofollow(path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(corrupt()),
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|e| CasError::io("read derivation", path, e))?;
        let header = 8 + OBJECT_ID_BYTES;
        if bytes.len() < header
            || &bytes[..4] != RECORD_MAGIC
            || bytes[4..8] != RECORD_VERSION.to_le_bytes()
        {
            return Err(corrupt());
        }
        let output = ObjectId::decode(&bytes[8..header]).ok_or_else(corrupt)?;
        let encoding = bytes[header..].to_vec();
        let inputs = DerivationInputs::decode(&encoding).map_err(|_| corrupt())?;
        if DerivationKey::of_encoding(&encoding) != *key || inputs.output_domain != output.domain {
            return Err(corrupt());
        }
        Ok(Some((output, encoding)))
    }

    // ----------------------------------------------------------------------
    // Materialization
    // ----------------------------------------------------------------------

    /// Materialize a tree as a fresh, writable directory at `dest`.
    ///
    /// `dest` must not exist. The tree is written into a uniquely named
    /// sibling staging directory and renamed into place only after every
    /// file has been verified, so `dest` either appears complete or not at
    /// all. Workspace files are independent copies (reflink clones or byte
    /// copies); they are never hard links or symlinks into the store, so
    /// editing a workspace cannot modify a stored object.
    pub fn materialize(
        &self,
        tree: &ObjectId,
        dest: impl AsRef<Path>,
        options: MaterializeOptions,
    ) -> Result<MaterializeReport> {
        let dest = dest.as_ref();
        if tree.kind != ObjectKind::Tree {
            return Err(CasError::WrongKind {
                id: *tree,
                expected: ObjectKind::Tree,
            });
        }
        if dest.file_name().and_then(|n| n.to_str()).is_none() {
            return Err(CasError::InvalidName {
                path: dest.to_owned(),
                reason: NameError::DotOrDotDot,
            });
        }
        match fs::symlink_metadata(dest) {
            Ok(_) => return Err(CasError::DestinationExists(dest.to_owned())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(CasError::io("inspect destination", dest, e)),
        }
        let parent = match dest.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_owned(),
            _ => PathBuf::from("."),
        };
        // Do not include the destination's leaf: a valid NAME_MAX-sized
        // destination would make a suffixed staging name exceed NAME_MAX.
        let staging = parent.join(fsutil::unique_name(".vesper-staging-"));
        fs::create_dir(&staging)
            .map_err(|e| CasError::io("create staging directory", &staging, e))?;
        let mut guard = StagedDir {
            path: staging.clone(),
            armed: true,
        };
        let mut report = MaterializeReport {
            directories: 1,
            ..MaterializeReport::default()
        };
        self.materialize_tree(tree, &staging, 0, options, &mut report)?;
        match fsutil::publish_dir_noreplace(&staging, dest)
            .map_err(|e| CasError::io("publish workspace", dest, e))?
        {
            Publish::Published => {
                guard.armed = false;
                fsutil::sync_dir(&parent).map_err(|e| CasError::io("sync", &parent, e))?;
                Ok(report)
            }
            Publish::Exists => Err(CasError::DestinationExists(dest.to_owned())),
        }
    }

    fn materialize_tree(
        &self,
        id: &ObjectId,
        dir: &Path,
        depth: usize,
        options: MaterializeOptions,
        report: &mut MaterializeReport,
    ) -> Result<()> {
        if depth >= MAX_TREE_DEPTH {
            return Err(TreeError::TooDeep {
                limit: MAX_TREE_DEPTH,
            }
            .into());
        }
        // Names were validated by the strict decoder; join cannot escape.
        let tree = self.read_tree(id)?;
        for (name, entry) in tree.entries() {
            let target = dir.join(name);
            let child = tree.child_id(entry);
            match entry.kind {
                EntryKind::Directory => {
                    fs::create_dir(&target)
                        .map_err(|e| CasError::io("create directory", &target, e))?;
                    report.directories += 1;
                    self.materialize_tree(&child, &target, depth + 1, options, report)?;
                }
                EntryKind::File { executable } => {
                    self.materialize_file(&child, entry.size, &target, options, report)?;
                    fsutil::set_workspace_mode(&target, executable)
                        .map_err(|e| CasError::io("set permissions", &target, e))?;
                    report.files += 1;
                    report.bytes += entry.size;
                }
            }
        }
        Ok(())
    }

    fn materialize_file(
        &self,
        id: &ObjectId,
        size: u64,
        target: &Path,
        options: MaterializeOptions,
        report: &mut MaterializeReport,
    ) -> Result<()> {
        let src = self.object_path(id);
        if options.strategy != CopyStrategy::CopyOnly {
            // Make sure the source is a regular object before cloning it.
            drop(self.open_object_at(id, &src)?);
            match reflink_copy::reflink(&src, target) {
                Ok(()) => {
                    if options.verify_reflinks {
                        let actual = hash_file(id.domain, id.kind, target)?;
                        if actual.0 != *id {
                            let _ = fs::remove_file(target);
                            return Err(CasError::Corrupt {
                                id: *id,
                                actual: actual.0,
                                path: src,
                            });
                        }
                        if actual.1 != size {
                            let _ = fs::remove_file(target);
                            return Err(CasError::SizeMismatch {
                                id: *id,
                                expected: size,
                                actual: actual.1,
                            });
                        }
                    }
                    report.reflinked += 1;
                    return Ok(());
                }
                Err(e) => {
                    // A failed clone may leave an empty file behind; it lives
                    // inside our private staging directory.
                    if let Ok(meta) = fs::symlink_metadata(target)
                        && meta.is_file()
                    {
                        let _ = fs::remove_file(target);
                    }
                    if options.strategy == CopyStrategy::ReflinkOnly {
                        return Err(CasError::ReflinkUnavailable {
                            path: target.to_owned(),
                            source: e,
                        });
                    }
                }
            }
        }
        self.copy_verified(id, size, &src, target)?;
        report.copied += 1;
        Ok(())
    }

    fn copy_verified(&self, id: &ObjectId, size: u64, src: &Path, target: &Path) -> Result<()> {
        let mut input = self.open_object_at(id, src)?;
        let mut output =
            fsutil::create_new(target).map_err(|e| CasError::io("create file", target, e))?;
        let mut hasher = ObjectHasher::new(id.domain, id.kind);
        let mut buf = vec![0u8; BUF];
        loop {
            let n = read_retry(&mut input, &mut buf).map_err(|e| CasError::io("read", src, e))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            output
                .write_all(&buf[..n])
                .map_err(|e| CasError::io("write", target, e))?;
        }
        let actual = hasher.finish();
        if actual != *id {
            return Err(CasError::Corrupt {
                id: *id,
                actual,
                path: src.to_owned(),
            });
        }
        if hasher.len() != size {
            return Err(CasError::SizeMismatch {
                id: *id,
                expected: size,
                actual: hasher.len(),
            });
        }
        Ok(())
    }

    // ----------------------------------------------------------------------
    // Maintenance
    // ----------------------------------------------------------------------

    /// Remove staging files older than `min_age`, such as those left by a
    /// crashed writer. Staging files are never addressable, so removing them
    /// cannot affect readers. Use an age comfortably longer than the slowest
    /// expected ingestion, since a live writer's file is also in `tmp/`.
    pub fn sweep_temporaries(&self, min_age: Duration) -> Result<usize> {
        let now = SystemTime::now();
        let mut removed = 0;
        let entries =
            fs::read_dir(&self.tmp).map_err(|e| CasError::io("read directory", &self.tmp, e))?;
        for entry in entries {
            let entry = entry.map_err(|e| CasError::io("read directory", &self.tmp, e))?;
            let path = entry.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                continue;
            }
            let age = meta
                .modified()
                .ok()
                .and_then(|m| now.duration_since(m).ok())
                .unwrap_or(Duration::ZERO);
            if age >= min_age {
                match fs::remove_file(&path) {
                    Ok(()) => removed += 1,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(CasError::io("remove staging file", path, e)),
                }
            }
        }
        Ok(removed)
    }

    /// Rehash every object and revalidate every derivation record.
    pub fn verify_all(&self) -> Result<StoreReport> {
        let mut report = StoreReport::default();
        for domain in Domain::ALL {
            for kind in ObjectKind::ALL {
                let base = self.objects.join(domain.as_str()).join(kind.as_str());
                for (path, hex) in shard_files(&base, &mut report.stray_paths)? {
                    let Some(digest) = Digest::from_hex(&hex) else {
                        report.stray_paths.push(path);
                        continue;
                    };
                    let id = ObjectId::new(domain, kind, digest);
                    let ok = match self.verify_at(&id, &path) {
                        Ok(_) => kind != ObjectKind::Tree || self.read_tree(&id).is_ok(),
                        Err(CasError::Corrupt { .. }) | Err(CasError::UnsafeStoreLayout { .. }) => {
                            false
                        }
                        Err(CasError::NotFound(_)) => continue,
                        Err(e) => return Err(e),
                    };
                    if ok {
                        report.verified_objects += 1;
                    } else {
                        report.corrupt_objects.push(id);
                    }
                }
            }
        }
        for (path, hex) in shard_files(&self.derivations, &mut report.stray_paths)? {
            let Some(digest) = Digest::from_hex(&hex) else {
                report.stray_paths.push(path);
                continue;
            };
            match self.read_record(&DerivationKey(digest), &path) {
                Ok(Some(_)) => report.verified_derivations += 1,
                Ok(None) => {}
                Err(CasError::CorruptDerivationRecord { .. }) => {
                    report.corrupt_derivations.push(path)
                }
                Err(e) => return Err(e),
            }
        }
        Ok(report)
    }
}

fn object_lock_name(id: &ObjectId) -> String {
    format!("obj-{}-{}-{}", id.domain, id.kind, id.digest.to_hex())
}

/// List `<base>/<aa>/<hex>` files, reporting anything misplaced as stray.
fn shard_files(base: &Path, stray: &mut Vec<PathBuf>) -> Result<Vec<(PathBuf, String)>> {
    let mut out = Vec::new();
    let shards = match fs::read_dir(base) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(CasError::io("read directory", base, e)),
    };
    for shard in shards {
        let shard = shard.map_err(|e| CasError::io("read directory", base, e))?;
        let shard_path = shard.path();
        let shard_name = shard.file_name().to_string_lossy().into_owned();
        let is_dir = fs::symlink_metadata(&shard_path)
            .map(|m| m.is_dir())
            .unwrap_or(false);
        if !is_dir || shard_name.len() != 2 {
            stray.push(shard_path);
            continue;
        }
        for file in
            fs::read_dir(&shard_path).map_err(|e| CasError::io("read directory", &shard_path, e))?
        {
            let file = file.map_err(|e| CasError::io("read directory", &shard_path, e))?;
            let name = file.file_name().to_string_lossy().into_owned();
            if !name.starts_with(&shard_name) {
                stray.push(file.path());
                continue;
            }
            out.push((file.path(), name));
        }
    }
    out.sort();
    Ok(out)
}

fn ensure_plain_dir(dir: &Path) -> Result<()> {
    match fs::symlink_metadata(dir) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(CasError::UnsafeStoreLayout {
            path: dir.to_owned(),
            reason: "expected a plain directory (symlinks are not followed)".into(),
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => match fs::create_dir(dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => ensure_plain_dir(dir),
            Err(e) => Err(CasError::io("create store directory", dir, e)),
        },
        Err(e) => Err(CasError::io("inspect store directory", dir, e)),
    }
}

fn open_input_file(path: &Path) -> Result<fs::File> {
    let meta = fs::symlink_metadata(path).map_err(|e| CasError::io("inspect file", path, e))?;
    if !meta.is_file() {
        return Err(CasError::UnsupportedFileType {
            path: path.to_owned(),
            kind: file_kind(&meta),
        });
    }
    fsutil::open_regular_nofollow(path).map_err(|e| {
        // Swapped for a symlink or special file after the check above.
        if fs::symlink_metadata(path).is_ok_and(|m| !m.is_file()) {
            CasError::UnsupportedFileType {
                path: path.to_owned(),
                kind: "non-regular file",
            }
        } else {
            CasError::io("open file", path, e)
        }
    })
}

fn file_kind(meta: &fs::Metadata) -> &'static str {
    let ft = meta.file_type();
    if ft.is_symlink() {
        return "symlink";
    }
    if ft.is_dir() {
        return "directory";
    }
    if ft.is_file() {
        return "regular file";
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if ft.is_fifo() {
            return "FIFO";
        }
        if ft.is_socket() {
            return "socket";
        }
        if ft.is_block_device() || ft.is_char_device() {
            return "device";
        }
    }
    "special file"
}

fn read_retry(file: &mut fs::File, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        match file.read(buf) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            other => return other,
        }
    }
}

fn hash_file(domain: Domain, kind: ObjectKind, path: &Path) -> Result<(ObjectId, u64)> {
    let mut file =
        fsutil::open_regular_nofollow(path).map_err(|e| CasError::io("open", path, e))?;
    let mut hasher = ObjectHasher::new(domain, kind);
    let mut buf = vec![0u8; BUF];
    loop {
        let n = read_retry(&mut file, &mut buf).map_err(|e| CasError::io("read", path, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok((hasher.finish(), hasher.len()))
}
