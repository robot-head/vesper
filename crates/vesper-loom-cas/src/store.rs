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

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::derivation::{DerivationInputs, DerivationKey};
use crate::error::{CasError, NameError, Result};
use crate::id::{Domain, ObjectId, ObjectKind};
use crate::tree::Tree;

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
    pub(crate) root: PathBuf,
    pub(crate) objects: PathBuf,
    #[cfg_attr(vesper_port, allow(dead_code))]
    pub(crate) derivations: PathBuf,
    #[cfg_attr(vesper_port, allow(dead_code))]
    pub(crate) tmp: PathBuf,
    #[cfg_attr(vesper_port, allow(dead_code))]
    pub(crate) quarantine: PathBuf,
    #[cfg_attr(vesper_port, allow(dead_code))]
    pub(crate) locks: PathBuf,
    /// Whether writes are flushed to stable storage before they are
    /// published. Only [`Store::open_unsynced`] clears it.
    pub(crate) durable: bool,
}

impl Store {
    /// Open (creating if necessary) a store at `root`.
    pub fn open(root: impl AsRef<Path>) -> Result<Store> {
        Self::open_with(root.as_ref(), true)
    }

    /// Open a store whose writes are never flushed to stable storage: no
    /// `fsync` of staged files, no directory syncs. Publication is still
    /// atomic (staging plus no-replace rename) and every read still verifies
    /// digests, so the store is exactly as safe as [`Store::open`] against
    /// process crashes and concurrent writers, but not against power loss or
    /// an operating-system crash.
    ///
    /// This is for deterministic crash simulation, where faults are injected
    /// inside the process and the page cache always survives. Flushing every
    /// object there only measures the disk: a Flux kill-at-every-step sweep
    /// issues tens of thousands of flushes, which take most of an hour on a
    /// Windows CI runner. Never use it for a store that must survive a
    /// machine crash.
    pub fn open_unsynced(root: impl AsRef<Path>) -> Result<Store> {
        Self::open_with(root.as_ref(), false)
    }

    /// Whether this handle flushes writes to stable storage.
    pub fn is_durable(&self) -> bool {
        self.durable
    }

    fn open_with(root: &Path, durable: bool) -> Result<Store> {
        #[cfg(vesper_port)]
        return Self::open_with_port(root, durable);
        #[cfg(not(vesper_port))]
        Self::open_with_rust(root, durable)
    }

