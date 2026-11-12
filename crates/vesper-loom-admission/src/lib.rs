//! Fresh, bounded configured-key admission of actual artifact blob bytes.
//!
//! Every operation reparses the complete caller-expected identity, current key
//! policy and attestation bundle, verifies strict Ed25519/operator quorum under
//! the explicit clock, and hashes the actual bytes before mutating CAS/output.
//! Reports are diagnostics, never reusable authorization. This does not
//! authenticate how the caller obtained the expected source/derivation, policy,
//! operator grouping or clock, and supplies no OIDC/namespace/build replay proof.
#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};
use vesper_loom_cas::{CasError, Domain, ObjectId, ObjectKind, Store, blob_id};
use vesper_loom_trust::{
    VerificationReport, parse_bundle, parse_identity, parse_policy, verify_attestations,
};

pub const SCHEMA_VERSION: u32 = 1;
/// Owned blobs are deliberately bounded; streaming/tree admission is separate.
pub const MAX_BLOB_BYTES: usize = 64 * 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_blob_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_blob_bytes: MAX_BLOB_BYTES,
        }
    }
}
/// All three JSON inputs are independently limited to trust's 4 MiB hard bound.
/// These are borrowed inputs to EACH operation, not cached approval handles.
#[derive(Clone, Copy, Debug)]
pub struct Context<'a> {
    pub expected_json: &'a [u8],
    pub policy_json: &'a [u8],
    pub bundle_json: &'a [u8],
    pub evaluated_at: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextField {
    Expected,
    Policy,
    Bundle,
}
#[derive(Debug)]
pub enum AdmissionError {
    InvalidLimits,
    Parse {
        field: ContextField,
        message: String,
    },
    QuorumRejected(Box<VerificationReport>),
    UnsupportedOutput,
    BlobLimit,
    BlobMismatch {
        expected: ObjectId,
        actual: ObjectId,
    },
    Cas(Box<CasError>),
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    UnsafeStoredObject,
    UnsupportedPlatform,
    AllocationFailed,
    Encoding(String),
}
impl std::fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::QuorumRejected(report) => write!(
                f,
                "configured builder quorum rejected: {:?}",
                report.diagnostics
            ),
            Self::Parse { field, message } => write!(f, "invalid {field:?} JSON: {message}"),
            Self::Io {
                operation,
                path,
                source,
            } => write!(f, "{operation} {}: {source}", path.display()),
            Self::Cas(error) => write!(f, "CAS admission: {error}"),
            other => write!(f, "artifact admission error: {other:?}"),
        }
    }
}
impl std::error::Error for AdmissionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Cas(error) => Some(error.as_ref()),
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
/// All import failures, including post-validation storage errors, return the
/// exact original artifact allocation. No diagnostic report authorizes a retry.
#[derive(Debug)]
pub struct AdmissionFailure {
    pub error: AdmissionError,
    pub artifact: Vec<u8>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Import,
    Retrieve,
    Materialize,
}
/// Content identities describe the full evaluated context, not an authenticated
/// policy source or a permanent trust bit. Policy/bundle order is canonicalized.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextReport {
    pub context_blake3: String,
    pub expected_blake3: String,
    pub policy_blake3: String,
    pub bundle_blake3: String,
    pub evaluated_at: u64,
    pub max_blob_bytes: usize,
}
/// Public and serializable for diagnostics only. No API accepts a report in
/// place of fresh Context. Digest identity includes operation/result metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdmissionReport {
    pub schema_version: u32,
    pub report_blake3: String,
    pub context: ContextReport,
    pub operation: Operation,
    pub output: String,
    pub size: u64,
    pub newly_stored: Option<bool>,
    pub verification: VerificationReport,
}
#[derive(Debug)]
pub struct RetrievedBlob {
    pub artifact: Vec<u8>,
    pub report: AdmissionReport,
}
#[derive(Debug)]
pub struct MaterializedBlob {
    pub path: PathBuf,
    pub report: AdmissionReport,
}
struct Validated {
    output: ObjectId,
    context: ContextReport,
    verification: VerificationReport,
}
impl Validated {
    fn report(
        self,
        operation: Operation,
        size: usize,
        newly_stored: Option<bool>,
    ) -> Result<AdmissionReport, AdmissionError> {
        let mut report = AdmissionReport {
            schema_version: SCHEMA_VERSION,
            report_blake3: String::new(),
            context: self.context,
            operation,
            output: self.output.to_string(),
            size: size as u64,
            newly_stored,
            verification: self.verification,
        };
        // The empty digest field is part of the versioned report preimage.
        report.report_blake3 = digest(b"vesper.loom.admission.report.v1", &report)?;
        Ok(report)
    }
    fn check_bytes(&self, artifact: &[u8], limits: Limits) -> Result<(), AdmissionError> {
        if artifact.len() > limits.max_blob_bytes {
            return Err(AdmissionError::BlobLimit);
        }
        let actual = blob_id(Domain::Artifact, artifact);
        if actual != self.output {
            return Err(AdmissionError::BlobMismatch {
                expected: self.output,
                actual,
            });
        }
        Ok(())
    }
}
fn digest<T: Serialize>(domain: &[u8], value: &T) -> Result<String, AdmissionError> {
    let bytes =
        serde_json::to_vec(value).map_err(|error| AdmissionError::Encoding(error.to_string()))?;
    let mut hash = blake3::Hasher::new();
    hash.update(&(domain.len() as u64).to_le_bytes());
    hash.update(domain);
    hash.update(&(bytes.len() as u64).to_le_bytes());
    hash.update(&bytes);
    Ok(hash.finalize().to_hex().to_string())
}
fn validate(context: Context<'_>, limits: Limits) -> Result<Validated, AdmissionError> {
    if limits.max_blob_bytes > MAX_BLOB_BYTES {
        return Err(AdmissionError::InvalidLimits);
    }
    let expected =
        parse_identity(context.expected_json).map_err(|message| AdmissionError::Parse {
            field: ContextField::Expected,
            message,
        })?;
    let mut policy =
        parse_policy(context.policy_json).map_err(|message| AdmissionError::Parse {
            field: ContextField::Policy,
            message,
        })?;
    let mut bundle =
        parse_bundle(context.bundle_json).map_err(|message| AdmissionError::Parse {
            field: ContextField::Bundle,
            message,
        })?;
    let verification = verify_attestations(&expected, &bundle, &policy, context.evaluated_at);
    if !verification.accepted {
        return Err(AdmissionError::QuorumRejected(Box::new(verification)));
    }
    let output: ObjectId = expected
        .output
        .parse()
        .map_err(|_| AdmissionError::UnsupportedOutput)?;
    if output.domain != Domain::Artifact || output.kind != ObjectKind::Blob {
        return Err(AdmissionError::UnsupportedOutput);
    }
    // All fields (including unused/revoked policy keys) affect policy identity.
    // Sorting is safe only after strict verification rejected duplicate key IDs.
    policy
        .builders
        .sort_by(|left, right| left.key_id.cmp(&right.key_id));
    bundle
        .attestations
        .sort_by(|left, right| left.statement.key_id.cmp(&right.statement.key_id));
    let mut report = ContextReport {
        context_blake3: String::new(),
        expected_blake3: digest(b"vesper.loom.admission.expected.v1", &expected)?,
        policy_blake3: digest(b"vesper.loom.admission.policy.v1", &policy)?,
        bundle_blake3: digest(b"vesper.loom.admission.bundle.v1", &bundle)?,
        evaluated_at: context.evaluated_at,
        max_blob_bytes: limits.max_blob_bytes,
    };
    report.context_blake3 = digest(b"vesper.loom.admission.context.v1", &report)?;
    Ok(Validated {
        output,
        context: report,
        verification,
    })
}

