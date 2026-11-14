//! Page formats of the table store. Every page is 16 KiB and starts with the
//! 32-byte §24.2 header of [`crate::page`] (page LSN, page id, CRC32C, slot
//! count, free-space offset, kind, level, flags, right sibling).
//!
//! | Kind | Page | Body |
//! | --- | --- | --- |
//! | 16 | meta (page 0) | catalog, table roots, row-id counters, last commit timestamp, page count |
//! | 17 | free-space map (page 1) | bitmap, bit `p` set when page `p` is free |
//! | 18 | B+Tree internal node | fixed child (`u32`) and key (16-byte) arrays |
//! | 19 | B+Tree PAX leaf | row versions column by column (see [`LeafLayout`]) |
//!
//! Keys are row versions `(row id, !commit_ts)`: all versions of a row are
//! adjacent, newest first. Every decoder validates untrusted bytes fully and
//! returns `Corrupt`, never panics.
use super::error::{corrupt, limit, StoreResult};
use crate::page::{page_crc32c, PAGE_SIZE};
use crate::{ColumnDef, ScalarType, Value};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const KIND_META: u8 = 16;
pub(crate) const KIND_FSM: u8 = 17;
pub(crate) const KIND_INTERNAL: u8 = 18;
pub(crate) const KIND_LEAF: u8 = 19;
pub(crate) const META_PAGE: u64 = 0;
pub(crate) const FSM_PAGE: u64 = 1;
/// Pages the free-space map can describe.
pub(crate) const FSM_CAPACITY: u64 = ((PAGE_SIZE - 32) * 8) as u64;

pub(crate) type Page = [u8; PAGE_SIZE];
pub(crate) fn new_page() -> Box<Page> {
    Box::new([0u8; PAGE_SIZE])
}

pub(crate) fn rd16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
pub(crate) fn rd32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("4"))
}
pub(crate) fn rd64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("8"))
}
pub(crate) fn wr16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}
pub(crate) fn wr32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
pub(crate) fn wr64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

pub(crate) fn page_lsn(b: &[u8]) -> u64 {
    rd64(b, 0)
}
pub(crate) fn set_page_lsn(b: &mut [u8], lsn: u64) {
    wr64(b, 0, lsn);
}
pub(crate) fn page_id(b: &[u8]) -> u64 {
    rd64(b, 8)
}
pub(crate) fn kind(b: &[u8]) -> u8 {
    b[24]
}
pub(crate) fn level(b: &[u8]) -> u8 {
    b[25]
}
/// Right sibling of a leaf (0: none). Internal nodes keep the field zero.
pub(crate) fn sibling(b: &[u8]) -> u64 {
    u64::from(rd32(b, 28))
}
pub(crate) fn set_sibling(b: &mut [u8], id: u64) {
    wr32(b, 28, id as u32);
}
pub(crate) fn count(b: &[u8]) -> usize {
    rd16(b, 20) as usize
}
fn set_count(b: &mut [u8], n: usize) {
    wr16(b, 20, n as u16);
}
fn free_offset(b: &[u8]) -> usize {
    rd16(b, 22) as usize
}
fn set_free_offset(b: &mut [u8], v: usize) {
    wr16(b, 22, v as u16);
}
fn heap_start(b: &[u8]) -> usize {
    free_offset(b)
}
/// Zeroes the page and writes id, kind and level.
pub(crate) fn format(b: &mut [u8], id: u64, kind: u8, level: u8) {
    b.fill(0);
    wr64(b, 8, id);
    b[24] = kind;
    b[25] = level;
}
/// Writes the checksum field (done only when a page goes to the device).
pub(crate) fn seal(b: &mut Page) {
    let crc = page_crc32c(b).expect("page size");
    wr32(b, 16, crc);
}
/// Byte ranges excluded from logged diffs: page LSN and checksum.
pub(crate) fn diff_skips(at: usize) -> bool {
    at < 8 || (16..20).contains(&at)
}

/// Validates a page read from the device: checksum, id and structure.
pub(crate) fn check_disk(b: &Page, id: u64) -> StoreResult<()> {
    let crc = page_crc32c(b).expect("page size");
    if crc != rd32(b, 16) {
        return Err(corrupt(format!("page {id} failed its checksum")));
    }
    check_structure(b, id)
}
/// Structural validation independent of the checksum (also applied to
/// logged page images and after every redo).
pub(crate) fn check_structure(b: &Page, id: u64) -> StoreResult<()> {
    if page_id(b) != id {
        return Err(corrupt(format!("page {id} holds page id {}", page_id(b))));
    }
    match kind(b) {
        KIND_META if id == META_PAGE => Meta::decode(b).map(drop),
        KIND_FSM if id == FSM_PAGE => decode_fsm(b).map(drop),
        KIND_INTERNAL if id > FSM_PAGE => check_internal(b),
        KIND_LEAF if id > FSM_PAGE => leaf_decode(b).map(drop),
        // A freed page keeps whatever it held; a never-used one is zero.
        0 if id > FSM_PAGE => Ok(()),
        k => Err(corrupt(format!("page {id} has unexpected kind {k}"))),
    }
}

