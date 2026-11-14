//! Snapshot fork clones that remap the snapshot `MAP_PRIVATE` (§37.1.2,
//! §37.1.3), Linux with the `uffd` feature.
//!
//! [`SealedSnapshot::seal`] writes a frozen [`SnapshotImage`] into a sealed
//! memfd once. [`SealedSnapshot::fork_mapped`] gives each clone its own
//! `MAP_PRIVATE` mapping of that memfd, registered for userfaultfd MINOR
//! faults: pages are mapped in place with `UFFDIO_CONTINUE` (zero copy, one
//! physical page shared by every clone) and become private only when the
//! clone writes them. This replaces the host-model `ForkClone` overlay for real
//! guest memory.
//!
//! The linear discipline is the same as [`crate::fork`]: a [`MappedClone`]
//! serves no page until [`SnapshotResumeTicket::reseed_mapped`] has written its
//! `AetherVmGenIdWire` block (a copy-on-write store into the clone's private
//! mapping, made inside a serving session) and minted its sealer from the root
//! key, generation id, epoch and fresh [`CloneEntropy`].
#![forbid(unsafe_code)]

use crate::entropy::CloneEntropy;
use crate::fork::{
    ForkAuthority, ForkError, ReseededVaultEpoch, RootKey, SnapshotImage, SnapshotResumeTicket,
    VMGENID_WIRE_BYTES, VmGenId, VmGenIdWire,
};
use crate::host::{MinorRegion, SealedMemfd, Toucher, UffdError, UffdStats};
use crate::hotset::HotSet;
use crate::{Error, PAGE_BYTES};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MappedForkError {
    Fork(ForkError),
    Uffd(UffdError),
    /// The clone has not been reseeded; no guest may touch its memory yet.
    NotReseeded,
}
impl From<ForkError> for MappedForkError {
    fn from(value: ForkError) -> Self {
        Self::Fork(value)
    }
}
impl From<UffdError> for MappedForkError {
    fn from(value: UffdError) -> Self {
        Self::Uffd(value)
    }
}

/// A snapshot image written once into a sealed memfd.
#[derive(Debug)]
pub struct SealedSnapshot {
    image: SnapshotImage,
    memfd: Arc<SealedMemfd>,
}

impl SealedSnapshot {
    pub fn seal(image: &SnapshotImage) -> Result<Self, MappedForkError> {
        Self::seal_with(image, false)
    }
    /// [`SealedSnapshot::seal`] on 2 MiB huge pages where the host allows it
    /// (a whole number of huge pages and enough free pages in the pool),
    /// 4 KiB pages otherwise; [`SealedMemfd::is_huge`] says which.
    pub fn seal_preferring_huge(image: &SnapshotImage) -> Result<Self, MappedForkError> {
        Self::seal_with(image, true)
    }
    fn seal_with(image: &SnapshotImage, prefer_huge: bool) -> Result<Self, MappedForkError> {
        let fill = |i: usize, page: &mut [u8; crate::PAGE_BYTES]| match image
            .page_bytes(i)
            .ok_or(Error::UnknownFault)?
        {
            Some(bytes) => {
                page.copy_from_slice(bytes);
                Ok(true)
            }
            None => Ok(false),
        };
        let memfd = if prefer_huge {
            SealedMemfd::build_preferring_huge(image.page_count(), fill)?
        } else {
            SealedMemfd::build(image.page_count(), fill)?
        };
        Ok(Self {
            image: image.clone(),
            memfd: Arc::new(memfd),
        })
    }
    pub fn memfd(&self) -> &Arc<SealedMemfd> {
        &self.memfd
    }
    pub fn page_count(&self) -> usize {
        self.image.page_count()
    }
    /// A new clone mapping plus the ticket that must reseed it.
    pub fn fork_mapped(
        &self,
        authority: &mut ForkAuthority,
    ) -> Result<(MappedClone, SnapshotResumeTicket), MappedForkError> {
        let region = MinorRegion::map_private(&self.memfd)?;
        let ticket = self.image.issue_ticket(authority)?;
        let loc = self.image.genid_location();
        Ok((
            MappedClone {
                region,
                epoch: ticket.fork_epoch_id(),
                generation: ticket.generation(),
                reseeded: false,
                genid_offset: loc.page * PAGE_BYTES + loc.offset,
                host_tsc_khz: self.image.host_tsc_khz(),
                virtio_mmio_count: self.image.virtio_mmio_count(),
            },
            ticket,
        ))
    }
}