    #[cfg(vesper_port)]
    fn open_with_port(root: &Path, durable: bool) -> Result<Store> {
        let root = crate::port_store::open(root, durable)?;
        Ok(Store {
            objects: root.join("objects").join("v1"),
            derivations: root.join("derivations").join("v1"),
            tmp: root.join("tmp"),
            quarantine: root.join("quarantine"),
            locks: root.join("locks"),
            root,
            durable,
        })
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

    /// Whether an object file is present (contents are not rehashed).
    pub fn contains(&self, id: &ObjectId) -> Result<bool> {
        #[cfg(vesper_port)]
        return crate::port_store::contains(&self.root, id);
        #[cfg(not(vesper_port))]
        self.contains_rust(id)
    }

    // ----------------------------------------------------------------------
    // Ingestion
    // ----------------------------------------------------------------------

    /// Store `bytes` as a blob.
    pub fn put_blob(&self, domain: Domain, bytes: &[u8]) -> Result<Ingested> {
        #[cfg(vesper_port)]
        return crate::port_store::put_blob(&self.root, self.durable, domain, bytes);
        #[cfg(not(vesper_port))]
        self.put_blob_rust(domain, bytes)
    }

    /// Stream a blob into the store. If the reader fails, nothing is
    /// published and the staging file is removed.
    pub fn put_blob_reader(&self, domain: Domain, reader: impl Read) -> Result<Ingested> {
        #[cfg(vesper_port)]
        return crate::port_store::put_stream(&self.root, self.durable, domain, reader, None);
        #[cfg(not(vesper_port))]
        self.put_blob_reader_rust(domain, reader)
    }

    /// Stream a blob and require it to hash to `expected` (e.g. a digest from
    /// a lockfile). On mismatch nothing is published.
    pub fn put_blob_expected(&self, expected: ObjectId, reader: impl Read) -> Result<Ingested> {
        #[cfg(vesper_port)]
        return crate::port_store::put_stream(
            &self.root,
            self.durable,
            expected.domain,
            reader,
            Some(expected),
        );
        #[cfg(not(vesper_port))]
        self.put_blob_expected_rust(expected, reader)
    }

    /// Ingest a regular file. Symlinks and special files are rejected.
    pub fn put_file(&self, domain: Domain, path: impl AsRef<Path>) -> Result<Ingested> {
        let path = path.as_ref();
        #[cfg(vesper_port)]
        return crate::port_store::put_file(&self.root, self.durable, domain, path);
        #[cfg(not(vesper_port))]
        self.put_file_rust(domain, path)
    }

    /// Store a tree after checking that every child is present with the
    /// recorded size and kind.
    pub fn put_tree(&self, tree: &Tree) -> Result<Ingested> {
        #[cfg(vesper_port)]
        return crate::port_store::put_tree(&self.root, self.durable, tree);
        #[cfg(not(vesper_port))]
        self.put_tree_rust(tree)
    }

    /// Recursively ingest a directory as a tree of blobs in `domain`.
    ///
    /// Only regular files and directories are accepted. Symlinks (to files,
    /// directories or nowhere), FIFOs, sockets and devices are rejected, as
    /// are names that fail the portable naming policy. Hard-linked inputs are
    /// read like any other file; the store never hard-links them.
    pub fn ingest_dir(&self, domain: Domain, path: impl AsRef<Path>) -> Result<Ingested> {
        let path = path.as_ref();
        #[cfg(vesper_port)]
        return crate::port_store::ingest_dir(&self.root, self.durable, domain, path);
        #[cfg(not(vesper_port))]
        self.ingest_dir_rust(domain, path)
    }

    // ----------------------------------------------------------------------
    // Readback
    // ----------------------------------------------------------------------

    /// Rehash an object and return its size.
    pub fn verify_object(&self, id: &ObjectId) -> Result<u64> {
        #[cfg(vesper_port)]
        return crate::port_store::verify(&self.root, id);
        #[cfg(not(vesper_port))]
        self.verify_object_rust(id)
    }

    /// Read a blob and verify it hashes to `id`.
    pub fn read_blob(&self, id: &ObjectId) -> Result<Vec<u8>> {
        #[cfg(vesper_port)]
        return crate::port_store::read(&self.root, id, ObjectKind::Blob);
        #[cfg(not(vesper_port))]
        self.read_blob_rust(id)
    }

    /// Read, verify and strictly decode a tree.
    pub fn read_tree(&self, id: &ObjectId) -> Result<Tree> {
        #[cfg(vesper_port)]
        return Ok(Tree::decode(
            id.domain,
            &crate::port_store::read(&self.root, id, ObjectKind::Tree)?,
        )?);
        #[cfg(not(vesper_port))]
        self.read_tree_rust(id)
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
        #[cfg(vesper_port)]
        return crate::port_store::publish_derivation(&self.root, self.durable, inputs, output);
        #[cfg(not(vesper_port))]
        self.publish_derivation_rust(inputs, output)
    }

    /// Look up the recorded output for `inputs`. Returns `Ok(None)` on a
    /// miss. A record whose output object has been removed is an error, not a
    /// silent miss. The output's contents are verified when it is read.
    pub fn lookup_derivation(&self, inputs: &DerivationInputs) -> Result<Option<ObjectId>> {
        #[cfg(vesper_port)]
        return crate::port_store::lookup_derivation(&self.root, inputs);
        #[cfg(not(vesper_port))]
        self.lookup_derivation_rust(inputs)
    }

    // ----------------------------------------------------------------------
    // Materialization
    // ----------------------------------------------------------------------

    /// Materialize a verified tree into `dest` (`Artifact` or `Source`).
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
        #[cfg(vesper_port)]
        {
            let parent = match dest.parent() {
                Some(p) if !p.as_os_str().is_empty() => p.to_owned(),
                _ => PathBuf::from("."),
            };
            crate::port_store::materialize(&self.root, self.durable, tree, dest, &parent, options)
        }
        #[cfg(not(vesper_port))]
        self.materialize_rust(tree, dest, options)
    }

    // ----------------------------------------------------------------------
    // Maintenance
    // ----------------------------------------------------------------------

    /// Remove staging files older than `min_age`, such as those left by a
    /// crashed writer. Staging files are never addressable, so removing them
    /// cannot affect readers. Use an age comfortably longer than the slowest
    /// expected ingestion, since a live writer's file is also in `tmp/`.
    pub fn sweep_temporaries(&self, min_age: Duration) -> Result<usize> {
        #[cfg(vesper_port)]
        return crate::port_store::sweep(&self.root, min_age);
        #[cfg(not(vesper_port))]
        self.sweep_temporaries_rust(min_age)
    }

    /// Rehash every object and revalidate every derivation record.
    pub fn verify_all(&self) -> Result<StoreReport> {
        #[cfg(vesper_port)]
        return crate::port_store::verify_all(&self.root);
        #[cfg(not(vesper_port))]
        self.verify_all_rust()
    }
}