// ---- meta page -----------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TableMeta {
    pub id: u32,
    pub root: u64,
    pub next_rowid: u64,
    pub columns: Vec<ColumnDef>,
}
/// A secondary index over one column of a table: a B+Tree keyed by
/// `(order-preserving value prefix, row id)` whose leaves hold the value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IndexMeta {
    pub id: u32,
    pub table: String,
    pub column: usize,
    pub root: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Meta {
    pub last_ts: u64,
    pub page_count: u64,
    pub catalog_revision: u64,
    /// Next id for a table or an index (one id space).
    pub next_table_id: u32,
    pub tables: BTreeMap<String, TableMeta>,
    pub indexes: BTreeMap<String, IndexMeta>,
}
const META_MAGIC: &[u8; 8] = b"QRYSTORE";
/// Format 2 adds leaf right-sibling links and secondary indexes; format 1
/// stores (no links) are rejected.
const META_FORMAT: u32 = 2;

pub(crate) fn type_tag(t: ScalarType) -> u8 {
    match t {
        ScalarType::U64 => 1,
        ScalarType::I64 => 2,
        ScalarType::F64 => 3,
        ScalarType::Bool => 4,
        ScalarType::Text => 5,
        ScalarType::Bytes => 6,
    }
}
pub(crate) fn tag_type(t: u8) -> Option<ScalarType> {
    Some(match t {
        1 => ScalarType::U64,
        2 => ScalarType::I64,
        3 => ScalarType::F64,
        4 => ScalarType::Bool,
        5 => ScalarType::Text,
        6 => ScalarType::Bytes,
        _ => return None,
    })
}

struct Cursor<'a> {
    b: &'a [u8],
    at: usize,
}
impl Cursor<'_> {
    fn take(&mut self, n: usize) -> StoreResult<&[u8]> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.b.len());
        let end = end.ok_or_else(|| corrupt("meta page overrun"))?;
        let s = &self.b[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn u8(&mut self) -> StoreResult<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> StoreResult<u16> {
        Ok(rd16(self.take(2)?, 0))
    }
    fn u32(&mut self) -> StoreResult<u32> {
        Ok(rd32(self.take(4)?, 0))
    }
    fn u64(&mut self) -> StoreResult<u64> {
        Ok(rd64(self.take(8)?, 0))
    }
    fn name(&mut self) -> StoreResult<String> {
        let n = self.u8()? as usize;
        let s = std::str::from_utf8(self.take(n)?).map_err(|_| corrupt("meta name"))?;
        crate::identifier(s)
            .ok()
            .filter(|x| x == s)
            .ok_or_else(|| corrupt("meta name is not a normalized identifier"))
    }
}

