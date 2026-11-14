//! The store's buffer pool: resident 16 KiB frames shared by threads.
//!
//! Every frame has a *pin count* (kept under the pool mutex; a pinned frame
//! is never evicted) and a *latch* (a shared/exclusive lock that a thread
//! holds across calls, so readers can couple latches down the tree). The
//! §24.2 pin protocol is the pair: [`Core::fix_shared`] pins and latches
//! shared, [`Core::unpin_clean`] releases both; installs pin and latch
//! exclusive. A waiting exclusive request blocks new shared requests, so a
//! writer is not starved by a stream of readers.
//!
//! Lock order (never acquired the other way round): store writer mutex,
//! store view lock, frame latches in tree order (higher levels first, then
//! leaves left to right), pool mutex, device/log mutex. The pool mutex is
//! never held while waiting for a latch.
use super::error::{corrupt, StoreError, StoreErrorKind, StoreResult};
use super::log::StoreLog;
use super::pages::{check_disk, new_page, page_lsn, seal, set_page_lsn, Page};
use crate::device::{Device, DeviceError, FileId};
use crate::page::PAGE_SIZE;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock};

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Default)]
struct LatchState {
    shared: u32,
    exclusive: bool,
    waiting: u32,
}
/// A logical shared/exclusive latch, held across calls (unlike a guard).
#[derive(Default)]
struct Latch {
    st: Mutex<LatchState>,
    cv: Condvar,
}
impl Latch {
    fn exclusive_now() -> Self {
        Self {
            st: Mutex::new(LatchState {
                exclusive: true,
                ..LatchState::default()
            }),
            cv: Condvar::new(),
        }
    }
    fn shared(&self) {
        let mut g = lock(&self.st);
        while g.exclusive || g.waiting > 0 {
            g = self.cv.wait(g).unwrap_or_else(|e| e.into_inner());
        }
        g.shared += 1;
    }
    fn exclusive(&self) {
        let mut g = lock(&self.st);
        g.waiting += 1;
        while g.exclusive || g.shared > 0 {
            g = self.cv.wait(g).unwrap_or_else(|e| e.into_inner());
        }
        g.waiting -= 1;
        g.exclusive = true;
    }
    fn release_shared(&self) {
        let mut g = lock(&self.st);
        g.shared -= 1;
        if g.shared == 0 {
            self.cv.notify_all();
        }
    }
    fn release_exclusive(&self) {
        let mut g = lock(&self.st);
        g.exclusive = false;
        self.cv.notify_all();
    }
}

pub(crate) struct Frame {
    latch: Latch,
    bytes: RwLock<Box<Page>>,
}
impl Frame {
    fn read(&self) -> std::sync::RwLockReadGuard<'_, Box<Page>> {
        self.bytes.read().unwrap_or_else(|e| e.into_inner())
    }
}

struct Slot {
    frame: Arc<Frame>,
    pins: u32,
    dirty: bool,
    used: u64,
}

/// A shared pin and latch. Its only exit is [`Core::unpin_clean`];
/// dropping it instead is counted as a leaked pin, and the frame stays
/// pinned and latched.
#[must_use = "release the pin with unpin_clean"]
pub(crate) struct SharedFrame {
    page: u64,
    pool: u64,
    frame: Arc<Frame>,
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

struct PoolState {
    slots: BTreeMap<u64, Slot>,
    capacity: usize,
    tick: u64,
    pinned: usize,
}
impl PoolState {
    fn touch(&mut self, page: u64) {
        self.tick += 1;
        let tick = self.tick;
        if let Some(s) = self.slots.get_mut(&page) {
            s.used = tick;
        }
    }
    /// Least recently used unpinned frame.
    fn victim(&self) -> Option<u64> {
        self.slots
            .iter()
            .filter(|(_, s)| s.pins == 0)
            .min_by_key(|(_, s)| s.used)
            .map(|(&p, _)| p)
    }
    fn insert(&mut self, page: u64, frame: Arc<Frame>, pins: u32, dirty: bool) {
        self.tick += 1;
        self.pinned += pins as usize;
        self.slots.insert(
            page,
            Slot {
                frame,
                pins,
                dirty,
                used: self.tick,
            },
        );
    }
}

pub(crate) struct FramePool {
    id: u64,
    state: Mutex<PoolState>,
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
            state: Mutex::new(PoolState {
                slots: BTreeMap::new(),
                capacity,
                tick: 0,
                pinned: 0,
            }),
            leaks: Arc::new(AtomicU64::new(0)),
        }
    }
    pub fn pinned(&self) -> usize {
        lock(&self.state).pinned
    }
    pub fn leaked(&self) -> u64 {
        self.leaks.load(Ordering::Relaxed)
    }
    pub fn resident(&self, page: u64) -> bool {
        lock(&self.state).slots.contains_key(&page)
    }
    pub fn len(&self) -> usize {
        lock(&self.state).slots.len()
    }
    pub fn dirty_pages(&self) -> Vec<u64> {
        lock(&self.state)
            .slots
            .iter()
            .filter(|(_, s)| s.dirty)
            .map(|(&p, _)| p)
            .collect()
    }
}