/// Import exact owned bytes only after full current quorum AND blob validation.
/// Invalid inputs do not even open/create the store directory. No derivation
/// index, publication record or persistent trust state is written. On storage
/// I/O failure after approval, valid immutable objects/staging may have changed;
/// the original artifact owner is still returned for inspection/retry.
pub fn admit_blob(
    store_path: impl AsRef<Path>,
    artifact: Vec<u8>,
    context: Context<'_>,
    limits: Limits,
) -> Result<AdmissionReport, Box<AdmissionFailure>> {
    let result = (|| {
        if artifact.len() > limits.max_blob_bytes {
            return Err(AdmissionError::BlobLimit);
        }
        let validated = validate(context, limits)?;
        validated.check_bytes(&artifact, limits)?;
        // No filesystem operations occur above this line.
        let store =
            Store::open(store_path).map_err(|error| AdmissionError::Cas(Box::new(error)))?;
        let ingested = store
            .put_blob_expected(validated.output, artifact.as_slice())
            .map_err(|error| AdmissionError::Cas(Box::new(error)))?;
        validated.report(
            Operation::Import,
            artifact.len(),
            Some(ingested.newly_stored),
        )
    })();
    result.map_err(|error| Box::new(AdmissionFailure { error, artifact }))
}

/// Freshly verify the current context, then read a bounded regular object into
/// owned memory and rehash THAT returned memory. Store paths/reports are never
/// used as authorization. Corrupt bytes are rejected without quarantine writes.
pub fn retrieve_blob(
    store: &Store,
    context: Context<'_>,
    limits: Limits,
) -> Result<RetrievedBlob, AdmissionError> {
    let validated = validate(context, limits)?;
    let artifact = read_stored(store, validated.output, limits)?;
    validated.check_bytes(&artifact, limits)?;
    let report = validated.report(Operation::Retrieve, artifact.len(), None)?;
    Ok(RetrievedBlob { artifact, report })
}

