//! The Vesper build of the batch update (`vesper/src/batch.ves`). The
//! descriptors are lent as four words each (slot, generation, event kind,
//! value) from a stack buffer, so a batch allocates nothing.

use crate::batch::{ConnEvent, ConnEventDesc, MAX_BATCH};
use crate::error::BatchError;
use crate::port::table::ConnTable;
use crate::port::types::CoreBatchFault;
use crate::simd::{IsaPath, Kernels};
use std::hash::BuildHasher;

fn encode(batch: &[ConnEventDesc], words: &mut [u64; MAX_BATCH * 4]) {
    for (d, w) in batch.iter().zip(words.chunks_exact_mut(4)) {
        let (kind, value) = match d.event {
            ConnEvent::PacketReceived { pn } => (0, pn),
            ConnEvent::PacketSent { bytes } => (1, u64::from(bytes)),
            ConnEvent::BytesAcked { bytes } => (2, u64::from(bytes)),
            ConnEvent::BytesLost { bytes } => (3, u64::from(bytes)),
            ConnEvent::CwndUpdate { cwnd_bytes } => (4, u64::from(cwnd_bytes)),
        };
        w.copy_from_slice(&[
            u64::from(d.slot.index()),
            u64::from(d.slot.generation()),
            kind,
            value,
        ]);
    }
}

// Descriptor indices are below MAX_BATCH.
fn fault(e: CoreBatchFault) -> BatchError {
    match e {
        CoreBatchFault::TooLong(len) => BatchError::TooLong { len: len as usize },
        CoreBatchFault::StaleHandle(i) => BatchError::StaleHandle { index: i as usize },
        CoreBatchFault::PacketNumberRange { index, pn } => BatchError::PacketNumberRange {
            index: index as usize,
            pn,
        },
        CoreBatchFault::BytesInFlightUnderflow(i) => {
            BatchError::BytesInFlightUnderflow { index: i as usize }
        }
        CoreBatchFault::BytesInFlightOverflow(i) => {
            BatchError::BytesInFlightOverflow { index: i as usize }
        }
    }
}

impl<S: BuildHasher> ConnTable<S> {
    /// Applies `batch` atomically with the table's kernels.
    pub fn apply_batch(&mut self, batch: &[ConnEventDesc]) -> Result<(), BatchError> {
        self.apply_batch_with(self.kernels, batch)
    }

    /// Applies `batch` atomically with scalar per-connection aggregation (the
    /// reference for [`ConnTable::apply_batch`]).
    pub fn apply_batch_scalar(&mut self, batch: &[ConnEventDesc]) -> Result<(), BatchError> {
        self.apply_batch_with(Kernels::scalar(), batch)
    }

    /// Applies `batch` atomically through per-tile staging with the given
    /// kernels.
    pub fn apply_batch_with(
        &mut self,
        kernels: Kernels,
        batch: &[ConnEventDesc],
    ) -> Result<(), BatchError> {
        if batch.len() > MAX_BATCH {
            return Err(BatchError::TooLong { len: batch.len() });
        }
        let mut words = [0u64; MAX_BATCH * 4];
        encode(batch, &mut words);
        match self
            .core
            .apply_batch(&words[..batch.len() * 4], kernels.path() != IsaPath::Scalar)
        {
            None => Ok(()),
            Some(e) => Err(fault(e)),
        }
    }
}
