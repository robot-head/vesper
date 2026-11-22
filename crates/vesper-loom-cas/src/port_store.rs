//! Adapters to the Vesper port of the store (vesper/src/store.ves, feature
//! the `vesper-loom-cas-port` package). Each takes the store's canonical root and durability,
//! calls the port, and rebuilds the crate's values and `CasError`s from the
//! reply the port answers (the payload and a 0, or an error's fields and
//! its code: the code is the last byte, so a large payload is never moved).

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::derivation::{DerivationInputs, DerivationKey};
use crate::error::{CasError, Result};
use crate::id::{Digest, Domain, OBJECT_ID_BYTES, ObjectId, ObjectKind};
use crate::port::binding;
use crate::port_fs::{self, Reader};
use crate::store::{DerivationOutcome, Ingested};
use crate::tree::Tree;

/// The operations of `CasError::Io`, by the port's `OP_*` code.
const OPS: [&str; 29] = [
    "create store root",
    "resolve store root",
    "create store directory",
    "inspect store directory",
    "publish layout marker",
    "inspect layout marker",
    "read layout marker",
    "write staging file",
    "create staging file",
    "read ingestion input",
    "flush staging file",
    "create shard",
    "publish object",
    "sync shard",
    "inspect occupied object",
    "read occupied object",
    "open repair lock",
    "acquire repair lock",
    "quarantine",
    "read",
    "inspect object",
    "open object",
    "read object",
    "inspect tree child",
    "publish derivation",
    "inspect derivation",
    "read derivation",
    "read directory",
    "remove staging file",
];

/// A reply being read.
struct Reply {
    bytes: Vec<u8>,
    at: usize,
}

impl Reply {
    fn u8(&mut self) -> u8 {
        let b = self.bytes[self.at];
        self.at += 1;
        b
    }

    fn u64(&mut self) -> u64 {
        let mut w = [0u8; 8];
        w.copy_from_slice(&self.bytes[self.at..self.at + 8]);
        self.at += 8;
        u64::from_le_bytes(w)
    }

    fn take(&mut self, n: usize) -> &[u8] {
        let s = &self.bytes[self.at..self.at + n];
        self.at += n;
        s
    }

    fn bytes(&mut self) -> &[u8] {
        let n = self.u64() as usize;
        self.take(n)
    }

    fn path(&mut self) -> PathBuf {
        port_fs::path_buf(self.bytes())
    }

    fn id(&mut self) -> ObjectId {
        ObjectId::decode(self.take(OBJECT_ID_BYTES)).expect("the port answers valid ids")
    }

    fn digest(&mut self) -> Digest {
        let mut d = [0u8; 32];
        d.copy_from_slice(self.take(32));
        Digest(d)
    }

    fn rest(self) -> Vec<u8> {
        debug_assert_eq!(self.at, 0, "a payload read whole");
        self.bytes
    }
}

fn kind_of(tag: u8) -> ObjectKind {
    ObjectKind::from_tag(tag).expect("the port answers valid kinds")
}

fn domain_of(tag: u8) -> Domain {
    Domain::from_tag(tag).expect("the port answers valid domains")
}

/// The reply's payload, or its error.
fn reply(mut bytes: Vec<u8>) -> Result<Reply> {
    let code = bytes.pop().expect("a reply ends with its code");
    let mut r = Reply { bytes, at: 0 };
    if code == 0 {
        return Ok(r);
    }
    Err(match code {
        1 => {
            let op = OPS[r.u8() as usize];
            let path = r.path();
            let word = r.u64();
            let source = if word == 0 {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "stream did not contain valid UTF-8",
                )
            } else {
                port_fs::take_error(word)
            };
            CasError::io(op, path, source)
        }
        2 => {
            let id = r.id();
            let actual = r.id();
            CasError::Corrupt {
                id,
                actual,
                path: r.path(),
            }
        }
        3 => CasError::DigestMismatch {
            expected: r.id(),
            actual: r.id(),
        },
        4 => CasError::SizeMismatch {
            id: r.id(),
            expected: r.u64(),
            actual: r.u64(),
        },
        5 => CasError::NotFound(r.id()),
        6 => CasError::WrongKind {
            id: r.id(),
            expected: kind_of(r.u8()),
        },
        7 => CasError::DomainMismatch {
            expected: domain_of(r.u8()),
            found: domain_of(r.u8()),
        },
        12 => CasError::DerivationConflict {
            key: DerivationKey(r.digest()),
            existing: r.id(),
            proposed: r.id(),
        },
        13 => CasError::CorruptDerivationRecord {
            key: DerivationKey(r.digest()),
            path: r.path(),
        },
        16 => {
            let path = r.path();
            let reason = match r.u8() {
                1 => "expected a plain directory (symlinks are not followed)".into(),
                2 => "layout marker is not a regular file".into(),
                3 => {
                    let text = String::from_utf8_lossy(r.bytes()).into_owned();
                    format!("unknown layout marker {text:?}")
                }
                _ => "object is not a regular file".into(),
            };
            CasError::UnsafeStoreLayout { path, reason }
        }
        18 => CasError::PublishContention(r.id()),
        other => unreachable!("unknown store reply code {other}"),
    })
}

