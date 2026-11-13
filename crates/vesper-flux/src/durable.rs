//! Durable, incremental executor checkpoints in the `vesper-loom-cas` store.
//!
//! A checkpoint is one *manifest* blob plus one *arrangement* blob per original
//! circuit node (that node's complete canonical integral). Both live in the
//! immutable BLAKE3 content-addressed store under `Domain::Artifact`. The
//! manifest names every arrangement by content identity, so an unchanged
//! arrangement is referenced again instead of rewritten: each checkpoint writes
//! only the arrangements whose integrals changed since the previous checkpoint
//! made by the same [`DurableCheckpointer`] over the same executor history.
//!
//! Integrity on restore is layered: the store rehashes every object it reads
//! (tampered bytes are `Error::Storage("corrupt object")`); every blob carries
//! its own magic, version and the circuit execution fingerprint; arrangements
//! carry their node index, width and row count, which must match the manifest;
//! rows must be canonical; and the restored integrals are revalidated as a
//! relationally consistent state of the circuit before anything is published.
//! Hash identity proves content identity only, never provenance: a producer
//! able to write the store can still write a different consistent state.
//!
//! The store writes objects with its own durability protocol (temporary file,
//! fsync, rename). Restore never trusts in-memory change counters: every
//! restore assigns the executor a fresh lineage.
use super::super::{Circuit, Executor};
use super::{
    CheckpointLimits, Reader, Writer, count_rows, identity, read_source, validate_state,
    write_source,
};
use crate::{Error, ZSet};
use std::collections::BTreeMap;
use vesper_loom_cas::{CasError, Digest, Domain, ObjectId, ObjectKind, Store};

const MANIFEST_MAGIC: &[u8; 8] = b"VFLUXDM1";
const ARRANGEMENT_MAGIC: &[u8; 8] = b"VFLUXAR1";
const DURABLE_VERSION: u16 = 1;

/// Outcome of one durable checkpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DurableReport {
    /// Content identity of the manifest; the handle used to restore.
    pub manifest: ObjectId,
    pub ticks: u64,
    /// Arrangements serialized and offered to the store by this checkpoint.
    pub written: usize,
    /// Arrangements referenced unchanged from the previous checkpoint.
    pub reused: usize,
    /// Bytes of the manifest and written arrangements (including deduplicated ones).
    pub bytes: u64,
}

/// Outcome of one durable restore.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DurableRestore {
    pub manifest: ObjectId,
    pub ticks: u64,
    /// The manifest this one was incrementally derived from, if any.
    pub parent: Option<ObjectId>,
    pub arrangements: Vec<ObjectId>,
}

#[derive(Debug)]
struct Committed {
    lineage: u64,
    fingerprint: [u8; 32],
    versions: Vec<u64>,
    arrangements: Vec<ObjectId>,
    manifest: ObjectId,
}

/// Writes incremental durable checkpoints of one executor history. Bound to the
/// executor lineage it last checkpointed or restored; a different executor, or
/// the same one after any restore, gets a full (store-deduplicated) write.
#[derive(Debug, Default)]
pub struct DurableCheckpointer {
    committed: Option<Committed>,
}

pub(crate) fn storage(error: CasError) -> Error {
    Error::Storage(match error {
        CasError::Corrupt { .. } | CasError::SizeMismatch { .. } => "corrupt object",
        CasError::NotFound(_) => "missing object",
        CasError::WrongKind { .. } | CasError::DomainMismatch { .. } => "wrong object kind",
        CasError::DigestMismatch { .. } => "digest mismatch",
        CasError::Io { .. } => "io",
        _ => "store",
    })
}

pub(crate) fn blob_id(digest: [u8; 32]) -> ObjectId {
    ObjectId::new(Domain::Artifact, ObjectKind::Blob, Digest(digest))
}

/// Read one artifact blob with the store's hash verification and a size cap.
pub(crate) fn read_blob(store: &Store, id: &ObjectId, max_bytes: usize) -> Result<Vec<u8>, Error> {
    if id.domain != Domain::Artifact || id.kind != ObjectKind::Blob {
        return Err(Error::Storage("wrong object kind"));
    }
    let path = store.object_path(id);
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.len() > u64::try_from(max_bytes).map_err(|_| Error::Overflow)? => {
            return Err(Error::Limit("checkpoint bytes"));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::Storage("missing object"));
        }
        Err(_) => return Err(Error::Storage("io")),
    }
    let bytes = store.read_blob(id).map_err(storage)?;
    if bytes.len() > max_bytes {
        return Err(Error::Limit("checkpoint bytes"));
    }
    Ok(bytes)
}

