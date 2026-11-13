//! Validate the complete canonical closure before initializing a destination.
//! Only existing CAS publication/I/O operations run after that boundary.
use std::path::Path;

use super::{
    Budget, HEADER_BYTES, MAGIC, PACK_VERSION, RECORD_BYTES, TransferError, TransferLimits,
    TransferReport, add, decode_tree, mul,
};
use crate::{OBJECT_SCHEMA_VERSION, ObjectHasher, ObjectId, ObjectKind, Store, Tree};

#[derive(Debug)]
struct Record<'a> {
    id: ObjectId,
    payload: &'a [u8],
    tree: Option<Tree>,
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], TransferError> {
        let end = add(self.pos, len, "pack bytes")?;
        let bytes = self
            .bytes
            .get(self.pos..end)
            .ok_or(TransferError::InvalidPack("truncated record"))?;
        self.pos = end;
        Ok(bytes)
    }
    fn u32(&mut self) -> Result<u32, TransferError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, TransferError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn id(&mut self) -> Result<ObjectId, TransferError> {
        ObjectId::decode(self.take(34)?).ok_or(TransferError::InvalidPack("object identity tags"))
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Visit {
    state: u8,
    height: usize,
}
struct Closure<'r, 'a, 'b> {
    records: &'r [Record<'a>],
    visits: Vec<Visit>,
    next: usize,
    budget: &'b mut Budget,
}
impl Closure<'_, '_, '_> {
    fn lookup(&mut self, id: ObjectId) -> Result<usize, TransferError> {
        // An ObjectId comparison has a fixed 34-byte domain/kind/digest bound.
        for (index, record) in self.records.iter().enumerate() {
            self.budget.work(1)?;
            if record.id == id {
                return Ok(index);
            }
        }
        Err(TransferError::InvalidPack("missing child object"))
    }
    fn depth(&self, depth: usize, height: usize) -> Result<(), TransferError> {
        let deepest = add(depth, height - 1, "depth")?;
        if deepest > self.budget.limits().max_depth {
            return Err(TransferError::Limit("depth"));
        }
        Ok(())
    }
    fn visit(&mut self, index: usize, depth: usize) -> Result<usize, TransferError> {
        self.budget.work(1)?;
        self.depth(depth, 1)?;
        match self.visits[index].state {
            1 => return Err(TransferError::InvalidPack("cyclic object closure")),
            2 => {
                let height = self.visits[index].height;
                // A shared subtree visited first on a shorter path must still
                // satisfy the complete depth envelope on every deeper path.
                self.depth(depth, height)?;
                return Ok(height);
            }
            _ => {}
        }
        self.visits[index].state = 1;
        let mut height = 1usize;
        if let Some(tree) = &self.records[index].tree {
            for (_, entry) in tree.entries() {
                self.budget.work(1)?;
                let child = self.lookup(tree.child_id(entry))?;
                if entry.kind.object_kind() == ObjectKind::Blob {
                    let actual = u64::try_from(self.records[child].payload.len())
                        .map_err(|_| TransferError::Limit("payload bytes"))?;
                    if actual != entry.size {
                        return Err(TransferError::InvalidPack("recorded file size"));
                    }
                }
                let child_height = self.visit(child, add(depth, 1, "depth")?)?;
                height = height.max(add(child_height, 1, "depth")?);
            }
        }
        self.depth(depth, height)?;
        // Wire records must be exactly the first-completion DFS postorder.
        // This simultaneously rejects extras and alternate valid topological
        // orders, without allocating a second expected-order vector.
        if index != self.next {
            return Err(TransferError::InvalidPack("noncanonical closure order"));
        }
        self.next = add(self.next, 1, "objects")?;
        self.visits[index] = Visit { state: 2, height };
        Ok(height)
    }
}

