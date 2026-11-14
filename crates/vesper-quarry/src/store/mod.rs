//! Durable SQL table store (§24): B+Tree tables over PAX leaf pages, MVCC
//! snapshot isolation with record locks, grouped commits, fuzzy checkpoints
//! and WAL crash recovery over a [`Device`].
//!
//! # Model
//!
//! * Every table is a B+Tree keyed by row version `(row id, !commit_ts)`;
//!   leaves store versions column by column (PAX). A delete is a tombstone
//!   version. Versions no snapshot can see are removed (on the next write
//!   of the row, or by [`Store::vacuum`]), empty leaves are freed into the
//!   free-space map and reused, and vacuum compacts leaf heaps.
//! * A transaction reads the snapshot of commits that were durable when it
//!   began, plus its own writes. Writes are buffered in the transaction and
//!   take a record lock (no-wait: `Conflict`); writing a row with a version
//!   newer than the snapshot is a `Conflict` (first committer wins). This is
//!   snapshot isolation: it prevents dirty reads, non-repeatable reads,
//!   phantoms and lost updates, and permits write skew.
//! * Commit applies the writes to private copies of the pages they touch,
//!   logs one *group* (full images or byte-range diffs, then a `Commit`
//!   record with the timestamp), and only then installs the pages in the
//!   buffer pool. A page can reach the data file only after the log holding
//!   its whole group is durable, so recovery never sees half a group: it
//!   redoes complete groups and ignores an incomplete trailing one.
//! * [`Store::commit`] syncs the log; [`Store::commit_deferred`] does not,
//!   and one [`Store::flush`] (or [`SharedStore`] across threads) makes
//!   every pending commit durable with one sync (group commit). A commit
//!   becomes visible to new snapshots once it is durable.
//! * Checkpoints are fuzzy: switching log regions does not wait for
//!   transactions or block commits; pages dirty at the switch are written
//!   back incrementally while commits continue, then the old region is
//!   dropped.
//!
//! See the crate README for formats, limits and what is not claimed.
mod btree;
mod error;
mod frames;
mod log;
mod pages;
mod shared;

pub use btree::TreeShape;
pub use error::{StoreError, StoreErrorKind, StoreResult};
pub use log::{LogTail, StoreRecord, LOG_HEADER_BYTES, MAX_LOG_PAYLOAD};
pub use shared::SharedStore;

use crate::device::{Device, FileId};
use crate::execute::{DataSet, QueryResult};
use crate::page::PAGE_SIZE;
use crate::plan::RelPlan;
use crate::vector::VectorEngine;
use crate::{
    identifier, ColumnDef, DdlChange, HermeticCatalog, Limits, ParamDef, ScalarType, Value,
};
use btree::{Pager, TreeCfg};
use error::{corrupt, limit, overflow};
use frames::{lock, Core, Io, SharedFrame};
use log::StoreLog;
use pages::{
    check_structure, decode_fsm, encode_fsm, format, kind, layout_of, leaf_compact, leaf_garbage,
    new_page, page_lsn, Entry, Key, Meta, Page, TableMeta, FSM_CAPACITY, FSM_PAGE, INTERNAL_CAP,
    KIND_FSM, KIND_INTERNAL, KIND_LEAF, KIND_META, META_PAGE,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

/// Largest variable-width payload (text and bytes) of one row, so that any
/// leaf can always be split into two pages.
pub const MAX_ROW_BYTES: usize = 4096;

/// Limits may be lowered, never raised above the hard ceilings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoreLimits {
    /// Resident page frames, 4..=4096.
    pub pool_frames: usize,
    /// Pages in the data file, 8..=130,816 (the free-space map's reach).
    pub max_pages: u64,
    /// Bytes of each of the two log regions, 64 KiB..=128 MiB (a multiple
    /// of 1 KiB). A region switch starts a checkpoint once half is used.
    pub log_region_bytes: u64,
    /// Concurrently active transactions, 1..=1024.
    pub max_active_txns: usize,
    /// Buffered row writes per transaction, 1..=100,000.
    pub max_txn_writes: usize,
    /// Pages one commit group may change, 8..=4096.
    pub max_group_pages: usize,
    /// Pages a checkpoint writes back after each commit, 1..=4096.
    pub checkpoint_step_pages: usize,
    /// Separator keys per internal node, 3..=817 (lower values build deep
    /// trees from few rows).
    pub btree_fanout: usize,
    /// Versions per leaf, 2..=4096 (further bounded by the page).
    pub leaf_max_versions: usize,
}
impl Default for StoreLimits {
    fn default() -> Self {
        Self {
            pool_frames: 64,
            max_pages: 16_384,
            log_region_bytes: 16 << 20,
            max_active_txns: 64,
            max_txn_writes: 10_000,
            max_group_pages: 1024,
            checkpoint_step_pages: 16,
            btree_fanout: INTERNAL_CAP,
            leaf_max_versions: 4096,
        }
    }
}
impl StoreLimits {
    fn validate(self) -> StoreResult<()> {
        let ok = (4..=4096).contains(&self.pool_frames)
            && (8..=FSM_CAPACITY).contains(&self.max_pages)
            && (64 << 10..=128 << 20).contains(&self.log_region_bytes)
            && self.log_region_bytes.is_multiple_of(1024)
            && (1..=1024).contains(&self.max_active_txns)
            && (1..=100_000).contains(&self.max_txn_writes)
            && (8..=4096).contains(&self.max_group_pages)
            && (1..=4096).contains(&self.checkpoint_step_pages)
            && (3..=INTERNAL_CAP).contains(&self.btree_fanout)
            && (2..=4096).contains(&self.leaf_max_versions);
        if ok {
            Ok(())
        } else {
            Err(limit("store limits out of range"))
        }
    }
    fn tree(self) -> TreeCfg {
        TreeCfg {
            fanout: self.btree_fanout,
            leaf_max: self.leaf_max_versions,
        }
    }
}

/// Active transaction owner. Not `Clone`; its only exits are
/// [`Store::commit`], [`Store::commit_deferred`] and [`Store::abort`].
/// Dropping it instead is counted by [`Store::leaked_txns`]; its locks and
/// snapshot then stay held until the store is reopened.
///
/// ```compile_fail
/// fn duplicate(t: vesper_quarry::store::Txn) { let _c = t.clone(); }
/// ```
#[must_use = "commit or abort the transaction"]
#[derive(Debug)]
pub struct Txn {
    id: u64,
    store: u64,
    snapshot: u64,
    leaks: Arc<AtomicU64>,
    finished: bool,
}
impl Txn {
    pub fn id(&self) -> u64 {
        self.id
    }
    /// Commit timestamp of the newest commit this transaction can see.
    pub fn snapshot(&self) -> u64 {
        self.snapshot
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
    snapshot: u64,
    /// (table id, row id) -> new row, or `None` for a delete.
    writes: BTreeMap<(u32, u64), Option<Vec<Value>>>,
}

/// Outcome of a commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Commit {
    /// Commit timestamp (the snapshot for a read-only transaction).
    pub ts: u64,
    /// LSN of the group's `Commit` record (0 for read-only transactions).
    pub lsn: u64,
    /// Whether the commit is durable when the call returns.
    pub durable: bool,
}

/// What [`Store::open`] found and did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoreRecovery {
    pub created: bool,
    /// Valid log records read (both regions).
    pub records: usize,
    /// Complete commit groups replayed.
    pub groups: usize,
    /// Records of an incomplete trailing group, ignored.
    pub incomplete: usize,
    /// The previous log region was still needed (a checkpoint was running).
    pub previous_region: bool,
    pub tail: Option<LogTail>,
    /// Logged page images installed over missing, torn or older pages.
    pub images_restored: usize,
    /// Page diffs reapplied.
    pub redone: usize,
    /// Records already reflected in their page (by page LSN).
    pub skipped: usize,
    /// Newest commit timestamp after recovery.
    pub last_ts: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StoreStats {
    pub pages: u64,
    pub free_pages: usize,
    pub resident: usize,
    pub dirty: usize,
    pub pinned: usize,
    pub leaked_pins: u64,
    pub log_region_used: u64,
    pub log_region_bytes: u64,
    pub log_flushes: u64,
    pub next_lsn: u64,
    pub durable_lsn: u64,
    pub evictions: u64,
    pub page_writes: u64,
    pub active_txns: usize,
    pub locks: usize,
    pub last_ts: u64,
    pub visible_ts: u64,
    pub pending_commits: usize,
    pub groups: u64,
    pub checkpoint_running: bool,
    pub checkpoints_begun: u64,
    pub checkpoints_done: u64,
    pub versions_collected: u64,
}