impl Meta {
    pub fn new() -> Self {
        Self {
            last_ts: 0,
            page_count: 2,
            catalog_revision: 0,
            next_table_id: 1,
            tables: BTreeMap::new(),
            indexes: BTreeMap::new(),
        }
    }
    /// Writes the meta body into `b` (already formatted as the meta page).
    pub fn encode(&self, b: &mut [u8]) -> StoreResult<()> {
        let mut out = Vec::with_capacity(256);
        out.extend_from_slice(META_MAGIC);
        out.extend_from_slice(&META_FORMAT.to_le_bytes());
        for v in [self.last_ts, self.page_count, self.catalog_revision] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.next_table_id.to_le_bytes());
        out.extend_from_slice(&(self.tables.len() as u16).to_le_bytes());
        for (name, t) in &self.tables {
            out.push(name.len() as u8);
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&t.id.to_le_bytes());
            out.extend_from_slice(&t.root.to_le_bytes());
            out.extend_from_slice(&t.next_rowid.to_le_bytes());
            out.extend_from_slice(&(t.columns.len() as u16).to_le_bytes());
            for c in &t.columns {
                out.push(c.name.len() as u8);
                out.extend_from_slice(c.name.as_bytes());
                out.push(type_tag(c.ty));
                out.push(u8::from(c.nullable));
            }
        }
        out.extend_from_slice(&(self.indexes.len() as u16).to_le_bytes());
        for (name, ix) in &self.indexes {
            out.push(name.len() as u8);
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&ix.id.to_le_bytes());
            out.push(ix.table.len() as u8);
            out.extend_from_slice(ix.table.as_bytes());
            out.extend_from_slice(&(ix.column as u16).to_le_bytes());
            out.extend_from_slice(&ix.root.to_le_bytes());
        }
        if 32 + 4 + out.len() > PAGE_SIZE {
            return Err(limit("catalog does not fit the meta page"));
        }
        b[32..PAGE_SIZE].fill(0);
        wr32(b, 32, out.len() as u32);
        b[36..36 + out.len()].copy_from_slice(&out);
        set_count(b, self.tables.len());
        Ok(())
    }
    pub fn decode(b: &[u8]) -> StoreResult<Self> {
        if kind(b) != KIND_META || level(b) != 0 {
            return Err(corrupt("meta page kind"));
        }
        let len = rd32(b, 32) as usize;
        if 36 + len > PAGE_SIZE || b[36 + len..].iter().any(|&x| x != 0) {
            return Err(corrupt("meta page length"));
        }
        let mut c = Cursor {
            b: &b[36..36 + len],
            at: 0,
        };
        if c.take(8)? != META_MAGIC || c.u32()? != META_FORMAT {
            return Err(corrupt("meta page magic or format"));
        }
        let last_ts = c.u64()?;
        let page_count = c.u64()?;
        let catalog_revision = c.u64()?;
        let next_table_id = c.u32()?;
        let n = c.u16()? as usize;
        let mut tables = BTreeMap::new();
        let mut ids = BTreeSet::new();
        for _ in 0..n {
            let name = c.name()?;
            let id = c.u32()?;
            let root = c.u64()?;
            let next_rowid = c.u64()?;
            let ncols = c.u16()? as usize;
            let mut columns = Vec::with_capacity(ncols.min(128));
            let mut names = BTreeSet::new();
            for _ in 0..ncols {
                let cname = c.name()?;
                let ty = tag_type(c.u8()?).ok_or_else(|| corrupt("meta column type"))?;
                let nullable = match c.u8()? {
                    0 => false,
                    1 => true,
                    _ => return Err(corrupt("meta nullability")),
                };
                if !names.insert(cname.clone()) {
                    return Err(corrupt("duplicate meta column"));
                }
                columns.push(ColumnDef {
                    name: cname,
                    ty,
                    nullable,
                });
            }
            if id == 0
                || id >= next_table_id
                || !ids.insert(id)
                || root <= FSM_PAGE
                || root >= page_count
                || next_rowid == 0
                || columns.is_empty()
            {
                return Err(corrupt("meta table entry"));
            }
            if tables
                .insert(
                    name,
                    TableMeta {
                        id,
                        root,
                        next_rowid,
                        columns,
                    },
                )
                .is_some()
            {
                return Err(corrupt("duplicate meta table"));
            }
        }
        let ni = c.u16()? as usize;
        let mut indexes = BTreeMap::new();
        for _ in 0..ni {
            let name = c.name()?;
            let id = c.u32()?;
            let table = c.name()?;
            let column = c.u16()? as usize;
            let root = c.u64()?;
            let ok = tables
                .get(&table)
                .is_some_and(|t: &TableMeta| column < t.columns.len());
            if !ok
                || id == 0
                || id >= next_table_id
                || !ids.insert(id)
                || root <= FSM_PAGE
                || root >= page_count
                || tables.contains_key(&name)
            {
                return Err(corrupt("meta index entry"));
            }
            let ix = IndexMeta {
                id,
                table,
                column,
                root,
            };
            if indexes.insert(name, ix).is_some() {
                return Err(corrupt("duplicate meta index"));
            }
        }
        if c.at != len || page_count < 2 || count(b) != n {
            return Err(corrupt("meta page trailer"));
        }
        Ok(Self {
            last_ts,
            page_count,
            catalog_revision,
            next_table_id,
            tables,
            indexes,
        })
    }
}

// ---- free-space map ------------------------------------------------------

pub(crate) fn encode_fsm(b: &mut [u8], free: &BTreeSet<u64>) {
    b[32..].fill(0);
    for &p in free {
        let bit = p as usize;
        b[32 + bit / 8] |= 1 << (bit % 8);
    }
    set_count(b, free.len().min(u16::MAX as usize));
}
pub(crate) fn decode_fsm(b: &[u8]) -> StoreResult<BTreeSet<u64>> {
    if kind(b) != KIND_FSM || level(b) != 0 {
        return Err(corrupt("free-space map kind"));
    }
    let mut free = BTreeSet::new();
    for (i, &byte) in b[32..].iter().enumerate() {
        for bit in 0..8 {
            if byte & (1 << bit) != 0 {
                free.insert((i * 8 + bit) as u64);
            }
        }
    }
    if free.contains(&META_PAGE) || free.contains(&FSM_PAGE) {
        return Err(corrupt("free-space map frees a fixed page"));
    }
    Ok(free)
}

