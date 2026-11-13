//! Vesper Loom content-addressed store (design §8.2.6, §8.3).
//!
//! An immutable, BLAKE3-addressed object store with:
//!
//! * explicit [`Domain`]s (source, interface, artifact) mixed into every hash,
//! * canonical Merkle [`Tree`]s with a portable, traversal-free naming policy,
//! * versioned [`DerivationInputs`] with canonical [`DerivationKey`]s,
//! * verified ingestion (hash while staging, optional expected digest) and
//!   verified readback (every read rehashes),
//! * atomic, no-replace publication that is safe for concurrent writers in
//!   any number of threads and processes,
//! * corruption rejection and quarantine,
//! * workspace materialization into a fresh directory using reflinks where
//!   the file system supports them and a verified copy otherwise.
//!
//! A digest match establishes content identity only. Provenance, signatures,
//! transparency logs and builder-quorum checks are the job of a separate
//! verifier (`vesper-loom-trust`), and nothing in this crate reports trust.
//! The store performs no network access and runs no builds.
//!
//! ```
//! use vesper_loom_cas::{DerivationInputs, Domain, MaterializeOptions, Store};
//!
//! let dir = tempfile::tempdir()?;
//! let pkg = dir.path().join("pkg");
//! std::fs::create_dir(&pkg)?;
//! std::fs::write(pkg.join("main.vsp"), "fn main() -> i64 { 0 }\n")?;
//!
//! let store = Store::open(dir.path().join("store"))?;
//! let source = store.ingest_dir(Domain::Source, &pkg)?;
//!
//! // A (separately executed) build produced this artifact.
//! let object = store.put_blob(Domain::Artifact, b"\x7fELF...")?;
//! let inputs = DerivationInputs::new(Domain::Artifact, "vesper-codegen-clif", "0.1.0")
//!     .with_target("x86_64-unknown-linux-gnu")
//!     .with_input("source", source.id);
//! store.publish_derivation(&inputs, object.id)?;
//! assert_eq!(store.lookup_derivation(&inputs)?, Some(object.id));
//!
//! let ws = dir.path().join("workspace");
//! store.materialize(&source.id, &ws, MaterializeOptions::default())?;
//! assert_eq!(std::fs::read(ws.join("main.vsp"))?, b"fn main() -> i64 { 0 }\n");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![forbid(unsafe_op_in_unsafe_fn)]
#![warn(missing_debug_implementations)]

mod derivation;
mod error;
mod fsutil;
mod id;
mod store;
pub mod transfer;
mod tree;

pub use derivation::{DerivationInputs, DerivationKey, InputSchema};
pub use error::{CasError, DerivationError, NameError, Result, TreeError};
pub use id::{Digest, Domain, OBJECT_SCHEMA_VERSION, ObjectHasher, ObjectId, ObjectKind, blob_id};
pub use store::{
    CopyStrategy, DerivationOutcome, Ingested, MaterializeOptions, MaterializeReport, Store,
    StoreReport,
};
pub use tree::{
    EntryKind, MAX_TREE_DEPTH, MAX_TREE_ENTRIES, TREE_SCHEMA_VERSION, Tree, TreeEntry,
    validate_name,
};
