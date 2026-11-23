//! Frozen Rust pack transfer engine (budget, export and import).

use std::path::Path;

use crate::transfer::{MAGIC, PACK_VERSION, TransferError, TransferLimits, TransferReport};
use crate::{
    Domain, EntryKind, OBJECT_SCHEMA_VERSION, ObjectHasher, ObjectId, ObjectKind, Store, Tree,
};

const HEADER_BYTES: usize = 54;
const RECORD_BYTES: usize = 42;

/// all verified objects share one cap. The final pack has a separate cap.
#[derive(Debug)]
pub(crate) struct Budget {
    limits: TransferLimits,
    work: usize,
    metadata: usize,
    payload_reserved: usize,
    pack_reserved: usize,
    entries: usize,
}

impl Budget {
    pub(crate) fn new(limits: TransferLimits) -> Result<Self, TransferError> {
        limits.validate()?;
        Ok(Self {
            limits,
            work: 0,
            metadata: 0,
            payload_reserved: 0,
            pack_reserved: 0,
            entries: 0,
        })
    }

    pub(crate) fn limits(&self) -> TransferLimits {
        self.limits
    }

    pub(crate) fn work(&mut self, amount: usize) -> Result<(), TransferError> {
        charge(&mut self.work, amount, self.limits.max_work, "work")
    }

    pub(crate) fn metadata(&mut self, amount: usize) -> Result<(), TransferError> {
        charge(
            &mut self.metadata,
            amount,
            self.limits.max_metadata_bytes,
            "metadata bytes",
        )
    }

    pub(crate) fn entries(&mut self, amount: usize) -> Result<(), TransferError> {
        charge(
            &mut self.entries,
            amount,
            self.limits.max_tree_entries,
            "tree entries",
        )
    }

    /// `additional` is relative to len, exactly as for Vec::try_reserve_exact.
    pub(crate) fn reserve_metadata<T>(
        &mut self,
        out: &mut Vec<T>,
        additional: usize,
    ) -> Result<(), TransferError> {
        let target = add(out.len(), additional, "metadata bytes")?;
        if target <= out.capacity() {
            return Ok(());
        }
        let bytes = mul(target, size_of::<T>(), "metadata bytes")?;
        self.metadata(bytes)?;
        self.work(bytes)?;
        reserve(out, additional)
    }

    pub(crate) fn reserve_payload(
        &mut self,
        out: &mut Vec<u8>,
        additional: usize,
    ) -> Result<(), TransferError> {
        let target = add(out.len(), additional, "payload bytes")?;
        if target > self.limits.max_pack_bytes {
            return Err(TransferError::Limit("payload bytes"));
        }
        if target <= out.capacity() {
            return Ok(());
        }
        charge(
            &mut self.payload_reserved,
            target,
            self.limits.max_pack_bytes,
            "payload capacity requests",
        )?;
        self.work(target)?;
        reserve(out, additional)
    }

    pub(crate) fn reserve_pack(
        &mut self,
        out: &mut Vec<u8>,
        additional: usize,
    ) -> Result<(), TransferError> {
        let target = add(out.len(), additional, "pack bytes")?;
        if target > self.limits.max_pack_bytes {
            return Err(TransferError::Limit("pack bytes"));
        }
        if target <= out.capacity() {
            return Ok(());
        }
        charge(
            &mut self.pack_reserved,
            target,
            self.limits.max_pack_bytes,
            "pack capacity requests",
        )?;
        self.work(target)?;
        reserve(out, additional)
    }
}

fn charge(
    used: &mut usize,
    amount: usize,
    limit: usize,
    name: &'static str,
) -> Result<(), TransferError> {
    let next = add(*used, amount, name)?;
    if next > limit {
        return Err(TransferError::Limit(name));
    }
    *used = next;
    Ok(())
}

pub(crate) fn add(a: usize, b: usize, name: &'static str) -> Result<usize, TransferError> {
    a.checked_add(b).ok_or(TransferError::Limit(name))
}