// ---- keys and entries ----------------------------------------------------

/// A row-version key: `(row id, !commit_ts)`, so a row's newest version
/// sorts first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Key {
    pub rowid: u64,
    pub inv_ts: u64,
}
impl Key {
    pub fn version(rowid: u64, ts: u64) -> Self {
        Self { rowid, inv_ts: !ts }
    }
    pub fn ts(self) -> u64 {
        !self.inv_ts
    }
    pub const MIN: Key = Key {
        rowid: 0,
        inv_ts: 0,
    };
    pub const MAX: Key = Key {
        rowid: u64::MAX,
        inv_ts: u64::MAX,
    };
    fn read(b: &[u8], at: usize) -> Self {
        Self {
            rowid: rd64(b, at),
            inv_ts: rd64(b, at + 8),
        }
    }
    fn write(self, b: &mut [u8], at: usize) {
        wr64(b, at, self.rowid);
        wr64(b, at + 8, self.inv_ts);
    }
}
/// One row version in a leaf. A tombstone (a committed delete) has no values.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Entry {
    pub key: Key,
    pub tomb: bool,
    pub values: Vec<Value>,
}

// ---- internal nodes ------------------------------------------------------

/// Hard capacity of an internal node in keys (children = keys + 1).
pub(crate) const INTERNAL_CAP: usize = (PAGE_SIZE - 32 - 4) / 20;
const CHILD_OFF: usize = 32;
const KEY_OFF: usize = 32 + 4 * (INTERNAL_CAP + 1);

