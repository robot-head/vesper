use crate::{EntryKind, OBJECT_SCHEMA_VERSION, ObjectId, ObjectKind, Store};

use super::{
    Budget, HEADER_BYTES, MAGIC, PACK_VERSION, RECORD_BYTES, TransferError, TransferLimits, add,
    decode_tree, mul,
};

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

/// Export a completely verified immutable closure in canonical DFS postorder.
/// No partial output is returned on failure; source CAS is never changed.
/// A successful pack establishes content identity only, never provenance.
pub fn export_pack(
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
    let count = u32::try_from(export.records.len()).map_err(|_| TransferError::Limit("objects"))?;
    bytes.extend_from_slice(&count.to_le_bytes());
    for record in export.records {
        record.id.encode_into(&mut bytes);
        let length =
            u64::try_from(record.bytes.len()).map_err(|_| TransferError::Limit("object bytes"))?;
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
