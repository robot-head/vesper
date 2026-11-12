//! Single-owner bounded host buffer pool. Handles are nonclone and explicit;
//! Rust can still drop/forget a pin, leaving a frame pinned until pool teardown.
//! This is not source-language linear exhaustion or a durable WAL protocol.
use crate::page::{PageError, SlottedPage};
use std::{
    collections::BTreeSet,
    fmt,
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolError {
    Limit,
    Full,
    DuplicatePage,
    UnknownPage,
    Pinned,
    Conflict,
    WrongPool,
    StalePin,
    InvalidLsn,
    Unmodified,
    Overflow,
    Page(PageError),
}
impl From<PageError> for PoolError {
    fn from(error: PageError) -> Self {
        Self::Page(error)
    }
}
impl fmt::Display for PoolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "pool {self:?}")
    }
}
impl std::error::Error for PoolError {}

/// Release failures return the exact supplied owner, including staged exclusive bytes.
#[derive(Debug)]
pub struct ReleaseError<P> {
    pub error: PoolError,
    pub pin: P,
}
impl<P: fmt::Debug> fmt::Display for ReleaseError<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}
impl<P: fmt::Debug> std::error::Error for ReleaseError<P> {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolLimits {
    pub frames: usize,
    pub pins: usize,
}
impl Default for PoolLimits {
    fn default() -> Self {
        Self {
            frames: 16,
            pins: 256,
        }
    }
}
impl PoolLimits {
    fn validate(self) -> Result<(), PoolError> {
        if self.frames == 0 || self.frames > 256 || self.pins == 0 || self.pins > 4096 {
            Err(PoolError::Limit)
        } else {
            Ok(())
        }
    }
}

/// An owned page plus the earliest unacknowledged dirty LSN. Eviction returns
/// this complete image; reinstalling it preserves dirty tracking.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageImage {
    page: SlottedPage,
    dirty_lsn: Option<u64>,
}
impl PageImage {
    pub fn clean(page: SlottedPage) -> Self {
        Self {
            page,
            dirty_lsn: None,
        }
    }
    pub fn page(&self) -> &SlottedPage {
        &self.page
    }
    pub fn dirty_lsn(&self) -> Option<u64> {
        self.dirty_lsn
    }
}

#[derive(Clone, Copy, Debug)]
struct PinKey {
    pool: u64,
    slot: usize,
    generation: u64,
    serial: u64,
    page_id: u64,
}

/// Nonclone shared owner. Release explicitly with `BufferPool::unpin_clean`.
/// ```compile_fail
/// use vesper_quarry::pool::SharedPin;
/// fn duplicate(pin: SharedPin) { let _copy = pin.clone(); }
/// ```
#[must_use = "release the shared pin explicitly"]
#[derive(Debug)]
pub struct SharedPin {
    key: PinKey,
}
impl SharedPin {
    pub fn page_id(&self) -> u64 {
        self.key.page_id
    }
}

