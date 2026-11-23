use crate::{ObjectId, Store};

use super::{TransferError, TransferLimits};

/// Export a completely verified immutable closure in canonical DFS postorder.
/// No partial output is returned on failure; source CAS is never changed.
/// A successful pack establishes content identity only, never provenance.
pub fn export_pack(
    store: &Store,
    root: ObjectId,
    limits: TransferLimits,
) -> Result<Vec<u8>, TransferError> {
    #[cfg(vesper_port)]
    return crate::port_store::export_pack(store.root(), &root, limits);
    #[cfg(not(vesper_port))]
    crate::engine::transfer::export_pack_rust(store, root, limits)
}
