//! Copy-on-write snapshot fork with per-clone nonce reseeding (§37.2, §37.3).
//!
//! Host model of the `AetherVm` linear effect:
//!
//! - [`quiesce_and_snapshot`] consumes the running VM's [`Sealer`]
//!   (`sink old_sealer`) and freezes its memory into a [`SnapshotImage`].
//! - [`SnapshotImage::fork`] makes a [`ForkClone`] that shares every snapshot
//!   page until its first write, plus a [`SnapshotResumeTicket`] carrying a
//!   fresh fork epoch and VM generation id.
//! - Only [`SnapshotResumeTicket::reseed_after_fork`] produces a new
//!   [`Sealer`] for a clone. It writes the 24-byte `AetherVmGenIdWire` block
//!   into the clone's memory and derives the clone's key from the root key,
//!   generation id and epoch. A clone's pages cannot be served to a lazy-restore
//!   server before it has been reseeded ([`crate::Error::NotReseeded`]).
//!
//! Nonces are `fork_epoch_id (LE u64) || counter (LE u32)`. A [`ForkAuthority`]
//! never issues the same epoch twice, so two sealers from one authority never
//! produce the same 96-bit nonce, independently of the derived keys; that is the
//! property `lean/AetherMemory.lean` proves about its model. Key derivation
//! (BLAKE3 `derive_key`) adds key separation; no injectivity is assumed of it.
//!
//! `Sealer` and `SnapshotResumeTicket` are linear obligations: non-Clone,
//! `#[must_use]`, left only by their documented consuming calls. Dropping an
//! armed one increments [`leaked_obligations`]. Rust ownership here is not
//! evidence that the Vesper source typing rules for these types are implemented.
#![forbid(unsafe_code)]

use crate::page_source::{PageFill, PageSource};
use crate::{Error, PAGE_BYTES};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub type Page = [u8; PAGE_BYTES];
/// Size of `AetherVmGenIdWire` (§37.2).
pub const VMGENID_WIRE_BYTES: usize = 24;
/// Hard ceiling on snapshot pages (4 GiB of 4 KiB pages).
pub const MAX_SNAPSHOT_PAGES: usize = 1 << 20;
const KEY_CONTEXT: &str = "vesper.aether.fork-epoch/1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForkError {
    EmptyImage,
    Budget,
    InvalidLayout,
    OutOfRange,
    EpochExhausted,
    NonceExhausted,
    TicketMismatch,
    AlreadyReseeded,
    WrongLength,
}
impl std::fmt::Display for ForkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ForkError {}

/// 128-bit VM generation id: `lo` is the fork epoch, `hi` the authority instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VmGenId {
    pub lo: u64,
    pub hi: u64,
}

/// `wire struct AetherVmGenIdWire` (§37.2): four little-endian fields, 24 bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VmGenIdWire {
    pub vm_gen_lo: u64,
    pub vm_gen_hi: u64,
    pub host_tsc_khz: u32,
    pub virtio_mmio_count: u32,
}
impl VmGenIdWire {
    pub fn encode(&self) -> [u8; VMGENID_WIRE_BYTES] {
        let mut out = [0u8; VMGENID_WIRE_BYTES];
        out[0..8].copy_from_slice(&self.vm_gen_lo.to_le_bytes());
        out[8..16].copy_from_slice(&self.vm_gen_hi.to_le_bytes());
        out[16..20].copy_from_slice(&self.host_tsc_khz.to_le_bytes());
        out[20..24].copy_from_slice(&self.virtio_mmio_count.to_le_bytes());
        out
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, ForkError> {
        let b: &[u8; VMGENID_WIRE_BYTES] = bytes.try_into().map_err(|_| ForkError::WrongLength)?;
        let u64_at = |i: usize| {
            let mut w = [0u8; 8];
            w.copy_from_slice(&b[i..i + 8]);
            u64::from_le_bytes(w)
        };
        let u32_at = |i: usize| {
            let mut w = [0u8; 4];
            w.copy_from_slice(&b[i..i + 4]);
            u32::from_le_bytes(w)
        };
        Ok(Self {
            vm_gen_lo: u64_at(0),
            vm_gen_hi: u64_at(8),
            host_tsc_khz: u32_at(16),
            virtio_mmio_count: u32_at(20),
        })
    }
}

/// Root key material (`AeadKeySlot`); never printed or cloned.
pub struct RootKey([u8; 32]);
impl RootKey {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}
impl std::fmt::Debug for RootKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RootKey(..)")
    }
}
impl Drop for RootKey {
    fn drop(&mut self) {
        self.0 = [0; 32];
        std::hint::black_box(&self.0);
    }
}

