//! Durable record store: file-backed slotted pages behind the §24.2 pin
//! protocol, a steal/no-force buffer pool with dirty write-back, an
//! ARIES-style write-ahead log (redo, undo with compensation records,
//! full-page images against torn writes) and sharp checkpoints that truncate
//! the log.
//!
//! See the crate README for the protocol, its invariants and its limits.
use crate::device::{Device, DeviceError, FileId};
use crate::page::{PageError, SlottedPage, MAX_RECORD_SIZE, PAGE_SIZE, SLOT_SIZE};
use crate::pool::{BufferPool, ExclusivePin, PageImage, PoolError, PoolLimits};
use crate::wal::{self, LogRecord, TailDefect, Wal};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DbErrorKind {
    /// Host I/O failed. The store is poisoned; reopen it to recover.
    Io,
    /// The simulated device crashed. The store is poisoned.
    Crashed,
    /// A page or log frame failed validation where no repair is possible.
    Corrupt,
    /// The page is locked by another active transaction (no-wait locking).
    Conflict,
    /// The record id does not name a live record.
    NotFound,
    /// A configured or hard limit was exceeded.
    Limit,
    /// The log has no room for this change and its rollback.
    LogFull,
    /// The record does not fit in the page.
    Full,
    /// The call is not valid in the current state (for example a checkpoint
    /// with active transactions, or use after poisoning).
    State,
    /// The handle belongs to a different store.
    WrongStore,
    Overflow,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DbError {
    pub kind: DbErrorKind,
    pub message: String,
}
impl DbError {
    pub(crate) fn new(kind: DbErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}
impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}
impl std::error::Error for DbError {}
impl From<DeviceError> for DbError {
    fn from(e: DeviceError) -> Self {
        match e {
            DeviceError::Crashed => Self::new(DbErrorKind::Crashed, "device crashed"),
            other => Self::new(DbErrorKind::Io, other.to_string()),
        }
    }
}
fn pool_error(e: PoolError) -> DbError {
    match e {
        PoolError::Page(PageError::Full) => DbError::new(DbErrorKind::Full, "page is full"),
        PoolError::Page(PageError::SlotOutOfBounds) => {
            DbError::new(DbErrorKind::NotFound, "no such slot")
        }
        PoolError::Page(PageError::RecordSize) => {
            DbError::new(DbErrorKind::Limit, "record size out of range")
        }
        other => DbError::new(DbErrorKind::Corrupt, format!("buffer pool: {other}")),
    }
}

/// Limits may be lowered, never raised above the hard ceilings in the README.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DbLimits {
    /// Resident page frames, 2..=256.
    pub pool_frames: usize,
    /// Pages in the data file, 1..=65,536 (1 GiB).
    pub max_pages: u64,
    /// Log bytes after the headers, 64 KiB..=256 MiB.
    pub max_wal_bytes: u64,
    /// Concurrently active transactions, 1..=64.
    pub max_active_txns: usize,
}
impl Default for DbLimits {
    fn default() -> Self {
        Self {
            pool_frames: 16,
            max_pages: 4096,
            max_wal_bytes: 64 << 20,
            max_active_txns: 16,
        }
    }
}
impl DbLimits {
    fn validate(self) -> Result<(), DbError> {
        let ok = (2..=256).contains(&self.pool_frames)
            && (1..=65_536).contains(&self.max_pages)
            && (64 << 10..=256 << 20).contains(&self.max_wal_bytes)
            && (1..=64).contains(&self.max_active_txns);
        if ok {
            Ok(())
        } else {
            Err(DbError::new(DbErrorKind::Limit, "limits out of range"))
        }
    }
}

/// Page and slot of one record. Slots are never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RecordId {
    pub page: u64,
    pub slot: u16,
}

/// Largest user record: one page record minus Quarry's one-byte tag.
pub const MAX_VALUE: usize = MAX_RECORD_SIZE - 1;
const LIVE: u8 = 1;
const TOMBSTONE: u8 = 0;
/// Frame bytes of a Commit or End record.
const FINISH_FRAME: u64 = 16 + 1 + 16;
/// Frame bytes of a full page image.
const IMAGE_FRAME: u64 = 16 + 1 + 8 + 4 + PAGE_SIZE as u64;
fn clr_frame(image_len: usize) -> u64 {
    16 + 1 + 32 + 2 + 4 + image_len as u64
}

