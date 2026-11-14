//! The store's buffer pool: resident 16 KiB frames behind the §24.2 pin
//! protocol (`fix_shared`/`unpin_clean`, `fix_exclusive`/`unpin_dirty`/
//! `unpin_aborted`), LRU eviction of unpinned frames, and dirty write-back
//! that obeys the WAL rule.
use super::error::{corrupt, StoreError, StoreErrorKind, StoreResult};
use super::log::StoreLog;
use super::pages::{check_disk, new_page, page_lsn, seal, set_page_lsn, Page};
use crate::device::{Device, DeviceError, FileId};
use crate::page::PAGE_SIZE;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

struct Frame {
    bytes: Box<Page>,
    dirty: bool,
    shared: u32,
    exclusive: bool,
    stage: Option<Box<Page>>,
    used: u64,
}

/// A shared (reader) pin. Its only exit is [`FramePool::unpin_clean`];
/// dropping it instead is counted as a leaked pin and the frame stays pinned.
#[must_use = "release the pin with unpin_clean"]
pub(crate) struct SharedFrame {
    page: u64,
    pool: u64,
    released: bool,
    leaks: Arc<AtomicU64>,
}
/// An exclusive (writer) pin over a private stage. Its exits are
/// [`FramePool::unpin_dirty`] and [`FramePool::unpin_aborted`].
#[must_use = "release the pin with unpin_dirty or unpin_aborted"]
pub(crate) struct ExclusiveFrame {
    page: u64,
    pool: u64,
    released: bool,
    leaks: Arc<AtomicU64>,
}
impl Drop for SharedFrame {
    fn drop(&mut self) {
        if !self.released {
            self.leaks.fetch_add(1, Ordering::Relaxed);
        }
    }
}
impl Drop for ExclusiveFrame {
    fn drop(&mut self) {
        if !self.released {
            self.leaks.fetch_add(1, Ordering::Relaxed);
        }
    }
}