/// What [`Store::vacuum`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VacuumReport {
    pub versions_removed: u64,
    pub leaves_compacted: u64,
    pub groups: u64,
}

/// What readers see: the meta page and the SQL catalog, replaced as a whole
/// when a commit group installs.
#[derive(Clone, Debug)]
struct View {
    meta: Meta,
    catalog: HermeticCatalog,
}

/// State only the single writer (commit groups, DDL, vacuum, checkpoints)
/// changes; guarded by the writer mutex.
#[derive(Debug, Default)]
struct Writer {
    free: BTreeSet<u64>,
    /// Pages with a full image in the current log region.
    imaged: BTreeSet<u64>,
    /// Pages a running checkpoint still has to write back.
    ckpt: Option<BTreeSet<u64>>,
    groups: u64,
    checkpoints_begun: u64,
    checkpoints_done: u64,
    versions_collected: u64,
}

/// The transaction table, guarded by its own mutex (taken last: nothing
/// else is acquired while it is held).
#[derive(Debug, Default)]
struct Txns {
    active: BTreeMap<u64, TxnState>,
    locks: BTreeMap<(u32, u64), u64>,
    /// Next row id per table id (ahead of the meta page while uncommitted
    /// inserts exist; aborted ids are never reused).
    next_rowid: BTreeMap<u32, u64>,
    next_txn: u64,
    visible_ts: u64,
    /// Deferred commits not yet durable: (commit LSN, ts).
    pending: VecDeque<(u64, u64)>,
}

/// A durable SQL table store over a [`Device`].
///
/// Readers ([`Store::get`], [`Store::scan`], queries) and writers
/// ([`Store::insert`], [`Store::commit`], ...) take `&self` and may run on
/// many threads at once (`Store` is `Sync` when the device is `Send`):
/// readers traverse installed pages with latch coupling while one commit
/// group at a time is built and installed. DDL, vacuum, checkpoints and
/// the tree check take `&mut self`.
pub struct Store<D: Device> {
    id: u64,
    limits: StoreLimits,
    core: Core<D>,
    view: RwLock<Arc<View>>,
    writer: Mutex<Writer>,
    txns: Mutex<Txns>,
    leaks: Arc<AtomicU64>,
    poisoned: AtomicBool,
}
impl<D: Device> fmt::Debug for Store<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Store")
            .field("stats", &self.stats())
            .finish()
    }
}

/// Which tree a reader walks.
#[derive(Clone, Copy, Debug)]
enum Tree {
    Table(u32),
}
impl View {
    fn root(&self, tree: Tree) -> StoreResult<u64> {
        match tree {
            Tree::Table(tid) => self
                .meta
                .tables
                .values()
                .find(|t| t.id == tid)
                .map(|t| t.root)
                .ok_or_else(|| StoreError::new(StoreErrorKind::NotFound, "table dropped")),
        }
    }
    fn table(&self, name: &str) -> StoreResult<&TableMeta> {
        let name = identifier(name)?;
        self.meta
            .tables
            .get(&name)
            .ok_or_else(|| StoreError::new(StoreErrorKind::NotFound, format!("table {name}")))
    }
}

// ---- page staging for one group -----------------------------------------

/// Tree-order position of a page for exclusive latching: pages readers can
/// reach before the group by level (root first), leaves left to right by
/// their first key; unreachable pages after them.
type LatchKey = (u8, std::cmp::Reverse<u8>, Key, u64);
fn latch_key(id: u64, before: Option<&Page>, free_before: &BTreeSet<u64>) -> LatchKey {
    use std::cmp::Reverse;
    match before {
        Some(b) if !free_before.contains(&id) && kind(&b[..]) == KIND_LEAF => {
            let first = layout_of(&b[..])
                .ok()
                .filter(|_| pages::count(&b[..]) > 0)
                .map_or(Key::MIN, |lay| pages::leaf_key(&b[..], &lay, 0));
            (0, Reverse(0), first, id)
        }
        Some(b) if !free_before.contains(&id) && kind(&b[..]) == KIND_INTERNAL => {
            (0, Reverse(pages::level(&b[..])), Key::MIN, id)
        }
        _ => (1, Reverse(0), Key::MIN, id),
    }
}

/// What a finished group logs and installs.
struct Finished {
    records: Vec<StoreRecord>,
    /// (latch order, page, new bytes or `None` for a freed page to latch).
    plan: Vec<(LatchKey, u64, Option<Box<Page>>)>,
    meta: Meta,
    free: BTreeSet<u64>,
}

/// Private copies of the pages one commit group reads and changes.
struct Staging<'a, D: Device> {
    core: &'a Core<D>,
    /// page -> (bytes before the group, or `None` for a page new to the
    /// file; bytes now)
    pages: BTreeMap<u64, (Option<Box<Page>>, Box<Page>)>,
    meta: Meta,
    free: BTreeSet<u64>,
    free_before: BTreeSet<u64>,
    max_pages: u64,
    max_group_pages: usize,
}
impl<D: Device> Staging<'_, D> {
    fn stage(&mut self, id: u64) -> StoreResult<()> {
        if self.pages.contains_key(&id) {
            return Ok(());
        }
        if id >= self.meta.page_count {
            return Err(corrupt(format!("page {id} is beyond the store")));
        }
        if self.pages.len() >= self.max_group_pages {
            return Err(limit("commit group touches too many pages"));
        }
        let copy = self.core.copy(id)?;
        self.pages.insert(id, (Some(copy.clone()), copy));
        Ok(())
    }
    /// Log records for every changed page plus the commit record; the
    /// install plan in latch order.
    fn finish(mut self, imaged: &BTreeSet<u64>, txn: u64, ts: u64) -> StoreResult<Finished> {
        let meta = self.meta.clone();
        meta.encode(&mut self.page_mut(META_PAGE)?[..])?;
        let free = self.free.clone();
        encode_fsm(&mut self.page_mut(FSM_PAGE)?[..], &free);
        let mut records = Vec::new();
        let mut plan = Vec::new();
        for (id, (before, now)) in self.pages {
            let key = latch_key(id, before.as_deref(), &self.free_before);
            let record = match &before {
                Some(b) if imaged.contains(&id) => {
                    let ranges = diff(&b[..], &now[..]);
                    if ranges.is_empty() {
                        None
                    } else {
                        Some(StoreRecord::PageDiff {
                            page: id,
                            prev_lsn: page_lsn(&b[..]),
                            ranges,
                        })
                    }
                }
                Some(b) if diff(&b[..], &now[..]).is_empty() => None,
                _ => {
                    let mut bytes = now.to_vec();
                    bytes[..8].fill(0);
                    bytes[16..20].fill(0);
                    Some(StoreRecord::PageImage { page: id, bytes })
                }
            };
            match record {
                Some(r) => {
                    records.push(r);
                    plan.push((key, id, Some(now)));
                }
                // A page freed by this group: latched, so no reader stays.
                None if free.contains(&id) && !self.free_before.contains(&id) => {
                    plan.push((key, id, None));
                }
                None => {}
            }
        }
        records.push(StoreRecord::Commit { txn, ts });
        Ok(Finished {
            records,
            plan,
            meta: self.meta,
            free: self.free,
        })
    }
}
impl<D: Device> Pager for Staging<'_, D> {
    fn page(&mut self, id: u64) -> StoreResult<&Page> {
        self.stage(id)?;
        Ok(&self.pages[&id].1)
    }
    fn page_mut(&mut self, id: u64) -> StoreResult<&mut Page> {
        self.stage(id)?;
        Ok(&mut self.pages.get_mut(&id).expect("staged").1)
    }
    fn alloc(&mut self) -> StoreResult<u64> {
        if let Some(&id) = self.free.iter().next() {
            self.stage(id)?;
            self.free.remove(&id);
            let now = &mut self.pages.get_mut(&id).expect("staged").1;
            format(&mut now[..], id, 0, 0);
            return Ok(id);
        }
        let id = self.meta.page_count;
        if id >= self.max_pages {
            return Err(limit("page limit reached"));
        }
        if self.pages.len() >= self.max_group_pages {
            return Err(limit("commit group touches too many pages"));
        }
        self.meta.page_count += 1;
        let mut page = new_page();
        format(&mut page[..], id, 0, 0);
        self.pages.insert(id, (None, page));
        Ok(id)
    }
    fn free(&mut self, id: u64) -> StoreResult<()> {
        if id <= FSM_PAGE || id >= self.meta.page_count || !self.free.insert(id) {
            return Err(corrupt(format!("freeing page {id} twice or out of range")));
        }
        self.stage(id)
    }
}

