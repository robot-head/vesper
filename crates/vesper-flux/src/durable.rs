//! Durable, incremental executor checkpoints in the `vesper-loom-cas` store,
//! with every node's integral kept as an LSM *spine* of immutable batches.
//!
//! A checkpoint is one *manifest* blob plus, per original circuit node, an
//! ordered spine of *batch* blobs whose Z-set sum is that node's integral.
//! Each batch covers a half-open tick range `(lo, hi]`; ranges of one spine
//! are disjoint and increasing. A checkpoint appends one batch holding only
//! the change of a node's integral since the previous checkpoint of the same
//! executor history, then merges the two newest batches while the older is
//! at most twice the newer's size or the spine has more than
//! [`MAX_SPINE_LEVELS`] batches (geometric compaction; merged rows that
//! cancel are elided). Unchanged spines are referenced again, never
//! rewritten, so a checkpoint writes O(change) rows plus O(log) merges, not
//! the whole integral.
//!
//! Integrity on restore is layered: the store rehashes every object it reads
//! (tampered bytes are `Error::Storage("corrupt object")`); every blob carries
//! its own magic, version and the circuit execution fingerprint; batches
//! carry their node index, width, tick range and row count, which must match
//! the manifest; rows must be canonical; the spine sum must have the row count
//! the manifest records; and the restored integrals are revalidated as a
//! relationally consistent state of the circuit before anything is published.
//! Hash identity proves content identity only, never provenance: a producer
//! able to write the store can still write a different consistent state.
//!
//! The store writes objects with its own durability protocol (temporary file,
//! fsync, rename). Restore never trusts in-memory change counters: every
//! restore assigns the executor a fresh lineage. The checkpointer keeps a copy
//! of the integrals it last wrote (twice the state in memory) to compute the
//! appended batches.
use super::super::{Circuit, Executor};
use super::{CheckpointLimits, count_rows, identity, read_source, source_words, validate_state};
use crate::zset::Accumulator;
use crate::{Budget, Error, Limits, ZSet, port};
use vesper_loom_cas::{CasError, Digest, Domain, ObjectId, ObjectKind, Store};

/// The blobs' magics (`vesper/src/spine.ves` writes and checks them; the
/// cluster's garbage collector classifies objects by them).
pub(crate) const MANIFEST_MAGIC: &[u8; 8] = b"VFLUXDM2";
pub(crate) const BATCH_MAGIC: &[u8; 8] = b"VFLUXSB1";
/// Most batches one node's spine holds after a checkpoint.
pub const MAX_SPINE_LEVELS: usize = 8;

/// Outcome of one durable checkpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DurableReport {
    /// Content identity of the manifest; the handle used to restore.
    pub manifest: ObjectId,
    pub ticks: u64,
    /// Batches serialized and offered to the store (appended and merged).
    pub written: usize,
    /// Batches referenced unchanged from the previous checkpoint.
    pub reused: usize,
    /// Merges performed by compaction.
    pub merged: usize,
    /// Rows serialized in written batches.
    pub rows_written: usize,
    /// Bytes of the manifest and written batches (including deduplicated ones).
    pub bytes: u64,
}

/// Outcome of one durable restore.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DurableRestore {
    pub manifest: ObjectId,
    pub ticks: u64,
    /// The manifest this one was incrementally derived from, if any.
    pub parent: Option<ObjectId>,
    /// Every node's spine, oldest batch first.
    pub spines: Vec<Vec<ObjectId>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BatchRef {
    pub(crate) id: ObjectId,
    pub(crate) lo: u64,
    pub(crate) hi: u64,
    pub(crate) rows: usize,
}

#[derive(Debug)]
struct Committed {
    /// Store the batches live in; reuse never crosses stores.
    store: std::path::PathBuf,
    lineage: u64,
    fingerprint: [u8; 32],
    versions: Vec<u64>,
    spines: Vec<Vec<BatchRef>>,
    /// Integrals as of `manifest`, to compute appended batches.
    base: Vec<ZSet>,
    manifest: ObjectId,
}

/// Writes incremental durable checkpoints of one executor history. Bound to the
/// executor lineage it last checkpointed or restored; a different executor, or
/// the same one after any restore through another value, gets a full write.
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

pub(crate) fn encode_batch(
    fingerprint: &[u8; 32],
    node: usize,
    lo: u64,
    hi: u64,
    set: &ZSet,
    limits: CheckpointLimits,
) -> Result<Vec<u8>, Error> {
    port::dur_batch(fingerprint, node, lo, hi, set, limits.max_bytes)
}

