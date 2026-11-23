use std::path::Path;

use crate::ObjectId;

use super::{TransferError, TransferLimits, TransferReport};

/// Import an untrusted pack into a new or existing store.
///
/// The pack, expected root, closure integrity and admission limits are
/// completely verified before opening or creating `store_path`. A validated
/// failure leaves `store_path` absent or untouched.
pub fn import_pack(
    store_path: impl AsRef<Path>,
    expected_root: ObjectId,
    bytes: &[u8],
    limits: TransferLimits,
) -> Result<TransferReport, TransferError> {
    #[cfg(vesper_port)]
    return crate::port_store::import_pack(store_path.as_ref(), &expected_root, bytes, limits);
    #[cfg(not(vesper_port))]
    crate::engine::transfer::import_pack_rust(store_path.as_ref(), expected_root, bytes, limits)
}