/// Active transaction owner. Not `Clone`; its only exits are
/// [`Db::commit`] and [`Db::abort`]. Dropping it instead is counted by
/// [`Db::leaked_txns`]; the transaction then stays active (holding its page
/// locks) until the store is reopened, whose recovery rolls it back.
///
/// ```compile_fail
/// fn duplicate(t: vesper_quarry::durable::Txn) { let _c = t.clone(); }
/// ```
#[must_use = "commit or abort the transaction"]
#[derive(Debug)]
pub struct Txn {
    id: u64,
    store: u64,
    leaks: Arc<AtomicU64>,
    finished: bool,
}
impl Txn {
    pub fn id(&self) -> u64 {
        self.id
    }
}
impl Drop for Txn {
    fn drop(&mut self) {
        if !self.finished {
            self.leaks.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[derive(Debug, Default)]
struct TxnState {
    last_lsn: u64,
    undo: Vec<Undo>,
    reserved: u64,
    insert_page: Option<u64>,
    /// Per page: bytes the page uses now minus the bytes it would use after
    /// rolling this transaction back. A rollback restores shrunk records
    /// and leaves a slot plus a one-byte tombstone per insert, so inserts
    /// must leave room for it (see `insert_inner`).
    rollback_slack: BTreeMap<u64, i64>,
}
#[derive(Debug)]
struct Undo {
    prev: u64,
    page: u64,
    slot: u16,
    before: Option<Vec<u8>>,
}

/// What `Db::open` found and did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// A new store was initialized.
    pub created: bool,
    pub base_lsn: u64,
    /// Valid log records read.
    pub records: usize,
    /// The reason the log ended early, if it did; the tail was truncated.
    pub tail: Option<TailDefect>,
    /// Page images restored over missing, torn or older pages.
    pub images_restored: usize,
    /// Page changes reapplied because the page was older than the record.
    pub redone: usize,
    /// Changes already on the page (redo skipped by page LSN).
    pub skipped: usize,
    pub committed: Vec<u64>,
    /// Transactions without a commit or end record, rolled back.
    pub rolled_back: Vec<u64>,
    /// Compensation records written while rolling back.
    pub compensations: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DbStats {
    pub pages: u64,
    pub resident: usize,
    pub pinned: usize,
    pub wal_bytes: u64,
    pub wal_reserved: u64,
    pub next_lsn: u64,
    pub durable_lsn: u64,
    pub evictions: u64,
    pub page_writes: u64,
    pub active_txns: usize,
}

enum PageOp<'a> {
    Insert(&'a [u8]),
    Set { slot: u16, image: &'a [u8] },
}

/// A durable store of opaque records over a [`Device`].
pub struct Db<D: Device> {
    id: u64,
    dev: D,
    wal: Wal,
    pool: BufferPool,
    lru: VecDeque<u64>,
    touched: BTreeSet<u64>,
    unwritten: BTreeSet<u64>,
    page_count: u64,
    txns: BTreeMap<u64, TxnState>,
    locks: BTreeMap<u64, u64>,
    next_txn: u64,
    limits: DbLimits,
    leaks: Arc<AtomicU64>,
    poisoned: bool,
    evictions: u64,
    page_writes: u64,
}
impl<D: Device> fmt::Debug for Db<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Db").field("stats", &self.stats()).finish()
    }
}

impl<D: Device> Db<D> {
    /// Opens (creating if empty) and recovers a store, then checkpoints.
    ///
    /// Recovery: read the log from its active header up to the first invalid
    /// frame and durably cut the tail; redo every change whose LSN is newer
    /// than its page (restoring logged page images over missing, torn or
    /// older pages first); roll back every transaction without a Commit or
    /// End record, writing compensation records; then write back all pages,
    /// sync, and truncate the log. Recovery is itself crash-safe: a crash at
    /// any point leaves a state the next `open` recovers the same way.
    pub fn open(mut dev: D, limits: DbLimits) -> Result<(Self, RecoveryReport), DbError> {
        limits.validate()?;
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let pool = BufferPool::new(PoolLimits {
            frames: limits.pool_frames,
            pins: 4,
        })
        .map_err(pool_error)?;
        let data_len = dev.len(FileId::Data)?;
        let mut report = RecoveryReport::default();
        let scan = wal::scan(&mut dev, limits.max_wal_bytes)?;
        let Some(scan) = scan else {
            // Initialization writes only the two header slots, so a longer
            // log without a valid header held frames: never overwrite it.
            if data_len != 0 || dev.len(FileId::Wal)? > wal::HEADER_BYTES {
                return Err(DbError::new(
                    DbErrorKind::Corrupt,
                    "existing store without a valid log header",
                ));
            }
            let wal = Wal::create(&mut dev, limits.max_wal_bytes)?;
            report.created = true;
            report.base_lsn = 1;
            let db = Self::assemble(id, dev, wal, pool, 0, limits);
            return Ok((db, report));
        };
        if scan.valid_end < scan.file_len {
            dev.set_len(FileId::Wal, scan.valid_end)?;
            dev.sync(FileId::Wal)?;
        }
        report.base_lsn = scan.base_lsn;
        report.records = scan.records.len();
        report.tail = scan.tail;
        let logged_pages = scan
            .records
            .iter()
            .filter_map(|(_, r)| r.page())
            .max()
            .map_or(0, |p| p + 1);
        let page_count = data_len.div_ceil(PAGE_SIZE as u64).max(logged_pages);
        if page_count > limits.max_pages {
            return Err(DbError::new(DbErrorKind::Limit, "store exceeds page limit"));
        }
        let wal = Wal::resume(&scan, limits.max_wal_bytes);
        let mut db = Self::assemble(id, dev, wal, pool, page_count, limits);
        let result = db.recover(scan.records, &mut report);
        match result {
            Ok(()) => Ok((db, report)),
            Err(e) => Err(e),
        }
    }

    fn assemble(id: u64, dev: D, wal: Wal, pool: BufferPool, pages: u64, limits: DbLimits) -> Self {
        Self {
            id,
            dev,
            wal,
            pool,
            lru: VecDeque::new(),
            touched: BTreeSet::new(),
            unwritten: BTreeSet::new(),
            page_count: pages,
            txns: BTreeMap::new(),
            locks: BTreeMap::new(),
            next_txn: 1,
            limits,
            leaks: Arc::new(AtomicU64::new(0)),
            poisoned: false,
            evictions: 0,
            page_writes: 0,
        }
    }

    fn recover(
        &mut self,
        records: Vec<(u64, LogRecord)>,
        report: &mut RecoveryReport,
    ) -> Result<(), DbError> {
        // Analysis and redo in one forward pass (repeating history).
        let mut last: BTreeMap<u64, u64> = BTreeMap::new();
        let mut finished: BTreeSet<u64> = BTreeSet::new();
        let mut by_lsn: BTreeMap<u64, LogRecord> = BTreeMap::new();
        for (lsn, record) in records {
            if let Some(txn) = record.txn() {
                last.insert(txn, lsn);
                self.next_txn = self.next_txn.max(txn + 1);
            }
            match &record {
                LogRecord::PageImage { page, bytes } => {
                    if self.pool.info(*page).is_ok() {
                        return Err(DbError::new(
                            DbErrorKind::Corrupt,
                            "page image logged after changes to the page",
                        ));
                    }
                    self.touched.insert(*page);
                    let image = SlottedPage::decode(bytes)
                        .map_err(|_| DbError::new(DbErrorKind::Corrupt, "logged page image"))?;
                    let disk_lsn = self.read_disk(*page).ok().map(|p| p.header().page_lsn);
                    if disk_lsn.is_none_or(|l| l < image.header().page_lsn) {
                        self.dev
                            .write_at(FileId::Data, page * PAGE_SIZE as u64, bytes)?;
                        report.images_restored += 1;
                    }
                }
                LogRecord::Insert {
                    page, slot, after, ..
                } => self.redo(*page, PageOp::Insert(after), Some(*slot), lsn, report)?,
                LogRecord::Update {
                    page, slot, after, ..
                } => self.redo(
                    *page,
                    PageOp::Set {
                        slot: *slot,
                        image: after,
                    },
                    None,
                    lsn,
                    report,
                )?,
                LogRecord::Compensation {
                    page, slot, image, ..
                } => self.redo(*page, PageOp::Set { slot: *slot, image }, None, lsn, report)?,
                LogRecord::Commit { txn, .. } => {
                    finished.insert(*txn);
                    report.committed.push(*txn);
                }
                LogRecord::End { txn, .. } => {
                    finished.insert(*txn);
                }
            }
            by_lsn.insert(lsn, record);
        }
        // Undo losers, newest change first across all of them.
        let mut next_undo: BTreeMap<u64, (u64, u64)> = BTreeMap::new(); // txn -> (next, last)
        for (&txn, &lsn) in &last {
            if finished.contains(&txn) {
                continue;
            }
            report.rolled_back.push(txn);
            let next = match &by_lsn[&lsn] {
                LogRecord::Compensation { undo_next, .. } => *undo_next,
                _ => lsn,
            };
            next_undo.insert(txn, (next, lsn));
        }
        while let Some((&txn, &(next, prev))) = next_undo.iter().max_by_key(|(_, (n, _))| *n) {
            if next == 0 {
                self.wal.append(&LogRecord::End { txn, prev })?;
                next_undo.remove(&txn);
                continue;
            }
            let record = by_lsn
                .get(&next)
                .ok_or_else(|| DbError::new(DbErrorKind::Corrupt, "undo chain leaves the log"))?;
            let (page, slot, image, undo_next) = match record {
                LogRecord::Insert {
                    page,
                    slot,
                    prev: p,
                    txn: t,
                    ..
                } if *t == txn => (*page, *slot, vec![TOMBSTONE], *p),
                LogRecord::Update {
                    page,
                    slot,
                    before,
                    prev: p,
                    txn: t,
                    ..
                } if *t == txn => (*page, *slot, before.clone(), *p),
                LogRecord::Compensation {
                    undo_next, txn: t, ..
                } if *t == txn => {
                    next_undo.insert(txn, (*undo_next, prev));
                    continue;
                }
                _ => {
                    return Err(DbError::new(
                        DbErrorKind::Corrupt,
                        "undo chain names a foreign record",
                    ))
                }
            };
            let (lsn, _, _) = self.log_and_apply(
                page,
                PageOp::Set {
                    slot,
                    image: &image,
                },
                |slot, _| LogRecord::Compensation {
                    txn,
                    prev,
                    page,
                    slot,
                    image: image.clone(),
                    undo_next,
                },
            )?;
            report.compensations += 1;
            next_undo.insert(txn, (undo_next, lsn));
        }
        self.checkpoint_inner()
    }

    fn redo(
        &mut self,
        page: u64,
        op: PageOp<'_>,
        expect_slot: Option<u16>,
        lsn: u64,
        report: &mut RecoveryReport,
    ) -> Result<(), DbError> {
        self.ensure_resident(page)?;
        let info = self.pool.info(page).map_err(pool_error)?;
        if lsn <= info.page_lsn {
            report.skipped += 1;
            return Ok(());
        }
        let mut pin = self.pool.fix_exclusive(page).map_err(pool_error)?;
        let applied = match op {
            PageOp::Insert(image) => self.pool.insert(&mut pin, image).and_then(|slot| {
                if Some(slot) == expect_slot {
                    Ok(())
                } else {
                    Err(PoolError::Page(PageError::SlotLayout))
                }
            }),
            PageOp::Set { slot, image } => self.pool.update(&mut pin, slot, image),
        };
        if let Err(e) = applied {
            self.abort_pin(pin)?;
            return Err(DbError::new(
                DbErrorKind::Corrupt,
                format!("redo of LSN {lsn} does not apply: {e}"),
            ));
        }
        self.release_dirty(pin, lsn)?;
        report.redone += 1;
        Ok(())
    }

    // ---- page residency -------------------------------------------------

    fn read_disk(&mut self, page: u64) -> Result<SlottedPage, DbError> {
        let mut bytes = vec![0u8; PAGE_SIZE];
        self.dev
            .read_at(FileId::Data, page * PAGE_SIZE as u64, &mut bytes)
            .map_err(|e| match e {
                DeviceError::ShortRead => {
                    DbError::new(DbErrorKind::Corrupt, format!("page {page} is missing"))
                }
                other => other.into(),
            })?;
        let decoded = SlottedPage::decode(&bytes).map_err(|e| {
            DbError::new(
                DbErrorKind::Corrupt,
                format!("page {page} failed validation: {e}"),
            )
        })?;
        if decoded.header().page_id != page {
            return Err(DbError::new(
                DbErrorKind::Corrupt,
                format!("page {page} holds another page id"),
            ));
        }
        Ok(decoded)
    }
    fn ensure_resident(&mut self, page: u64) -> Result<(), DbError> {
        if self.pool.info(page).is_ok() {
            if let Some(i) = self.lru.iter().position(|&p| p == page) {
                self.lru.remove(i);
            }
            self.lru.push_back(page);
            return Ok(());
        }
        if page >= self.page_count {
            return Err(DbError::new(DbErrorKind::NotFound, "no such page"));
        }
        let image = self.read_disk(page)?;
        self.make_room()?;
        self.pool
            .install(&PageImage::clean(image))
            .map_err(pool_error)?;
        self.lru.push_back(page);
        Ok(())
    }
    fn make_room(&mut self) -> Result<(), DbError> {
        if self.pool.resident_count() < self.pool.capacity() {
            return Ok(());
        }
        let victim = self
            .lru
            .iter()
            .copied()
            .find(|&p| {
                self.pool
                    .info(p)
                    .is_ok_and(|i| i.shared_pins == 0 && !i.exclusive)
            })
            .ok_or_else(|| DbError::new(DbErrorKind::Limit, "every frame is pinned"))?;
        self.write_back(victim)?;
        self.evictions += 1;
        Ok(())
    }
    /// Evicts `page`, writing it first when dirty or never written. The log
    /// is flushed through the page LSN before the page write (the WAL rule).
    fn write_back(&mut self, page: u64) -> Result<(), DbError> {
        let image = self.pool.evict(page).map_err(pool_error)?;
        if let Some(i) = self.lru.iter().position(|&p| p == page) {
            self.lru.remove(i);
        }
        if image.dirty_lsn().is_some() || self.unwritten.contains(&page) {
            self.wal
                .flush_to(&mut self.dev, image.page().header().page_lsn)?;
            self.dev.write_at(
                FileId::Data,
                page * PAGE_SIZE as u64,
                image.page().as_bytes(),
            )?;
            self.unwritten.remove(&page);
            self.page_writes += 1;
        }
        Ok(())
    }
    fn allocate(&mut self) -> Result<u64, DbError> {
        if self.page_count >= self.limits.max_pages {
            return Err(DbError::new(DbErrorKind::Limit, "page limit reached"));
        }
        let page = self.page_count;
        self.make_room()?;
        self.pool
            .install(&PageImage::clean(SlottedPage::new(page, 0)))
            .map_err(pool_error)?;
        self.lru.push_back(page);
        self.unwritten.insert(page);
        self.page_count += 1;
        Ok(page)
    }

    // ---- logged page changes --------------------------------------------

    fn abort_pin(&mut self, pin: ExclusivePin) -> Result<(), DbError> {
        self.pool
            .unpin_aborted(pin)
            .map_err(|e| DbError::new(DbErrorKind::Corrupt, format!("pin release: {e}")))
    }
    fn release_dirty(&mut self, pin: ExclusivePin, lsn: u64) -> Result<(), DbError> {
        match self.pool.unpin_dirty(pin, lsn) {
            Ok(()) => Ok(()),
            Err(e) => {
                let error = e.error;
                self.abort_pin(e.pin)?;
                Err(DbError::new(
                    DbErrorKind::Corrupt,
                    format!("dirty release: {error}"),
                ))
            }
        }
    }
    /// Logs a page image on first touch since the checkpoint, applies `op`
    /// to an exclusive pin's stage, logs the record built by `make` and
    /// releases the pin dirty at that LSN. Every failure aborts the pin.
    fn log_and_apply(
        &mut self,
        page: u64,
        op: PageOp<'_>,
        make: impl FnOnce(u16, Option<Vec<u8>>) -> LogRecord,
    ) -> Result<(u64, u16, Option<Vec<u8>>), DbError> {
        self.ensure_resident(page)?;
        if !self.touched.contains(&page) {
            let pin = self.pool.fix_shared(page).map_err(pool_error)?;
            let bytes = match self.pool.view_shared(&pin) {
                Ok(p) => Ok(p.as_bytes().to_vec()),
                Err(e) => Err(e),
            };
            self.pool
                .unpin_clean(pin)
                .map_err(|e| DbError::new(DbErrorKind::Corrupt, format!("pin release: {e}")))?;
            let bytes = bytes.map_err(pool_error)?;
            self.wal.append(&LogRecord::PageImage { page, bytes })?;
            self.touched.insert(page);
        }
        let mut pin = self.pool.fix_exclusive(page).map_err(pool_error)?;
        let staged = match op {
            PageOp::Insert(image) => self.pool.insert(&mut pin, image).map(|s| (s, None)),
            PageOp::Set { slot, image } => self
                .pool
                .view_exclusive(&pin)
                .and_then(|p| p.read(slot).map_err(PoolError::Page))
                .map(<[u8]>::to_vec)
                .and_then(|before| {
                    self.pool
                        .update(&mut pin, slot, image)
                        .map(|()| (slot, Some(before)))
                }),
        };
        let (slot, before) = match staged {
            Ok(x) => x,
            Err(e) => {
                self.abort_pin(pin)?;
                return Err(pool_error(e));
            }
        };
        let lsn = match self.wal.append(&make(slot, before.clone())) {
            Ok(lsn) => lsn,
            Err(e) => {
                self.abort_pin(pin)?;
                return Err(e);
            }
        };
        self.release_dirty(pin, lsn)?;
        Ok((lsn, slot, before))
    }

    // ---- public transactional API ---------------------------------------

    fn check_usable(&self) -> Result<(), DbError> {
        if self.poisoned {
            Err(DbError::new(
                DbErrorKind::State,
                "store is poisoned by an earlier I/O failure; reopen it",
            ))
        } else {
            Ok(())
        }
    }
    fn poison_on_io<T>(&mut self, r: Result<T, DbError>) -> Result<T, DbError> {
        if let Err(e) = &r {
            if matches!(
                e.kind,
                DbErrorKind::Io | DbErrorKind::Crashed | DbErrorKind::Corrupt
            ) {
                self.poisoned = true;
            }
        }
        r
    }
    fn state_of(&self, txn: &Txn) -> Result<(), DbError> {
        if txn.store != self.id {
            return Err(DbError::new(
                DbErrorKind::WrongStore,
                "transaction of another store",
            ));
        }
        if !self.txns.contains_key(&txn.id) {
            return Err(DbError::new(
                DbErrorKind::State,
                "transaction is not active",
            ));
        }
        Ok(())
    }
    fn lock(&mut self, txn: u64, page: u64) -> Result<(), DbError> {
        match self.locks.get(&page) {
            Some(&owner) if owner != txn => Err(DbError::new(
                DbErrorKind::Conflict,
                format!("page {page} is locked by transaction {owner}"),
            )),
            _ => {
                self.locks.insert(page, txn);
                Ok(())
            }
        }
    }

    pub fn begin(&mut self) -> Result<Txn, DbError> {
        self.check_usable()?;
        if self.txns.len() >= self.limits.max_active_txns {
            return Err(DbError::new(
                DbErrorKind::Limit,
                "too many active transactions",
            ));
        }
        self.wal.reserve(0, FINISH_FRAME)?;
        let id = self.next_txn;
        self.next_txn = id
            .checked_add(1)
            .ok_or_else(|| DbError::new(DbErrorKind::Overflow, "transaction ids"))?;
        self.txns.insert(
            id,
            TxnState {
                reserved: FINISH_FRAME,
                ..TxnState::default()
            },
        );
        Ok(Txn {
            id,
            store: self.id,
            leaks: Arc::clone(&self.leaks),
            finished: false,
        })
    }

    pub fn insert(&mut self, txn: &mut Txn, value: &[u8]) -> Result<RecordId, DbError> {
        self.check_usable()?;
        self.state_of(txn)?;
        if value.len() > MAX_VALUE {
            return Err(DbError::new(DbErrorKind::Limit, "record too large"));
        }
        let r = self.insert_inner(txn.id, value);
        self.poison_on_io(r)
    }
    fn insert_inner(&mut self, txn: u64, value: &[u8]) -> Result<RecordId, DbError> {
        let mut image = Vec::with_capacity(value.len() + 1);
        image.push(LIVE);
        image.extend_from_slice(value);
        let state = &self.txns[&txn];
        let mut candidates = Vec::new();
        candidates.extend(state.insert_page);
        if let Some(last) = self.page_count.checked_sub(1) {
            if Some(last) != state.insert_page {
                candidates.push(last);
            }
        }
        let mut target = None;
        for page in candidates {
            if self.locks.get(&page).is_some_and(|&o| o != txn) {
                continue;
            }
            self.ensure_resident(page)?;
            let pin = self.pool.fix_shared(page).map_err(pool_error)?;
            let slack = self.txns[&txn]
                .rollback_slack
                .get(&page)
                .copied()
                .unwrap_or(0);
            let fits = self.pool.view_shared(&pin).map(|p| {
                // Rolled back, the page would use `used - slack` bytes; this
                // insert would add a slot and a one-byte tombstone to that.
                let after_rollback = p.used_bytes() as i64 - slack + (SLOT_SIZE + 1) as i64;
                p.free_bytes() >= image.len() + SLOT_SIZE && after_rollback <= PAGE_SIZE as i64
            });
            self.pool
                .unpin_clean(pin)
                .map_err(|e| DbError::new(DbErrorKind::Corrupt, format!("pin release: {e}")))?;
            if fits.map_err(pool_error)? {
                target = Some(page);
                break;
            }
        }
        let fpi = |db: &Self, page: Option<u64>| {
            if page.is_some_and(|p| db.touched.contains(&p)) {
                0
            } else {
                IMAGE_FRAME
            }
        };
        let record_frame = (16 + 1 + 24 + 2 + 4 + image.len()) as u64;
        let clr = clr_frame(1);
        self.wal.reserve(fpi(self, target) + record_frame, clr)?;
        let page = match target {
            Some(p) => p,
            None => match self.allocate() {
                Ok(p) => p,
                Err(e) => {
                    self.wal.release(clr);
                    return Err(e);
                }
            },
        };
        if let Err(e) = self.lock(txn, page) {
            self.wal.release(clr);
            return Err(e);
        }
        let prev = self.txns[&txn].last_lsn;
        let result =
            self.log_and_apply(page, PageOp::Insert(&image), |slot, _| LogRecord::Insert {
                txn,
                prev,
                page,
                slot,
                after: image.clone(),
            });
        let (lsn, slot, _) = match result {
            Ok(x) => x,
            Err(e) => {
                self.wal.release(clr);
                return Err(e);
            }
        };
        let state = self.txns.get_mut(&txn).expect("active");
        state.last_lsn = lsn;
        state.reserved += clr;
        state.insert_page = Some(page);
        // Now: payload + slot. After rollback: tombstone + slot.
        *state.rollback_slack.entry(page).or_insert(0) += image.len() as i64 - 1;
        state.undo.push(Undo {
            prev,
            page,
            slot,
            before: None,
        });
        Ok(RecordId { page, slot })
    }

    pub fn update(&mut self, txn: &mut Txn, id: RecordId, value: &[u8]) -> Result<(), DbError> {
        self.check_usable()?;
        self.state_of(txn)?;
        if value.len() > MAX_VALUE {
            return Err(DbError::new(DbErrorKind::Limit, "record too large"));
        }
        let mut image = Vec::with_capacity(value.len() + 1);
        image.push(LIVE);
        image.extend_from_slice(value);
        let r = self.set_inner(txn.id, id, image);
        self.poison_on_io(r)
    }
    pub fn delete(&mut self, txn: &mut Txn, id: RecordId) -> Result<(), DbError> {
        self.check_usable()?;
        self.state_of(txn)?;
        let r = self.set_inner(txn.id, id, vec![TOMBSTONE]);
        self.poison_on_io(r)
    }
    fn set_inner(&mut self, txn: u64, id: RecordId, image: Vec<u8>) -> Result<(), DbError> {
        if self.locks.get(&id.page).is_some_and(|&o| o != txn) {
            return Err(DbError::new(
                DbErrorKind::Conflict,
                format!("page {} is locked by another transaction", id.page),
            ));
        }
        let before = self.read_slot(id)?;
        if before.first() != Some(&LIVE) {
            return Err(DbError::new(DbErrorKind::NotFound, "record is deleted"));
        }
        if before == image {
            return Ok(());
        }
        let fpi = if self.touched.contains(&id.page) {
            0
        } else {
            IMAGE_FRAME
        };
        let record_frame = (16 + 1 + 24 + 2 + 8 + before.len() + image.len()) as u64;
        let clr = clr_frame(before.len());
        self.wal.reserve(fpi + record_frame, clr)?;
        self.lock(txn, id.page)?;
        let prev = self.txns[&txn].last_lsn;
        let page = id.page;
        let result = self.log_and_apply(
            page,
            PageOp::Set {
                slot: id.slot,
                image: &image,
            },
            |slot, before| LogRecord::Update {
                txn,
                prev,
                page,
                slot,
                before: before.unwrap_or_default(),
                after: image.clone(),
            },
        );
        let (lsn, slot, before) = match result {
            Ok(x) => x,
            Err(e) => {
                self.wal.release(clr);
                return Err(e);
            }
        };
        let state = self.txns.get_mut(&txn).expect("active");
        state.last_lsn = lsn;
        state.reserved += clr;
        let before_len = before.as_ref().map_or(0, Vec::len) as i64;
        *state.rollback_slack.entry(page).or_insert(0) += image.len() as i64 - before_len;
        state.undo.push(Undo {
            prev,
            page,
            slot,
            before,
        });
        Ok(())
    }
    fn read_slot(&mut self, id: RecordId) -> Result<Vec<u8>, DbError> {
        self.ensure_resident(id.page)?;
        let pin = self.pool.fix_shared(id.page).map_err(pool_error)?;
        let bytes = self
            .pool
            .view_shared(&pin)
            .and_then(|p| p.read(id.slot).map(<[u8]>::to_vec).map_err(PoolError::Page));
        self.pool
            .unpin_clean(pin)
            .map_err(|e| DbError::new(DbErrorKind::Corrupt, format!("pin release: {e}")))?;
        bytes.map_err(pool_error)
    }

    /// Reads a record: `Ok(None)` when deleted. With `txn`, the
    /// transaction's own uncommitted changes are visible; pages locked by
    /// any other transaction are a `Conflict` (no dirty reads).
    pub fn get(&mut self, txn: Option<&Txn>, id: RecordId) -> Result<Option<Vec<u8>>, DbError> {
        self.check_usable()?;
        let me = match txn {
            Some(t) => {
                self.state_of(t)?;
                Some(t.id)
            }
            None => None,
        };
        if let Some(&owner) = self.locks.get(&id.page) {
            if Some(owner) != me {
                return Err(DbError::new(DbErrorKind::Conflict, "page is locked"));
            }
        }
        let r = self.read_slot(id);
        let bytes = self.poison_on_io(r)?;
        Ok(match bytes.split_first() {
            Some((&LIVE, rest)) => Some(rest.to_vec()),
            _ => None,
        })
    }

    /// All live committed records in page/slot order. Fails with `Conflict`
    /// while any transaction holds a page lock.
    pub fn scan(&mut self) -> Result<Vec<(RecordId, Vec<u8>)>, DbError> {
        self.check_usable()?;
        if !self.locks.is_empty() {
            return Err(DbError::new(
                DbErrorKind::Conflict,
                "active transactions hold page locks",
            ));
        }
        let r = self.scan_inner();
        self.poison_on_io(r)
    }
    fn scan_inner(&mut self) -> Result<Vec<(RecordId, Vec<u8>)>, DbError> {
        let mut out = Vec::new();
        for page in 0..self.page_count {
            self.ensure_resident(page)?;
            let pin = self.pool.fix_shared(page).map_err(pool_error)?;
            let collected = self.pool.view_shared(&pin).and_then(|p| {
                let mut rows = Vec::new();
                for slot in 0..p.slot_count() as u16 {
                    let bytes = p.read(slot).map_err(PoolError::Page)?;
                    if let Some((&LIVE, rest)) = bytes.split_first() {
                        rows.push((RecordId { page, slot }, rest.to_vec()));
                    }
                }
                Ok(rows)
            });
            self.pool
                .unpin_clean(pin)
                .map_err(|e| DbError::new(DbErrorKind::Corrupt, format!("pin release: {e}")))?;
            out.extend(collected.map_err(pool_error)?);
        }
        Ok(out)
    }

    /// Appends a Commit record and syncs the log. On `Ok` the transaction is
    /// durable. On an I/O error its outcome is unknown until reopening.
    pub fn commit(&mut self, mut txn: Txn) -> Result<u64, DbError> {
        self.check_usable()?;
        self.state_of(&txn)?;
        txn.finished = true;
        let state = self.txns.remove(&txn.id).expect("active");
        self.wal.release(state.reserved);
        self.locks.retain(|_, owner| *owner != txn.id);
        let r = self
            .wal
            .append(&LogRecord::Commit {
                txn: txn.id,
                prev: state.last_lsn,
            })
            .and_then(|lsn| self.wal.flush(&mut self.dev).map(|()| lsn));
        if r.is_err() {
            self.poisoned = true;
        }
        r
    }

    /// Rolls the transaction back with compensation records and an End
    /// record. The log is not synced: a crash before the next sync makes
    /// recovery roll it back again.
    pub fn abort(&mut self, mut txn: Txn) -> Result<(), DbError> {
        self.check_usable()?;
        self.state_of(&txn)?;
        txn.finished = true;
        let mut state = self.txns.remove(&txn.id).expect("active");
        let r = self.rollback(txn.id, &mut state);
        self.wal.release(state.reserved);
        self.locks.retain(|_, owner| *owner != txn.id);
        if r.is_err() {
            // The rollback is incomplete; only recovery can finish it.
            self.poisoned = true;
        }
        r
    }
    fn rollback(&mut self, txn: u64, state: &mut TxnState) -> Result<(), DbError> {
        while let Some(undo) = state.undo.pop() {
            let image = undo.before.unwrap_or_else(|| vec![TOMBSTONE]);
            let prev = state.last_lsn;
            let (lsn, _, _) = self.log_and_apply(
                undo.page,
                PageOp::Set {
                    slot: undo.slot,
                    image: &image,
                },
                |slot, _| LogRecord::Compensation {
                    txn,
                    prev,
                    page: undo.page,
                    slot,
                    image: image.clone(),
                    undo_next: undo.prev,
                },
            )?;
            state.last_lsn = lsn;
        }
        self.wal.append(&LogRecord::End {
            txn,
            prev: state.last_lsn,
        })?;
        Ok(())
    }

    /// Sharp checkpoint: requires no active transaction; writes back every
    /// resident page, syncs the data file, then truncates the log.
    pub fn checkpoint(&mut self) -> Result<(), DbError> {
        self.check_usable()?;
        if !self.txns.is_empty() {
            return Err(DbError::new(
                DbErrorKind::State,
                "checkpoint requires no active transactions",
            ));
        }
        let r = self.checkpoint_inner();
        self.poison_on_io(r)
    }
    fn checkpoint_inner(&mut self) -> Result<(), DbError> {
        self.wal.flush(&mut self.dev)?;
        let resident: Vec<u64> = self.lru.iter().copied().collect();
        for page in resident {
            self.write_back(page)?;
        }
        self.dev.sync(FileId::Data)?;
        self.wal.truncate(&mut self.dev)?;
        self.touched.clear();
        Ok(())
    }

    /// Checkpoints and returns the device (a clean shutdown).
    pub fn close(mut self) -> Result<D, DbError> {
        self.checkpoint()?;
        Ok(self.dev)
    }
    /// Returns the device without writing anything (a crash, for tests).
    pub fn into_device(self) -> D {
        self.dev
    }
    pub fn device(&self) -> &D {
        &self.dev
    }
    /// Transaction owners dropped without commit or abort.
    pub fn leaked_txns(&self) -> u64 {
        self.leaks.load(Ordering::Relaxed)
    }
    pub fn poisoned(&self) -> bool {
        self.poisoned
    }
    pub fn stats(&self) -> DbStats {
        DbStats {
            pages: self.page_count,
            resident: self.pool.resident_count(),
            pinned: self.pool.pin_count(),
            wal_bytes: self.wal.used(),
            wal_reserved: self.wal.reserved(),
            next_lsn: self.wal.next_lsn(),
            durable_lsn: self.wal.durable_lsn(),
            evictions: self.evictions,
            page_writes: self.page_writes,
            active_txns: self.txns.len(),
        }
    }
    pub fn base_lsn(&self) -> u64 {
        self.wal.base_lsn()
    }
}