/// One clone's private mapping of a [`SealedSnapshot`].
#[derive(Debug)]
pub struct MappedClone {
    region: MinorRegion,
    epoch: u64,
    generation: VmGenId,
    reseeded: bool,
    genid_offset: usize,
    host_tsc_khz: u32,
    virtio_mmio_count: u32,
}

impl MappedClone {
    pub fn fork_epoch_id(&self) -> u64 {
        self.epoch
    }
    pub fn generation(&self) -> VmGenId {
        self.generation
    }
    pub fn is_reseeded(&self) -> bool {
        self.reseeded
    }
    pub fn page_count(&self) -> usize {
        self.region.page_count()
    }
    /// See [`MinorRegion::frame_number`].
    pub fn frame_number(&self, index: usize) -> Option<u64> {
        self.region.frame_number(index)
    }
    /// Maps the hot set ahead of the guest (allowed before reseeding: it runs
    /// no guest code and writes nothing).
    pub fn prefetch(&self, hot: &HotSet) -> Result<UffdStats, MappedForkError> {
        Ok(self.region.prefetch(hot)?)
    }
    /// Runs `touch` as the guest, serving minor faults. Refused until reseeded.
    pub fn serve<R, F>(
        &self,
        fault_budget: u64,
        touch: F,
    ) -> Result<(R, UffdStats), MappedForkError>
    where
        F: FnOnce(&Toucher<'_>) -> R,
    {
        self.serve_recording(fault_budget, touch)
            .map(|(r, s, _)| (r, s))
    }
    pub fn serve_recording<R, F>(
        &self,
        fault_budget: u64,
        touch: F,
    ) -> Result<(R, UffdStats, Vec<usize>), MappedForkError>
    where
        F: FnOnce(&Toucher<'_>) -> R,
    {
        if !self.reseeded {
            return Err(MappedForkError::NotReseeded);
        }
        Ok(self.region.serve_recording(fault_budget, touch)?)
    }
}

impl SnapshotResumeTicket {
    /// `AetherVm::reseed_after_fork` for a mapped clone: writes the clone's
    /// generation block into its private mapping (copy-on-write; the sealed
    /// memfd and sibling clones are unchanged) and mints its sealer. A ticket
    /// for another clone, or a failed write, hands the ticket back.
    pub fn reseed_mapped(
        self,
        master_key: &RootKey,
        clone: &mut MappedClone,
        entropy: CloneEntropy,
    ) -> Result<ReseededVaultEpoch, Box<(MappedForkError, SnapshotResumeTicket)>> {
        if clone.epoch != self.fork_epoch_id() || clone.generation != self.generation() {
            return Err(Box::new((
                MappedForkError::Fork(ForkError::TicketMismatch),
                self,
            )));
        }
        if clone.reseeded {
            return Err(Box::new((
                MappedForkError::Fork(ForkError::AlreadyReseeded),
                self,
            )));
        }
        let wire = self
            .wire(clone.host_tsc_khz, clone.virtio_mmio_count)
            .encode();
        let at = clone.genid_offset;
        let written = clone.region.serve(4, |t| {
            t.write_bytes(at, &wire) && read_wire(t, at) == Some(wire)
        });
        match written {
            Ok((true, _)) => {}
            Ok((false, _)) => {
                return Err(Box::new((
                    MappedForkError::Fork(ForkError::InvalidLayout),
                    self,
                )));
            }
            Err(e) => return Err(Box::new((MappedForkError::Uffd(e), self))),
        }
        clone.reseeded = true;
        Ok(self.discharge(master_key, entropy))
    }
}

fn read_wire(t: &Toucher<'_>, at: usize) -> Option<[u8; VMGENID_WIRE_BYTES]> {
    let mut b = [0u8; VMGENID_WIRE_BYTES];
    for (i, slot) in b.iter_mut().enumerate() {
        *slot = t.read_u8(at + i)?;
    }
    Some(b)
}

/// The generation block a mapped clone's guest reads.
pub fn read_genid(t: &Toucher<'_>, clone: &MappedClone) -> Option<VmGenIdWire> {
    VmGenIdWire::decode(&read_wire(t, clone.genid_offset)?).ok()
}
