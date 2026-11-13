//! Page sources a lazy-restore server fills missing pages from.
#![forbid(unsafe_code)]

use crate::{Error, PAGE_BYTES, PAGE_SIZE_4K, SnapshotMap, UffdPageAction};
use vesper_loom_cas::Store;

/// How one missing 4 KiB page is filled: `UFFDIO_ZEROPAGE` or `UFFDIO_COPY`.
#[derive(Debug)]
pub enum PageFill<'a> {
    Zero,
    Borrowed(&'a [u8; PAGE_BYTES]),
    Owned(Box<[u8; PAGE_BYTES]>),
}
impl PageFill<'_> {
    /// `None` for a zero page.
    pub fn bytes(&self) -> Option<&[u8; PAGE_BYTES]> {
        match self {
            Self::Zero => None,
            Self::Borrowed(b) => Some(b),
            Self::Owned(b) => Some(b),
        }
    }
}

/// A region of `page_count()` pages, indexed from zero.
pub trait PageSource {
    fn page_count(&self) -> usize;
    /// Out-of-range indices and unreadable pages are errors, never zero fills.
    fn fill(&self, index: usize) -> Result<PageFill<'_>, Error>;
}

/// One VM slot of a validated [`SnapshotMap`], read from the CAS store. Every
/// page read re-verifies its blob's BLAKE3 identity (see [`SnapshotMap::resolve`]).
/// Shared read-only CAS pages are also copied: zero-copy `UFFDIO_CONTINUE`
/// over a shared memfd is not implemented.
#[derive(Debug)]
pub struct CasSlotSource<'a> {
    map: &'a SnapshotMap,
    store: &'a Store,
    vm_id: u64,
    guest_start: u64,
    pages: usize,
}
impl SnapshotMap {
    pub fn slot_source<'a>(
        &'a self,
        store: &'a Store,
        vm_id: u64,
        slot_id: u32,
    ) -> Result<CasSlotSource<'a>, Error> {
        let slot = self
            .slots
            .iter()
            .find(|s| s.slot.vm_id == vm_id && s.slot.slot_id == slot_id)
            .ok_or(Error::UnknownFault)?;
        Ok(CasSlotSource {
            map: self,
            store,
            vm_id,
            guest_start: slot.guest.start(),
            pages: slot.pages.len(),
        })
    }
}
impl PageSource for CasSlotSource<'_> {
    fn page_count(&self) -> usize {
        self.pages
    }
    fn fill(&self, index: usize) -> Result<PageFill<'_>, Error> {
        if index >= self.pages {
            return Err(Error::UnknownFault);
        }
        let delta = u64::try_from(index)
            .map_err(|_| Error::Overflow)?
            .checked_mul(PAGE_SIZE_4K)
            .ok_or(Error::Overflow)?;
        let gpa = self.guest_start.checked_add(delta).ok_or(Error::Overflow)?;
        let page = self.map.resolve(self.store, self.vm_id, gpa)?;
        Ok(match page.action() {
            UffdPageAction::ZeroPage4K => PageFill::Zero,
            _ => PageFill::Owned(page.into_bytes()),
        })
    }
}