/// Device and log of one store, behind the store's I/O mutex.
pub(crate) struct Io<D: Device> {
    pub dev: D,
    pub log: StoreLog,
    pub evictions: u64,
    pub page_writes: u64,
}
impl<D: Device> Io<D> {
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
    /// Writes page bytes after flushing the log through their LSN (the WAL
    /// rule).
    fn write_page(&mut self, page: u64, bytes: &Page) -> StoreResult<()> {
        let mut copy = Box::new(*bytes);
        self.log.flush_to(&mut self.dev, page_lsn(&copy[..]))?;
        seal(&mut copy);
        self.dev
            .write_at(FileId::Data, page * PAGE_SIZE as u64, &copy[..])?;
        self.page_writes += 1;
        Ok(())
    }
}

/// Pool, device and log of one store. Every method takes `&self`.
pub(crate) struct Core<D: Device> {
    pub io: Mutex<Io<D>>,
    pub pool: FramePool,
    /// Pages in use (meta page's count; may exceed the data file length).
    pub page_count: AtomicU64,
}
impl<D: Device> Core<D> {
    pub fn new(dev: D, log: StoreLog, frames: usize, pages: u64) -> Self {
        Self {
            io: Mutex::new(Io {
                dev,
                log,
                evictions: 0,
                page_writes: 0,
            }),
            pool: FramePool::new(frames),
            page_count: AtomicU64::new(pages),
        }
    }
    pub fn io(&self) -> MutexGuard<'_, Io<D>> {
        lock(&self.io)
    }
    pub fn page_count(&self) -> u64 {
        self.page_count.load(Ordering::Acquire)
    }
    pub fn set_page_count(&self, n: u64) {
        self.page_count.store(n, Ordering::Release);
    }
    pub fn read_disk(&self, id: u64) -> StoreResult<Option<Box<Page>>> {
        self.io().read_disk(id)
    }
    pub fn into_parts(self) -> Io<D> {
        self.io.into_inner().unwrap_or_else(|e| e.into_inner())
    }

    /// Evicts unpinned frames (writing dirty ones back) until there is room
    /// for one more. When every frame is pinned the pool grows past its
    /// capacity instead; it shrinks back as pins are released and frames
    /// are evicted by later loads.
    fn make_room(&self, st: &mut PoolState) -> StoreResult<()> {
        while st.slots.len() >= st.capacity {
            let Some(victim) = st.victim() else {
                return Ok(());
            };
            let slot = st.slots.remove(&victim).expect("victim");
            if slot.dirty {
                let bytes = slot.frame.read();
                if let Err(e) = self.io().write_page(victim, &bytes) {
                    drop(bytes);
                    st.slots.insert(victim, slot);
                    return Err(e);
                }
            }
            self.io().evictions += 1;
        }
        Ok(())
    }
    /// Pins `page`, loading and validating it from the device on a miss.
    fn pin(&self, page: u64) -> StoreResult<Arc<Frame>> {
        let mut st = lock(&self.pool.state);
        if let Some(s) = st.slots.get_mut(&page) {
            s.pins += 1;
            let frame = Arc::clone(&s.frame);
            st.pinned += 1;
            st.touch(page);
            return Ok(frame);
        }
        if page >= self.page_count() {
            return Err(corrupt(format!("page {page} is beyond the store")));
        }
        let bytes = self
            .read_disk(page)?
            .ok_or_else(|| corrupt(format!("page {page} is missing from the data file")))?;
        self.make_room(&mut st)?;
        let frame = Arc::new(Frame {
            latch: Latch::default(),
            bytes: RwLock::new(bytes),
        });
        st.insert(page, Arc::clone(&frame), 1, false);
        Ok(frame)
    }
    fn unpin(&self, page: u64) -> StoreResult<()> {
        let mut st = lock(&self.pool.state);
        let s = st
            .slots
            .get_mut(&page)
            .ok_or_else(|| pin_error("pinned page vanished"))?;
        s.pins -= 1;
        st.pinned -= 1;
        Ok(())
    }
    /// Pins and shared-latches `page` (blocking while it is exclusively
    /// latched or an exclusive request waits).
    pub fn fix_shared(&self, page: u64) -> StoreResult<SharedFrame> {
        let frame = self.pin(page)?;
        frame.latch.shared();
        Ok(SharedFrame {
            page,
            pool: self.pool.id,
            frame,
            released: false,
            leaks: Arc::clone(&self.pool.leaks),
        })
    }
    /// Runs `f` over the latched page.
    pub fn view<R>(&self, pin: &SharedFrame, f: impl FnOnce(&Page) -> R) -> StoreResult<R> {
        if pin.pool != self.pool.id {
            return Err(pin_error("pin of another pool"));
        }
        Ok(f(&pin.frame.read()))
    }
    pub fn unpin_clean(&self, mut pin: SharedFrame) -> StoreResult<()> {
        if pin.pool != self.pool.id {
            return Err(pin_error("pin of another pool"));
        }
        pin.released = true;
        pin.frame.latch.release_shared();
        self.unpin(pin.page)
    }
    /// A copy of a resident or loadable page, read under a shared latch.
    pub fn copy(&self, page: u64) -> StoreResult<Box<Page>> {
        let pin = self.fix_shared(page)?;
        let copy = self.view(&pin, |b| Box::new(*b));
        self.unpin_clean(pin)?;
        copy
    }
    /// Installs a clean frame read during recovery (single-threaded).
    pub fn insert_clean(&self, page: u64, bytes: Box<Page>) -> StoreResult<()> {
        let mut st = lock(&self.pool.state);
        if st.slots.contains_key(&page) {
            return Err(pin_error("page already resident"));
        }
        self.make_room(&mut st)?;
        let frame = Arc::new(Frame {
            latch: Latch::default(),
            bytes: RwLock::new(bytes),
        });
        st.insert(page, frame, 0, false);
        Ok(())
    }

    /// Publishes one commit group's pages atomically for latch-coupled
    /// readers. `pages` is in tree order (callers sort it): each entry is
    /// new bytes at an LSN, or `None` for a page the group frees, which is
    /// only latched (when resident) so that no reader stays on it. Every
    /// page is pinned and exclusive-latched in that order, then the new
    /// bytes are written and marked dirty and everything is released. A
    /// page that is not resident gets a frame latched from creation, so no
    /// reader can see it before its bytes are written.
    pub fn install_group(&self, pages: &[(u64, Option<(&Page, u64)>)]) -> StoreResult<()> {
        type Held<'p> = (u64, Arc<Frame>, Option<(&'p Page, u64)>);
        let mut held: Vec<Held<'_>> = Vec::new();
        let result = (|| {
            for &(page, install) in pages {
                let (frame, latched) = {
                    let mut st = lock(&self.pool.state);
                    if let Some(s) = st.slots.get_mut(&page) {
                        s.pins += 1;
                        let f = Arc::clone(&s.frame);
                        st.pinned += 1;
                        st.touch(page);
                        (f, false)
                    } else if install.is_some() {
                        self.make_room(&mut st)?;
                        let f = Arc::new(Frame {
                            latch: Latch::exclusive_now(),
                            bytes: RwLock::new(new_page()),
                        });
                        st.insert(page, Arc::clone(&f), 1, false);
                        (f, true)
                    } else {
                        continue;
                    }
                };
                if !latched {
                    frame.latch.exclusive();
                }
                held.push((page, frame, install));
            }
            for (_, frame, install) in &held {
                let Some((bytes, lsn)) = install else {
                    continue;
                };
                let mut b = frame.bytes.write().unwrap_or_else(|e| e.into_inner());
                let old = page_lsn(&b[..]);
                if *lsn <= old && old != 0 {
                    return Err(pin_error("dirty release must advance the page LSN"));
                }
                b.copy_from_slice(&bytes[..]);
                set_page_lsn(&mut b[..], *lsn);
            }
            let mut st = lock(&self.pool.state);
            for (page, _, install) in &held {
                if install.is_some() {
                    if let Some(s) = st.slots.get_mut(page) {
                        s.dirty = true;
                    }
                }
            }
            Ok(())
        })();
        for (page, frame, _) in held {
            frame.latch.release_exclusive();
            self.unpin(page)?;
        }
        result
    }
    /// Publishes one page (recovery, initialization).
    pub fn install(&self, page: u64, bytes: &Page, lsn: u64) -> StoreResult<()> {
        self.install_group(&[(page, Some((bytes, lsn)))])
    }

    /// Writes a resident dirty page (the WAL rule first) and marks it clean.
    /// Called by the single writer, so no install races it.
    pub fn write_back(&self, page: u64) -> StoreResult<()> {
        let mut st = lock(&self.pool.state);
        let Some(slot) = st.slots.get(&page) else {
            return Ok(());
        };
        if !slot.dirty {
            return Ok(());
        }
        let frame = Arc::clone(&slot.frame);
        {
            let bytes = frame.read();
            self.io().write_page(page, &bytes)?;
        }
        if let Some(s) = st.slots.get_mut(&page) {
            s.dirty = false;
        }
        Ok(())
    }
}