pub(crate) struct FramePool {
    id: u64,
    frames: BTreeMap<u64, Frame>,
    capacity: usize,
    tick: u64,
    pinned: usize,
    leaks: Arc<AtomicU64>,
}
fn pin_error(msg: &str) -> StoreError {
    StoreError::new(StoreErrorKind::State, format!("pin protocol: {msg}"))
}
impl FramePool {
    pub fn new(capacity: usize) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            frames: BTreeMap::new(),
            capacity,
            tick: 0,
            pinned: 0,
            leaks: Arc::new(AtomicU64::new(0)),
        }
    }
    pub fn pinned(&self) -> usize {
        self.pinned
    }
    pub fn leaked(&self) -> u64 {
        self.leaks.load(Ordering::Relaxed)
    }
    pub fn resident(&self, page: u64) -> bool {
        self.frames.contains_key(&page)
    }
    pub fn len(&self) -> usize {
        self.frames.len()
    }
    pub fn full(&self) -> bool {
        self.frames.len() >= self.capacity
    }
    pub fn dirty(&self, page: u64) -> bool {
        self.frames.get(&page).is_some_and(|f| f.dirty)
    }
    pub fn dirty_pages(&self) -> Vec<u64> {
        self.frames
            .iter()
            .filter(|(_, f)| f.dirty)
            .map(|(&p, _)| p)
            .collect()
    }
    fn touch(&mut self, page: u64) {
        self.tick += 1;
        if let Some(f) = self.frames.get_mut(&page) {
            f.used = self.tick;
        }
    }
    /// Least recently used unpinned frame.
    pub fn victim(&self) -> Option<u64> {
        self.frames
            .iter()
            .filter(|(_, f)| f.shared == 0 && !f.exclusive)
            .min_by_key(|(_, f)| f.used)
            .map(|(&p, _)| p)
    }
    /// Installs a clean frame (the caller made room).
    pub fn insert(&mut self, page: u64, bytes: Box<Page>, dirty: bool) {
        debug_assert!(!self.full() && !self.resident(page));
        self.tick += 1;
        self.frames.insert(
            page,
            Frame {
                bytes,
                dirty,
                shared: 0,
                exclusive: false,
                stage: None,
                used: self.tick,
            },
        );
    }
    /// Removes an unpinned frame, returning its bytes and dirtiness.
    pub fn take(&mut self, page: u64) -> StoreResult<(Box<Page>, bool)> {
        match self.frames.get(&page) {
            Some(f) if f.shared == 0 && !f.exclusive => {
                let f = self.frames.remove(&page).expect("present");
                Ok((f.bytes, f.dirty))
            }
            Some(_) => Err(pin_error("evicting a pinned frame")),
            None => Err(pin_error("evicting a page that is not resident")),
        }
    }
    pub fn mark_clean(&mut self, page: u64) {
        if let Some(f) = self.frames.get_mut(&page) {
            f.dirty = false;
        }
    }
    pub fn fix_shared(&mut self, page: u64) -> StoreResult<SharedFrame> {
        let f = self
            .frames
            .get_mut(&page)
            .ok_or_else(|| pin_error("page not resident"))?;
        if f.exclusive {
            return Err(pin_error("page is exclusively pinned"));
        }
        f.shared += 1;
        self.pinned += 1;
        self.touch(page);
        Ok(SharedFrame {
            page,
            pool: self.id,
            released: false,
            leaks: Arc::clone(&self.leaks),
        })
    }
    pub fn view(&self, pin: &SharedFrame) -> StoreResult<&Page> {
        if pin.pool != self.id {
            return Err(pin_error("pin of another pool"));
        }
        self.frames
            .get(&pin.page)
            .map(|f| &*f.bytes)
            .ok_or_else(|| pin_error("pinned page vanished"))
    }
    pub fn unpin_clean(&mut self, mut pin: SharedFrame) -> StoreResult<()> {
        if pin.pool != self.id {
            return Err(pin_error("pin of another pool"));
        }
        let f = self
            .frames
            .get_mut(&pin.page)
            .ok_or_else(|| pin_error("pinned page vanished"))?;
        f.shared -= 1;
        self.pinned -= 1;
        pin.released = true;
        Ok(())
    }
    pub fn fix_exclusive(&mut self, page: u64) -> StoreResult<ExclusiveFrame> {
        let f = self
            .frames
            .get_mut(&page)
            .ok_or_else(|| pin_error("page not resident"))?;
        if f.exclusive || f.shared > 0 {
            return Err(pin_error("page is pinned"));
        }
        f.exclusive = true;
        f.stage = Some(f.bytes.clone());
        self.pinned += 1;
        self.touch(page);
        Ok(ExclusiveFrame {
            page,
            pool: self.id,
            released: false,
            leaks: Arc::clone(&self.leaks),
        })
    }
    pub fn stage(&mut self, pin: &mut ExclusiveFrame) -> StoreResult<&mut Page> {
        if pin.pool != self.id {
            return Err(pin_error("pin of another pool"));
        }
        self.frames
            .get_mut(&pin.page)
            .and_then(|f| f.stage.as_deref_mut())
            .ok_or_else(|| pin_error("pinned page vanished"))
    }
    /// Publishes the stage as the page at `lsn` (which must advance the page
    /// LSN) and releases the pin. On error the stage is discarded and the
    /// pin released anyway, so no frame stays pinned.
    pub fn unpin_dirty(&mut self, mut pin: ExclusiveFrame, lsn: u64) -> StoreResult<()> {
        if pin.pool != self.id {
            return Err(pin_error("pin of another pool"));
        }
        pin.released = true;
        let f = self
            .frames
            .get_mut(&pin.page)
            .ok_or_else(|| pin_error("pinned page vanished"))?;
        self.pinned -= 1;
        f.exclusive = false;
        let mut stage = f.stage.take().ok_or_else(|| pin_error("missing stage"))?;
        if lsn <= page_lsn(&f.bytes[..]) && page_lsn(&f.bytes[..]) != 0 {
            return Err(pin_error("dirty release must advance the page LSN"));
        }
        set_page_lsn(&mut stage[..], lsn);
        f.bytes = stage;
        f.dirty = true;
        Ok(())
    }
    pub fn unpin_aborted(&mut self, mut pin: ExclusiveFrame) -> StoreResult<()> {
        if pin.pool != self.id {
            return Err(pin_error("pin of another pool"));
        }
        pin.released = true;
        let f = self
            .frames
            .get_mut(&pin.page)
            .ok_or_else(|| pin_error("pinned page vanished"))?;
        self.pinned -= 1;
        f.exclusive = false;
        f.stage = None;
        Ok(())
    }
}