pub(crate) fn mul(a: usize, b: usize, name: &'static str) -> Result<usize, TransferError> {
    a.checked_mul(b).ok_or(TransferError::Limit(name))
}

fn reserve<T>(out: &mut Vec<T>, additional: usize) -> Result<(), TransferError> {
    out.try_reserve_exact(additional)
        .map_err(|_| TransferError::AllocationFailed)
}

/// Decode only after prepaying all Tree allocations, error clones, reencoding
/// and later publication tree buffers. This does not pay publication's generic
/// Store I/O scratch/path costs; the importer must prepay those for every record.
pub(crate) fn decode_tree(
    domain: Domain,
    bytes: &[u8],
    budget: &mut Budget,
) -> Result<Tree, TransferError> {
    budget.work(bytes.len())?;
    if bytes.len() < 16 || &bytes[..4] != b"VLTR" {
        return Err(TransferError::InvalidPack("tree header"));
    }
    if u32::from_le_bytes(bytes[4..8].try_into().unwrap()) != crate::TREE_SCHEMA_VERSION {
        return Err(TransferError::InvalidPack("tree version"));
    }
    let count = usize::try_from(u64::from_le_bytes(bytes[8..16].try_into().unwrap()))
        .map_err(|_| TransferError::Limit("tree entries"))?;
    budget.entries(count)?;
    // Every entry needs u16 name length, one name byte, tag, digest and size.
    if count > (bytes.len() - 16) / 44 {
        return Err(TransferError::InvalidPack("truncated tree entries"));
    }
    let mut pos = 16usize;
    let mut names = 0usize;
    for _ in 0..count {
        let length_end = add(pos, 2, "tree bytes")?;
        let raw = bytes
            .get(pos..length_end)
            .ok_or(TransferError::InvalidPack("tree name length"))?;
        let length = u16::from_le_bytes(raw.try_into().unwrap()) as usize;
        if length == 0 || length > 255 {
            return Err(TransferError::InvalidPack("tree name length"));
        }
        names = add(names, length, "tree name bytes")?;
        pos = add(length_end, add(length, 41, "tree bytes")?, "tree bytes")?;
        if pos > bytes.len() {
            return Err(TransferError::InvalidPack("truncated tree entry"));
        }
    }
    if pos != bytes.len() {
        return Err(TransferError::InvalidPack("tree trailing bytes"));
    }
    // Two BTree maps: reserve 2 KiB per entry for logical node storage,
    // including sparsely populated nodes. 32x name bytes covers originals,
    // previous/error strings, uppercase names, Unicode lowercase expansion
    // (at most 3x bytes) and temporary String growth/copies. 16x encode capacity
    // covers decode canonical checks and eventual Tree::encode publication,
    // including geometric Vec growth from its initial 16 + 64*count capacity.
    let encode = add(16, mul(count, 64, "tree metadata")?, "tree metadata")?;
    let encode = add(encode, bytes.len(), "tree metadata")?;
    let metadata = add(
        add(
            mul(count, 2048, "tree metadata")?,
            mul(names, 32, "tree metadata")?,
            "tree metadata",
        )?,
        mul(encode, 16, "tree metadata")?,
        "tree metadata",
    )?;
    budget.metadata(metadata)?;
    // BTree comparisons, name transformations and encoding/copies. A tree's
    // height is bounded by binary log(count)+1; allow 64 name comparisons per
    // level across map operations, each inspecting at most 255 bytes.
    let levels = (usize::BITS - count.leading_zeros()) as usize + 1;
    let comparison_work = mul(mul(count, levels, "tree work")?, 64 * 255, "tree work")?;
    budget.work(add(comparison_work, metadata, "tree work")?)?;
    Tree::decode(domain, bytes).map_err(|error| TransferError::Cas(error.into()))
}

pub(crate) fn export_pack_rust(
    store: &Store,
    root: ObjectId,
    limits: TransferLimits,
) -> Result<Vec<u8>, TransferError> {
    export::export_pack_rust(store, root, limits)
}