/// Changed byte runs, skipping page LSN and checksum, merged across short
/// equal gaps.
fn diff(before: &[u8], after: &[u8]) -> Vec<(u16, Vec<u8>)> {
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < PAGE_SIZE {
        if pages::diff_skips(i) || before[i] == after[i] {
            i += 1;
            continue;
        }
        let start = i;
        while i < PAGE_SIZE && !pages::diff_skips(i) && before[i] != after[i] {
            i += 1;
        }
        match ranges.last_mut() {
            Some(last) if start - last.1 < 8 && !(last.1..start).any(pages::diff_skips) => {
                last.1 = i;
            }
            _ => ranges.push((start, i)),
        }
    }
    ranges
        .into_iter()
        .map(|(s, e)| (s as u16, after[s..e].to_vec()))
        .collect()
}

/// Read-only page access by the writer (one cached page at a time). Only
/// the writer changes pages, so its reads need no coupling.
struct Reader<'a, D: Device> {
    core: &'a Core<D>,
    page_count: u64,
    current: Option<(u64, Box<Page>)>,
}
impl<D: Device> Pager for Reader<'_, D> {
    fn page(&mut self, id: u64) -> StoreResult<&Page> {
        if self.current.as_ref().is_none_or(|(p, _)| *p != id) {
            if id >= self.page_count {
                return Err(corrupt(format!("page {id} is beyond the store")));
            }
            self.current = Some((id, self.core.copy(id)?));
        }
        Ok(&self.current.as_ref().expect("cached").1)
    }
    fn page_mut(&mut self, _: u64) -> StoreResult<&mut Page> {
        Err(StoreError::new(
            StoreErrorKind::State,
            "read-only page access",
        ))
    }
    fn alloc(&mut self) -> StoreResult<u64> {
        Err(StoreError::new(
            StoreErrorKind::State,
            "read-only page access",
        ))
    }
    fn free(&mut self, _: u64) -> StoreResult<()> {
        Err(StoreError::new(
            StoreErrorKind::State,
            "read-only page access",
        ))
    }
}

/// Versions of one row, newest first: (key, tombstone).
fn row_versions(p: &mut dyn Pager, root: u64, rowid: u64) -> StoreResult<Vec<(Key, bool)>> {
    let mut out = Vec::new();
    btree::visit(
        p,
        root,
        Key { rowid, inv_ts: 0 },
        Key {
            rowid,
            inv_ts: u64::MAX,
        },
        &mut |e| {
            out.push((e.key, e.tomb));
            Ok(true)
        },
    )?;
    Ok(out)
}
/// Versions no snapshot can see. `versions` is newest first; `snapshots`
/// holds every active snapshot plus the visible timestamp (the snapshot of
/// every future transaction). A version is kept if it is newer than every
/// snapshot (not yet visible) or is the newest version at or below some
/// snapshot; then kept tombstones with nothing older are dropped too.
fn collectable(versions: &[(Key, bool)], snapshots: &BTreeSet<u64>) -> Vec<Key> {
    let newest = snapshots.iter().next_back().copied().unwrap_or(0);
    let mut keep: Vec<bool> = versions.iter().map(|(k, _)| k.ts() > newest).collect();
    for &s in snapshots {
        if let Some(i) = versions.iter().position(|(k, _)| k.ts() <= s) {
            keep[i] = true;
        }
    }
    // A kept tombstone with no kept version below it reads the same as no
    // version at all.
    for i in (0..versions.len()).rev() {
        if !keep[i] {
            continue;
        }
        if versions[i].1 {
            keep[i] = false;
        } else {
            break;
        }
    }
    versions
        .iter()
        .zip(keep)
        .filter(|(_, k)| !k)
        .map(|((key, _), _)| *key)
        .collect()
}

fn table_types(t: &TableMeta) -> Vec<ScalarType> {
    t.columns.iter().map(|c| c.ty).collect()
}

fn state_error(message: &str) -> StoreError {
    StoreError::new(StoreErrorKind::State, message)
}

impl<D: Device> Store<D> {
    /// Opens (creating if empty) and recovers a store, then checkpoints.
    ///
    /// Recovery syncs the log, reads both regions when a checkpoint was
    /// running, redoes every complete commit group (installing logged page
    /// images over missing, torn or older pages, and applying a diff only to
    /// a page at exactly the diff's predecessor LSN), ignores an incomplete
    /// trailing group, writes every page back, syncs, and starts a fresh log
    /// generation. A crash at any point of recovery recovers the same way.
    pub fn open(mut dev: D, limits: StoreLimits) -> StoreResult<(Self, StoreRecovery)> {
        limits.validate()?;
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let mut report = StoreRecovery::default();
        dev.sync(FileId::Wal)?;
        let data_len = dev.len(FileId::Data)?;
        let scan = log::scan(&mut dev)?;
        let Some(scan) = scan else {
            if data_len != 0 || dev.len(FileId::Wal)? > LOG_HEADER_BYTES {
                return Err(corrupt("existing store without a valid log header"));
            }
            let log = StoreLog::reset(&mut dev, limits.log_region_bytes, 0, 1)?;
            let store = Self::assemble(id, dev, log, limits, 0);
            store.initialize()?;
            report.created = true;
            return Ok((store, report));
        };
        report.records = scan.records.len();
        report.tail = scan.tail;
        report.previous_region = scan.prev_needed;
        let logged = scan
            .records
            .iter()
            .filter_map(|(_, r)| r.page())
            .max()
            .map_or(0, |p| p + 1);
        let page_bound = data_len.div_ceil(PAGE_SIZE as u64).max(logged);
        if page_bound > FSM_CAPACITY {
            return Err(corrupt("store larger than the free-space map"));
        }
        let log = StoreLog::recovering(&scan, limits.log_region_bytes);
        let (hseq, next_lsn) = (scan.hseq, scan.next_lsn);
        let store = Self::assemble(id, dev, log, limits, page_bound);
        store.redo(scan.records, &mut report)?;
        // Sharp checkpoint: every page durable, then a fresh log.
        for page in store.core.pool.dirty_pages() {
            store.core.write_back(page)?;
        }
        {
            let mut io = store.core.io();
            io.dev.sync(FileId::Data)?;
            let Io { dev, log, .. } = &mut *io;
            *log = StoreLog::reset(dev, limits.log_region_bytes, hseq, next_lsn)?;
        }
        let initialized = page_bound > FSM_PAGE
            && (store.core.pool.resident(META_PAGE) || store.core.read_disk(META_PAGE)?.is_some());
        if !initialized {
            // The log never held a complete initialization group.
            store.core.set_page_count(0);
            store.initialize()?;
            report.created = true;
            return Ok((store, report));
        }
        store.core.set_page_count(page_bound.max(2));
        let meta = Meta::decode(&store.core.copy(META_PAGE)?[..])?;
        if meta.page_count > limits.max_pages {
            return Err(limit("store has more pages than max_pages"));
        }
        store.core.set_page_count(meta.page_count);
        let free = decode_fsm(&store.core.copy(FSM_PAGE)?[..])?;
        if free.iter().any(|&p| p >= meta.page_count) {
            return Err(corrupt("free-space map names pages beyond the store"));
        }
        lock(&store.writer).free = free;
        let catalog = HermeticCatalog::from_parts(
            meta.tables
                .iter()
                .map(|(n, t)| (n.clone(), t.columns.clone()))
                .collect(),
            meta.catalog_revision,
        );
        {
            let mut t = store.txns();
            t.next_rowid = meta.tables.values().map(|t| (t.id, t.next_rowid)).collect();
            t.visible_ts = meta.last_ts;
        }
        report.last_ts = meta.last_ts;
        store.publish(View { meta, catalog });
        Ok((store, report))
    }