fn put_batch(
    store: &Store,
    fingerprint: &[u8; 32],
    node: usize,
    lo: u64,
    hi: u64,
    set: &ZSet,
    limits: CheckpointLimits,
) -> Result<(BatchRef, u64), Error> {
    let blob = encode_batch(fingerprint, node, lo, hi, set, limits)?;
    let ingested = store.put_blob(Domain::Artifact, &blob).map_err(storage)?;
    Ok((
        BatchRef {
            id: ingested.id,
            lo,
            hi,
            rows: set.len(),
        },
        ingested.size,
    ))
}

/// Read and check one batch against its manifest reference.
fn read_batch(
    store: &Store,
    b: &BatchRef,
    fingerprint: &[u8; 32],
    node: usize,
    width: usize,
    limits: CheckpointLimits,
    budget: &mut Budget,
) -> Result<ZSet, Error> {
    let blob = read_blob(store, &b.id, limits.max_bytes)?;
    decode_batch(&blob, b, fingerprint, node, width, budget)
}

/// Check one batch blob against its manifest reference and decode it.
pub(crate) fn decode_batch(
    blob: &[u8],
    b: &BatchRef,
    fingerprint: &[u8; 32],
    node: usize,
    width: usize,
    budget: &mut Budget,
) -> Result<ZSet, Error> {
    port::dur_batch_read(blob, fingerprint, node, width, b.lo, b.hi, b.rows, budget)
}

/// Checked Z-set sum with an explicit weight per operand (+1 / -1).
fn signed_sum(
    parts: &[(&ZSet, i128)],
    width: usize,
    limits: Limits,
    budget: &mut Budget,
) -> Result<ZSet, Error> {
    let mut acc = Accumulator::new(&[width], limits)?;
    for (set, sign) in parts {
        if set.width != width {
            return Err(Error::Schema);
        }
        for (row, w) in set.entries() {
            acc.add(0, row, i128::from(w) * sign, budget)?;
        }
    }
    Ok(acc.finish()?.remove(0))
}

/// A parsed, header-checked manifest (batches not yet read).
pub(crate) struct ParsedManifest {
    pub(crate) ticks: u64,
    pub(crate) source: Option<super::super::cdc::SourceState>,
    pub(crate) nodes: Vec<(usize, usize, Vec<BatchRef>)>,
    pub(crate) parent: Option<ObjectId>,
}
impl ParsedManifest {
    /// Every batch the manifest names.
    pub(crate) fn batches(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.nodes
            .iter()
            .flat_map(|(_, _, s)| s.iter().map(|b| b.id))
    }
}

pub(crate) fn parse_manifest(
    store: &Store,
    manifest: &ObjectId,
    c: &Circuit,
    fingerprint: &[u8; 32],
    limits: CheckpointLimits,
    budget: &mut Budget,
) -> Result<ParsedManifest, Error> {
    let image = read_blob(store, manifest, limits.max_bytes)?;
    decode_manifest(&image, c, fingerprint, limits, budget)
}

/// Parse and check a manifest blob (its batches are not read). The codec
/// runs in Vesper (`vesper/src/spine.ves`); the policy is rebuilt and
/// checked between its head and its nodes, in the Rust parser's order.
pub(crate) fn decode_manifest(
    image: &[u8],
    c: &Circuit,
    fingerprint: &[u8; 32],
    limits: CheckpointLimits,
    budget: &mut Budget,
) -> Result<ParsedManifest, Error> {
    let head = port::dur_head(image, fingerprint, limits.max_sources, budget)?;
    let ticks = head[1];
    let source = read_source(&head, c)?;
    let widths: Vec<usize> = c.nodes.iter().map(|n| n.width).collect();
    let words = port::dur_nodes(
        image,
        head[0],
        &widths,
        ticks,
        c.limits.max_rows,
        limits.max_nodes,
        limits.max_total_rows,
        limits.max_total_cells,
        budget,
    )?;
    let id = |w: &[u64]| {
        let mut d = [0u8; 32];
        for (chunk, word) in d.as_chunks_mut::<8>().0.iter_mut().zip(w) {
            *chunk = word.to_le_bytes();
        }
        blob_id(d)
    };
    let mut at = 0;
    let mut nodes = Vec::with_capacity(widths.len());
    for _ in &widths {
        let (width, rows, batches) = (words[at] as usize, words[at + 1] as usize, words[at + 2]);
        at += 3;
        let mut spine = Vec::with_capacity(batches as usize);
        for _ in 0..batches {
            let b = &words[at..at + 7];
            spine.push(BatchRef {
                id: id(&b[..4]),
                lo: b[4],
                hi: b[5],
                rows: b[6] as usize,
            });
            at += 7;
        }
        nodes.push((width, rows, spine));
    }
    let parent = (words[at] == 1).then(|| id(&words[at + 1..at + 5]));
    Ok(ParsedManifest {
        ticks,
        source,
        nodes,
        parent,
    })
}