pub(crate) fn import_pack_rust(
    store_path: &Path,
    expected_root: ObjectId,
    bytes: &[u8],
    limits: TransferLimits,
) -> Result<TransferReport, TransferError> {
    import::import_pack_rust(store_path, expected_root, bytes, limits)
}

mod export {
    use super::*;
    struct Record {
        id: ObjectId,
        bytes: Vec<u8>,
        height: usize,
    }

    struct Export<'a> {
        store: &'a Store,
        budget: Budget,
        records: Vec<Record>,
        active: Vec<ObjectId>,
        pack_bytes: usize,
    }
    pub(super) fn export_pack_rust(
        store: &Store,
        root: ObjectId,
        limits: TransferLimits,
    ) -> Result<Vec<u8>, TransferError> {
        let budget = Budget::new(limits)?;
        if limits.max_pack_bytes < HEADER_BYTES {
            return Err(TransferError::Limit("pack bytes"));
        }
        let mut export = Export {
            store,
            budget,
            records: Vec::new(),
            active: Vec::new(),
            pack_bytes: HEADER_BYTES,
        };
        export.visit(root, 1)?;
        let mut bytes = Vec::new();
        export.budget.reserve_pack(&mut bytes, export.pack_bytes)?;
        export.budget.work(export.pack_bytes)?;
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&PACK_VERSION.to_le_bytes());
        bytes.extend_from_slice(&OBJECT_SCHEMA_VERSION.to_le_bytes());
        root.encode_into(&mut bytes);
        let count =
            u32::try_from(export.records.len()).map_err(|_| TransferError::Limit("objects"))?;
        bytes.extend_from_slice(&count.to_le_bytes());
        for record in export.records {
            record.id.encode_into(&mut bytes);
            let length = u64::try_from(record.bytes.len())
                .map_err(|_| TransferError::Limit("object bytes"))?;
            bytes.extend_from_slice(&length.to_le_bytes());
            bytes.extend_from_slice(&record.bytes);
        }
        debug_assert_eq!(bytes.len(), export.pack_bytes);
        Ok(bytes)
    }

    impl Export<'_> {
        fn visit(&mut self, id: ObjectId, depth: usize) -> Result<usize, TransferError> {
            self.budget.work(1)?;
            let limits = self.budget.limits();
            if depth > limits.max_depth {
                return Err(TransferError::Limit("depth"));
            }
            // Linear tables avoid hidden map allocations. Prepay every possible
            // full identity comparison before scanning them.
            self.budget
                .work(mul(self.records.len(), 34, "graph work")?)?;
            if let Some(index) = self.records.iter().position(|record| record.id == id) {
                let height = self.records[index].height;
                if add(depth, height - 1, "depth")? > limits.max_depth {
                    return Err(TransferError::Limit("depth"));
                }
                return Ok(index);
            }
            self.budget
                .work(mul(self.active.len(), 34, "graph work")?)?;
            if self.active.contains(&id) {
                return Err(TransferError::InvalidPack("cyclic object closure"));
            }
            // Active ancestors will also become records. Count them before any
            // object read, so a deep closure cannot defer the object limit check.
            if add(self.records.len(), self.active.len(), "objects")? >= limits.max_objects {
                return Err(TransferError::Limit("objects"));
            }
            grow(&mut self.budget, &mut self.active, limits.max_depth)?;
            self.active.push(id);
            let base = add(self.pack_bytes, RECORD_BYTES, "pack bytes")?;
            let max_payload = limits
                .max_pack_bytes
                .checked_sub(base)
                .ok_or(TransferError::Limit("pack bytes"))?;
            let payload = self
                .store
                .read_verified_bounded(&id, max_payload, &mut self.budget)?;
            self.pack_bytes = add(base, payload.len(), "pack bytes")?;
            let mut height = 1;
            if id.kind == ObjectKind::Tree {
                let tree = decode_tree(id.domain, &payload, &mut self.budget)?;
                for (_, entry) in tree.entries() {
                    self.budget.work(64)?;
                    let child = tree.child_id(entry);
                    let index = self.visit(child, add(depth, 1, "depth")?)?;
                    let record = &self.records[index];
                    if let EntryKind::File { .. } = entry.kind
                        && u64::try_from(record.bytes.len()).ok() != Some(entry.size)
                    {
                        return Err(TransferError::InvalidPack("tree file size mismatch"));
                    }
                    height = height.max(add(record.height, 1, "depth")?);
                }
            }
            self.active.pop();
            grow(&mut self.budget, &mut self.records, limits.max_objects)?;
            let index = self.records.len();
            self.records.push(Record {
                id,
                bytes: payload,
                height,
            });
            Ok(index)
        }
    }

    /// Explicit geometric requests make cumulative capacity accounting linear in
    /// retained metadata, without reserving the maximum inventory for tiny packs.
    fn grow<T>(budget: &mut Budget, values: &mut Vec<T>, max: usize) -> Result<(), TransferError> {
        if values.len() < values.capacity() {
            return Ok(());
        }
        let target = if values.capacity() == 0 {
            4.min(max)
        } else {
            mul(values.capacity(), 2, "graph metadata")?.min(max)
        };
        if target <= values.len() {
            return Err(TransferError::Limit("graph metadata"));
        }
        budget.reserve_metadata(values, target - values.len())
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn port_sizes_export() {
            assert_eq!(std::mem::size_of::<super::Record>(), 72);
            assert_eq!(std::mem::size_of::<crate::ObjectId>(), 34);
        }
    }
}

