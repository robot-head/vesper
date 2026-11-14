//! Byte devices under the durable store: a real two-file directory backend and
//! a deterministic crash/fsync model.
//!
//! The model is the contract the recovery code is written against: a write or
//! length change is visible to later reads at once but survives a crash only
//! after `sync` of that file returns. At a crash any subset of the unsynced
//! operations may have reached the medium, in order, and one surviving write
//! per file may be torn at a byte boundary. It is a software model of a POSIX
//! page cache, not evidence about any real filesystem, kernel or disk.
use std::fmt;

/// The two files of one store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FileId {
    /// 16 KiB slotted pages at `page_id * PAGE_SIZE`.
    Data,
    /// Write-ahead log: two header slots, then checked frames.
    Wal,
}
impl FileId {
    fn index(self) -> usize {
        match self {
            Self::Data => 0,
            Self::Wal => 1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceError {
    /// The simulated device stopped at its crash point; nothing later happens.
    Crashed,
    /// A read went past the end of the file.
    ShortRead,
    /// Another open device holds the store's lock.
    Locked,
    /// A host I/O error (message from the OS).
    Io(String),
}
impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Crashed => f.write_str("device crashed"),
            Self::ShortRead => f.write_str("read past end of file"),
            Self::Locked => f.write_str("store is locked by another open handle"),
            Self::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}
impl std::error::Error for DeviceError {}

/// Positional file access with explicit durability barriers.
pub trait Device {
    fn len(&mut self, file: FileId) -> Result<u64, DeviceError>;
    /// Fills `buf` exactly or returns `ShortRead`.
    fn read_at(&mut self, file: FileId, offset: u64, buf: &mut [u8]) -> Result<(), DeviceError>;
    fn write_at(&mut self, file: FileId, offset: u64, data: &[u8]) -> Result<(), DeviceError>;
    fn set_len(&mut self, file: FileId, len: u64) -> Result<(), DeviceError>;
    /// Makes every earlier write and length change of `file` durable.
    fn sync(&mut self, file: FileId) -> Result<(), DeviceError>;
}

/// Lets a caller keep ownership of a device across a failed `Db::open` or a
/// simulated crash.
impl<T: Device + ?Sized> Device for &mut T {
    fn len(&mut self, file: FileId) -> Result<u64, DeviceError> {
        (**self).len(file)
    }
    fn read_at(&mut self, file: FileId, offset: u64, buf: &mut [u8]) -> Result<(), DeviceError> {
        (**self).read_at(file, offset, buf)
    }
    fn write_at(&mut self, file: FileId, offset: u64, data: &[u8]) -> Result<(), DeviceError> {
        (**self).write_at(file, offset, data)
    }
    fn set_len(&mut self, file: FileId, len: u64) -> Result<(), DeviceError> {
        (**self).set_len(file, len)
    }
    fn sync(&mut self, file: FileId) -> Result<(), DeviceError> {
        (**self).sync(file)
    }
}

/// `quarry.data` and `quarry.wal` in one directory, using `std::fs`.
///
/// `sync` is `File::sync_all`. On Unix the store directory is synced after
/// the files are created, and so is the parent of every directory this call
/// created, so that the names are durable. An exclusive advisory lock on
/// `quarry.lock` (`File::try_lock`) is held for the device's lifetime: a
/// second open of the same directory fails with `DeviceError::Locked`
/// instead of letting two stores write one log. Whether the OS and disk
/// honour these barriers and locks is outside this crate's control.
#[derive(Debug)]
pub struct FsDevice {
    files: [std::fs::File; 2],
    _lock: std::fs::File,
}
impl FsDevice {
    pub fn open(dir: &std::path::Path) -> Result<Self, DeviceError> {
        let io = |e: std::io::Error| DeviceError::Io(e.to_string());
        // Directories that do not exist yet, outermost first.
        let mut created: Vec<&std::path::Path> = dir
            .ancestors()
            .take_while(|a| !a.as_os_str().is_empty() && !a.exists())
            .collect();
        created.reverse();
        std::fs::create_dir_all(dir).map_err(io)?;
        let open = |name: &str| {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(dir.join(name))
                .map_err(io)
        };
        let lock = open("quarry.lock")?;
        lock.try_lock().map_err(|e| match e {
            std::fs::TryLockError::WouldBlock => DeviceError::Locked,
            std::fs::TryLockError::Error(e) => io(e),
        })?;
        let files = [open("quarry.data")?, open("quarry.wal")?];
        #[cfg(unix)]
        {
            let sync_dir = |d: &std::path::Path| {
                std::fs::File::open(d)
                    .and_then(|d| d.sync_all())
                    .map_err(io)
            };
            sync_dir(dir)?;
            for d in &created {
                if let Some(parent) = d.parent().filter(|p| !p.as_os_str().is_empty()) {
                    sync_dir(parent)?;
                }
            }
        }
        #[cfg(not(unix))]
        let _ = &created;
        Ok(Self { files, _lock: lock })
    }
}
impl Device for FsDevice {
    fn len(&mut self, file: FileId) -> Result<u64, DeviceError> {
        self.files[file.index()]
            .metadata()
            .map(|m| m.len())
            .map_err(|e| DeviceError::Io(e.to_string()))
    }
    fn read_at(&mut self, file: FileId, offset: u64, buf: &mut [u8]) -> Result<(), DeviceError> {
        use std::io::{Read, Seek, SeekFrom};
        let f = &mut self.files[file.index()];
        f.seek(SeekFrom::Start(offset))
            .map_err(|e| DeviceError::Io(e.to_string()))?;
        f.read_exact(buf).map_err(|e| match e.kind() {
            std::io::ErrorKind::UnexpectedEof => DeviceError::ShortRead,
            _ => DeviceError::Io(e.to_string()),
        })
    }
    fn write_at(&mut self, file: FileId, offset: u64, data: &[u8]) -> Result<(), DeviceError> {
        use std::io::{Seek, SeekFrom, Write};
        let f = &mut self.files[file.index()];
        f.seek(SeekFrom::Start(offset))
            .and_then(|_| f.write_all(data))
            .map_err(|e| DeviceError::Io(e.to_string()))
    }
    fn set_len(&mut self, file: FileId, len: u64) -> Result<(), DeviceError> {
        self.files[file.index()]
            .set_len(len)
            .map_err(|e| DeviceError::Io(e.to_string()))
    }
    fn sync(&mut self, file: FileId) -> Result<(), DeviceError> {
        self.files[file.index()]
            .sync_all()
            .map_err(|e| DeviceError::Io(e.to_string()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PendingOp {
    Write { offset: u64, data: Vec<u8> },
    SetLen(u64),
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SimFile {
    durable: Vec<u8>,
    visible: Vec<u8>,
    pending: Vec<PendingOp>,
}
fn apply(bytes: &mut Vec<u8>, op: &PendingOp) {
    match op {
        PendingOp::Write { offset, data } => {
            let start = *offset as usize;
            let end = start + data.len();
            if bytes.len() < end {
                bytes.resize(end, 0);
            }
            bytes[start..end].copy_from_slice(data);
        }
        PendingOp::SetLen(len) => bytes.resize(*len as usize, 0),
    }
}

/// What reaches the medium at a crash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrashChoice {
    /// Only synced state survives.
    LoseUnsynced,
    /// Every unsynced operation also reached the medium.
    KeepUnsynced,
    /// A seeded subset of unsynced operations survives in order, and the last
    /// surviving write of each file is torn at a seeded byte offset.
    Subset { seed: u64 },
}

/// Counters a test can use to enumerate crash points.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SimStats {
    /// Mutating operations (write, set_len, sync) issued so far.
    pub ops: u64,
    pub writes: u64,
    pub syncs: u64,
}

/// In-memory deterministic crash/fsync model (see the module docs).
#[derive(Clone, Debug, Default)]
pub struct SimDevice {
    files: [SimFile; 2],
    stats: SimStats,
    crash_after: Option<u64>,
    crashed: bool,
}
const MAX_SIM_FILE: u64 = 1 << 30;

impl SimDevice {
    pub fn new() -> Self {
        Self::default()
    }
    /// The device accepts `ops` more mutating operations; the next one fails
    /// with `Crashed` without taking effect, and so does everything after it.
    pub fn crash_after(&mut self, ops: u64) {
        self.crash_after = Some(self.stats.ops.saturating_add(ops));
    }
    pub fn stats(&self) -> SimStats {
        self.stats
    }
    pub fn crashed(&self) -> bool {
        self.crashed
    }
    /// Unsynced operations currently pending on `file`.
    pub fn pending(&self, file: FileId) -> usize {
        self.files[file.index()].pending.len()
    }
    /// The medium after a crash with `choice`. The result has no crash point
    /// and no pending operations.
    pub fn crash(&self, choice: CrashChoice) -> SimDevice {
        let mut rng = choice_seed(choice);
        let files = std::array::from_fn(|i| {
            let file = &self.files[i];
            let mut medium = file.durable.clone();
            let kept: Vec<&PendingOp> = match choice {
                CrashChoice::LoseUnsynced => Vec::new(),
                CrashChoice::KeepUnsynced => file.pending.iter().collect(),
                CrashChoice::Subset { .. } => file
                    .pending
                    .iter()
                    .filter(|_| next(&mut rng) & 1 == 1)
                    .collect(),
            };
            let last_write = kept
                .iter()
                .rposition(|op| matches!(op, PendingOp::Write { .. }));
            for (j, op) in kept.iter().enumerate() {
                match (choice, op, Some(j) == last_write) {
                    (CrashChoice::Subset { .. }, PendingOp::Write { offset, data }, true) => {
                        let cut = (next(&mut rng) % (data.len() as u64 + 1)) as usize;
                        apply(
                            &mut medium,
                            &PendingOp::Write {
                                offset: *offset,
                                data: data[..cut].to_vec(),
                            },
                        );
                    }
                    _ => apply(&mut medium, op),
                }
            }
            SimFile {
                durable: medium.clone(),
                visible: medium,
                pending: Vec::new(),
            }
        });
        SimDevice {
            files,
            ..SimDevice::default()
        }
    }
    /// The last unsynced write of `file`: (offset, bytes).
    pub fn last_pending_write(&self, file: FileId) -> Option<(u64, &[u8])> {
        self.files[file.index()]
            .pending
            .iter()
            .rev()
            .find_map(|op| match op {
                PendingOp::Write { offset, data } => Some((*offset, data.as_slice())),
                PendingOp::SetLen(_) => None,
            })
    }
    /// The medium after a crash in which every unsynced operation of `file`
    /// reached it in order except that its last write stopped after `keep`
    /// bytes, and no unsynced operation of the other file did. Lets a test
    /// cut the log at an exact frame boundary.
    pub fn crash_tearing(&self, file: FileId, keep: usize) -> SimDevice {
        let files = std::array::from_fn(|i| {
            let f = &self.files[i];
            let mut medium = f.durable.clone();
            if i == file.index() {
                let last = f
                    .pending
                    .iter()
                    .rposition(|op| matches!(op, PendingOp::Write { .. }));
                for (j, op) in f.pending.iter().enumerate() {
                    match op {
                        PendingOp::Write { offset, data } if Some(j) == last => apply(
                            &mut medium,
                            &PendingOp::Write {
                                offset: *offset,
                                data: data[..keep.min(data.len())].to_vec(),
                            },
                        ),
                        _ => apply(&mut medium, op),
                    }
                }
            }
            SimFile {
                durable: medium.clone(),
                visible: medium,
                pending: Vec::new(),
            }
        });
        SimDevice {
            files,
            ..SimDevice::default()
        }
    }
    /// Test hook: flips bits of durable and visible bytes in place (bit rot).
    pub fn corrupt(&mut self, file: FileId, offset: u64, xor: u8) {
        let f = &mut self.files[file.index()];
        for bytes in [&mut f.durable, &mut f.visible] {
            if let Some(b) = bytes.get_mut(offset as usize) {
                *b ^= xor;
            }
        }
    }
    fn mutate(&mut self) -> Result<(), DeviceError> {
        if self.crashed
            || self
                .crash_after
                .is_some_and(|limit| self.stats.ops >= limit)
        {
            self.crashed = true;
            return Err(DeviceError::Crashed);
        }
        self.stats.ops += 1;
        Ok(())
    }
}
fn choice_seed(choice: CrashChoice) -> u64 {
    match choice {
        CrashChoice::Subset { seed } => seed ^ 0x9e37_79b9_7f4a_7c15,
        _ => 0,
    }
}
fn next(state: &mut u64) -> u64 {
    // SplitMix64: deterministic test-choice generator, not a security RNG.
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

impl Device for SimDevice {
    fn len(&mut self, file: FileId) -> Result<u64, DeviceError> {
        if self.crashed {
            return Err(DeviceError::Crashed);
        }
        Ok(self.files[file.index()].visible.len() as u64)
    }
    fn read_at(&mut self, file: FileId, offset: u64, buf: &mut [u8]) -> Result<(), DeviceError> {
        if self.crashed {
            return Err(DeviceError::Crashed);
        }
        let bytes = &self.files[file.index()].visible;
        let start = usize::try_from(offset).map_err(|_| DeviceError::ShortRead)?;
        let end = start.checked_add(buf.len()).ok_or(DeviceError::ShortRead)?;
        let src = bytes.get(start..end).ok_or(DeviceError::ShortRead)?;
        buf.copy_from_slice(src);
        Ok(())
    }
    fn write_at(&mut self, file: FileId, offset: u64, data: &[u8]) -> Result<(), DeviceError> {
        if offset.saturating_add(data.len() as u64) > MAX_SIM_FILE {
            return Err(DeviceError::Io("simulated file limit".into()));
        }
        self.mutate()?;
        self.stats.writes += 1;
        let op = PendingOp::Write {
            offset,
            data: data.to_vec(),
        };
        let f = &mut self.files[file.index()];
        apply(&mut f.visible, &op);
        f.pending.push(op);
        Ok(())
    }
    fn set_len(&mut self, file: FileId, len: u64) -> Result<(), DeviceError> {
        if len > MAX_SIM_FILE {
            return Err(DeviceError::Io("simulated file limit".into()));
        }
        self.mutate()?;
        let op = PendingOp::SetLen(len);
        let f = &mut self.files[file.index()];
        apply(&mut f.visible, &op);
        f.pending.push(op);
        Ok(())
    }
    fn sync(&mut self, file: FileId) -> Result<(), DeviceError> {
        self.mutate()?;
        self.stats.syncs += 1;
        let f = &mut self.files[file.index()];
        f.durable = f.visible.clone();
        f.pending.clear();
        Ok(())
    }
}