pub(crate) fn child(b: &[u8], i: usize) -> u64 {
    u64::from(rd32(b, CHILD_OFF + 4 * i))
}
pub(crate) fn internal_key(b: &[u8], i: usize) -> Key {
    Key::read(b, KEY_OFF + 16 * i)
}
/// Child index whose subtree holds `key`: the number of separators <= key.
pub(crate) fn route(b: &[u8], key: Key) -> usize {
    let n = count(b);
    let (mut lo, mut hi) = (0, n);
    while lo < hi {
        let mid = (lo + hi) / 2;
        if internal_key(b, mid) <= key {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}
pub(crate) fn internal_build(b: &mut [u8], id: u64, lvl: u8, children: &[u64], keys: &[Key]) {
    debug_assert_eq!(children.len(), keys.len() + 1);
    format(b, id, KIND_INTERNAL, lvl);
    for (i, &c) in children.iter().enumerate() {
        wr32(b, CHILD_OFF + 4 * i, c as u32);
    }
    for (i, &k) in keys.iter().enumerate() {
        k.write(b, KEY_OFF + 16 * i);
    }
    set_count(b, keys.len());
}
/// Replaces separator `i` (the caller keeps the key order).
pub(crate) fn internal_set_key(b: &mut [u8], i: usize, key: Key) {
    debug_assert!(i < count(b));
    key.write(b, KEY_OFF + 16 * i);
}
pub(crate) fn internal_parts(b: &[u8]) -> (Vec<u64>, Vec<Key>) {
    let n = count(b);
    (
        (0..=n).map(|i| child(b, i)).collect(),
        (0..n).map(|i| internal_key(b, i)).collect(),
    )
}
/// Inserts separator `key` at `pos` with `right` as child `pos + 1`.
pub(crate) fn internal_insert(b: &mut [u8], pos: usize, key: Key, right: u64) {
    let n = count(b);
    let ks = KEY_OFF + 16 * pos;
    b.copy_within(ks..KEY_OFF + 16 * n, ks + 16);
    key.write(b, ks);
    let cs = CHILD_OFF + 4 * (pos + 1);
    b.copy_within(cs..CHILD_OFF + 4 * (n + 1), cs + 4);
    wr32(b, cs, right as u32);
    set_count(b, n + 1);
}
/// Removes child `i` and one adjacent separator (requires at least one key).
pub(crate) fn internal_remove(b: &mut [u8], i: usize) {
    let n = count(b);
    let k = i.saturating_sub(1);
    b.copy_within(KEY_OFF + 16 * (k + 1)..KEY_OFF + 16 * n, KEY_OFF + 16 * k);
    b[KEY_OFF + 16 * (n - 1)..KEY_OFF + 16 * n].fill(0);
    b.copy_within(
        CHILD_OFF + 4 * (i + 1)..CHILD_OFF + 4 * (n + 1),
        CHILD_OFF + 4 * i,
    );
    b[CHILD_OFF + 4 * n..CHILD_OFF + 4 * (n + 1)].fill(0);
    set_count(b, n - 1);
}
fn check_internal(b: &[u8]) -> StoreResult<()> {
    let n = count(b);
    if level(b) == 0 || n > INTERNAL_CAP {
        return Err(corrupt("internal node header"));
    }
    for i in 1..n {
        if internal_key(b, i - 1) >= internal_key(b, i) {
            return Err(corrupt("internal keys out of order"));
        }
    }
    for i in 0..=n {
        if child(b, i) <= FSM_PAGE {
            return Err(corrupt("internal child names a fixed page"));
        }
    }
    let tail_zero = b[CHILD_OFF + 4 * (n + 1)..KEY_OFF].iter().all(|&x| x == 0)
        && b[KEY_OFF + 16 * n..].iter().all(|&x| x == 0);
    if !tail_zero || b[26..32].iter().any(|&x| x != 0) || rd16(b, 22) != 0 {
        return Err(corrupt("internal node padding"));
    }
    Ok(())
}

// ---- PAX leaves ----------------------------------------------------------

/// Column kind of a PAX minipage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Store {
    Fixed(usize),
    Var,
}
fn store_of(t: ScalarType) -> Store {
    match t {
        ScalarType::U64 | ScalarType::I64 | ScalarType::F64 => Store::Fixed(8),
        ScalarType::Bool => Store::Fixed(1),
        ScalarType::Text | ScalarType::Bytes => Store::Var,
    }
}

/// Byte layout of a PAX leaf with physical capacity `c` (header `flags`):
///
/// ```text
/// 32        ncols: u16, then one type tag per column
/// dir       c × u16: physical slots in key order (first `slot_count` used)
/// used      ceil(c/8): occupied physical slots
/// keys      c × 16: (row id, !ts)
/// tomb      c × 1: 1 for a delete version
/// per column: nulls ceil(c/8), then c × width (fixed) or c × (u16 off, u16 len)
/// heap      [free_space_offset, 16384): variable-width values, growing down
/// ```
///
/// Each column is one contiguous minipage, so a scan reads a column of a
/// page as one slice (Ailamaki et al.'s PAX layout).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LeafLayout {
    pub c: usize,
    pub types: Vec<ScalarType>,
    dir: usize,
    used: usize,
    keys: usize,
    tomb: usize,
    cols: Vec<(usize, usize, Store)>,
    pub end: usize,
}
fn bitmap(c: usize) -> usize {
    c.div_ceil(8)
}
pub(crate) fn leaf_layout(types: &[ScalarType], c: usize) -> LeafLayout {
    let mut at = 34 + types.len();
    at += at % 2;
    let dir = at;
    at += 2 * c;
    let used = at;
    at += bitmap(c);
    let keys = at;
    at += 16 * c;
    let tomb = at;
    at += c;
    let mut cols = Vec::with_capacity(types.len());
    for &t in types {
        let nulls = at;
        at += bitmap(c);
        let s = store_of(t);
        let data = at;
        at += c * match s {
            Store::Fixed(w) => w,
            Store::Var => 4,
        };
        cols.push((nulls, data, s));
    }
    LeafLayout {
        c,
        types: types.to_vec(),
        dir,
        used,
        keys,
        tomb,
        cols,
        end: at,
    }
}
/// Largest physical capacity whose fixed area fits a page.
pub(crate) fn max_capacity(types: &[ScalarType]) -> usize {
    let (mut lo, mut hi) = (1usize, 4096usize);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if leaf_layout(types, mid).end <= PAGE_SIZE {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}
/// Variable-width payload bytes of an entry.
pub(crate) fn var_bytes(e: &Entry) -> usize {
    e.values
        .iter()
        .map(|v| match v {
            Value::Text(s) => s.len(),
            Value::Bytes(b) => b.len(),
            _ => 0,
        })
        .sum()
}
/// Bytes one entry adds to a page with a tight capacity (for split choice).
pub(crate) fn entry_bytes(types: &[ScalarType], e: &Entry) -> usize {
    let fixed: usize = types
        .iter()
        .map(|&t| match store_of(t) {
            Store::Fixed(w) => w,
            Store::Var => 4,
        })
        .sum();
    // dir + key + tomb + one bit per bitmap rounded up to a byte each.
    2 + 16 + 1 + fixed + 1 + types.len() + var_bytes(e)
}

pub(crate) fn leaf_types(b: &[u8]) -> StoreResult<Vec<ScalarType>> {
    let n = rd16(b, 32) as usize;
    if n == 0 || n > 128 {
        return Err(corrupt("leaf column count"));
    }
    (0..n)
        .map(|i| tag_type(b[34 + i]).ok_or_else(|| corrupt("leaf column type")))
        .collect()
}
pub(crate) fn layout_of(b: &[u8]) -> StoreResult<LeafLayout> {
    if kind(b) != KIND_LEAF || level(b) != 0 {
        return Err(corrupt("not a leaf"));
    }
    let types = leaf_types(b)?;
    let c = rd16(b, 26) as usize;
    let lay = leaf_layout(&types, c);
    if c == 0 || lay.end > PAGE_SIZE || count(b) > c {
        return Err(corrupt("leaf capacity"));
    }
    Ok(lay)
}
fn bit(b: &[u8], base: usize, i: usize) -> bool {
    b[base + i / 8] & (1 << (i % 8)) != 0
}
fn set_bit(b: &mut [u8], base: usize, i: usize, on: bool) {
    if on {
        b[base + i / 8] |= 1 << (i % 8);
    } else {
        b[base + i / 8] &= !(1 << (i % 8));
    }
}
pub(crate) fn leaf_slot(b: &[u8], lay: &LeafLayout, pos: usize) -> usize {
    rd16(b, lay.dir + 2 * pos) as usize
}
pub(crate) fn leaf_key(b: &[u8], lay: &LeafLayout, pos: usize) -> Key {
    Key::read(b, lay.keys + 16 * leaf_slot(b, lay, pos))
}
/// Binary search over the key-ordered directory.
pub(crate) fn leaf_search(b: &[u8], lay: &LeafLayout, key: Key) -> Result<usize, usize> {
    let (mut lo, mut hi) = (0, count(b));
    while lo < hi {
        let mid = (lo + hi) / 2;
        match leaf_key(b, lay, mid).cmp(&key) {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
            std::cmp::Ordering::Equal => return Ok(mid),
        }
    }
    Err(lo)
}
fn read_value(b: &[u8], lay: &LeafLayout, col: usize, slot: usize) -> StoreResult<Value> {
    let (nulls, data, s) = lay.cols[col];
    if bit(b, nulls, slot) {
        return Ok(Value::Null);
    }
    Ok(match (lay.types[col], s) {
        (ScalarType::U64, _) => Value::U64(rd64(b, data + 8 * slot)),
        (ScalarType::I64, _) => Value::I64(rd64(b, data + 8 * slot) as i64),
        (ScalarType::F64, _) => {
            let v = f64::from_bits(rd64(b, data + 8 * slot));
            if !v.is_finite() {
                return Err(corrupt("non-finite float in leaf"));
            }
            Value::F64(v)
        }
        (ScalarType::Bool, _) => match b[data + slot] {
            0 => Value::Bool(false),
            1 => Value::Bool(true),
            _ => return Err(corrupt("leaf boolean")),
        },
        (t, _) => {
            let off = rd16(b, data + 4 * slot) as usize;
            let len = rd16(b, data + 4 * slot + 2) as usize;
            let bytes = var_slice(b, lay, off, len)?;
            if t == ScalarType::Text {
                Value::Text(
                    std::str::from_utf8(bytes)
                        .map_err(|_| corrupt("leaf text is not UTF-8"))?
                        .to_string(),
                )
            } else {
                Value::Bytes(bytes.to_vec())
            }
        }
    })
}
fn var_slice<'a>(b: &'a [u8], _lay: &LeafLayout, off: usize, len: usize) -> StoreResult<&'a [u8]> {
    if len == 0 {
        return Ok(&[]);
    }
    if off < heap_start(b) || off + len > PAGE_SIZE {
        return Err(corrupt("leaf value outside the heap"));
    }
    Ok(&b[off..off + len])
}
fn read_entry(b: &[u8], lay: &LeafLayout, slot: usize) -> StoreResult<Entry> {
    let key = Key::read(b, lay.keys + 16 * slot);
    let tomb = match b[lay.tomb + slot] {
        0 => false,
        1 => true,
        _ => return Err(corrupt("leaf tombstone flag")),
    };
    let values = if tomb {
        Vec::new()
    } else {
        (0..lay.types.len())
            .map(|c| read_value(b, lay, c, slot))
            .collect::<StoreResult<_>>()?
    };
    Ok(Entry { key, tomb, values })
}
/// Decodes and fully validates a leaf: directory permutation, occupancy
/// bitmap, key order, values, disjoint heap ranges and zeroed free slots.
pub(crate) fn leaf_decode(b: &[u8]) -> StoreResult<(LeafLayout, Vec<Entry>)> {
    let lay = layout_of(b)?;
    let n = count(b);
    let heap = heap_start(b);
    if heap < lay.end || heap > PAGE_SIZE {
        return Err(corrupt("leaf heap offset"));
    }
    let sib = sibling(b);
    if sib != 0 && (sib <= FSM_PAGE || sib == page_id(b)) {
        return Err(corrupt("leaf right sibling"));
    }
    let mut seen = vec![false; lay.c];
    for pos in 0..n {
        let s = leaf_slot(b, &lay, pos);
        if s >= lay.c || seen[s] {
            return Err(corrupt("leaf directory"));
        }
        seen[s] = true;
    }
    for (s, &live) in seen.iter().enumerate() {
        if bit(b, lay.used, s) != live {
            return Err(corrupt("leaf occupancy bitmap"));
        }
    }
    let mut ranges = Vec::new();
    let mut entries = Vec::with_capacity(n);
    for pos in 0..n {
        let s = leaf_slot(b, &lay, pos);
        let e = read_entry(b, &lay, s)?;
        if let Some(prev) = entries.last().map(|p: &Entry| p.key) {
            if prev >= e.key {
                return Err(corrupt("leaf keys out of order"));
            }
        }
        for &(nulls, data, st) in &lay.cols {
            if e.tomb && !bit(b, nulls, s) {
                return Err(corrupt("tombstone with values"));
            }
            if st == Store::Var && !bit(b, nulls, s) {
                let len = rd16(b, data + 4 * s + 2) as usize;
                if len > 0 {
                    ranges.push((rd16(b, data + 4 * s) as usize, len));
                }
            }
        }
        entries.push(e);
    }
    ranges.sort_unstable();
    for w in ranges.windows(2) {
        if w[0].0 + w[0].1 > w[1].0 {
            return Err(corrupt("leaf heap values overlap"));
        }
    }
    for (s, &live) in seen.iter().enumerate() {
        if !live && !slot_is_zero(b, &lay, s) {
            return Err(corrupt("free leaf slot is not zeroed"));
        }
    }
    Ok((lay, entries))
}
fn slot_is_zero(b: &[u8], lay: &LeafLayout, s: usize) -> bool {
    b[lay.keys + 16 * s..lay.keys + 16 * s + 16]
        .iter()
        .all(|&x| x == 0)
        && b[lay.tomb + s] == 0
        && lay.cols.iter().all(|&(nulls, data, st)| {
            let w = match st {
                Store::Fixed(w) => w,
                Store::Var => 4,
            };
            !bit(b, nulls, s) && b[data + w * s..data + w * (s + 1)].iter().all(|&x| x == 0)
        })
}
/// Live variable-width bytes in the heap.
fn heap_live(b: &[u8], lay: &LeafLayout) -> usize {
    let mut live = 0;
    for pos in 0..count(b) {
        let s = leaf_slot(b, lay, pos);
        for &(nulls, data, st) in &lay.cols {
            if st == Store::Var && !bit(b, nulls, s) {
                live += rd16(b, data + 4 * s + 2) as usize;
            }
        }
    }
    live
}
/// Rewrites the heap without holes left by removed versions.
pub(crate) fn leaf_compact(b: &mut [u8], lay: &LeafLayout) {
    let mut values: Vec<(usize, usize, Vec<u8>)> = Vec::new(); // (slot, col data base, bytes)
    for pos in 0..count(b) {
        let s = leaf_slot(b, lay, pos);
        for &(nulls, data, st) in &lay.cols {
            if st == Store::Var && !bit(b, nulls, s) {
                let off = rd16(b, data + 4 * s) as usize;
                let len = rd16(b, data + 4 * s + 2) as usize;
                values.push((s, data, b[off..off + len].to_vec()));
            }
        }
    }
    b[lay.end..].fill(0);
    let mut top = PAGE_SIZE;
    for (s, data, bytes) in values {
        top -= bytes.len();
        b[top..top + bytes.len()].copy_from_slice(&bytes);
        let off = if bytes.is_empty() { 0 } else { top as u16 };
        wr16(b, data + 4 * s, off);
    }
    set_free_offset(b, top);
}
/// Heap bytes held by removed versions (reclaimed by [`leaf_compact`]).
pub(crate) fn leaf_garbage(b: &[u8], lay: &LeafLayout) -> usize {
    PAGE_SIZE - heap_start(b) - heap_live(b, lay)
}
fn write_entry(b: &mut [u8], lay: &LeafLayout, slot: usize, e: &Entry, top: &mut usize) {
    e.key.write(b, lay.keys + 16 * slot);
    b[lay.tomb + slot] = u8::from(e.tomb);
    for (c, &(nulls, data, st)) in lay.cols.iter().enumerate() {
        let v = if e.tomb { &Value::Null } else { &e.values[c] };
        set_bit(b, nulls, slot, *v == Value::Null);
        match (st, v) {
            (_, Value::Null) => {}
            (Store::Fixed(_), Value::U64(x)) => wr64(b, data + 8 * slot, *x),
            (Store::Fixed(_), Value::I64(x)) => wr64(b, data + 8 * slot, *x as u64),
            (Store::Fixed(_), Value::F64(x)) => wr64(b, data + 8 * slot, x.to_bits()),
            (Store::Fixed(_), Value::Bool(x)) => b[data + slot] = u8::from(*x),
            (Store::Var, Value::Text(_) | Value::Bytes(_)) => {
                let bytes: &[u8] = match v {
                    Value::Text(s) => s.as_bytes(),
                    Value::Bytes(x) => x,
                    _ => unreachable!(),
                };
                if bytes.is_empty() {
                    wr16(b, data + 4 * slot, 0);
                    wr16(b, data + 4 * slot + 2, 0);
                } else {
                    *top -= bytes.len();
                    b[*top..*top + bytes.len()].copy_from_slice(bytes);
                    wr16(b, data + 4 * slot, *top as u16);
                    wr16(b, data + 4 * slot + 2, bytes.len() as u16);
                }
            }
            _ => unreachable!("rows are validated against the schema before storage"),
        }
    }
}
fn clear_slot(b: &mut [u8], lay: &LeafLayout, s: usize) {
    b[lay.keys + 16 * s..lay.keys + 16 * s + 16].fill(0);
    b[lay.tomb + s] = 0;
    for &(nulls, data, st) in &lay.cols {
        set_bit(b, nulls, s, false);
        let w = match st {
            Store::Fixed(w) => w,
            Store::Var => 4,
        };
        b[data + w * s..data + w * (s + 1)].fill(0);
    }
}
/// Builds a leaf holding `entries` (sorted). Chooses a physical capacity
/// with some free slots when room allows. `false` when they do not fit.
/// A leaf keeps its right-sibling link; any other page starts without one.
pub(crate) fn leaf_build(
    b: &mut [u8],
    id: u64,
    types: &[ScalarType],
    entries: &[Entry],
    max_entries: usize,
) -> bool {
    let n = entries.len();
    let var: usize = entries.iter().map(var_bytes).sum();
    let ceiling = max_capacity(types).min(max_entries);
    if n > ceiling {
        return false;
    }
    let mut c = (n + n / 2 + 4).min(ceiling).max(n.max(1));
    while c > n.max(1) && leaf_layout(types, c).end + var > PAGE_SIZE {
        c -= 1;
    }
    let lay = leaf_layout(types, c);
    if lay.end + var > PAGE_SIZE {
        return false;
    }
    let sib = if kind(b) == KIND_LEAF { sibling(b) } else { 0 };
    format(b, id, KIND_LEAF, 0);
    set_sibling(b, sib);
    wr16(b, 32, types.len() as u16);
    for (i, &t) in types.iter().enumerate() {
        b[34 + i] = type_tag(t);
    }
    wr16(b, 26, c as u16);
    let mut top = PAGE_SIZE;
    for (pos, e) in entries.iter().enumerate() {
        write_entry(b, &lay, pos, e, &mut top);
        wr16(b, lay.dir + 2 * pos, pos as u16);
        set_bit(b, lay.used, pos, true);
    }
    set_count(b, n);
    set_free_offset(b, top);
    true
}
/// Inserts `e` at directory position `pos` in place: reuses the lowest free
/// physical slot and compacts the heap first when holes would make room.
/// `false` when there is no free slot or heap space.
pub(crate) fn leaf_insert(b: &mut [u8], lay: &LeafLayout, pos: usize, e: &Entry) -> bool {
    let n = count(b);
    if n >= lay.c {
        return false;
    }
    let need = var_bytes(e);
    if heap_start(b) - lay.end < need {
        if heap_start(b) - lay.end + leaf_garbage(b, lay) < need {
            return false;
        }
        leaf_compact(b, lay);
    }
    let Some(slot) = (0..lay.c).find(|&s| !bit(b, lay.used, s)) else {
        return false;
    };
    let mut top = heap_start(b);
    write_entry(b, lay, slot, e, &mut top);
    set_free_offset(b, top);
    set_bit(b, lay.used, slot, true);
    b.copy_within(lay.dir + 2 * pos..lay.dir + 2 * n, lay.dir + 2 * pos + 2);
    wr16(b, lay.dir + 2 * pos, slot as u16);
    set_count(b, n + 1);
    true
}
/// Removes the entry at directory position `pos`; its heap bytes become a
/// hole and its physical slot is zeroed for reuse.
pub(crate) fn leaf_remove(b: &mut [u8], lay: &LeafLayout, pos: usize) {
    let n = count(b);
    let s = leaf_slot(b, lay, pos);
    clear_slot(b, lay, s);
    set_bit(b, lay.used, s, false);
    b.copy_within(lay.dir + 2 * (pos + 1)..lay.dir + 2 * n, lay.dir + 2 * pos);
    wr16(b, lay.dir + 2 * (n - 1), 0);
    set_count(b, n - 1);
    if count(b) == 0 {
        // An empty heap needs no holes.
        b[lay.end..].fill(0);
        set_free_offset(b, PAGE_SIZE);
    }
}