static LEAKED_SEALERS: AtomicU64 = AtomicU64::new(0);
static LEAKED_TICKETS: AtomicU64 = AtomicU64::new(0);

/// Process-wide counts of armed obligations dropped without their consuming call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeakCounts {
    pub sealers: u64,
    pub tickets: u64,
}
pub fn leaked_obligations() -> LeakCounts {
    LeakCounts {
        sealers: LEAKED_SEALERS.load(Ordering::SeqCst),
        tickets: LEAKED_TICKETS.load(Ordering::SeqCst),
    }
}

fn derive_key(root: &RootKey, generation: VmGenId, epoch: u64) -> [u8; 32] {
    let mut material = [0u8; 56];
    material[..32].copy_from_slice(&root.0);
    material[32..40].copy_from_slice(&generation.lo.to_le_bytes());
    material[40..48].copy_from_slice(&generation.hi.to_le_bytes());
    material[48..56].copy_from_slice(&epoch.to_le_bytes());
    let key = blake3::derive_key(KEY_CONTEXT, &material);
    material = [0; 56];
    std::hint::black_box(&material);
    key
}

/// Nonce state of one `AeadSealer` epoch (linear).
#[must_use = "a Sealer must be consumed by quiesce_and_snapshot or retire"]
pub struct Sealer {
    key: [u8; 32],
    epoch: u64,
    generation: VmGenId,
    next: u64,
    armed: bool,
}
impl std::fmt::Debug for Sealer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sealer")
            .field("epoch", &self.epoch)
            .field("generation", &self.generation)
            .field("next", &self.next)
            .finish_non_exhaustive()
    }
}
impl Sealer {
    fn new(root: &RootKey, epoch: u64, generation: VmGenId) -> Self {
        Self {
            key: derive_key(root, generation, epoch),
            epoch,
            generation,
            next: 0,
            armed: true,
        }
    }
    pub fn fork_epoch_id(&self) -> u64 {
        self.epoch
    }
    pub fn generation(&self) -> VmGenId {
        self.generation
    }
    /// BLAKE3 hash of the epoch key, for comparing keys without exposing them.
    pub fn key_id(&self) -> [u8; 32] {
        *blake3::hash(&self.key).as_bytes()
    }
    /// 96-bit nonce `epoch || counter`; 2^32 nonces per epoch, then an error.
    pub fn next_nonce(&mut self) -> Result<[u8; 12], ForkError> {
        let counter = u32::try_from(self.next).map_err(|_| ForkError::NonceExhausted)?;
        self.next += 1;
        let mut nonce = [0u8; 12];
        nonce[..8].copy_from_slice(&self.epoch.to_le_bytes());
        nonce[8..].copy_from_slice(&counter.to_le_bytes());
        Ok(nonce)
    }
    /// Consumes the sealer without snapshotting (VM shutdown).
    pub fn retire(mut self) {
        self.armed = false;
    }
    #[cfg(test)]
    pub(crate) fn set_counter_for_test(&mut self, next: u64) {
        self.next = next;
    }
}
impl Drop for Sealer {
    fn drop(&mut self) {
        if self.armed {
            LEAKED_SEALERS.fetch_add(1, Ordering::SeqCst);
        }
        self.key = [0; 32];
        std::hint::black_box(&self.key);
    }
}

