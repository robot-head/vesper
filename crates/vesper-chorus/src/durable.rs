//! A durable Raft log on disk (design §25.3): term and vote, a snapshot of
//! the compacted prefix, and the log suffix, written so that a crash at any
//! point recovers a consistent state that holds everything that was synced.
//!
//! Files in one directory ([`Disk`]):
//!
//! * `meta`: term and vote, replaced atomically (write `meta.tmp`, sync,
//!   rename, sync the directory).
//! * `snapshot`: the snapshot point (index and term) and its image,
//!   replaced atomically the same way.
//! * `log`: a header with the base index (the snapshot index when the file
//!   was started) followed by one record per entry, each with its own
//!   length, CRC-32C and index. New entries are appended; a conflicting
//!   suffix is removed by truncating the file at the first removed record;
//!   compaction and snapshot installation write a new file and rename it
//!   over the old one.
//!
//! [`DurableLog::sync`] makes appends and truncations durable; a Raft node
//! calls it before it answers (the follower's `persist` action of
//! `APPEND_CHOREOGRAPHY`). Recovery ([`DurableLog::open`]) reads the three
//! files, drops a torn tail of the log (a partial record or a CRC
//! mismatch at the end, which is what an interrupted append leaves), keeps
//! the log suffix only if it agrees with the snapshot at the snapshot
//! point (the Raft InstallSnapshot rule), and rewrites a clean log if
//! anything was dropped. Anything else that does not parse is a typed
//! [`StorageError`], never a panic and never silently accepted.
//!
//! [`FsDisk`] uses real files (`File::sync_all`, directory sync on Unix).
//! [`SimDisk`] is an in-memory model of a file system that loses
//! unsynced data on [`SimDisk::crash`]: each file keeps only its synced
//! contents, possibly followed by a prefix of what was appended since and
//! a few garbage bytes (a torn write); a truncation since the last sync
//! may or may not survive; directory operations since the last directory
//! sync survive as a prefix in order. It models these failure modes only,
//! not a particular file system.

use crate::raft::Entry;
use crate::simnet::SplitMix;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

const META: &str = "meta";
const SNAPSHOT: &str = "snapshot";
const LOG: &str = "log";
const MAGIC_META: [u8; 4] = *b"VRM1";
const MAGIC_SNAP: [u8; 4] = *b"VRS1";
const MAGIC_LOG: [u8; 4] = *b"VRL1";
/// Log header: magic, base index (u64), CRC-32C of both.
const LOG_HEADER_LEN: usize = 16;
/// Record: payload length (u32), CRC-32C of the payload (u32), payload.
const RECORD_HEAD: usize = 8;
/// Payload: index, term, command (u64 each).
const ENTRY_PAYLOAD: usize = 24;
/// Largest snapshot image accepted on recovery, in entries.
pub const MAX_SNAPSHOT_ENTRIES: u64 = 1 << 24;

// ---------------------------------------------------------------- CRC-32C