/// Nonclone exclusive owner; mutations live in private staged page bytes until
/// successful dirty release. An abort discards them without changing the frame.
/// ```compile_fail
/// use vesper_quarry::pool::ExclusivePin;
/// fn duplicate(pin: ExclusivePin) { let _copy = pin.clone(); }
/// ```
#[must_use = "commit or abort the exclusive pin explicitly"]
#[derive(Debug)]
pub struct ExclusivePin {
    key: PinKey,
    staged: SlottedPage,
}
impl ExclusivePin {
    pub fn page_id(&self) -> u64 {
        self.key.page_id
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameInfo {
    pub page_id: u64,
    pub generation: u64,
    pub shared_pins: usize,
    pub exclusive: bool,
    pub page_lsn: u64,
    pub dirty_lsn: Option<u64>,
}
#[derive(Debug)]
struct Frame {
    image: PageImage,
    shared: BTreeSet<u64>,
    exclusive: Option<u64>,
}
#[derive(Debug)]
struct Slot {
    generation: u64,
    frame: Option<Frame>,
}
#[derive(Debug)]
pub struct BufferPool {
    owner: u64,
    slots: Vec<Slot>,
    limits: PoolLimits,
    pins: usize,
    next_serial: u64,
}
impl BufferPool {
    pub fn new(limits: PoolLimits) -> Result<Self, PoolError> {
        limits.validate()?;
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let mut owner = NEXT.load(Ordering::Relaxed);
        loop {
            let next = owner.checked_add(1).ok_or(PoolError::Overflow)?;
            match NEXT.compare_exchange_weak(owner, next, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => break,
                Err(observed) => owner = observed,
            }
        }
        Ok(Self {
            owner,
            slots: (0..limits.frames)
                .map(|_| Slot {
                    generation: 0,
                    frame: None,
                })
                .collect(),
            limits,
            pins: 0,
            next_serial: 0,
        })
    }
    pub fn capacity(&self) -> usize {
        self.slots.len()
    }
    pub fn resident_count(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.frame.is_some())
            .count()
    }
    pub fn pin_count(&self) -> usize {
        self.pins
    }
    /// Borrowed image survives all failures. There is no automatic eviction.
    pub fn install(&mut self, image: &PageImage) -> Result<(), PoolError> {
        let id = image.page.header().page_id;
        if self.find(id).is_ok() {
            return Err(PoolError::DuplicatePage);
        }
        let slot = self
            .slots
            .iter_mut()
            .find(|slot| slot.frame.is_none())
            .ok_or(PoolError::Full)?;
        let generation = slot.generation.checked_add(1).ok_or(PoolError::Overflow)?;
        slot.frame = Some(Frame {
            image: image.clone(),
            shared: BTreeSet::new(),
            exclusive: None,
        });
        slot.generation = generation;
        Ok(())
    }
    /// Returns dirty metadata with the actual owned page; pinned frames cannot
    /// be evicted. This operation neither writes nor acknowledges persistence.
    pub fn evict(&mut self, page_id: u64) -> Result<PageImage, PoolError> {
        let i = self.find(page_id)?;
        let frame = self.slots[i].frame.as_ref().ok_or(PoolError::UnknownPage)?;
        if !frame.shared.is_empty() || frame.exclusive.is_some() {
            return Err(PoolError::Pinned);
        }
        Ok(self.slots[i]
            .frame
            .take()
            .ok_or(PoolError::UnknownPage)?
            .image)
    }
    pub fn info(&self, page_id: u64) -> Result<FrameInfo, PoolError> {
        let i = self.find(page_id)?;
        let frame = self.slots[i].frame.as_ref().ok_or(PoolError::UnknownPage)?;
        Ok(FrameInfo {
            page_id,
            generation: self.slots[i].generation,
            shared_pins: frame.shared.len(),
            exclusive: frame.exclusive.is_some(),
            page_lsn: frame.image.page.header().page_lsn,
            dirty_lsn: frame.image.dirty_lsn,
        })
    }
    pub fn fix_shared(&mut self, page_id: u64) -> Result<SharedPin, PoolError> {
        let i = self.find(page_id)?;
        if self.slots[i]
            .frame
            .as_ref()
            .ok_or(PoolError::UnknownPage)?
            .exclusive
            .is_some()
        {
            return Err(PoolError::Conflict);
        }
        let key = self.next_key(i, page_id)?;
        let pins = self.pins.checked_add(1).ok_or(PoolError::Overflow)?;
        self.slots[i]
            .frame
            .as_mut()
            .ok_or(PoolError::UnknownPage)?
            .shared
            .insert(key.serial);
        self.next_serial = key.serial;
        self.pins = pins;
        Ok(SharedPin { key })
    }
    pub fn fix_exclusive(&mut self, page_id: u64) -> Result<ExclusivePin, PoolError> {
        let i = self.find(page_id)?;
        let frame = self.slots[i].frame.as_ref().ok_or(PoolError::UnknownPage)?;
        if !frame.shared.is_empty() || frame.exclusive.is_some() {
            return Err(PoolError::Conflict);
        }
        let key = self.next_key(i, page_id)?;
        let pins = self.pins.checked_add(1).ok_or(PoolError::Overflow)?;
        let staged = frame.image.page.clone();
        self.slots[i]
            .frame
            .as_mut()
            .ok_or(PoolError::UnknownPage)?
            .exclusive = Some(key.serial);
        self.next_serial = key.serial;
        self.pins = pins;
        Ok(ExclusivePin { key, staged })
    }
    pub fn view_shared(&self, pin: &SharedPin) -> Result<&SlottedPage, PoolError> {
        Ok(&self.shared_frame(pin)?.image.page)
    }
    pub fn view_exclusive<'a>(&self, pin: &'a ExclusivePin) -> Result<&'a SlottedPage, PoolError> {
        self.exclusive_frame(pin)?;
        Ok(&pin.staged)
    }
    pub fn insert(&self, pin: &mut ExclusivePin, record: &[u8]) -> Result<u16, PoolError> {
        self.exclusive_frame(pin)?;
        Ok(pin.staged.insert(record)?)
    }
    pub fn update(
        &self,
        pin: &mut ExclusivePin,
        slot: u16,
        record: &[u8],
    ) -> Result<(), PoolError> {
        self.exclusive_frame(pin)?;
        Ok(pin.staged.update(slot, record)?)
    }
    pub fn unpin_clean(&mut self, pin: SharedPin) -> Result<(), ReleaseError<SharedPin>> {
        let preflight = self
            .shared_frame(&pin)
            .and_then(|_| self.pins.checked_sub(1).ok_or(PoolError::Overflow));
        let remaining = match preflight {
            Ok(n) => n,
            Err(error) => return Err(ReleaseError { error, pin }),
        };
        self.slots[pin.key.slot]
            .frame
            .as_mut()
            .expect("validated frame")
            .shared
            .remove(&pin.key.serial);
        self.pins = remaining;
        Ok(())
    }
    /// Requires actual changed bytes and a nonzero LSN strictly newer than the
    /// committed page LSN. `dirty_lsn` retains the first outstanding change.
    /// This checks sequencing only; no WAL record/durability receipt is verified.
    pub fn unpin_dirty(
        &mut self,
        mut pin: ExclusivePin,
        wal_lsn: u64,
    ) -> Result<(), ReleaseError<ExclusivePin>> {
        let preflight = (|| {
            let frame = self.exclusive_frame(&pin)?;
            if wal_lsn == 0 || wal_lsn <= frame.image.page.header().page_lsn {
                return Err(PoolError::InvalidLsn);
            }
            if pin.staged == frame.image.page {
                return Err(PoolError::Unmodified);
            }
            let remaining = self.pins.checked_sub(1).ok_or(PoolError::Overflow)?;
            Ok((remaining, frame.image.dirty_lsn.unwrap_or(wal_lsn)))
        })();
        let (remaining, first_lsn) = match preflight {
            Ok(x) => x,
            Err(error) => return Err(ReleaseError { error, pin }),
        };
        pin.staged.set_lsn(wal_lsn);
        let frame = self.slots[pin.key.slot]
            .frame
            .as_mut()
            .expect("validated frame");
        frame.image = PageImage {
            page: pin.staged,
            dirty_lsn: Some(first_lsn),
        };
        frame.exclusive = None;
        self.pins = remaining;
        Ok(())
    }
    pub fn unpin_aborted(&mut self, pin: ExclusivePin) -> Result<(), ReleaseError<ExclusivePin>> {
        let preflight = self
            .exclusive_frame(&pin)
            .and_then(|_| self.pins.checked_sub(1).ok_or(PoolError::Overflow));
        let remaining = match preflight {
            Ok(n) => n,
            Err(error) => return Err(ReleaseError { error, pin }),
        };
        self.slots[pin.key.slot]
            .frame
            .as_mut()
            .expect("validated frame")
            .exclusive = None;
        self.pins = remaining;
        Ok(())
    }
    fn find(&self, page_id: u64) -> Result<usize, PoolError> {
        self.slots
            .iter()
            .position(|slot| {
                slot.frame
                    .as_ref()
                    .is_some_and(|frame| frame.image.page.header().page_id == page_id)
            })
            .ok_or(PoolError::UnknownPage)
    }
    fn next_key(&self, i: usize, page_id: u64) -> Result<PinKey, PoolError> {
        if self.pins >= self.limits.pins {
            return Err(PoolError::Limit);
        }
        let serial = self.next_serial.checked_add(1).ok_or(PoolError::Overflow)?;
        Ok(PinKey {
            pool: self.owner,
            slot: i,
            generation: self.slots[i].generation,
            serial,
            page_id,
        })
    }
    fn frame(&self, key: PinKey) -> Result<&Frame, PoolError> {
        if key.pool != self.owner {
            return Err(PoolError::WrongPool);
        }
        let slot = self.slots.get(key.slot).ok_or(PoolError::StalePin)?;
        if slot.generation != key.generation {
            return Err(PoolError::StalePin);
        }
        let frame = slot.frame.as_ref().ok_or(PoolError::StalePin)?;
        if frame.image.page.header().page_id != key.page_id {
            return Err(PoolError::StalePin);
        }
        Ok(frame)
    }
    fn shared_frame(&self, pin: &SharedPin) -> Result<&Frame, PoolError> {
        let frame = self.frame(pin.key)?;
        if !frame.shared.contains(&pin.key.serial) {
            return Err(PoolError::StalePin);
        }
        Ok(frame)
    }
    fn exclusive_frame(&self, pin: &ExclusivePin) -> Result<&Frame, PoolError> {
        let frame = self.frame(pin.key)?;
        if frame.exclusive != Some(pin.key.serial) {
            return Err(PoolError::StalePin);
        }
        Ok(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pool() -> BufferPool {
        BufferPool::new(PoolLimits { frames: 1, pins: 4 }).unwrap()
    }
    #[test]
    fn stale_active_ids_and_reused_slot_generations_cannot_release_live_pins() {
        let mut pool = pool();
        let image = PageImage::clean(SlottedPage::new(7, 0));
        pool.install(&image).unwrap();
        let old = pool.fix_shared(7).unwrap();
        let key = old.key;
        pool.unpin_clean(old).unwrap();
        let live = pool.fix_shared(7).unwrap();
        let stale = SharedPin { key };
        assert_eq!(pool.view_shared(&stale).unwrap_err(), PoolError::StalePin);
        let error = pool.unpin_clean(stale).unwrap_err();
        assert_eq!(error.error, PoolError::StalePin);
        assert_eq!(pool.pin_count(), 1);
        pool.unpin_clean(live).unwrap();
        pool.evict(7).unwrap();
        pool.install(&image).unwrap();
        let live = pool.fix_exclusive(7).unwrap();
        let stale = ExclusivePin {
            key,
            staged: image.page.clone(),
        };
        let error = pool.unpin_aborted(stale).unwrap_err();
        assert_eq!(error.error, PoolError::StalePin);
        assert_eq!(pool.pin_count(), 1);
        pool.unpin_aborted(live).unwrap();
        let w = pool.fix_exclusive(7).unwrap();
        let key = w.key;
        let staged = w.staged.clone();
        pool.unpin_aborted(w).unwrap();
        let live = pool.fix_exclusive(7).unwrap();
        let error = pool
            .unpin_dirty(ExclusivePin { key, staged }, 1)
            .unwrap_err();
        assert_eq!(error.error, PoolError::StalePin);
        assert_eq!(pool.pin_count(), 1);
        pool.unpin_aborted(live).unwrap();
    }
    #[test]
    fn generation_and_pin_serial_exhaustion_leave_state_and_input_unchanged() {
        let mut pool = pool();
        let image = PageImage::clean(SlottedPage::new(1, 0));
        pool.slots[0].generation = u64::MAX;
        assert_eq!(pool.install(&image).unwrap_err(), PoolError::Overflow);
        assert_eq!(pool.resident_count(), 0);
        assert_eq!(image.page.header().page_id, 1);
        pool.slots[0].generation = 0;
        pool.install(&image).unwrap();
        pool.next_serial = u64::MAX;
        assert_eq!(pool.fix_shared(1).unwrap_err(), PoolError::Overflow);
        assert_eq!(pool.fix_exclusive(1).unwrap_err(), PoolError::Overflow);
        assert_eq!(pool.pin_count(), 0);
        assert_eq!(pool.evict(1).unwrap(), image);
    }
}