fn publication_resources(
    records: &[Record<'_>],
    input_path_bytes: usize,
    budget: &mut Budget,
) -> Result<(), TransferError> {
    const IO_BUFFER: usize = 64 * 1024;
    const VERIFY_CHECKS: usize = 8;
    const FIXED_PATH_BYTES: usize = 64 * 1024;
    const CHILD_PATH_BYTES: usize = 4096;

    // Prepay initialization marker/path work too. The canonical resolved root
    // prefix and its legacy copies are explicitly outside this envelope, like
    // caller-path resolution. Fixed relative paths/IDs/retries remain charged.
    let path_bytes = mul(input_path_bytes, 32, "publication metadata")?;
    let initialization = add(FIXED_PATH_BYTES, path_bytes, "publication metadata")?;
    budget.metadata(initialization)?;
    budget.work(initialization)?;
    for record in records {
        // One put_reader staging buffer plus two occupied-object checks for
        // each of the Store's four publication attempts. This deliberately
        // charges cumulative requests, including buffers freed between calls.
        let buffers = mul(IO_BUFFER, 1 + VERIFY_CHECKS, "publication metadata")?;
        let paths = add(FIXED_PATH_BYTES, path_bytes, "publication metadata")?;
        let child_paths = if let Some(tree) = &record.tree {
            mul(tree.len(), CHILD_PATH_BYTES, "publication metadata")?
        } else {
            0
        };
        let metadata = add(
            add(buffers, paths, "publication metadata")?,
            child_paths,
            "publication metadata",
        )?;
        budget.metadata(metadata)?;
        budget.work(metadata)?;

        // Staging read/hash/write, plus read+hash of at most staged_size+1
        // bytes in every occupied-object verification. The Store's bounded
        // publication helper enforces that probe limit on its actual handle.
        let staging = mul(record.payload.len(), 3, "publication work")?;
        let probe = add(record.payload.len(), 1, "publication work")?;
        let verification = mul(
            mul(probe, VERIFY_CHECKS, "publication work")?,
            2,
            "publication work",
        )?;
        budget.work(add(staging, verification, "publication work")?)?;
        // Hasher/path/entry bookkeeping has fixed bounded per-object overhead.
        budget.work(4096)?;
    }
    Ok(())
}

/// Import one exact immutable closure. Malformed packs and logical resource
/// refusals cannot create or change the destination. After full admission,
/// store/I/O failures may leave only objects published through existing CAS
/// discipline; no filesystem-wide rollback or execution/trust approval follows.
pub fn import_pack(
    store_path: impl AsRef<Path>,
    expected_root: ObjectId,
    bytes: &[u8],
    limits: TransferLimits,
) -> Result<TransferReport, TransferError> {
    let mut budget = Budget::new(limits)?;
    if bytes.len() > limits.max_pack_bytes {
        return Err(TransferError::Limit("pack bytes"));
    }
    if bytes.len() < HEADER_BYTES {
        return Err(TransferError::InvalidPack("truncated header"));
    }
    budget.work(HEADER_BYTES)?;
    let mut reader = Reader { bytes, pos: 0 };
    if reader.take(8)? != MAGIC {
        return Err(TransferError::InvalidPack("magic"));
    }
    if reader.u32()? != PACK_VERSION || reader.u32()? != OBJECT_SCHEMA_VERSION {
        return Err(TransferError::InvalidPack("version"));
    }
    let root = reader.id()?;
    if root != expected_root {
        return Err(TransferError::UnexpectedRoot {
            expected: expected_root,
            found: root,
        });
    }
    let count = usize::try_from(reader.u32()?).map_err(|_| TransferError::Limit("objects"))?;
    if count == 0 {
        return Err(TransferError::InvalidPack("empty closure"));
    }
    if count > limits.max_objects {
        return Err(TransferError::Limit("objects"));
    }
    if count > (bytes.len() - HEADER_BYTES) / RECORD_BYTES {
        return Err(TransferError::InvalidPack("truncated records"));
    }
    let mut records = Vec::new();
    budget.reserve_metadata(&mut records, count)?;
    let mut payload_bytes = 0usize;
    for _ in 0..count {
        budget.work(RECORD_BYTES)?;
        let id = reader.id()?;
        for earlier in &records {
            let earlier: &Record<'_> = earlier;
            budget.work(1)?;
            if earlier.id == id {
                return Err(TransferError::InvalidPack("duplicate object"));
            }
        }
        let len =
            usize::try_from(reader.u64()?).map_err(|_| TransferError::Limit("payload bytes"))?;
        let payload = reader.take(len)?;
        payload_bytes = add(payload_bytes, len, "payload bytes")?;
        // Include fixed domain-context/finalization work before constructing
        // the hasher, in addition to the actual payload scan.
        budget.work(add(len, 2048, "hash work")?)?;
        let mut hasher = ObjectHasher::new(id.domain, id.kind);
        hasher.update(payload);
        let actual = hasher.finish();
        if actual != id {
            return Err(TransferError::DigestMismatch {
                expected: id,
                actual,
            });
        }
        let tree = if id.kind == ObjectKind::Tree {
            let tree = decode_tree(id.domain, payload, &mut budget)?;
            // Shared preflight has already paid this complete encode/copy
            // envelope, including the Store's eventual publication encoding.
            let canonical = tree.encode();
            if canonical != payload {
                return Err(TransferError::InvalidPack("noncanonical tree"));
            }
            Some(tree)
        } else {
            None
        };
        records.push(Record { id, payload, tree });
    }
    if reader.pos != bytes.len() {
        return Err(TransferError::InvalidPack("trailing bytes"));
    }
    if records.last().unwrap().id != root {
        return Err(TransferError::InvalidPack("root is not last"));
    }
    let mut visits = Vec::new();
    budget.reserve_metadata(&mut visits, count)?;
    visits.resize(count, Visit::default());
    let mut closure = Closure {
        records: &records,
        visits,
        next: 0,
        budget: &mut budget,
    };
    closure.visit(count - 1, 1)?;
    if closure.next != count {
        return Err(TransferError::InvalidPack("unreachable records"));
    }
    let store_path = store_path.as_ref();
    publication_resources(
        &records,
        store_path.as_os_str().as_encoded_bytes().len(),
        &mut budget,
    )?;

    // Complete validation and resource admission end here. No budget operation,
    // arithmetic capable of producing Limit, or pack validation follows open.
    let store = Store::open(store_path)?;
    let mut inserted = 0usize;
    for record in &records {
        let result = if let Some(tree) = &record.tree {
            store.put_tree(tree)?
        } else {
            store.put_blob_expected(record.id, record.payload)?
        };
        // Count is prevalidated <= HARD_MAX_OBJECTS, so these increments and
        // the final subtraction cannot overflow or produce a late refusal.
        inserted += usize::from(result.newly_stored);
    }
    Ok(TransferReport {
        root,
        objects: count,
        pack_bytes: bytes.len(),
        payload_bytes,
        inserted,
        reused: count - inserted,
    })
}