const fn crc_table() -> [u32; 256] {
    // Castagnoli polynomial, reflected.
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0x82f6_3b78 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

static CRC_TABLE: [u32; 256] = crc_table();

/// CRC-32C (Castagnoli) of `b`. An integrity check against torn writes,
/// not an authenticator.
pub fn crc32c(b: &[u8]) -> u32 {
    let mut c = !0u32;
    for &x in b {
        c = CRC_TABLE[((c ^ u32::from(x)) & 0xff) as usize] ^ (c >> 8);
    }
    !c
}

// ---------------------------------------------------------------- disks

/// The file operations the log needs, on one directory.
pub trait Disk {
    /// The whole file, or `None` if it does not exist.
    fn read(&mut self, name: &str) -> io::Result<Option<Vec<u8>>>;
    /// Appends, creating the file if needed. Not durable until [`Disk::sync`].
    fn append(&mut self, name: &str, bytes: &[u8]) -> io::Result<()>;
    /// Shortens the file to `len` bytes. Not durable until [`Disk::sync`].
    fn truncate(&mut self, name: &str, len: u64) -> io::Result<()>;
    /// Makes the file's contents and length durable.
    fn sync(&mut self, name: &str) -> io::Result<()>;
    /// Atomically replaces `to` with `from`. Durable after [`Disk::sync_dir`].
    fn rename(&mut self, from: &str, to: &str) -> io::Result<()>;
    /// Removes the file if it exists. Durable after [`Disk::sync_dir`].
    fn remove(&mut self, name: &str) -> io::Result<()>;
    /// Makes creations, renames and removals durable.
    fn sync_dir(&mut self) -> io::Result<()>;
}

/// Real files in a directory.
#[derive(Debug)]
pub struct FsDisk {
    dir: PathBuf,
}

impl FsDisk {
    /// Uses (and creates) `dir`.
    pub fn new(dir: impl AsRef<Path>) -> io::Result<Self> {
        std::fs::create_dir_all(dir.as_ref())?;
        Ok(Self {
            dir: dir.as_ref().to_path_buf(),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

impl Disk for FsDisk {
    fn read(&mut self, name: &str) -> io::Result<Option<Vec<u8>>> {
        match std::fs::read(self.path(name)) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn append(&mut self, name: &str, bytes: &[u8]) -> io::Result<()> {
        use std::io::Write;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path(name))?
            .write_all(bytes)
    }

    fn truncate(&mut self, name: &str, len: u64) -> io::Result<()> {
        std::fs::OpenOptions::new()
            .write(true)
            .open(self.path(name))?
            .set_len(len)
    }

    fn sync(&mut self, name: &str) -> io::Result<()> {
        // Windows needs write access for FlushFileBuffers.
        std::fs::OpenOptions::new()
            .write(true)
            .open(self.path(name))?
            .sync_all()
    }

    fn rename(&mut self, from: &str, to: &str) -> io::Result<()> {
        std::fs::rename(self.path(from), self.path(to))
    }

    fn remove(&mut self, name: &str) -> io::Result<()> {
        match std::fs::remove_file(self.path(name)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    fn sync_dir(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        {
            std::fs::File::open(&self.dir)?.sync_all()?;
        }
        // Windows: directory handles cannot be opened through std; NTFS
        // journals the rename itself.
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
struct SimFile {
    synced: Vec<u8>,
    cur: Vec<u8>,
    /// Shortest length since the last sync (a truncation point), if the
    /// file was truncated below its synced length.
    cut: Option<usize>,
}

#[derive(Clone, Debug)]
enum DirOp {
    Create(String, u64),
    Rename(String, String),
    Remove(String),
}

/// Counters of a [`SimDisk`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SimDiskStats {
    pub syncs: u64,
    pub dir_syncs: u64,
    pub crashes: u64,
    /// Unsynced bytes lost in crashes.
    pub lost_bytes: u64,
    /// Garbage bytes written by torn writes in crashes.
    pub torn_bytes: u64,
    /// Directory operations lost in crashes.
    pub lost_dir_ops: u64,
}

/// An in-memory file system that loses unsynced data on [`SimDisk::crash`].
#[derive(Clone, Debug)]
pub struct SimDisk {
    files: BTreeMap<u64, SimFile>,
    /// Directory as of the last [`Disk::sync_dir`], and now.
    dir_synced: BTreeMap<String, u64>,
    dir_cur: BTreeMap<String, u64>,
    pending: Vec<DirOp>,
    next_id: u64,
    rng: SplitMix,
    /// Mutating operations left before the power fails (see
    /// [`SimDisk::power_fail_after`]).
    budget: Option<u64>,
    pub stats: SimDiskStats,
}

impl SimDisk {
    pub fn new(seed: u64) -> Self {
        Self {
            files: BTreeMap::new(),
            dir_synced: BTreeMap::new(),
            dir_cur: BTreeMap::new(),
            pending: Vec::new(),
            next_id: 0,
            rng: SplitMix(seed ^ 0x6469_736b_6372_6173),
            budget: None,
            stats: SimDiskStats::default(),
        }
    }

    /// After `ops` more mutating operations (append, truncate, sync,
    /// rename, remove, directory sync), every mutating operation fails
    /// without effect, as if the power had failed between two of them.
    /// [`SimDisk::crash`] restores power.
    pub fn power_fail_after(&mut self, ops: u64) {
        self.budget = Some(ops);
    }

    fn spend(&mut self) -> io::Result<()> {
        match self.budget {
            Some(0) => Err(io::Error::other("power failed")),
            Some(n) => {
                self.budget = Some(n - 1);
                Ok(())
            }
            None => Ok(()),
        }
    }

    fn file(&mut self, name: &str) -> io::Result<&mut SimFile> {
        let id = *self
            .dir_cur
            .get(name)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        Ok(self.files.get_mut(&id).expect("directory names a file"))
    }

    /// Power loss: every file keeps its synced contents plus, at random, a
    /// prefix of what was appended since (with up to 15 garbage bytes after
    /// it); a truncation since the last sync survives or not; a prefix of
    /// the directory operations since the last directory sync survives.
    pub fn crash(&mut self) {
        self.stats.crashes += 1;
        self.budget = None;
        let ops = std::mem::take(&mut self.pending);
        let keep = self.rng.range(0, ops.len() as u64) as usize;
        self.stats.lost_dir_ops += (ops.len() - keep) as u64;
        let mut dir = self.dir_synced.clone();
        for op in ops.into_iter().take(keep) {
            match op {
                DirOp::Create(n, id) => {
                    dir.insert(n, id);
                }
                DirOp::Rename(a, b) => {
                    if let Some(id) = dir.remove(&a) {
                        dir.insert(b, id);
                    }
                }
                DirOp::Remove(n) => {
                    dir.remove(&n);
                }
            }
        }
        let rng = &mut self.rng;
        let stats = &mut self.stats;
        for f in self.files.values_mut() {
            // The durable base: the synced contents, cut if the truncation
            // reached the disk.
            let mut base = f.synced.clone();
            let mut from = base.len();
            if let Some(cut) = f.cut
                && rng.chance(500)
            {
                base.truncate(cut);
                from = cut;
            }
            let mut out = base;
            // Bytes written since the last sync beyond the base.
            if f.cur.len() > from && f.cur[..from.min(f.cur.len())] == out[..from] {
                let extra = &f.cur[from..];
                let k = rng.range(0, extra.len() as u64) as usize;
                out.extend_from_slice(&extra[..k]);
                stats.lost_bytes += (extra.len() - k) as u64;
                if k < extra.len() && rng.chance(500) {
                    let g = rng.range(1, 15) as usize;
                    for _ in 0..g {
                        out.push(rng.next_u64() as u8);
                    }
                    stats.torn_bytes += g as u64;
                }
            } else {
                stats.lost_bytes += f.cur.len().saturating_sub(out.len()) as u64;
            }
            f.synced = out.clone();
            f.cur = out;
            f.cut = None;
        }
        // Files no name refers to are gone.
        let live: std::collections::BTreeSet<u64> = dir.values().copied().collect();
        self.files.retain(|id, _| live.contains(id));
        self.dir_synced = dir.clone();
        self.dir_cur = dir;
    }

    /// Names of the files that exist now.
    pub fn names(&self) -> Vec<String> {
        self.dir_cur.keys().cloned().collect()
    }

    /// Overwrites a byte of a file's synced contents (media corruption).
    pub fn corrupt(&mut self, name: &str, at: usize, xor: u8) -> io::Result<()> {
        let f = self.file(name)?;
        if let Some(b) = f.cur.get_mut(at) {
            *b ^= xor;
        }
        if let Some(b) = f.synced.get_mut(at) {
            *b ^= xor;
        }
        Ok(())
    }
}

impl Disk for SimDisk {
    fn read(&mut self, name: &str) -> io::Result<Option<Vec<u8>>> {
        match self.file(name) {
            Ok(f) => Ok(Some(f.cur.clone())),
            Err(_) => Ok(None),
        }
    }

    fn append(&mut self, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.spend()?;
        if !self.dir_cur.contains_key(name) {
            let id = self.next_id;
            self.next_id += 1;
            self.files.insert(id, SimFile::default());
            self.dir_cur.insert(name.to_owned(), id);
            self.pending.push(DirOp::Create(name.to_owned(), id));
        }
        self.file(name)?.cur.extend_from_slice(bytes);
        Ok(())
    }

    fn truncate(&mut self, name: &str, len: u64) -> io::Result<()> {
        self.spend()?;
        let f = self.file(name)?;
        let len = usize::try_from(len).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        if len < f.cur.len() {
            f.cur.truncate(len);
            if len < f.synced.len() {
                f.cut = Some(f.cut.map_or(len, |c| c.min(len)));
            }
        }
        Ok(())
    }

    fn sync(&mut self, name: &str) -> io::Result<()> {
        self.spend()?;
        self.stats.syncs += 1;
        let f = self.file(name)?;
        f.synced = f.cur.clone();
        f.cut = None;
        Ok(())
    }

    fn rename(&mut self, from: &str, to: &str) -> io::Result<()> {
        self.spend()?;
        let id = self
            .dir_cur
            .remove(from)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        self.dir_cur.insert(to.to_owned(), id);
        self.pending
            .push(DirOp::Rename(from.to_owned(), to.to_owned()));
        Ok(())
    }

    fn remove(&mut self, name: &str) -> io::Result<()> {
        self.spend()?;
        if self.dir_cur.remove(name).is_some() {
            self.pending.push(DirOp::Remove(name.to_owned()));
        }
        Ok(())
    }

    fn sync_dir(&mut self) -> io::Result<()> {
        self.spend()?;
        self.stats.dir_syncs += 1;
        self.dir_synced = self.dir_cur.clone();
        self.pending.clear();
        Ok(())
    }
}

// ---------------------------------------------------------------- log

/// The persistent state of one Raft server.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PersistentState {
    pub term: u64,
    pub voted_for: Option<u32>,
    /// Entries `1..=snapshot_index` are in the snapshot.
    pub snapshot_index: u64,
    pub snapshot_term: u64,
    /// The snapshot image: the compacted prefix itself, because in the
    /// Chorus model the state machine is the command history
    /// (`snapshot_image.len() == snapshot_index`).
    pub snapshot_image: Vec<Entry>,
    /// Entries `snapshot_index + 1 ..`.
    pub log: Vec<Entry>,
}

impl PersistentState {
    /// Snapshot image followed by the log: the whole logical log.
    pub fn full_log(&self) -> Vec<Entry> {
        let mut v = self.snapshot_image.clone();
        v.extend_from_slice(&self.log);
        v
    }

    pub fn last_index(&self) -> u64 {
        self.snapshot_index + self.log.len() as u64
    }
}

#[derive(Debug)]
pub enum StorageError {
    Io(io::Error),
    /// A file's contents are inconsistent beyond a torn log tail.
    Corrupt {
        file: &'static str,
        reason: String,
    },
    /// A call that would break the log's invariants.
    Invalid(&'static str),
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "storage I/O: {e}"),
            Self::Corrupt { file, reason } => write!(f, "corrupt {file}: {reason}"),
            Self::Invalid(m) => write!(f, "invalid storage call: {m}"),
        }
    }
}

impl std::error::Error for StorageError {}

impl From<io::Error> for StorageError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// What [`DurableLog::open`] found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Recovery {
    /// No file existed: a fresh server.
    pub fresh: bool,
    /// Log records read.
    pub records: u64,
    /// Bytes of a torn log tail that were dropped.
    pub torn_bytes: u64,
    /// Log entries at or before the snapshot point that were dropped.
    pub covered_entries: u64,
    /// Log entries dropped because they disagreed with the snapshot.
    pub conflicting_entries: u64,
    /// Whether the log file was rewritten.
    pub rewrote_log: bool,
}

/// Counters of one [`DurableLog`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DurableStats {
    pub appended: u64,
    pub truncations: u64,
    pub truncated_entries: u64,
    pub snapshots: u64,
    pub log_rewrites: u64,
    pub meta_writes: u64,
    pub syncs: u64,
}

/// A Raft server's durable state on a [`Disk`].
#[derive(Debug)]
pub struct DurableLog<D: Disk> {
    disk: D,
    state: PersistentState,
    /// Byte offset in `log` of each entry of `state.log`.
    offsets: Vec<u64>,
    /// Length of the `log` file.
    log_len: u64,
    /// Appends or truncations not yet synced.
    dirty: bool,
    pub stats: DurableStats,
}

fn be(b: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}

fn corrupt(file: &'static str, reason: impl Into<String>) -> StorageError {
    StorageError::Corrupt {
        file,
        reason: reason.into(),
    }
}

fn encode_meta(term: u64, vote: Option<u32>) -> Vec<u8> {
    let mut b = MAGIC_META.to_vec();
    b.extend(term.to_be_bytes());
    b.extend(vote.map_or(u64::MAX, u64::from).to_be_bytes());
    let c = crc32c(&b);
    b.extend(c.to_be_bytes());
    b
}

fn decode_meta(b: &[u8]) -> Result<(u64, Option<u32>), StorageError> {
    if b.len() != 24 || b[..4] != MAGIC_META {
        return Err(corrupt(META, "bad length or magic"));
    }
    if crc32c(&b[..20]) != u32::from_be_bytes(b[20..24].try_into().expect("4")) {
        return Err(corrupt(META, "checksum mismatch"));
    }
    let vote = match be(b, 12) {
        u64::MAX => None,
        v => Some(u32::try_from(v).map_err(|_| corrupt(META, "vote out of range"))?),
    };
    Ok((be(b, 4), vote))
}

fn encode_snapshot(index: u64, term: u64, image: &[Entry]) -> Vec<u8> {
    let mut b = MAGIC_SNAP.to_vec();
    b.extend(index.to_be_bytes());
    b.extend(term.to_be_bytes());
    for e in image {
        b.extend(e.term.to_be_bytes());
        b.extend(e.cmd.to_be_bytes());
    }
    let c = crc32c(&b);
    b.extend(c.to_be_bytes());
    b
}

fn decode_snapshot(b: &[u8]) -> Result<(u64, u64, Vec<Entry>), StorageError> {
    if b.len() < 24 || b[..4] != MAGIC_SNAP || (b.len() - 24) % 16 != 0 {
        return Err(corrupt(SNAPSHOT, "bad length or magic"));
    }
    let body = b.len() - 4;
    if crc32c(&b[..body]) != u32::from_be_bytes(b[body..].try_into().expect("4")) {
        return Err(corrupt(SNAPSHOT, "checksum mismatch"));
    }
    let (index, term) = (be(b, 4), be(b, 12));
    let n = ((body - 20) / 16) as u64;
    if n != index || n > MAX_SNAPSHOT_ENTRIES {
        return Err(corrupt(SNAPSHOT, "image length differs from its index"));
    }
    let image: Vec<Entry> = (0..n as usize)
        .map(|i| Entry {
            term: be(b, 20 + 16 * i),
            cmd: be(b, 28 + 16 * i),
        })
        .collect();
    if image.last().map_or(0, |e| e.term) != term {
        return Err(corrupt(
            SNAPSHOT,
            "term differs from the image's last entry",
        ));
    }
    Ok((index, term, image))
}

fn log_header(base: u64) -> Vec<u8> {
    let mut b = MAGIC_LOG.to_vec();
    b.extend(base.to_be_bytes());
    let c = crc32c(&b);
    b.extend(c.to_be_bytes());
    b
}

fn encode_record(index: u64, e: &Entry, out: &mut Vec<u8>) {
    let mut p = [0u8; ENTRY_PAYLOAD];
    p[..8].copy_from_slice(&index.to_be_bytes());
    p[8..16].copy_from_slice(&e.term.to_be_bytes());
    p[16..].copy_from_slice(&e.cmd.to_be_bytes());
    out.extend((ENTRY_PAYLOAD as u32).to_be_bytes());
    out.extend(crc32c(&p).to_be_bytes());
    out.extend(p);
}

/// The payload of a well-formed record at `at`, if there is one.
fn record_at(b: &[u8], at: usize) -> Option<&[u8]> {
    let head = b.get(at..at + RECORD_HEAD)?;
    let len = u32::from_be_bytes(head[..4].try_into().ok()?) as usize;
    let crc = u32::from_be_bytes(head[4..].try_into().ok()?);
    if len != ENTRY_PAYLOAD {
        return None;
    }
    let p = b.get(at + RECORD_HEAD..at + RECORD_HEAD + len)?;
    (crc32c(p) == crc).then_some(p)
}

/// The parsed log file: base, `(index, entry, offset)` records, and the
/// length of the valid prefix.
type ParsedLog = (u64, Vec<(u64, Entry, u64)>, usize);

fn parse_log(b: &[u8]) -> Result<ParsedLog, StorageError> {
    if b.len() < LOG_HEADER_LEN {
        // A header torn while the file was being created: an empty log.
        return Err(corrupt(LOG, "short header"));
    }
    if b[..4] != MAGIC_LOG
        || crc32c(&b[..12]) != u32::from_be_bytes(b[12..16].try_into().expect("4"))
    {
        return Err(corrupt(LOG, "bad header"));
    }
    let base = be(b, 4);
    let mut at = LOG_HEADER_LEN;
    let mut out = Vec::new();
    loop {
        let expect = base + out.len() as u64 + 1;
        let Some(p) = record_at(b, at) else {
            // A torn tail ends the log. Media damage in the middle does
            // not: a well-formed record for a later index after the bad one
            // means synced data would be lost, so it is an error.
            let next = at + RECORD_HEAD + ENTRY_PAYLOAD;
            if record_at(b, next).is_some_and(|q| be(q, 0) == expect + 1) {
                return Err(corrupt(LOG, format!("damaged record for index {expect}")));
            }
            break;
        };
        let len = p.len();
        let index = be(p, 0);
        if index != expect {
            // A valid record with the wrong index is not a torn write.
            return Err(corrupt(
                LOG,
                format!("record index {index}, expected {expect}"),
            ));
        }
        out.push((
            index,
            Entry {
                term: be(p, 8),
                cmd: be(p, 16),
            },
            at as u64,
        ));
        at += RECORD_HEAD + len;
    }
    Ok((base, out, at))
}

impl<D: Disk> DurableLog<D> {
    /// Recovers the state on `disk` (an empty disk is a fresh server).
    pub fn open(mut disk: D) -> Result<(Self, Recovery), StorageError> {
        let mut rec = Recovery::default();
        // Leftovers of an interrupted atomic replacement.
        disk.remove("meta.tmp")?;
        disk.remove("snapshot.tmp")?;
        disk.remove("log.tmp")?;
        let meta = disk.read(META)?;
        let snap = disk.read(SNAPSHOT)?;
        let log = disk.read(LOG)?;
        rec.fresh = meta.is_none() && snap.is_none() && log.is_none();
        let (term, voted_for) = match meta {
            Some(b) => decode_meta(&b)?,
            None => (0, None),
        };
        let (snapshot_index, snapshot_term, snapshot_image) = match snap {
            Some(b) => decode_snapshot(&b)?,
            None => (0, 0, Vec::new()),
        };
        let mut state = PersistentState {
            term,
            voted_for,
            snapshot_index,
            snapshot_term,
            snapshot_image,
            log: Vec::new(),
        };
        let mut rewrite = log.is_none();
        let mut offsets = Vec::new();
        let mut log_len = 0;
        match log.as_deref().map(parse_log) {
            None => {}
            // A log whose header never reached the disk (a crash while the
            // first log was being created) holds nothing.
            Some(Err(StorageError::Corrupt { reason, .. })) if reason == "short header" => {
                rewrite = true;
            }
            Some(Err(e)) => return Err(e),
            Some(Ok((base, records, valid))) => {
                let total = log.as_ref().map_or(0, Vec::len);
                rec.records = records.len() as u64;
                rec.torn_bytes = (total - valid) as u64;
                if base > snapshot_index {
                    return Err(corrupt(LOG, "log starts after the snapshot point"));
                }
                // The suffix is kept only if the log agrees with the
                // snapshot at its point (or the snapshot is empty).
                let at_point = records
                    .iter()
                    .find(|(i, _, _)| *i == snapshot_index)
                    .map(|(_, e, _)| e.term);
                let agrees = snapshot_index == base || at_point == Some(snapshot_term);
                for (i, e, off) in records {
                    if i <= snapshot_index {
                        rec.covered_entries += 1;
                    } else if agrees {
                        state.log.push(e);
                        offsets.push(off);
                    } else {
                        rec.conflicting_entries += 1;
                    }
                }
                log_len = valid as u64;
                if base != snapshot_index || !agrees || rec.torn_bytes > 0 {
                    rewrite = true;
                }
            }
        }
        let mut this = Self {
            disk,
            state,
            offsets,
            log_len,
            dirty: false,
            stats: DurableStats::default(),
        };
        if rewrite && !rec.fresh {
            this.rewrite_log()?;
            rec.rewrote_log = true;
        } else if rec.fresh {
            this.rewrite_log()?;
        }
        Ok((this, rec))
    }

    pub fn state(&self) -> &PersistentState {
        &self.state
    }

    pub fn disk(&self) -> &D {
        &self.disk
    }

    pub fn disk_mut(&mut self) -> &mut D {
        &mut self.disk
    }

    /// Gives the disk back (for a crash and a later [`DurableLog::open`]).
    pub fn into_disk(self) -> D {
        self.disk
    }

    /// Whether appends or truncations await [`DurableLog::sync`].
    pub fn dirty(&self) -> bool {
        self.dirty
    }

    fn replace(&mut self, name: &str, bytes: &[u8]) -> Result<(), StorageError> {
        let tmp = format!("{name}.tmp");
        self.disk.remove(&tmp)?;
        self.disk.append(&tmp, bytes)?;
        self.disk.sync(&tmp)?;
        self.disk.rename(&tmp, name)?;
        self.disk.sync_dir()?;
        Ok(())
    }

    /// Writes a fresh log file holding `state.log` after the snapshot point.
    fn rewrite_log(&mut self) -> Result<(), StorageError> {
        let mut b = log_header(self.state.snapshot_index);
        let mut offsets = Vec::with_capacity(self.state.log.len());
        for (k, e) in self.state.log.iter().enumerate() {
            offsets.push(b.len() as u64);
            encode_record(self.state.snapshot_index + k as u64 + 1, e, &mut b);
        }
        self.replace(LOG, &b)?;
        self.offsets = offsets;
        self.log_len = b.len() as u64;
        self.dirty = false;
        self.stats.log_rewrites += 1;
        Ok(())
    }

    /// Durably records the current term and vote (synced on return).
    pub fn set_term_vote(&mut self, term: u64, voted_for: Option<u32>) -> Result<(), StorageError> {
        if term < self.state.term {
            return Err(StorageError::Invalid("the term never decreases"));
        }
        if term == self.state.term
            && self.state.voted_for.is_some()
            && voted_for != self.state.voted_for
        {
            return Err(StorageError::Invalid("one vote per term"));
        }
        if (term, voted_for) == (self.state.term, self.state.voted_for) {
            return Ok(());
        }
        self.replace(META, &encode_meta(term, voted_for))?;
        self.state.term = term;
        self.state.voted_for = voted_for;
        self.stats.meta_writes += 1;
        Ok(())
    }

    /// Appends entries after the last one (durable after [`Self::sync`]).
    pub fn append(&mut self, entries: &[Entry]) -> Result<(), StorageError> {
        if entries.is_empty() {
            return Ok(());
        }
        let mut b = Vec::with_capacity(entries.len() * (RECORD_HEAD + ENTRY_PAYLOAD));
        let mut index = self.state.last_index();
        for e in entries {
            index += 1;
            self.offsets.push(self.log_len + b.len() as u64);
            encode_record(index, e, &mut b);
        }
        self.disk.append(LOG, &b)?;
        self.log_len += b.len() as u64;
        self.state.log.extend_from_slice(entries);
        self.dirty = true;
        self.stats.appended += entries.len() as u64;
        Ok(())
    }

    /// Removes every entry after `index` (durable after [`Self::sync`]).
    /// Entries in the snapshot cannot be removed.
    pub fn truncate_after(&mut self, index: u64) -> Result<(), StorageError> {
        if index < self.state.snapshot_index {
            return Err(StorageError::Invalid("cannot truncate inside the snapshot"));
        }
        if index >= self.state.last_index() {
            return Ok(());
        }
        let keep = (index - self.state.snapshot_index) as usize;
        let at = self.offsets[keep];
        self.disk.truncate(LOG, at)?;
        self.stats.truncations += 1;
        self.stats.truncated_entries += (self.state.log.len() - keep) as u64;
        self.state.log.truncate(keep);
        self.offsets.truncate(keep);
        self.log_len = at;
        self.dirty = true;
        Ok(())
    }

    /// Makes appends and truncations durable.
    pub fn sync(&mut self) -> Result<(), StorageError> {
        if self.dirty {
            self.disk.sync(LOG)?;
            self.dirty = false;
            self.stats.syncs += 1;
        }
        Ok(())
    }

    /// Moves the snapshot point to `index` and keeps `log` as the entries
    /// after it, writing the snapshot first and then a new log (both
    /// synced on return). `image` is the whole prefix `1..=index`.
    /// Compaction passes its own prefix and suffix; InstallSnapshot passes
    /// the leader's image and the follower's suffix if it agrees, else
    /// nothing.
    pub fn install(
        &mut self,
        index: u64,
        term: u64,
        image: &[Entry],
        log: &[Entry],
    ) -> Result<(), StorageError> {
        if image.len() as u64 != index || image.last().map_or(0, |e| e.term) != term {
            return Err(StorageError::Invalid(
                "the image must end at the snapshot point",
            ));
        }
        if index < self.state.snapshot_index {
            return Err(StorageError::Invalid("the snapshot point never moves back"));
        }
        if self.dirty {
            // The new log replaces the old file; nothing is lost by not
            // syncing it first, but keep the order simple.
            self.sync()?;
        }
        self.replace(SNAPSHOT, &encode_snapshot(index, term, image))?;
        self.state.snapshot_index = index;
        self.state.snapshot_term = term;
        self.state.snapshot_image = image.to_vec();
        self.state.log = log.to_vec();
        self.rewrite_log()?;
        self.stats.snapshots += 1;
        Ok(())
    }

    /// Brings the disk to `target` with the fewest writes (meta, snapshot,
    /// truncation, appends) and syncs. Used by a server that keeps its
    /// state in memory and persists it before it answers.
    pub fn persist(&mut self, target: &PersistentState) -> Result<(), StorageError> {
        self.persist_unsynced(target)?;
        self.sync()
    }

    /// As [`Self::persist`] without the final log sync: appends and
    /// truncations may be lost by a crash (meta and snapshot replacements
    /// are synced as they happen).
    pub fn persist_unsynced(&mut self, target: &PersistentState) -> Result<(), StorageError> {
        if (target.term, target.voted_for) != (self.state.term, self.state.voted_for) {
            self.set_term_vote(target.term, target.voted_for)?;
        }
        if target.snapshot_index != self.state.snapshot_index {
            return self.install(
                target.snapshot_index,
                target.snapshot_term,
                &target.snapshot_image,
                &target.log,
            );
        }
        if target.snapshot_term != self.state.snapshot_term
            || target.snapshot_image != self.state.snapshot_image
        {
            return Err(StorageError::Invalid(
                "the snapshot changed at the same point",
            ));
        }
        let common = self
            .state
            .log
            .iter()
            .zip(&target.log)
            .take_while(|(a, b)| a == b)
            .count();
        if common < self.state.log.len() {
            self.truncate_after(self.state.snapshot_index + common as u64)?;
        }
        self.append(&target.log[common..])
    }
}