    fn assemble(id: u64, dev: D, log: StoreLog, limits: StoreLimits, pages: u64) -> Self {
        Self {
            id,
            limits,
            core: Core::new(dev, log, limits.pool_frames, pages),
            view: RwLock::new(Arc::new(View {
                meta: Meta::new(),
                catalog: HermeticCatalog::default(),
            })),
            writer: Mutex::new(Writer::default()),
            txns: Mutex::new(Txns {
                next_txn: 1,
                ..Txns::default()
            }),
            leaks: Arc::new(AtomicU64::new(0)),
            poisoned: AtomicBool::new(false),
        }
    }

    fn view(&self) -> Arc<View> {
        Arc::clone(&self.view.read().unwrap_or_else(|e| e.into_inner()))
    }
    fn publish(&self, view: View) {
        *self.view.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(view);
    }
    fn txns(&self) -> MutexGuard<'_, Txns> {
        lock(&self.txns)
    }
    fn writer(&self) -> MutexGuard<'_, Writer> {
        lock(&self.writer)
    }

    /// Writes the meta and free-space-map pages as the first group.
    fn initialize(&self) -> StoreResult<()> {
        let meta = Meta::new();
        let mut m = new_page();
        format(&mut m[..], META_PAGE, KIND_META, 0);
        meta.encode(&mut m[..])?;
        let mut f = new_page();
        format(&mut f[..], FSM_PAGE, KIND_FSM, 0);
        encode_fsm(&mut f[..], &BTreeSet::new());
        let records = vec![
            StoreRecord::PageImage {
                page: META_PAGE,
                bytes: m.to_vec(),
            },
            StoreRecord::PageImage {
                page: FSM_PAGE,
                bytes: f.to_vec(),
            },
            StoreRecord::Commit { txn: 0, ts: 0 },
        ];
        let lsns = {
            let mut io = self.core.io();
            let lsns = io.log.append_group(&records)?;
            let Io { dev, log, .. } = &mut *io;
            log.flush(dev)?;
            lsns
        };
        self.core.set_page_count(2);
        self.core.install(META_PAGE, &m, lsns[0])?;
        self.core.install(FSM_PAGE, &f, lsns[1])?;
        self.writer().imaged.extend([META_PAGE, FSM_PAGE]);
        self.publish(View {
            meta,
            catalog: HermeticCatalog::default(),
        });
        Ok(())
    }

    fn redo(&self, records: Vec<(u64, StoreRecord)>, report: &mut StoreRecovery) -> StoreResult<()> {
        let complete = records
            .iter()
            .rposition(|(_, r)| matches!(r, StoreRecord::Commit { .. }))
            .map_or(0, |i| i + 1);
        report.incomplete = records.len() - complete;
        for (lsn, record) in records.into_iter().take(complete) {
            match record {
                StoreRecord::Commit { .. } => report.groups += 1,
                StoreRecord::PageImage { page, bytes } => {
                    let mut image = new_page();
                    image.copy_from_slice(&bytes);
                    check_structure(&image, page)?;
                    if !self.core.pool.resident(page) {
                        match self.lenient_disk(page)? {
                            Some(disk) if page_lsn(&disk[..]) >= lsn => {
                                self.core.insert_clean(page, disk)?;
                                report.skipped += 1;
                                continue;
                            }
                            _ => {
                                self.core.install(page, &image, lsn)?;
                                report.images_restored += 1;
                                continue;
                            }
                        }
                    }
                    let current = self.core.copy(page)?;
                    if page_lsn(&current[..]) >= lsn {
                        report.skipped += 1;
                    } else {
                        self.core.install(page, &image, lsn)?;
                        report.redone += 1;
                    }
                }
                StoreRecord::PageDiff {
                    page,
                    prev_lsn,
                    ranges,
                } => {
                    if !self.core.pool.resident(page) {
                        let disk = self.lenient_disk(page)?.ok_or_else(|| {
                            corrupt(format!(
                                "page {page} is missing or torn and no logged image covers it"
                            ))
                        })?;
                        self.core.insert_clean(page, disk)?;
                    }
                    let mut current = self.core.copy(page)?;
                    let at = page_lsn(&current[..]);
                    if at >= lsn {
                        report.skipped += 1;
                        continue;
                    }
                    if at != prev_lsn {
                        return Err(corrupt(format!(
                            "page {page} is at LSN {at}, its diff at LSN {lsn} expects {prev_lsn}"
                        )));
                    }
                    for (offset, bytes) in ranges {
                        let o = offset as usize;
                        current[o..o + bytes.len()].copy_from_slice(&bytes);
                    }
                    check_structure(&current, page)?;
                    self.core.install(page, &current, lsn)?;
                    report.redone += 1;
                }
            }
        }
        Ok(())
    }
    /// A page from the data file, or `None` when missing or invalid (torn).
    fn lenient_disk(&self, page: u64) -> StoreResult<Option<Box<Page>>> {
        match self.core.read_disk(page) {
            Ok(p) => Ok(p),
            Err(e) if e.kind == StoreErrorKind::Corrupt => Ok(None),
            Err(e) => Err(e),
        }
    }

    // ---- state checks --------------------------------------------------

    fn check_usable(&self) -> StoreResult<()> {
        if self.poisoned.load(Ordering::Acquire) {
            Err(state_error(
                "store is poisoned by an earlier I/O failure; reopen it",
            ))
        } else {
            Ok(())
        }
    }
    fn poison(&self) {
        self.poisoned.store(true, Ordering::Release);
    }
    fn poison_on<T>(&self, r: StoreResult<T>) -> StoreResult<T> {
        if let Err(e) = &r {
            if e.poisons() {
                self.poison();
            }
        }
        r
    }
    fn check_store(&self, txn: &Txn) -> StoreResult<()> {
        if txn.store != self.id {
            return Err(StoreError::new(
                StoreErrorKind::WrongStore,
                "transaction of another store",
            ));
        }
        Ok(())
    }
    /// Commits durable now: advance the visible snapshot.
    fn refresh_visible(&self) {
        let durable = self.core.io().log.durable_lsn();
        let mut t = self.txns();
        while let Some(&(lsn, ts)) = t.pending.front() {
            if lsn > durable {
                break;
            }
            t.visible_ts = ts;
            t.pending.pop_front();
        }
    }
    /// Every snapshot a present or future transaction can read.
    fn snapshots(&self) -> BTreeSet<u64> {
        let t = self.txns();
        let mut out: BTreeSet<u64> = t.active.values().map(|s| s.snapshot).collect();
        out.insert(t.visible_ts);
        out
    }
    fn reader(&self) -> Reader<'_, D> {
        Reader {
            page_count: self.core.page_count(),
            core: &self.core,
            current: None,
        }
    }

    // ---- latch-coupled reads --------------------------------------------

    /// Visits entries of `tree` with `lo <= key <= hi` in key order until `f`
    /// returns `false`. The root is latched while the view naming it is
    /// held; each child (and each right sibling across leaves) is latched
    /// before its parent (or left neighbour) is released, so the walk never
    /// sees a commit group half installed and never stands on a freed page.
    fn read_tree(
        &self,
        tree: Tree,
        lo: Key,
        hi: Key,
        f: &mut dyn FnMut(Entry) -> StoreResult<bool>,
    ) -> StoreResult<()> {
        let first = {
            let view = self.view.read().unwrap_or_else(|e| e.into_inner());
            let root = view.root(tree)?;
            self.core.fix_shared(root)?
        };
        let mut cur = Some(first);
        let r = self.walk(&mut cur, lo, hi, f);
        let released = match cur.take() {
            Some(pin) => self.core.unpin_clean(pin),
            None => Ok(()),
        };
        let r = r.and(released);
        self.poison_on(r)
    }
    fn walk(
        &self,
        cur: &mut Option<SharedFrame>,
        lo: Key,
        hi: Key,
        f: &mut dyn FnMut(Entry) -> StoreResult<bool>,
    ) -> StoreResult<()> {
        enum Step {
            Child(u64, u8),
            Leaf(Vec<Entry>, u64),
        }
        let mut expect: Option<u8> = None;
        let bound = self.core.page_count().saturating_add(1);
        let mut steps = 0u64;
        while steps <= bound {
            steps += 1;
            let pin = cur.as_ref().expect("a latched page");
            let step = self.core.view(pin, |b| -> StoreResult<Step> {
                if expect.is_some_and(|l| l != pages::level(&b[..])) {
                    return Err(corrupt("B+Tree: child level"));
                }
                match kind(&b[..]) {
                    KIND_INTERNAL => {
                        let lvl = pages::level(&b[..]);
                        let at = pages::route(&b[..], lo);
                        Ok(Step::Child(pages::child(&b[..], at), lvl - 1))
                    }
                    KIND_LEAF => {
                        let (_, entries) = pages::leaf_decode(&b[..])?;
                        Ok(Step::Leaf(entries, pages::sibling(&b[..])))
                    }
                    _ => Err(corrupt("B+Tree: unexpected page kind")),
                }
            })??;
            let next = match step {
                Step::Child(c, lvl) => {
                    expect = Some(lvl);
                    c
                }
                Step::Leaf(entries, sib) => {
                    for e in entries {
                        if e.key < lo {
                            continue;
                        }
                        if e.key > hi || !f(e)? {
                            return Ok(());
                        }
                    }
                    if sib == 0 {
                        return Ok(());
                    }
                    expect = Some(0);
                    sib
                }
            };
            let pin = self.core.fix_shared(next)?;
            let old = cur.replace(pin).expect("a latched page");
            self.core.unpin_clean(old)?;
        }
        Err(corrupt("B+Tree: walk does not terminate"))
    }

    // ---- DDL -----------------------------------------------------------

    /// Applies `CREATE TABLE` / `DROP TABLE` statements atomically and
    /// durably (one system group, synced). Requires no active transaction.
    /// Dropping a table frees all its pages.
    pub fn execute_ddl(&mut self, sql: &str) -> StoreResult<()> {
        self.check_usable()?;
        if !self.txns().active.is_empty() {
            return Err(state_error("DDL requires no active transactions"));
        }
        let mut next = self.view().catalog.clone();
        let changes = next.apply_ddl_changes(sql, Limits::default())?;
        let cfg = self.limits.tree();
        let revision = next.revision();
        let mut w = lock(&self.writer);
        let r = self.run_group(&mut w, 0, 0, Some(&next), &mut |st| {
            for change in &changes {
                match change {
                    DdlChange::Create(name, columns) => {
                        let types: Vec<ScalarType> = columns.iter().map(|c| c.ty).collect();
                        let root = btree::new_leaf(st, &types, cfg)?;
                        let id = st.meta.next_table_id;
                        st.meta.next_table_id =
                            id.checked_add(1).ok_or_else(|| overflow("table ids"))?;
                        st.meta.tables.insert(
                            name.clone(),
                            TableMeta {
                                id,
                                root,
                                next_rowid: 1,
                                columns: columns.clone(),
                            },
                        );
                    }
                    DdlChange::Drop(name) => {
                        let t = st.meta.tables.remove(name).ok_or_else(|| {
                            corrupt(format!("catalog and meta page disagree on {name}"))
                        })?;
                        let mut all = Vec::new();
                        btree::pages(st, t.root, &mut all)?;
                        for p in all {
                            st.free(p)?;
                        }
                    }
                }
            }
            st.meta.catalog_revision = revision;
            Ok(())
        });
        drop(w);
        let r = r.and_then(|_| self.flush_log());
        let r = self.poison_on(r);
        self.refresh_visible();
        r?;
        let view = self.view();
        self.txns().next_rowid = view
            .meta
            .tables
            .values()
            .map(|t| (t.id, t.next_rowid))
            .collect();
        Ok(())
    }
    fn flush_log(&self) -> StoreResult<()> {
        let mut io = self.core.io();
        let Io { dev, log, .. } = &mut *io;
        log.flush(dev)
    }

    /// The SQL catalog of the durable tables.
    pub fn catalog(&self) -> HermeticCatalog {
        self.view().catalog.clone()
    }

    // ---- transactions ----------------------------------------------------

    /// Starts a transaction reading the snapshot of every durable commit.
    pub fn begin(&self) -> StoreResult<Txn> {
        self.check_usable()?;
        self.refresh_visible();
        let mut t = self.txns();
        if t.active.len() >= self.limits.max_active_txns {
            return Err(limit("too many active transactions"));
        }
        let id = t.next_txn;
        t.next_txn = id
            .checked_add(1)
            .ok_or_else(|| overflow("transaction ids"))?;
        let snapshot = t.visible_ts;
        t.active.insert(
            id,
            TxnState {
                snapshot,
                writes: BTreeMap::new(),
            },
        );
        Ok(Txn {
            id,
            store: self.id,
            snapshot,
            leaks: Arc::clone(&self.leaks),
            finished: false,
        })
    }

    fn validate_row(t: &TableMeta, row: &[Value]) -> StoreResult<()> {
        if row.len() != t.columns.len() {
            return Err(StoreError::new(StoreErrorKind::Data, "row width"));
        }
        let mut var = 0usize;
        for (v, c) in row.iter().zip(&t.columns) {
            if !v.valid(c.ty, c.nullable) {
                return Err(StoreError::new(
                    StoreErrorKind::Data,
                    format!("invalid value for column {}", c.name),
                ));
            }
            var += match v {
                Value::Text(s) => s.len(),
                Value::Bytes(b) => b.len(),
                _ => 0,
            };
        }
        if var > MAX_ROW_BYTES {
            return Err(limit("row exceeds MAX_ROW_BYTES"));
        }
        Ok(())
    }

    /// Inserts a row; returns its new row id.
    pub fn insert(&self, txn: &mut Txn, table: &str, row: &[Value]) -> StoreResult<u64> {
        self.check_usable()?;
        self.check_store(txn)?;
        let view = self.view();
        let t = view.table(table)?;
        Self::validate_row(t, row)?;
        let tid = t.id;
        let mut g = self.txns();
        let st = &mut *g;
        let state = st
            .active
            .get_mut(&txn.id)
            .ok_or_else(|| state_error("transaction is not active"))?;
        if state.writes.len() >= self.limits.max_txn_writes {
            return Err(limit("transaction write limit"));
        }
        let next = st.next_rowid.entry(tid).or_insert(1);
        let rowid = *next;
        *next = rowid.checked_add(1).ok_or_else(|| overflow("row ids"))?;
        st.locks.insert((tid, rowid), txn.id);
        state.writes.insert((tid, rowid), Some(row.to_vec()));
        Ok(rowid)
    }
    /// Replaces a row visible to the transaction.
    pub fn update(&self, txn: &mut Txn, table: &str, rowid: u64, row: &[Value]) -> StoreResult<()> {
        self.check_usable()?;
        self.check_store(txn)?;
        let view = self.view();
        let t = view.table(table)?;
        Self::validate_row(t, row)?;
        let tid = t.id;
        self.write_row(txn, tid, rowid, Some(row.to_vec()))
    }
    /// Deletes a row visible to the transaction.
    pub fn delete(&self, txn: &mut Txn, table: &str, rowid: u64) -> StoreResult<()> {
        self.check_usable()?;
        self.check_store(txn)?;
        let tid = self.view().table(table)?.id;
        self.write_row(txn, tid, rowid, None)
    }
    /// Takes the record lock first (no-wait), then checks the committed
    /// versions: a lock holder's versions are installed before it releases
    /// the lock, so a version committed after this snapshot is always seen.
    fn write_row(
        &self,
        txn: &Txn,
        tid: u32,
        rowid: u64,
        row: Option<Vec<Value>>,
    ) -> StoreResult<()> {
        let snapshot = {
            let mut g = self.txns();
            let st = &mut *g;
            let state = st
                .active
                .get_mut(&txn.id)
                .ok_or_else(|| state_error("transaction is not active"))?;
            if let Some(&owner) = st.locks.get(&(tid, rowid)) {
                if owner != txn.id {
                    return Err(StoreError::new(
                        StoreErrorKind::Conflict,
                        format!("row {rowid} is locked by transaction {owner}"),
                    ));
                }
            }
            match state.writes.get(&(tid, rowid)).map(Option::is_some) {
                Some(false) => {
                    return Err(StoreError::new(
                        StoreErrorKind::NotFound,
                        "row deleted by this transaction",
                    ));
                }
                Some(true) => {
                    state.writes.insert((tid, rowid), row);
                    return Ok(());
                }
                None => {}
            }
            if state.writes.len() >= self.limits.max_txn_writes {
                return Err(limit("transaction write limit"));
            }
            st.locks.insert((tid, rowid), txn.id);
            state.snapshot
        };
        let checked = self.check_writable(tid, rowid, snapshot);
        let mut g = self.txns();
        let st = &mut *g;
        match checked {
            Ok(()) => {
                st.active
                    .get_mut(&txn.id)
                    .expect("active")
                    .writes
                    .insert((tid, rowid), row);
                Ok(())
            }
            Err(e) => {
                st.locks.remove(&(tid, rowid));
                Err(e)
            }
        }
    }
    fn check_writable(&self, tid: u32, rowid: u64, snapshot: u64) -> StoreResult<()> {
        let mut newest: Option<(Key, bool)> = None;
        let mut visible: Option<bool> = None;
        self.read_tree(
            Tree::Table(tid),
            Key { rowid, inv_ts: 0 },
            Key {
                rowid,
                inv_ts: u64::MAX,
            },
            &mut |e| {
                newest.get_or_insert((e.key, e.tomb));
                if e.key.ts() <= snapshot {
                    visible = Some(e.tomb);
                    return Ok(false);
                }
                Ok(true)
            },
        )?;
        if let Some((k, _)) = newest {
            if k.ts() > snapshot {
                return Err(StoreError::new(
                    StoreErrorKind::Conflict,
                    format!(
                        "row {rowid} has a version committed after this snapshot (first committer wins)"
                    ),
                ));
            }
        }
        match visible {
            Some(false) => Ok(()),
            _ => Err(StoreError::new(
                StoreErrorKind::NotFound,
                format!("row {rowid}"),
            )),
        }
    }

    /// The row as the transaction sees it.
    pub fn get(&self, txn: &Txn, table: &str, rowid: u64) -> StoreResult<Option<Vec<Value>>> {
        self.check_usable()?;
        self.check_store(txn)?;
        let tid = self.view().table(table)?.id;
        {
            let t = self.txns();
            let state = t
                .active
                .get(&txn.id)
                .ok_or_else(|| state_error("transaction is not active"))?;
            if let Some(w) = state.writes.get(&(tid, rowid)) {
                return Ok(w.clone());
            }
        }
        let snapshot = txn.snapshot;
        let mut found = None;
        self.read_tree(
            Tree::Table(tid),
            Key::version(rowid, snapshot),
            Key {
                rowid,
                inv_ts: u64::MAX,
            },
            &mut |e| {
                found = Some(e);
                Ok(false)
            },
        )?;
        Ok(found.filter(|e| !e.tomb).map(|e| e.values))
    }

    /// Every row the transaction sees, by row id.
    pub fn scan(&self, txn: &Txn, table: &str) -> StoreResult<Vec<(u64, Vec<Value>)>> {
        self.check_usable()?;
        self.check_store(txn)?;
        let tid = self.view().table(table)?.id;
        if !self.txns().active.contains_key(&txn.id) {
            return Err(state_error("transaction is not active"));
        }
        let snapshot = txn.snapshot;
        let mut rows: BTreeMap<u64, Vec<Value>> = BTreeMap::new();
        let mut last = None;
        self.read_tree(Tree::Table(tid), Key::MIN, Key::MAX, &mut |e| {
            if last != Some(e.key.rowid) && e.key.ts() <= snapshot {
                last = Some(e.key.rowid);
                if !e.tomb {
                    rows.insert(e.key.rowid, e.values);
                }
            }
            Ok(true)
        })?;
        let t = self.txns();
        let state = t
            .active
            .get(&txn.id)
            .ok_or_else(|| state_error("transaction is not active"))?;
        for ((t, rowid), w) in state.writes.range((tid, 0)..=(tid, u64::MAX)) {
            debug_assert_eq!(*t, tid);
            match w {
                Some(row) => rows.insert(*rowid, row.clone()),
                None => rows.remove(rowid),
            };
        }
        Ok(rows.into_iter().collect())
    }

    /// Discards the transaction's writes and releases its locks.
    pub fn abort(&self, mut txn: Txn) -> StoreResult<()> {
        self.check_usable()?;
        self.check_store(&txn)?;
        let mut t = self.txns();
        if t.active.remove(&txn.id).is_none() {
            return Err(state_error("transaction is not active"));
        }
        txn.finished = true;
        t.locks.retain(|_, owner| *owner != txn.id);
        Ok(())
    }

    /// Commits and syncs the log: `durable` is true on `Ok`.
    pub fn commit(&self, txn: Txn) -> StoreResult<Commit> {
        let mut c = self.commit_deferred(txn)?;
        if !c.durable {
            self.flush()?;
            c.durable = true;
        }
        Ok(c)
    }

    /// Commits without syncing: the group is in the log buffer and its pages
    /// in the pool, but the commit is durable (and visible to new snapshots)
    /// only after a later [`Store::flush`], a synced commit, or a log write
    /// forced by page write-back. A crash before that loses it.
    pub fn commit_deferred(&self, mut txn: Txn) -> StoreResult<Commit> {
        self.check_usable()?;
        self.check_store(&txn)?;
        let state = self
            .txns()
            .active
            .remove(&txn.id)
            .ok_or_else(|| state_error("transaction is not active"))?;
        txn.finished = true;
        let release = |store: &Self| store.txns().locks.retain(|_, owner| *owner != txn.id);
        if state.writes.is_empty() {
            return Ok(Commit {
                ts: state.snapshot,
                lsn: 0,
                durable: true,
            });
        }
        let mut w = lock(&self.writer);
        let r = self.commit_group(&mut w, txn.id, &state);
        let r = self.poison_on(r);
        let (lsn, ts) = match r {
            Ok(x) => x,
            Err(e) => {
                drop(w);
                release(self);
                return Err(e);
            }
        };
        // Locks are released only after the versions are installed, so a
        // later writer of the same row sees this commit's version.
        {
            let mut t = self.txns();
            t.locks.retain(|_, owner| *owner != txn.id);
            t.pending.push_back((lsn, ts));
        }
        drop(w);
        self.refresh_visible();
        Ok(Commit {
            ts,
            lsn,
            durable: self.core.io().log.durable_lsn() >= lsn,
        })
    }
    /// Builds and installs a transaction's group; returns (commit LSN, ts).
    fn commit_group(&self, w: &mut Writer, txn: u64, state: &TxnState) -> StoreResult<(u64, u64)> {
        let view = self.view();
        let ts = view
            .meta
            .last_ts
            .checked_add(1)
            .ok_or_else(|| overflow("commit timestamps"))?;
        let snapshots = self.snapshots();
        let cfg = self.limits.tree();
        let tables: BTreeMap<u32, (String, Vec<ScalarType>)> = view
            .meta
            .tables
            .iter()
            .map(|(n, t)| (t.id, (n.clone(), table_types(t))))
            .collect();
        let mut collected = 0u64;
        let lsn = self.run_group(w, txn, ts, None, &mut |st| {
            collected = 0;
            for (&(tid, rowid), row) in &state.writes {
                let (name, types) = &tables[&tid];
                let root = st.meta.tables[name].root;
                let entry = Entry {
                    key: Key::version(rowid, ts),
                    tomb: row.is_none(),
                    values: row.clone().unwrap_or_default(),
                };
                let mut root = btree::insert(st, cfg, types, root, entry)?;
                let versions = row_versions(st, root, rowid)?;
                for key in collectable(&versions, &snapshots) {
                    let (r, found) = btree::remove(st, cfg, types, root, key)?;
                    if !found {
                        return Err(corrupt("collectable version vanished"));
                    }
                    root = r;
                    collected += 1;
                }
                let t = st.meta.tables.get_mut(name).expect("table");
                t.root = root;
                t.next_rowid = t.next_rowid.max(rowid + 1);
            }
            st.meta.last_ts = ts;
            Ok(())
        })?;
        w.versions_collected += collected;
        Ok((lsn, ts))
    }

    /// Syncs the log: every deferred commit becomes durable and visible.
    /// Returns the newest visible commit timestamp.
    pub fn flush(&self) -> StoreResult<u64> {
        self.check_usable()?;
        let r = self.flush_log();
        self.poison_on(r)?;
        self.refresh_visible();
        Ok(self.txns().visible_ts)
    }
    pub fn durable_lsn(&self) -> u64 {
        self.core.io().log.durable_lsn()
    }

    /// Builds, logs and installs one commit group. On `LogFull` it finishes
    /// any running checkpoint, switches log regions and retries once.
    /// `catalog` replaces the published catalog (DDL).
    fn run_group(
        &self,
        w: &mut Writer,
        txn: u64,
        ts: u64,
        catalog: Option<&HermeticCatalog>,
        body: &mut dyn FnMut(&mut Staging<'_, D>) -> StoreResult<()>,
    ) -> StoreResult<u64> {
        let (used, bytes) = {
            let io = self.core.io();
            (io.log.region_used(), io.log.region_bytes())
        };
        if w.ckpt.is_none() && used * 2 >= bytes {
            self.checkpoint_begin_inner(w)?;
        }
        let view = self.view();
        for attempt in 0..2 {
            let mut st = Staging {
                core: &self.core,
                pages: BTreeMap::new(),
                meta: view.meta.clone(),
                free: w.free.clone(),
                free_before: w.free.clone(),
                max_pages: self.limits.max_pages,
                max_group_pages: self.limits.max_group_pages,
            };
            body(&mut st)?;
            let fin = st.finish(&w.imaged, txn, ts)?;
            let appended = self.core.io().log.append_group(&fin.records);
            let lsns = match appended {
                Ok(lsns) => lsns,
                Err(e) if e.kind == StoreErrorKind::LogFull && attempt == 0 => {
                    if w.ckpt.is_some() {
                        self.checkpoint_finish_inner(w)?;
                    }
                    self.checkpoint_begin_inner(w)?;
                    continue;
                }
                Err(e) => return Err(e),
            };
            // From here on the group is in the log buffer: a failure leaves
            // memory and log disagreeing, so it poisons the store.
            let r = self.install(w, &fin, &lsns, view.catalog_for(catalog));
            if r.is_err() {
                self.poison();
            }
            r?;
            w.free = fin.free;
            w.groups += 1;
            let commit_lsn = *lsns.last().expect("commit record");
            if w.ckpt.is_some() {
                self.checkpoint_step_inner(w, self.limits.checkpoint_step_pages)?;
            }
            return Ok(commit_lsn);
        }
        Err(StoreError::new(
            StoreErrorKind::LogFull,
            "commit group does not fit an empty log region",
        ))
    }
    /// Installs a logged group's pages in latch order and publishes the new
    /// view while holding the view lock, so no reader starts from a root
    /// the group changes until every page is in place.
    fn install(
        &self,
        w: &mut Writer,
        fin: &Finished,
        lsns: &[u64],
        catalog: HermeticCatalog,
    ) -> StoreResult<()> {
        // The whole group is in the log buffer before any of its pages is
        // in the pool, so a page write (which flushes the log through its
        // LSN) always makes the complete group durable first.
        let mut lsn_of = BTreeMap::new();
        for (record, &lsn) in fin.records.iter().zip(lsns) {
            if let Some(p) = record.page() {
                lsn_of.insert(p, lsn);
                if matches!(record, StoreRecord::PageImage { .. }) {
                    w.imaged.insert(p);
                }
            }
        }
        let mut plan: Vec<&(LatchKey, u64, Option<Box<Page>>)> = fin.plan.iter().collect();
        plan.sort_by_key(|(k, _, _)| *k);
        let pages: Vec<(u64, Option<(&Page, u64)>)> = plan
            .iter()
            .map(|(_, id, now)| (*id, now.as_deref().map(|b| (b, lsn_of[id]))))
            .collect();
        let mut vg = self.view.write().unwrap_or_else(|e| e.into_inner());
        if fin.meta.page_count > self.core.page_count() {
            self.core.set_page_count(fin.meta.page_count);
        }
        self.core.install_group(&pages)?;
        *vg = Arc::new(View {
            meta: fin.meta.clone(),
            catalog,
        });
        Ok(())
    }

    // ---- checkpoints -------------------------------------------------------

    fn checkpoint_begin_inner(&self, w: &mut Writer) -> StoreResult<()> {
        {
            let mut io = self.core.io();
            let Io { dev, log, .. } = &mut *io;
            log.switch(dev)?;
        }
        self.refresh_visible();
        w.imaged.clear();
        w.ckpt = Some(self.core.pool.dirty_pages().into_iter().collect());
        w.checkpoints_begun += 1;
        Ok(())
    }
    /// Writes back up to `n` pages; finishes when none are left.
    fn checkpoint_step_inner(&self, w: &mut Writer, n: usize) -> StoreResult<bool> {
        let Some(set) = w.ckpt.as_mut() else {
            return Ok(true);
        };
        let batch: Vec<u64> = set.iter().copied().take(n).collect();
        for &p in &batch {
            set.remove(&p);
        }
        for p in batch {
            self.core.write_back(p)?;
        }
        if w.ckpt.as_ref().is_some_and(BTreeSet::is_empty) {
            self.checkpoint_finish_inner(w)?;
            return Ok(true);
        }
        Ok(false)
    }
    fn checkpoint_finish_inner(&self, w: &mut Writer) -> StoreResult<()> {
        if let Some(set) = w.ckpt.take() {
            for p in set {
                self.core.write_back(p)?;
            }
            let mut io = self.core.io();
            io.dev.sync(FileId::Data)?;
            let Io { dev, log, .. } = &mut *io;
            log.drop_prev(dev)?;
            w.checkpoints_done += 1;
        }
        Ok(())
    }
    /// Starts a fuzzy checkpoint: switches log regions (flushing the log)
    /// and records the pages dirty now. Transactions and commits continue.
    pub fn checkpoint_begin(&mut self) -> StoreResult<()> {
        self.check_usable()?;
        let mut w = lock(&self.writer);
        if w.ckpt.is_some() {
            return Err(state_error("a checkpoint is running"));
        }
        let r = self.checkpoint_begin_inner(&mut w);
        drop(w);
        self.poison_on(r)
    }
    /// Writes back up to `pages` pages of the running checkpoint; `true` once
    /// it has finished (data file synced, previous log region dropped).
    pub fn checkpoint_step(&mut self, pages: usize) -> StoreResult<bool> {
        self.check_usable()?;
        let mut w = lock(&self.writer);
        let r = self.checkpoint_step_inner(&mut w, pages.max(1));
        drop(w);
        self.poison_on(r)
    }
    /// A complete fuzzy checkpoint (begin, write back, finish). Active
    /// transactions are allowed: their writes are not in the log yet.
    pub fn checkpoint(&mut self) -> StoreResult<()> {
        self.check_usable()?;
        let mut w = lock(&self.writer);
        let r = (|| {
            if w.ckpt.is_none() {
                self.checkpoint_begin_inner(&mut w)?;
            }
            self.checkpoint_finish_inner(&mut w)
        })();
        drop(w);
        self.poison_on(r)
    }

    // ---- vacuum ----------------------------------------------------------

    /// Removes every version no snapshot can see (including old tombstones),
    /// frees emptied leaves, and compacts leaf heaps, in system groups of at
    /// most `max_group_pages` pages.
    pub fn vacuum(&mut self) -> StoreResult<VacuumReport> {
        self.check_usable()?;
        let mut w = lock(&self.writer);
        let r = self.vacuum_inner(&mut w);
        drop(w);
        self.poison_on(r)
    }
    fn vacuum_inner(&self, w: &mut Writer) -> StoreResult<VacuumReport> {
        let mut report = VacuumReport::default();
        let snapshots = self.snapshots();
        let cfg = self.limits.tree();
        let names: Vec<String> = self.view().meta.tables.keys().cloned().collect();
        for name in names {
            let (root, types) = {
                let view = self.view();
                let t = &view.meta.tables[&name];
                (t.root, table_types(t))
            };
            let mut victims = Vec::new();
            let mut versions: Vec<(Key, bool)> = Vec::new();
            btree::visit(&mut self.reader(), root, Key::MIN, Key::MAX, &mut |e| {
                if versions
                    .first()
                    .is_some_and(|(k, _)| k.rowid != e.key.rowid)
                {
                    victims.extend(collectable(&versions, &snapshots).into_iter().rev());
                    versions.clear();
                }
                versions.push((e.key, e.tomb));
                Ok(true)
            })?;
            // Oldest first within each row (`collectable` is newest first):
            // every committed prefix of the groups below must keep the
            // row's logical contents, so a tombstone goes only after every
            // older version it hides.
            victims.extend(collectable(&versions, &snapshots).into_iter().rev());
            // Bounded groups: halve the batch when a group would touch more
            // than `max_group_pages` pages.
            let mut per_group = (self.limits.max_group_pages / 4).max(1);
            let mut done = 0;
            while done < victims.len() {
                let chunk = &victims[done..(done + per_group).min(victims.len())];
                let r = self.run_group(w, 0, 0, None, &mut |st| {
                    let mut root = st.meta.tables[&name].root;
                    for &key in chunk {
                        let (r, found) = btree::remove(st, cfg, &types, root, key)?;
                        if !found {
                            return Err(corrupt("vacuum victim vanished"));
                        }
                        root = r;
                    }
                    st.meta.tables.get_mut(&name).expect("table").root = root;
                    Ok(())
                });
                match r {
                    Err(e) if e.kind == StoreErrorKind::Limit && per_group > 1 => {
                        per_group /= 2;
                        continue;
                    }
                    r => r?,
                };
                done += chunk.len();
                report.versions_removed += chunk.len() as u64;
                report.groups += 1;
            }
            // Tombstone/heap compaction of leaves with holes.
            let root = self.view().meta.tables[&name].root;
            let mut leaves = Vec::new();
            let mut all = Vec::new();
            btree::pages(&mut self.reader(), root, &mut all)?;
            for p in all {
                let b = self.core.copy(p)?;
                if kind(&b[..]) == KIND_LEAF {
                    let lay = layout_of(&b[..])?;
                    if leaf_garbage(&b[..], &lay) > 0 {
                        leaves.push(p);
                    }
                }
            }
            for chunk in leaves.chunks(self.limits.max_group_pages - 4) {
                self.run_group(w, 0, 0, None, &mut |st| {
                    for &p in chunk {
                        let b = st.page_mut(p)?;
                        let lay = layout_of(&b[..])?;
                        leaf_compact(&mut b[..], &lay);
                    }
                    Ok(())
                })?;
                report.leaves_compacted += chunk.len() as u64;
                report.groups += 1;
            }
        }
        self.flush_log()?;
        self.refresh_visible();
        w.versions_collected += report.versions_removed;
        Ok(report)
    }

    // ---- SQL over durable tables ------------------------------------------

    /// A [`DataSet`] of the named tables as the transaction sees them, rows
    /// in row-id order. Bounded by `limits` like any dataset.
    pub fn dataset(&self, txn: &Txn, tables: &[&str], limits: Limits) -> StoreResult<DataSet> {
        let catalog = self.view().catalog.clone();
        let mut data = DataSet::new(&catalog);
        for &name in tables {
            let rows: Vec<Vec<Value>> = self.scan(txn, name)?.into_iter().map(|(_, r)| r).collect();
            data.set_rows(name, &rows, limits)?;
        }
        Ok(data)
    }
    fn plan_tables(plan: &RelPlan, out: &mut BTreeSet<String>) {
        match plan {
            RelPlan::Scan { table, .. } => {
                out.insert(table.clone());
            }
            RelPlan::Filter { input, .. } => Self::plan_tables(input, out),
            RelPlan::Join { lhs, rhs, .. } => {
                Self::plan_tables(lhs, out);
                Self::plan_tables(rhs, out);
            }
        }
    }
    /// Prepares `sql` against the durable catalog and runs it (row
    /// executor, optimized plan) over the transaction's snapshot.
    pub fn query(
        &self,
        txn: &Txn,
        sql: &str,
        params: &[ParamDef],
        binds: &BTreeMap<String, Value>,
        limits: Limits,
    ) -> StoreResult<QueryResult> {
        let catalog = self.view().catalog.clone();
        let q = catalog.prepare(sql, params, limits)?.optimized();
        let data = self.query_data(txn, q.plan(), limits)?;
        Ok(q.execute(&catalog, &data, binds)?)
    }
    /// As [`Store::query`], with the vectorized executor (hash joins, column
    /// batches).
    pub fn query_vectorized(
        &self,
        txn: &Txn,
        sql: &str,
        params: &[ParamDef],
        binds: &BTreeMap<String, Value>,
        limits: Limits,
        engine: &VectorEngine,
    ) -> StoreResult<QueryResult> {
        let catalog = self.view().catalog.clone();
        let q = catalog.prepare(sql, params, limits)?.optimized();
        let data = self.query_data(txn, q.plan(), limits)?;
        Ok(q.execute_vectorized(&catalog, &data, binds, engine)?)
    }
    fn query_data(&self, txn: &Txn, plan: &RelPlan, limits: Limits) -> StoreResult<DataSet> {
        let mut tables = BTreeSet::new();
        Self::plan_tables(plan, &mut tables);
        let names: Vec<&str> = tables.iter().map(String::as_str).collect();
        self.dataset(txn, &names, limits)
    }

    // ---- inspection ----------------------------------------------------

    /// Validates every table tree and that tree pages, free pages and the two
    /// fixed pages partition the store. Returns each table's shape.
    pub fn check(&mut self) -> StoreResult<BTreeMap<String, TreeShape>> {
        self.check_usable()?;
        let view = self.view();
        let free = self.writer().free.clone();
        let mut shapes = BTreeMap::new();
        let mut owned: BTreeSet<u64> = BTreeSet::from([META_PAGE, FSM_PAGE]);
        for (name, t) in &view.meta.tables {
            let shape = btree::check(&mut self.reader(), t.root, &table_types(t))?;
            let mut all = Vec::new();
            btree::pages(&mut self.reader(), t.root, &mut all)?;
            for p in all {
                if !owned.insert(p) || free.contains(&p) {
                    return Err(corrupt(format!("page {p} is owned twice")));
                }
            }
            shapes.insert(name.clone(), shape);
        }
        let total = owned.len() + free.len();
        if total as u64 != view.meta.page_count {
            return Err(corrupt(format!(
                "{} pages are owned or free out of {}",
                total, view.meta.page_count
            )));
        }
        Ok(shapes)
    }
    pub fn stats(&self) -> StoreStats {
        let view = self.view();
        let w = self.writer();
        let (log_region_used, log_region_bytes, log_flushes, next_lsn, durable_lsn, ev, pw) = {
            let io = self.core.io();
            (
                io.log.region_used(),
                io.log.region_bytes(),
                io.log.flushes(),
                io.log.next_lsn(),
                io.log.durable_lsn(),
                io.evictions,
                io.page_writes,
            )
        };
        let t = self.txns();
        StoreStats {
            pages: view.meta.page_count,
            free_pages: w.free.len(),
            resident: self.core.pool.len(),
            dirty: self.core.pool.dirty_pages().len(),
            pinned: self.core.pool.pinned(),
            leaked_pins: self.core.pool.leaked(),
            log_region_used,
            log_region_bytes,
            log_flushes,
            next_lsn,
            durable_lsn,
            evictions: ev,
            page_writes: pw,
            active_txns: t.active.len(),
            locks: t.locks.len(),
            last_ts: view.meta.last_ts,
            visible_ts: t.visible_ts,
            pending_commits: t.pending.len(),
            groups: w.groups,
            checkpoint_running: w.ckpt.is_some(),
            checkpoints_begun: w.checkpoints_begun,
            checkpoints_done: w.checkpoints_done,
            versions_collected: w.versions_collected,
        }
    }
    /// Transaction owners dropped without commit or abort.
    pub fn leaked_txns(&self) -> u64 {
        self.leaks.load(Ordering::Relaxed)
    }
    pub fn poisoned(&self) -> bool {
        self.poisoned.load(Ordering::Acquire)
    }
    /// Checkpoints and returns the device (a clean shutdown).
    pub fn close(mut self) -> StoreResult<D> {
        // Deferred commits first. Finishing a running checkpoint writes back
        // only the pages dirty at its switch, so a second, full checkpoint
        // then writes back what later commits dirtied.
        self.flush()?;
        let running = self.writer().ckpt.is_some();
        self.checkpoint()?;
        if running {
            self.checkpoint()?;
        }
        Ok(self.into_device())
    }
    /// Returns the device without writing anything (a crash, for tests).
    pub fn into_device(self) -> D {
        self.core.into_parts().dev
    }
    pub fn device(&mut self) -> &D {
        &self.core.io.get_mut().unwrap_or_else(|e| e.into_inner()).dev
    }
    /// Columns of a durable table.
    pub fn columns(&self, table: &str) -> StoreResult<Vec<ColumnDef>> {
        Ok(self.view().table(table)?.columns.clone())
    }
}
impl View {
    fn catalog_for(&self, replace: Option<&HermeticCatalog>) -> HermeticCatalog {
        replace.cloned().unwrap_or_else(|| self.catalog.clone())
    }
}