mod import {
    use super::*;
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
            ObjectId::decode(self.take(34)?)
                .ok_or(TransferError::InvalidPack("object identity tags"))
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
    pub(super) fn import_pack_rust(
        store_path: &Path,
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
            let len = usize::try_from(reader.u64()?)
                .map_err(|_| TransferError::Limit("payload bytes"))?;
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

    #[cfg(test)]
    mod tests {
        #[test]
        fn port_sizes_import() {
            assert_eq!(std::mem::size_of::<super::Record<'_>>(), 112);
            assert_eq!(std::mem::size_of::<super::Visit>(), 16);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer::*;

    #[test]
    fn limits_reject_zero_and_hard_overflow() {
        let defaults = TransferLimits::default();
        assert!(defaults.validate().is_ok());
        assert!(
            TransferLimits {
                max_work: 0,
                ..defaults
            }
            .validate()
            .is_err()
        );
        assert!(
            TransferLimits {
                max_pack_bytes: HARD_MAX_PACK_BYTES + 1,
                ..defaults
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn capacity_requests_are_cumulative_but_no_growth_is_free() {
        let mut budget = Budget::new(TransferLimits {
            max_pack_bytes: 8,
            ..TransferLimits::default()
        })
        .unwrap();
        let mut payload = Vec::new();
        budget.reserve_payload(&mut payload, 4).unwrap();
        budget.reserve_payload(&mut payload, 4).unwrap();
        payload.extend_from_slice(b"1234");
        assert!(budget.reserve_payload(&mut payload, 4).is_err());
        let mut pack = Vec::new();
        budget.reserve_pack(&mut pack, 8).unwrap();
        assert_eq!(budget.payload_reserved, 4);
        assert_eq!(budget.pack_reserved, 8);
    }

    #[test]
    fn tree_counts_and_metadata_are_checked_before_decode() {
        let tree = Tree::new(Domain::Source);
        let bytes = tree.encode();
        let mut budget = Budget::new(TransferLimits::default()).unwrap();
        assert_eq!(
            decode_tree(Domain::Source, &bytes, &mut budget).unwrap(),
            tree
        );
        let mut budget = Budget::new(TransferLimits {
            max_metadata_bytes: 1,
            ..TransferLimits::default()
        })
        .unwrap();
        assert!(matches!(
            decode_tree(Domain::Source, &bytes, &mut budget),
            Err(TransferError::Limit("metadata bytes"))
        ));
        let mut bad = bytes;
        bad[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(
            decode_tree(
                Domain::Source,
                &bad,
                &mut Budget::new(TransferLimits::default()).unwrap()
            )
            .is_err()
        );
    }
}