/// Write a verified independent blob copy to a NEW destination. No overwrite,
/// executable marking, parent creation or package installation. Fresh complete
/// validation/read/hash precedes even staging-file creation. Publication uses a
/// same-directory no-clobber operation; temporary files clean up on ordinary
/// failure. This is not a process-crash or power-loss durability guarantee.
pub fn materialize_blob(
    store: &Store,
    destination: impl AsRef<Path>,
    context: Context<'_>,
    limits: Limits,
) -> Result<MaterializedBlob, AdmissionError> {
    let validated = validate(context, limits)?;
    let artifact = read_stored(store, validated.output, limits)?;
    validated.check_bytes(&artifact, limits)?;
    let report = validated.report(Operation::Materialize, artifact.len(), None)?;
    let destination = destination.as_ref();
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut staged = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| io_error("create materialization staging", parent, error))?;
    staged
        .write_all(&artifact)
        .map_err(|error| io_error("write materialization staging", staged.path(), error))?;
    staged
        .as_file()
        .sync_all()
        .map_err(|error| io_error("sync materialization staging", staged.path(), error))?;
    staged
        .persist_noclobber(destination)
        .map_err(|error| io_error("publish materialization", destination, error.error))?;
    Ok(MaterializedBlob {
        path: destination.to_owned(),
        report,
    })
}
fn io_error(operation: &'static str, path: &Path, source: io::Error) -> AdmissionError {
    AdmissionError::Io {
        operation,
        path: path.to_owned(),
        source,
    }
}
fn read_stored(store: &Store, output: ObjectId, limits: Limits) -> Result<Vec<u8>, AdmissionError> {
    let path = store.object_path(&output);
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| io_error("inspect stored blob", &path, error))?;
    if !metadata.is_file() {
        return Err(AdmissionError::UnsafeStoredObject);
    }
    let file = open_regular(&path)?;
    let metadata = file
        .metadata()
        .map_err(|error| io_error("inspect open blob", &path, error))?;
    if !metadata.is_file() {
        return Err(AdmissionError::UnsafeStoredObject);
    }
    if metadata.len() > limits.max_blob_bytes as u64 {
        return Err(AdmissionError::BlobLimit);
    }
    // Read at most limit+1, even if another process grows/replaces the file.
    // The open descriptor pins the object being read, and only the final owned
    // vector's hash can succeed. No metadata-check/read-to-end allocation race.
    let mut input = file.take(limits.max_blob_bytes as u64 + 1);
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(metadata.len() as usize)
        .map_err(|_| AdmissionError::AllocationFailed)?;
    let mut buffer = [0u8; 8192];
    loop {
        let count = match input.read(&mut buffer) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result.map_err(|error| io_error("read stored blob", &path, error))?,
        };
        if count == 0 {
            return Ok(bytes);
        }
        if count > limits.max_blob_bytes - bytes.len() {
            return Err(AdmissionError::BlobLimit);
        }
        bytes
            .try_reserve(count)
            .map_err(|_| AdmissionError::AllocationFailed)?;
        bytes.extend_from_slice(&buffer[..count]);
    }
}
fn open_regular(path: &Path) -> Result<File, AdmissionError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Open the reparse point itself, rather than following a raced symlink.
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(not(any(unix, windows)))]
    return Err(AdmissionError::UnsupportedPlatform);
    #[cfg(any(unix, windows))]
    options
        .open(path)
        .map_err(|error| io_error("open stored blob without following symlink", path, error))
}