fn encode_arrangement(
    fingerprint: &[u8; 32],
    node: usize,
    set: &ZSet,
    limits: CheckpointLimits,
) -> Result<Vec<u8>, Error> {
    let mut w = Writer {
        bytes: Vec::new(),
        limit: limits.max_bytes,
    };
    w.put(ARRANGEMENT_MAGIC)?;
    w.u16(DURABLE_VERSION)?;
    w.u16(0)?;
    w.put(fingerprint)?;
    w.u32(u32::try_from(node).map_err(|_| Error::Overflow)?)?;
    w.u32(u32::try_from(set.width).map_err(|_| Error::Overflow)?)?;
    w.usize(set.len())?;
    for (row, weight) in set.entries() {
        for value in row {
            w.i64(*value)?;
        }
        w.i64(weight)?;
    }
    Ok(w.bytes)
}

impl DurableCheckpointer {
    pub fn new() -> Self {
        Self::default()
    }
    /// Manifest of the last checkpoint written or restored through this value.
    pub fn last_manifest(&self) -> Option<ObjectId> {
        self.committed.as_ref().map(|c| c.manifest)
    }

    /// Durably checkpoint every integral, tick count and source offset. Only
    /// arrangements changed since this checkpointer's previous checkpoint of
    /// the same executor history are serialized. The state is revalidated
    /// first (as [`Executor::checkpoint`] does). On error the checkpointer is
    /// unchanged; objects already stored stay as unreferenced immutable blobs.
    pub fn checkpoint(
        &mut self,
        exec: &Executor,
        store: &Store,
        limits: CheckpointLimits,
    ) -> Result<DurableReport, Error> {
        limits.validate()?;
        let c = &exec.circuit;
        let mut budget = limits.budget(c);
        let fingerprint = identity(c, exec.source.as_ref(), limits, &mut budget)?;
        validate_state(
            c,
            &exec.prior,
            exec.ticks,
            exec.source.as_ref(),
            limits,
            &mut budget,
        )?;
        let reuse = self.committed.as_ref().filter(|p| {
            p.lineage != 0
                && p.lineage == exec.lineage
                && p.fingerprint == fingerprint
                && p.versions.len() == exec.versions.len()
        });
        let mut arrangements = Vec::with_capacity(exec.prior.len());
        let (mut written, mut reused, mut bytes) = (0usize, 0usize, 0u64);
        for (node, set) in exec.prior.iter().enumerate() {
            if let Some(p) = reuse
                && p.versions[node] == exec.versions[node]
            {
                arrangements.push(p.arrangements[node]);
                reused += 1;
                continue;
            }
            let blob = encode_arrangement(&fingerprint, node, set, limits)?;
            budget.charge(blob.len() / 8 + 1)?;
            let ingested = store.put_blob(Domain::Artifact, &blob).map_err(storage)?;
            bytes = bytes.checked_add(ingested.size).ok_or(Error::Overflow)?;
            arrangements.push(ingested.id);
            written += 1;
        }
        let mut w = Writer {
            bytes: Vec::new(),
            limit: limits.max_bytes,
        };
        w.put(MANIFEST_MAGIC)?;
        w.u16(DURABLE_VERSION)?;
        w.u16(0)?;
        w.put(&fingerprint)?;
        w.u64(exec.ticks)?;
        write_source(&mut w, exec.source.as_ref())?;
        w.u32(u32::try_from(exec.prior.len()).map_err(|_| Error::Overflow)?)?;
        for (set, id) in exec.prior.iter().zip(&arrangements) {
            w.put(id.digest.as_bytes())?;
            w.u32(u32::try_from(set.width).map_err(|_| Error::Overflow)?)?;
            w.usize(set.len())?;
        }
        match &self.committed {
            Some(p) => {
                w.u8(1)?;
                w.put(p.manifest.digest.as_bytes())?;
            }
            None => w.u8(0)?,
        }
        let ingested = store
            .put_blob(Domain::Artifact, &w.bytes)
            .map_err(storage)?;
        bytes = bytes.checked_add(ingested.size).ok_or(Error::Overflow)?;
        self.committed = Some(Committed {
            lineage: exec.lineage,
            fingerprint,
            versions: exec.versions.clone(),
            arrangements,
            manifest: ingested.id,
        });
        Ok(DurableReport {
            manifest: ingested.id,
            ticks: exec.ticks,
            written,
            reused,
            bytes,
        })
    }