/// Call the port with a fresh error list.
fn call(f: impl FnOnce() -> Vec<u8>) -> Result<Reply> {
    port_fs::clear_errors();
    reply(f())
}

fn root_bytes(root: &Path) -> &[u8] {
    root.as_os_str().as_encoded_bytes()
}

fn id_bytes(id: &ObjectId) -> Vec<u8> {
    let mut b = Vec::with_capacity(OBJECT_ID_BYTES);
    id.encode_into(&mut b);
    b
}

#[allow(unsafe_code)]
fn lend<'a>(r: &'a Reader<'_>) -> binding::Reader<'a> {
    // SAFETY: `cfs_reader_read` reads the handle word as `*const Reader`,
    // the type lent here.
    unsafe { binding::Reader::lend(r) }
}

fn ingested(mut r: Reply) -> Ingested {
    Ingested {
        id: r.id(),
        size: r.u64(),
        newly_stored: r.u8() == 1,
    }
}

/// `Store::open_with`'s checks and layout: the canonical root.
pub(crate) fn open(root: &Path, durable: bool) -> Result<PathBuf> {
    let r = call(|| binding::store_open(root_bytes(root), durable))?;
    Ok(port_fs::path_buf(&r.rest()))
}

/// `Store::contains`.
pub(crate) fn contains(root: &Path, id: &ObjectId) -> Result<bool> {
    let mut r = call(|| binding::store_contains(root_bytes(root), &id_bytes(id)))?;
    Ok(r.u8() == 1)
}

/// `Store::put_blob`.
pub(crate) fn put_blob(
    root: &Path,
    durable: bool,
    domain: Domain,
    bytes: &[u8],
) -> Result<Ingested> {
    let mut input = bytes;
    let reader = Reader::new(&mut input);
    let r = call(|| {
        binding::store_put_blob(
            root_bytes(root),
            durable,
            domain.tag(),
            bytes,
            lend(&reader),
        )
    });
    reader.resume_panic();
    let r = r?;
    Ok(ingested(r))
}

/// `Store::put_blob_reader` and (with `expected`) `put_blob_expected`.
pub(crate) fn put_stream(
    root: &Path,
    durable: bool,
    domain: Domain,
    mut input: impl Read,
    expected: Option<ObjectId>,
) -> Result<Ingested> {
    let reader = Reader::new(&mut input);
    let expected_bytes = expected.as_ref().map(id_bytes).unwrap_or_default();
    let r = call(|| {
        binding::store_put_stream(
            root_bytes(root),
            durable,
            domain.tag(),
            lend(&reader),
            expected.is_some(),
            &expected_bytes,
        )
    });
    reader.resume_panic();
    let r = r?;
    Ok(ingested(r))
}

/// `Store::put_tree`.
pub(crate) fn put_tree(root: &Path, durable: bool, tree: &Tree) -> Result<Ingested> {
    let enc = tree.encode();
    let mut input = &enc[..];
    let reader = Reader::new(&mut input);
    let r = call(|| {
        binding::store_put_tree(
            root_bytes(root),
            durable,
            tree.domain().tag(),
            &enc,
            lend(&reader),
        )
    });
    reader.resume_panic();
    let r = r?;
    Ok(ingested(r))
}

/// `Store::verify_object`.
pub(crate) fn verify(root: &Path, id: &ObjectId) -> Result<u64> {
    let mut r = call(|| binding::store_verify(root_bytes(root), &id_bytes(id)))?;
    Ok(r.u64())
}

/// `Store::read_blob` (`kind` blob) and `read_tree`'s verified read.
pub(crate) fn read(root: &Path, id: &ObjectId, kind: ObjectKind) -> Result<Vec<u8>> {
    let r = call(|| binding::store_read(root_bytes(root), &id_bytes(id), kind.tag()))?;
    Ok(r.rest())
}

/// `Store::publish_derivation`.
pub(crate) fn publish_derivation(
    root: &Path,
    durable: bool,
    inputs: &DerivationInputs,
    output: ObjectId,
) -> Result<DerivationOutcome> {
    let enc = inputs.encode()?;
    let mut r = call(|| {
        binding::store_publish_derivation(root_bytes(root), durable, &enc, &id_bytes(&output))
    })?;
    let key = DerivationKey(r.digest());
    Ok(DerivationOutcome {
        key,
        output,
        newly_recorded: r.u8() == 1,
    })
}

/// `Store::lookup_derivation`.
pub(crate) fn lookup_derivation(
    root: &Path,
    inputs: &DerivationInputs,
) -> Result<Option<ObjectId>> {
    let enc = inputs.encode()?;
    let mut r = call(|| binding::store_lookup_derivation(root_bytes(root), &enc))?;
    Ok(if r.u8() == 1 { Some(r.id()) } else { None })
}

/// `Store::sweep_temporaries`.
pub(crate) fn sweep(root: &Path, min_age: Duration) -> Result<usize> {
    let mut r = call(|| {
        binding::store_sweep(
            root_bytes(root),
            min_age.as_secs(),
            u64::from(min_age.subsec_nanos()),
        )
    })?;
    Ok(r.u64() as usize)
}