/// Issues fork epochs; each epoch is issued at most once. `instance` should be
/// random per host boot so that generation ids also differ across hosts; the
/// nonce-uniqueness guarantee holds only among sealers of one authority.
#[derive(Debug)]
pub struct ForkAuthority {
    instance: u64,
    next_epoch: u64,
    last_epoch: u64,
}
impl ForkAuthority {
    pub fn new(instance: u64) -> Self {
        Self::with_epoch_limit(instance, u64::MAX)
    }
    /// Issues epochs `1..=last_epoch` and then fails with `EpochExhausted`.
    pub fn with_epoch_limit(instance: u64, last_epoch: u64) -> Self {
        Self {
            instance,
            next_epoch: 1,
            last_epoch,
        }
    }
    fn issue(&mut self) -> Result<(u64, VmGenId), ForkError> {
        let epoch = self.next_epoch;
        if epoch == 0 || epoch > self.last_epoch {
            return Err(ForkError::EpochExhausted);
        }
        // Wraps to 0 only after issuing u64::MAX; 0 is then refused above.
        self.next_epoch = epoch.wrapping_add(1);
        Ok((
            epoch,
            VmGenId {
                lo: epoch,
                hi: self.instance,
            },
        ))
    }
    /// The first sealer of a freshly booted (not forked) VM.
    pub fn boot_sealer(&mut self, root: &RootKey) -> Result<Sealer, ForkError> {
        let (epoch, generation) = self.issue()?;
        Ok(Sealer::new(root, epoch, generation))
    }
}

/// Where the guest finds its `AetherVmGenIdWire` block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VmGenIdLocation {
    pub page: usize,
    pub offset: usize,
}

#[derive(Debug)]
struct ImageInner {
    pages: Vec<Option<Arc<Page>>>,
    genid: VmGenIdLocation,
    host_tsc_khz: u32,
    virtio_mmio_count: u32,
    snapshot_generation: VmGenId,
}

/// Frozen guest memory; `None` pages are zero. Cheap to clone (shared).
#[derive(Clone, Debug)]
pub struct SnapshotImage {
    inner: Arc<ImageInner>,
}

/// Snapshot input: the quiesced VM's memory and device facts.
#[derive(Debug)]
pub struct QuiescedMemory {
    pub pages: Vec<Option<Box<Page>>>,
    pub genid: VmGenIdLocation,
    pub host_tsc_khz: u32,
    pub virtio_mmio_count: u32,
}

/// `AetherVm::quiesce_and_fork_snapshot`: consumes the VM's sealer. On a
/// malformed layout the sealer is handed back unconsumed with the error.
pub fn quiesce_and_snapshot(
    old_sealer: Sealer,
    memory: QuiescedMemory,
) -> Result<SnapshotImage, Box<(ForkError, Sealer)>> {
    let check = || {
        if memory.pages.is_empty() {
            return Err(ForkError::EmptyImage);
        }
        if memory.pages.len() > MAX_SNAPSHOT_PAGES {
            return Err(ForkError::Budget);
        }
        let end = memory
            .genid
            .offset
            .checked_add(VMGENID_WIRE_BYTES)
            .ok_or(ForkError::InvalidLayout)?;
        if memory.genid.page >= memory.pages.len() || end > PAGE_BYTES {
            return Err(ForkError::InvalidLayout);
        }
        Ok(())
    };
    if let Err(e) = check() {
        return Err(Box::new((e, old_sealer)));
    }
    let snapshot_generation = old_sealer.generation();
    let mut sealer = old_sealer;
    sealer.armed = false;
    drop(sealer);
    Ok(SnapshotImage {
        inner: Arc::new(ImageInner {
            pages: memory
                .pages
                .into_iter()
                .map(|p| p.map(|b| Arc::new(*b)))
                .collect(),
            genid: memory.genid,
            host_tsc_khz: memory.host_tsc_khz,
            virtio_mmio_count: memory.virtio_mmio_count,
            snapshot_generation,
        }),
    })
}