/// Device, log and pool of one store, with residency and write-back.
pub(crate) struct Core<D: Device> {
    pub dev: D,
    pub log: StoreLog,
    pub pool: FramePool,
    /// Pages in use (meta page's count; may exceed the data file length).
    pub page_count: u64,
    pub evictions: u64,
    pub page_writes: u64,
}
impl<D: Device> Core<D> {
    /// Reads page `id` from the data file. `Ok(None)`: missing (past the end
    /// of the file). Invalid pages are `Corrupt`.
    pub fn read_disk(&mut self, id: u64) -> StoreResult<Option<Box<Page>>> {
        let mut b = new_page();
        match self
            .dev
            .read_at(FileId::Data, id * PAGE_SIZE as u64, &mut b[..])
        {
            Ok(()) => {}
            Err(DeviceError::ShortRead) => return Ok(None),
            Err(e) => return Err(e.into()),
        }
        check_disk(&b, id)?;
        Ok(Some(b))
    }
    pub fn make_room(&mut self) -> StoreResult<()> {
        while self.pool.full() {
            let victim = self
                .pool
                .victim()
                .ok_or_else(|| StoreError::new(StoreErrorKind::Limit, "every frame is pinned"))?;
            self.evict(victim)?;
        }
        Ok(())
    }
    /// Writes a dirty page after flushing the log through its LSN, then
    /// drops the frame.
    pub fn evict(&mut self, page: u64) -> StoreResult<()> {
        self.write_back(page)?;
        self.pool.take(page)?;
        self.evictions += 1;
        Ok(())
    }
    /// Writes a resident dirty page (the WAL rule first) and marks it clean.
    pub fn write_back(&mut self, page: u64) -> StoreResult<()> {
        if !self.pool.dirty(page) {
            return Ok(());
        }
        let pin = self.pool.fix_shared(page)?;
        let copy = self.pool.view(&pin).map(|b| Box::new(*b));
        self.pool.unpin_clean(pin)?;
        let mut copy = copy?;
        self.log.flush_to(&mut self.dev, page_lsn(&copy[..]))?;
        seal(&mut copy);
        self.dev
            .write_at(FileId::Data, page * PAGE_SIZE as u64, &copy[..])?;
        self.pool.mark_clean(page);
        self.page_writes += 1;
        Ok(())
    }
    /// Makes `page` resident, reading and validating it from the device.
    pub fn load(&mut self, page: u64) -> StoreResult<()> {
        if self.pool.resident(page) {
            return Ok(());
        }
        if page >= self.page_count {
            return Err(corrupt(format!("page {page} is beyond the store")));
        }
        let bytes = self
            .read_disk(page)?
            .ok_or_else(|| corrupt(format!("page {page} is missing from the data file")))?;
        self.make_room()?;
        self.pool.insert(page, bytes, false);
        Ok(())
    }
    /// A copy of a resident or loadable page, read under a shared pin.
    pub fn copy(&mut self, page: u64) -> StoreResult<Box<Page>> {
        self.load(page)?;
        let pin = self.pool.fix_shared(page)?;
        let copy = self.pool.view(&pin).map(|b| Box::new(*b));
        self.pool.unpin_clean(pin)?;
        copy
    }
    /// Publishes new page bytes at `lsn` under an exclusive pin. A page that
    /// is not resident (new, or evicted) gets a frame first.
    pub fn install(&mut self, page: u64, bytes: &Page, lsn: u64) -> StoreResult<()> {
        if !self.pool.resident(page) {
            self.make_room()?;
            // Its previous contents are irrelevant: the stage replaces them.
            self.pool.insert(page, new_page(), false);
        }
        let mut pin = self.pool.fix_exclusive(page)?;
        match self.pool.stage(&mut pin) {
            Ok(stage) => stage.copy_from_slice(bytes),
            Err(e) => {
                self.pool.unpin_aborted(pin)?;
                return Err(e);
            }
        }
        self.pool.unpin_dirty(pin, lsn)
    }
}