/// The manifest blob of `exec`'s state over `spines`, after charging the
/// restore's parse work (so a successful checkpoint restores under the
/// same limits, as the in-memory image does).
pub(crate) fn encode_manifest(
    exec: &Executor,
    fingerprint: &[u8; 32],
    spines: &[Vec<BatchRef>],
    parent: Option<&ObjectId>,
    limits: CheckpointLimits,
    budget: &mut Budget,
) -> Result<Vec<u8>, Error> {
    let words = |d: &Digest| -> Vec<u64> {
        d.as_bytes()
            .as_chunks::<8>()
            .0
            .iter()
            .map(|c| u64::from_le_bytes(*c))
            .collect()
    };
    let mut flat = Vec::new();
    for spine in spines {
        flat.push(spine.len() as u64);
        for b in spine {
            flat.extend(words(&b.id.digest));
            flat.extend([b.lo, b.hi, b.rows as u64]);
        }
    }
    port::dur_manifest(
        fingerprint,
        exec.ticks,
        &source_words(exec.source.as_ref()),
        &exec.prior,
        &flat,
        &parent.map(|p| words(&p.digest)).unwrap_or_default(),
        limits.max_bytes,
        budget,
    )
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
    /// the change of each node's integral since this checkpointer's previous
    /// checkpoint of the same executor history is appended to its spine; a
    /// spine whose batch has disappeared from the store is rewritten whole.
    /// The state is revalidated first (as [`Executor::checkpoint`] does). On
    /// error the checkpointer is unchanged; objects already stored stay as
    /// unreferenced immutable blobs until garbage collection.
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
            p.store.as_path() == store.root()
                && p.lineage != 0
                && p.lineage == exec.lineage
                && p.fingerprint == fingerprint
                && p.versions.len() == exec.versions.len()
        });
        let eval = Limits {
            max_rows: c.limits.max_rows.max(limits.max_total_rows),
            ..c.limits
        };
        let ticks = exec.ticks;
        let mut spines = Vec::with_capacity(exec.prior.len());
        let (mut written, mut reused, mut merged, mut rows_written, mut bytes) =
            (0usize, 0usize, 0usize, 0usize, 0u64);
        for (node, set) in exec.prior.iter().enumerate() {
            let previous = match reuse {
                Some(p) => {
                    let mut present = true;
                    for b in &p.spines[node] {
                        if !store.contains(&b.id).map_err(storage)? {
                            present = false;
                            break;
                        }
                    }
                    present.then_some(p)
                }
                None => None,
            };
            let Some(p) = previous else {
                let mut spine = Vec::new();
                if !set.is_empty() {
                    let (b, size) = put_batch(store, &fingerprint, node, 0, ticks, set, limits)?;
                    bytes = bytes.checked_add(size).ok_or(Error::Overflow)?;
                    rows_written += set.len();
                    written += 1;
                    spine.push(b);
                }
                spines.push(spine);
                continue;
            };
            let mut spine = p.spines[node].clone();
            if p.versions[node] != exec.versions[node] {
                let delta = signed_sum(
                    &[(set, 1), (&p.base[node], -1)],
                    set.width,
                    eval,
                    &mut budget,
                )?;
                if !delta.is_empty() {
                    let lo = spine.last().map_or(0, |b| b.hi);
                    if lo >= ticks {
                        return Err(Error::Checkpoint("changed integral without ticks"));
                    }
                    let (b, size) =
                        put_batch(store, &fingerprint, node, lo, ticks, &delta, limits)?;
                    bytes = bytes.checked_add(size).ok_or(Error::Overflow)?;
                    rows_written += delta.len();
                    written += 1;
                    spine.push(b);
                }
            }
            // Geometric compaction of the newest batches.
            while spine.len() >= 2 {
                let n = spine.len();
                let (older, newer) = (spine[n - 2], spine[n - 1]);
                if n <= MAX_SPINE_LEVELS && older.rows > newer.rows.saturating_mul(2) {
                    break;
                }
                let a = read_batch(
                    store,
                    &older,
                    &fingerprint,
                    node,
                    set.width,
                    limits,
                    &mut budget,
                )?;
                let b = read_batch(
                    store,
                    &newer,
                    &fingerprint,
                    node,
                    set.width,
                    limits,
                    &mut budget,
                )?;
                let sum = signed_sum(&[(&a, 1), (&b, 1)], set.width, eval, &mut budget)?;
                spine.truncate(n - 2);
                merged += 1;
                if !sum.is_empty() {
                    let (m, size) =
                        put_batch(store, &fingerprint, node, older.lo, newer.hi, &sum, limits)?;
                    bytes = bytes.checked_add(size).ok_or(Error::Overflow)?;
                    rows_written += sum.len();
                    written += 1;
                    spine.push(m);
                }
            }
            spines.push(spine);
        }
        // Restore bounds every referenced batch, not just the integrals: a
        // retained base plus deltas can exceed what the integrals need. Fold
        // the spines that cost the most extra rows into one full batch each
        // until the manifest parses under the same limits.
        let cells = |s: &[BatchRef], w: usize| s.iter().map(|b| b.rows.saturating_mul(w)).sum();
        loop {
            let (mut rows, mut cell_total) = (0usize, 0usize);
            let mut worst: Option<(usize, usize)> = None;
            for (node, (spine, set)) in spines.iter().zip(&exec.prior).enumerate() {
                let r: usize = spine.iter().map(|b| b.rows).sum();
                rows = rows.saturating_add(r);
                cell_total = cell_total.saturating_add(cells(spine, set.width));
                let oversized = spine.iter().any(|b| b.rows > c.limits.max_rows);
                let extra = if oversized {
                    usize::MAX
                } else {
                    r - r.min(set.len())
                };
                if spine.len() > 1 && extra > 0 && worst.is_none_or(|(_, e)| extra > e) {
                    worst = Some((node, extra));
                }
            }
            let over = rows > limits.max_total_rows || cell_total > limits.max_total_cells;
            let Some((node, _)) = worst.filter(|(_, e)| over || *e == usize::MAX) else {
                break;
            };
            let set = &exec.prior[node];
            let mut spine = Vec::new();
            if !set.is_empty() {
                let (b, size) = put_batch(store, &fingerprint, node, 0, ticks, set, limits)?;
                bytes = bytes.checked_add(size).ok_or(Error::Overflow)?;
                rows_written += set.len();
                written += 1;
                spine.push(b);
            }
            merged += 1;
            spines[node] = spine;
        }
        if let Some(p) = reuse {
            for (node, spine) in spines.iter().enumerate() {
                reused += spine.iter().filter(|b| p.spines[node].contains(b)).count();
            }
        }
        let manifest = encode_manifest(
            exec,
            &fingerprint,
            &spines,
            self.committed.as_ref().map(|p| &p.manifest),
            limits,
            &mut budget,
        )?;
        let ingested = store
            .put_blob(Domain::Artifact, &manifest)
            .map_err(storage)?;
        bytes = bytes.checked_add(ingested.size).ok_or(Error::Overflow)?;
        self.committed = Some(Committed {
            store: store.root().to_path_buf(),
            lineage: exec.lineage,
            fingerprint,
            versions: exec.versions.clone(),
            spines,
            base: exec.prior.clone(),
            manifest: ingested.id,
        });
        Ok(DurableReport {
            manifest: ingested.id,
            ticks,
            written,
            reused,
            merged,
            rows_written,
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
        let c: &Circuit = &exec.circuit;
        let mut budget = limits.budget(c);
        let fingerprint = identity(c, exec.source.as_ref(), limits, &mut budget)?;
        let parsed = parse_manifest(store, manifest, c, &fingerprint, limits, &mut budget)?;
        if parsed.source.as_ref().map(|s| &s.policy) != exec.source.as_ref().map(|s| &s.policy) {
            return Err(Error::Checkpoint("source policy"));
        }
        let eval = Limits {
            max_rows: c.limits.max_rows.max(limits.max_total_rows),
            ..c.limits
        };
        let mut prior = Vec::with_capacity(parsed.nodes.len());
        for (node, (width, rows, spine)) in parsed.nodes.iter().enumerate() {
            let mut acc = Accumulator::new(&[*width], eval)?;
            for b in spine {
                let batch = read_batch(store, b, &fingerprint, node, *width, limits, &mut budget)?;
                for (row, w) in batch.entries() {
                    acc.add(0, row, i128::from(w), &mut budget)?;
                }
            }
            let set = acc.finish()?.remove(0);
            if set.len() != *rows {
                return Err(Error::Checkpoint("spine row count"));
            }
            prior.push(set);
        }
        count_rows(&prior, limits)?;
        validate_state(
            c,
            &prior,
            parsed.ticks,
            parsed.source.as_ref(),
            limits,
            &mut budget,
        )?;
        let spines: Vec<Vec<BatchRef>> = parsed.nodes.into_iter().map(|(_, _, s)| s).collect();
        exec.prior = prior;
        exec.ticks = parsed.ticks;
        exec.source = parsed.source;
        exec.lineage = super::super::fresh_lineage();
        let ids = spines
            .iter()
            .map(|s| s.iter().map(|b| b.id).collect())
            .collect();
        self.committed = Some(Committed {
            store: store.root().to_path_buf(),
            lineage: exec.lineage,
            fingerprint,
            versions: exec.versions.clone(),
            spines,
            base: exec.prior.clone(),
            manifest: *manifest,
        });
        Ok(DurableRestore {
            manifest: *manifest,
            ticks: parsed.ticks,
            parent: parsed.parent,
            spines: ids,
        })
    }
}