impl SnapshotImage {
    pub fn page_count(&self) -> usize {
        self.inner.pages.len()
    }
    /// Generation id of the VM that was snapshotted.
    pub fn snapshot_generation(&self) -> VmGenId {
        self.inner.snapshot_generation
    }
    /// Copy-on-write clone plus the ticket that must reseed it.
    pub fn fork(
        &self,
        authority: &mut ForkAuthority,
    ) -> Result<(ForkClone, SnapshotResumeTicket), ForkError> {
        let (epoch, generation) = authority.issue()?;
        Ok((
            ForkClone {
                image: Arc::clone(&self.inner),
                private: BTreeMap::new(),
                epoch,
                generation,
                reseeded: false,
            },
            SnapshotResumeTicket {
                epoch,
                generation,
                armed: true,
            },
        ))
    }
}

static ZERO_PAGE: Page = [0; PAGE_BYTES];

/// A forked guest memory: snapshot pages are shared until first write.
#[derive(Debug)]
pub struct ForkClone {
    image: Arc<ImageInner>,
    private: BTreeMap<usize, Box<Page>>,
    epoch: u64,
    generation: VmGenId,
    reseeded: bool,
}
impl ForkClone {
    pub fn page_count(&self) -> usize {
        self.image.pages.len()
    }
    pub fn fork_epoch_id(&self) -> u64 {
        self.epoch
    }
    pub fn generation(&self) -> VmGenId {
        self.generation
    }
    pub fn is_reseeded(&self) -> bool {
        self.reseeded
    }
    /// Number of pages this clone has copied.
    pub fn private_page_count(&self) -> usize {
        self.private.len()
    }
    /// True while page `index` is still the snapshot's (shared) page.
    pub fn is_shared(&self, index: usize) -> bool {
        !self.private.contains_key(&index)
    }
    /// Whether two clones share the same physical snapshot page allocation.
    pub fn shares_page_with(&self, other: &ForkClone, index: usize) -> bool {
        match (
            self.is_shared(index),
            other.is_shared(index),
            self.image.pages.get(index),
            other.image.pages.get(index),
        ) {
            (true, true, Some(Some(a)), Some(Some(b))) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
    pub fn page(&self, index: usize) -> Result<&Page, ForkError> {
        if let Some(p) = self.private.get(&index) {
            return Ok(p);
        }
        match self.image.pages.get(index) {
            Some(Some(p)) => Ok(p),
            Some(None) => Ok(&ZERO_PAGE),
            None => Err(ForkError::OutOfRange),
        }
    }
    fn page_mut(&mut self, index: usize) -> Result<&mut Page, ForkError> {
        let base = match self.image.pages.get(index) {
            Some(p) => p,
            None => return Err(ForkError::OutOfRange),
        };
        Ok(self.private.entry(index).or_insert_with(|| match base {
            Some(p) => Box::new(**p),
            None => Box::new([0; PAGE_BYTES]),
        }))
    }
    fn check_span(&self, offset: usize, len: usize) -> Result<(), ForkError> {
        let end = offset.checked_add(len).ok_or(ForkError::OutOfRange)?;
        let total = self
            .page_count()
            .checked_mul(PAGE_BYTES)
            .ok_or(ForkError::OutOfRange)?;
        if end > total {
            return Err(ForkError::OutOfRange);
        }
        Ok(())
    }
    /// Copy-on-write store; the whole span is checked before any page copies.
    pub fn write(&mut self, offset: usize, bytes: &[u8]) -> Result<(), ForkError> {
        self.check_span(offset, bytes.len())?;
        let mut done = 0;
        while done < bytes.len() {
            let at = offset + done;
            let (index, within) = (at / PAGE_BYTES, at % PAGE_BYTES);
            let n = (PAGE_BYTES - within).min(bytes.len() - done);
            self.page_mut(index)?[within..within + n].copy_from_slice(&bytes[done..done + n]);
            done += n;
        }
        Ok(())
    }
    pub fn read(&self, offset: usize, out: &mut [u8]) -> Result<(), ForkError> {
        self.check_span(offset, out.len())?;
        let mut done = 0;
        while done < out.len() {
            let at = offset + done;
            let (index, within) = (at / PAGE_BYTES, at % PAGE_BYTES);
            let n = (PAGE_BYTES - within).min(out.len() - done);
            out[done..done + n].copy_from_slice(&self.page(index)?[within..within + n]);
            done += n;
        }
        Ok(())
    }
    /// The generation block as the guest would read it.
    pub fn read_genid(&self) -> Result<VmGenIdWire, ForkError> {
        let at = self.image.genid.page * PAGE_BYTES + self.image.genid.offset;
        let mut b = [0u8; VMGENID_WIRE_BYTES];
        self.read(at, &mut b)?;
        VmGenIdWire::decode(&b)
    }
}

/// Lazy-restore view of a clone. Refused until the clone has been reseeded, so
/// no restored guest code can run with the snapshot's nonce state.
impl PageSource for ForkClone {
    fn page_count(&self) -> usize {
        self.image.pages.len()
    }
    fn fill(&self, index: usize) -> Result<PageFill<'_>, Error> {
        if !self.reseeded {
            return Err(Error::NotReseeded);
        }
        if let Some(p) = self.private.get(&index) {
            return Ok(PageFill::Borrowed(p));
        }
        match self.image.pages.get(index) {
            Some(Some(p)) => Ok(PageFill::Borrowed(p)),
            Some(None) => Ok(PageFill::Zero),
            None => Err(Error::UnknownFault),
        }
    }
}

/// `linear struct SnapshotResumeTicket` (§37.2).
#[must_use = "a SnapshotResumeTicket must be discharged by reseed_after_fork"]
#[derive(Debug)]
pub struct SnapshotResumeTicket {
    epoch: u64,
    generation: VmGenId,
    armed: bool,
}
/// `ReseededVaultEpoch` (§37.2).
#[must_use]
#[derive(Debug)]
pub struct ReseededVaultEpoch {
    pub fresh_sealer: Sealer,
    pub fork_epoch_id: u64,
}
impl SnapshotResumeTicket {
    pub fn generation(&self) -> VmGenId {
        self.generation
    }
    pub fn fork_epoch_id(&self) -> u64 {
        self.epoch
    }
    /// `AetherVm::reseed_after_fork`: writes the clone's generation block and
    /// mints its sealer. A ticket for another clone is handed back unconsumed.
    pub fn reseed_after_fork(
        mut self,
        master_key: &RootKey,
        clone: &mut ForkClone,
    ) -> Result<ReseededVaultEpoch, Box<(ForkError, SnapshotResumeTicket)>> {
        if clone.epoch != self.epoch || clone.generation != self.generation {
            return Err(Box::new((ForkError::TicketMismatch, self)));
        }
        if clone.reseeded {
            return Err(Box::new((ForkError::AlreadyReseeded, self)));
        }
        let wire = VmGenIdWire {
            vm_gen_lo: self.generation.lo,
            vm_gen_hi: self.generation.hi,
            host_tsc_khz: clone.image.host_tsc_khz,
            virtio_mmio_count: clone.image.virtio_mmio_count,
        };
        let at = clone.image.genid.page * PAGE_BYTES + clone.image.genid.offset;
        if let Err(e) = clone.write(at, &wire.encode()) {
            return Err(Box::new((e, self)));
        }
        clone.reseeded = true;
        self.armed = false;
        Ok(ReseededVaultEpoch {
            fresh_sealer: Sealer::new(master_key, self.epoch, self.generation),
            fork_epoch_id: self.epoch,
        })
    }
}
impl Drop for SnapshotResumeTicket {
    fn drop(&mut self) {
        if self.armed {
            LEAKED_TICKETS.fetch_add(1, Ordering::SeqCst);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonce_counter_exhaustion_is_typed() {
        let mut auth = ForkAuthority::new(9);
        let root = RootKey::new([1; 32]);
        let mut s = auth.boot_sealer(&root).unwrap();
        s.set_counter_for_test(u64::from(u32::MAX));
        let last = s.next_nonce().unwrap();
        assert_eq!(&last[8..], &u32::MAX.to_le_bytes());
        assert_eq!(s.next_nonce(), Err(ForkError::NonceExhausted));
        s.retire();
    }
}