    /// Restore `exec` from a durable manifest and continue the incremental
    /// chain from it. `exec` must run the identical circuit, limits and
    /// prebound CDC policy. Every check precedes publication: on error the
    /// executor and this checkpointer are unchanged.
    pub fn restore(
        &mut self,
        exec: &mut Executor,
        store: &Store,
        manifest: &ObjectId,
        limits: CheckpointLimits,
    ) -> Result<DurableRestore, Error> {
        limits.validate()?;
        let image = read_blob(store, manifest, limits.max_bytes)?;
        let c: &Circuit = &exec.circuit;
        let mut budget = limits.budget(c);
        let fingerprint = identity(c, exec.source.as_ref(), limits, &mut budget)?;
        let mut r = Reader {
            bytes: &image,
            cursor: 0,
            budget: &mut budget,
        };
        if r.take(8)? != MANIFEST_MAGIC || r.u16()? != DURABLE_VERSION || r.u16()? != 0 {
            return Err(Error::Checkpoint("manifest version"));
        }
        if r.take(32)? != fingerprint {
            return Err(Error::Checkpoint("identity"));
        }
        let ticks = r.u64()?;
        let source = read_source(&mut r, c, limits)?;
        if source.as_ref().map(|s| &s.policy) != exec.source.as_ref().map(|s| &s.policy) {
            return Err(Error::Checkpoint("source policy"));
        }
        let count = usize::try_from(r.u32()?).map_err(|_| Error::Overflow)?;
        if count != c.nodes.len() || count > limits.max_nodes {
            return Err(Error::Checkpoint("node count"));
        }
        let mut entries = Vec::with_capacity(count);
        let mut total_rows = 0usize;
        for n in &c.nodes {
            let digest: [u8; 32] = r
                .take(32)?
                .try_into()
                .map_err(|_| Error::Checkpoint("digest"))?;
            let width = usize::try_from(r.u32()?).map_err(|_| Error::Overflow)?;
            let rows = r.usize()?;
            if width != n.width || rows > c.limits.max_rows {
                return Err(Error::Checkpoint("row schema"));
            }
            total_rows = total_rows.checked_add(rows).ok_or(Error::Overflow)?;
            if total_rows > limits.max_total_rows {
                return Err(Error::Limit("checkpoint state"));
            }
            entries.push((blob_id(digest), width, rows));
        }
        let parent = match r.u8()? {
            0 => None,
            1 => Some(blob_id(
                r.take(32)?
                    .try_into()
                    .map_err(|_| Error::Checkpoint("digest"))?,
            )),
            _ => return Err(Error::Checkpoint("parent tag")),
        };
        if r.cursor != image.len() {
            return Err(Error::Checkpoint("trailing bytes"));
        }
        // Each arrangement is bound to its node index, so two nodes can never
        // legitimately name the same object; a manifest that does is rejected.
        let mut prior = Vec::with_capacity(count);
        let mut seen = BTreeMap::new();
        for (node, (id, width, rows)) in entries.iter().enumerate() {
            if seen.insert(*id, node).is_some() {
                return Err(Error::Checkpoint("arrangement reused across nodes"));
            }
            let blob = read_blob(store, id, limits.max_bytes)?;
            let mut r = Reader {
                bytes: &blob,
                cursor: 0,
                budget: &mut budget,
            };
            if r.take(8)? != ARRANGEMENT_MAGIC || r.u16()? != DURABLE_VERSION || r.u16()? != 0 {
                return Err(Error::Checkpoint("arrangement version"));
            }
            if r.take(32)? != fingerprint {
                return Err(Error::Checkpoint("arrangement identity"));
            }
            if usize::try_from(r.u32()?).map_err(|_| Error::Overflow)? != node
                || usize::try_from(r.u32()?).map_err(|_| Error::Overflow)? != *width
                || r.usize()? != *rows
            {
                return Err(Error::Checkpoint("arrangement header"));
            }
            let mut set = BTreeMap::new();
            for _ in 0..*rows {
                let (row, weight) = r.row(*width)?;
                if weight == 0 || set.last_key_value().is_some_and(|(last, _)| last >= &row) {
                    return Err(Error::Checkpoint("noncanonical rows"));
                }
                set.insert(row, weight);
            }
            if r.cursor != blob.len() {
                return Err(Error::Checkpoint("trailing bytes"));
            }
            prior.push(ZSet {
                width: *width,
                rows: set,
            });
        }
        count_rows(&prior, limits)?;
        validate_state(c, &prior, ticks, source.as_ref(), limits, &mut budget)?;
        let arrangements: Vec<ObjectId> = entries.iter().map(|(id, _, _)| *id).collect();
        exec.prior = prior;
        exec.ticks = ticks;
        exec.source = source;
        exec.lineage = super::super::fresh_lineage();
        self.committed = Some(Committed {
            lineage: exec.lineage,
            fingerprint,
            versions: exec.versions.clone(),
            arrangements: arrangements.clone(),
            manifest: *manifest,
        });
        Ok(DurableRestore {
            manifest: *manifest,
            ticks,
            parent,
            arrangements,
        })
    }
}
