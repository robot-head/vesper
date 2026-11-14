//! VirtualArena copy-on-write shard checkpoints (§31.1 item 2).
//!
//! A [`ShardArena`] is a contiguous logical region split into fixed-size
//! shards. A [`ShardArena::checkpoint`] shares every shard with the
//! checkpoint (reference-counted, no bytes copied); the first write to a
//! shared shard afterwards copies that shard only, so a checkpoint costs one
//! pointer per shard plus one shard copy per shard written since. Each
//! checkpoint names the shards written since the previous one, and
//! [`ArenaDelta`] serialises just those shards as an incremental snapshot
//! epoch that [`ArenaDelta::apply`] replays onto the previous state.
//!
//! **Write tracking.** Writes to a `ShardArena` are tracked at this API
//! ([`ShardArena::write`], [`ShardArena::shard_mut`]), exactly at shard
//! granularity, not by trapping stores in the MMU. For memory written
//! through raw pointers use [`crate::tracked_arena::TrackedArena`], whose
//! writes the kernel records (`mprotect` faults on Linux and macOS, write
//! watches on Windows, implemented in `vesper_fiber_abi::write_watch`) and
//! whose deltas use this module's [`ArenaDelta`] format.
//! [`probe_write_tracking`] reports each mechanism on the host, exercising
//! the one `TrackedArena` uses and the Linux soft-dirty bit for real.

use std::fmt;
use std::rc::Rc;

use crate::ring::{FlightRecorder, RecordError};

/// Delta image magic and version.
pub const ARENA_DELTA_MAGIC: [u8; 8] = *b"VARENA\0\x02";
const DIGEST_LEN: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArenaError {
    /// Shard length not a power of two in `64..=1 MiB`, or a zero-length
    /// arena, or one whose shard count does not fit `u32`.
    BadGeometry {
        len: usize,
        shard_len: usize,
    },
    OutOfBounds {
        offset: usize,
        len: usize,
        arena: usize,
    },
    /// A checkpoint or delta of a differently shaped arena.
    ShapeMismatch,
    /// A delta that does not follow the arena's current epoch.
    EpochMismatch {
        expected: u64,
        found: u64,
    },
    Truncated,
    BadMagic,
    Corrupt,
    BadShard(u32),
    TrailingBytes(usize),
    Record(RecordError),
    /// No keyframe among the images given to [`rebuild_arena`], or a
    /// keyframe interval of zero.
    NoKeyframe,
}

impl fmt::Display for ArenaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "chrono arena: {self:?}")
    }
}
impl std::error::Error for ArenaError {}

/// Counters for what checkpoints cost.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ArenaStats {
    pub checkpoints: u64,
    /// Shards copied because a write hit a shard shared with a checkpoint.
    pub shard_copies: u64,
    /// Shards written (marked dirty) for the first time in an epoch.
    pub shards_dirtied: u64,
}

/// A copy-on-write sharded region.
#[derive(Debug, Clone)]
pub struct ShardArena {
    len: usize,
    shard_len: usize,
    shards: Vec<Rc<Vec<u8>>>,
    dirty: Vec<bool>,
    dirty_list: Vec<u32>,
    epoch: u64,
    stats: ArenaStats,
}

/// A point-in-time state of a [`ShardArena`]; shares shards with it.
#[derive(Debug, Clone)]
pub struct ArenaCheckpoint {
    pub epoch: u64,
    len: usize,
    shard_len: usize,
    shards: Vec<Rc<Vec<u8>>>,
    /// Shards written between the previous checkpoint and this one, in the
    /// order they were first written.
    pub dirty: Vec<u32>,
}

impl ArenaCheckpoint {
    /// How many of this checkpoint's shards are still shared with `arena`
    /// (no copy made).
    pub fn shared_with(&self, arena: &ShardArena) -> usize {
        self.shards
            .iter()
            .zip(&arena.shards)
            .filter(|(a, b)| Rc::ptr_eq(a, b))
            .count()
    }
    /// The checkpointed bytes of one shard.
    pub fn shard(&self, index: usize) -> Option<&[u8]> {
        self.shards.get(index).map(|s| s.as_slice())
    }
    /// The incremental delta: the shards written since the previous
    /// checkpoint, with their bytes at this one.
    pub fn delta(&self) -> ArenaDelta {
        ArenaDelta {
            keyframe: false,
            epoch: self.epoch,
            len: self.len as u64,
            shard_len: self.shard_len as u32,
            shards: self
                .dirty
                .iter()
                .map(|&i| (i, self.shards[i as usize].as_ref().clone()))
                .collect(),
        }
    }
    /// A self-contained image of every shard, which [`ArenaDelta::apply`]
    /// accepts at any epoch (the base later deltas replay onto).
    pub fn keyframe(&self) -> ArenaDelta {
        ArenaDelta {
            keyframe: true,
            epoch: self.epoch,
            len: self.len as u64,
            shard_len: self.shard_len as u32,
            shards: self
                .shards
                .iter()
                .enumerate()
                .map(|(i, s)| (i as u32, s.as_ref().clone()))
                .collect(),
        }
    }
}

impl ShardArena {
    pub fn new(len: usize, shard_len: usize) -> Result<Self, ArenaError> {
        let bad = ArenaError::BadGeometry { len, shard_len };
        if !shard_len.is_power_of_two() || !(64..=1 << 20).contains(&shard_len) || len == 0 {
            return Err(bad);
        }
        let count = len.div_ceil(shard_len);
        if u32::try_from(count).is_err() {
            return Err(bad);
        }
        let zero = Rc::new(vec![0u8; shard_len]);
        Ok(Self {
            len,
            shard_len,
            // Fresh shards share one zero page until written.
            shards: vec![zero; count],
            dirty: vec![false; count],
            dirty_list: Vec::new(),
            epoch: 0,
            stats: ArenaStats::default(),
        })
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn shard_len(&self) -> usize {
        self.shard_len
    }
    pub fn shard_count(&self) -> usize {
        self.shards.len()
    }
    /// Checkpoints taken so far (the epoch the next checkpoint will have
    /// is `epoch() + 1`).
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn stats(&self) -> ArenaStats {
        self.stats
    }
    /// Shards written since the last checkpoint.
    pub fn dirty(&self) -> &[u32] {
        &self.dirty_list
    }
    fn check(&self, offset: usize, len: usize) -> Result<(), ArenaError> {
        match offset.checked_add(len) {
            Some(end) if end <= self.len => Ok(()),
            _ => Err(ArenaError::OutOfBounds {
                offset,
                len,
                arena: self.len,
            }),
        }
    }
    /// The bytes of one shard (the last shard may extend past `len`; those
    /// bytes stay zero unless written through [`Self::shard_mut`]).
    pub fn shard(&self, index: usize) -> Option<&[u8]> {
        self.shards.get(index).map(|s| s.as_slice())
    }
    /// Writable access to one shard: marks it dirty and copies it first if
    /// a checkpoint (or the shared zero shard) still holds it.
    pub fn shard_mut(&mut self, index: usize) -> Option<&mut [u8]> {
        if index >= self.shards.len() {
            return None;
        }
        if !self.dirty[index] {
            self.dirty[index] = true;
            self.dirty_list.push(index as u32);
            self.stats.shards_dirtied += 1;
        }
        let shard = &mut self.shards[index];
        if Rc::strong_count(shard) > 1 {
            self.stats.shard_copies += 1;
        }
        Some(Rc::make_mut(shard).as_mut_slice())
    }
    pub fn read(&self, offset: usize, out: &mut [u8]) -> Result<(), ArenaError> {
        self.check(offset, out.len())?;
        let mut done = 0;
        while done < out.len() {
            let at = offset + done;
            let (i, o) = (at / self.shard_len, at % self.shard_len);
            let n = (self.shard_len - o).min(out.len() - done);
            out[done..done + n].copy_from_slice(&self.shards[i][o..o + n]);
            done += n;
        }
        Ok(())
    }
    pub fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), ArenaError> {
        self.check(offset, data.len())?;
        let mut done = 0;
        while done < data.len() {
            let at = offset + done;
            let (i, o) = (at / self.shard_len, at % self.shard_len);
            let n = (self.shard_len - o).min(data.len() - done);
            let shard = self.shard_mut(i).expect("index checked against len");
            shard[o..o + n].copy_from_slice(&data[done..done + n]);
            done += n;
        }
        Ok(())
    }
    /// Take an incremental checkpoint: shares every shard, copies none.
    pub fn checkpoint(&mut self) -> ArenaCheckpoint {
        self.epoch += 1;
        self.stats.checkpoints += 1;
        let dirty = std::mem::take(&mut self.dirty_list);
        for &i in &dirty {
            self.dirty[i as usize] = false;
        }
        ArenaCheckpoint {
            epoch: self.epoch,
            len: self.len,
            shard_len: self.shard_len,
            shards: self.shards.clone(),
            dirty,
        }
    }
    /// Return to `checkpoint`'s contents (sharing its shards again). Writes
    /// since are discarded; the dirty set restarts empty, and the epoch
    /// becomes the checkpoint's, so a delta of a later checkpoint follows
    /// it.
    pub fn restore(&mut self, checkpoint: &ArenaCheckpoint) -> Result<(), ArenaError> {
        if checkpoint.len != self.len || checkpoint.shard_len != self.shard_len {
            return Err(ArenaError::ShapeMismatch);
        }
        self.shards.clone_from(&checkpoint.shards);
        for &i in &self.dirty_list {
            self.dirty[i as usize] = false;
        }
        self.dirty_list.clear();
        self.epoch = checkpoint.epoch;
        Ok(())
    }
}

/// The shards written in one checkpoint epoch, with their bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArenaDelta {
    /// Every shard, applicable at any epoch, rather than one epoch's writes.
    pub keyframe: bool,
    pub epoch: u64,
    pub len: u64,
    pub shard_len: u32,
    pub shards: Vec<(u32, Vec<u8>)>,
}

impl ArenaDelta {
    /// Versioned little-endian image with a BLAKE3 trailer.
    pub fn encode(&self) -> Vec<u8> {
        let mut out =
            Vec::with_capacity(32 + self.shards.len() * (4 + self.shard_len as usize) + DIGEST_LEN);
        out.extend_from_slice(&ARENA_DELTA_MAGIC);
        out.extend_from_slice(&self.epoch.to_le_bytes());
        out.extend_from_slice(&self.len.to_le_bytes());
        out.extend_from_slice(&self.shard_len.to_le_bytes());
        out.extend_from_slice(&u32::from(self.keyframe).to_le_bytes());
        out.extend_from_slice(&(self.shards.len() as u32).to_le_bytes());
        for (i, bytes) in &self.shards {
            out.extend_from_slice(&i.to_le_bytes());
            out.extend_from_slice(bytes);
        }
        let digest = blake3::hash(&out);
        out.extend_from_slice(digest.as_bytes());
        out
    }

    /// Parse a delta image. Every shard must lie inside the arena and
    /// appear once; `max_shards` bounds the count before any allocation.
    pub fn decode(bytes: &[u8], max_shards: usize) -> Result<Self, ArenaError> {
        const HEADER: usize = 8 + 8 + 8 + 4 + 4 + 4;
        if bytes.len() < HEADER + DIGEST_LEN {
            return Err(ArenaError::Truncated);
        }
        if bytes[..8] != ARENA_DELTA_MAGIC {
            return Err(ArenaError::BadMagic);
        }
        let (body, digest) = bytes.split_at(bytes.len() - DIGEST_LEN);
        if blake3::hash(body).as_bytes() != digest {
            return Err(ArenaError::Corrupt);
        }
        let u64_at = |at: usize| u64::from_le_bytes(body[at..at + 8].try_into().unwrap());
        let u32_at = |at: usize| u32::from_le_bytes(body[at..at + 4].try_into().unwrap());
        let epoch = u64_at(8);
        let len = u64_at(16);
        let shard_len = u32_at(24);
        let keyframe = match u32_at(28) {
            0 => false,
            1 => true,
            _ => return Err(ArenaError::BadMagic),
        };
        let count = u32_at(32) as usize;
        if !shard_len.is_power_of_two() || !(64..=1 << 20).contains(&shard_len) || len == 0 {
            return Err(ArenaError::BadGeometry {
                len: len as usize,
                shard_len: shard_len as usize,
            });
        }
        let total = len.div_ceil(u64::from(shard_len));
        if count > max_shards || count as u64 > total || (keyframe && count as u64 != total) {
            return Err(ArenaError::BadShard(count as u32));
        }
        let entry = 4 + shard_len as usize;
        let expected = count
            .checked_mul(entry)
            .and_then(|n| n.checked_add(HEADER))
            .ok_or(ArenaError::Truncated)?;
        if body.len() < expected {
            return Err(ArenaError::Truncated);
        }
        if body.len() > expected {
            return Err(ArenaError::TrailingBytes(body.len() - expected));
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut shards = Vec::with_capacity(count);
        for k in 0..count {
            let at = HEADER + k * entry;
            let i = u32_at(at);
            if u64::from(i) >= total || !seen.insert(i) {
                return Err(ArenaError::BadShard(i));
            }
            shards.push((i, body[at + 4..at + entry].to_vec()));
        }
        Ok(Self {
            keyframe,
            epoch,
            len,
            shard_len,
            shards,
        })
    }

    /// Replay this delta onto `arena`, which must be at the previous epoch
    /// (a keyframe applies at any epoch and moves `arena` to just before
    /// its own). The written shards become dirty in `arena`, as if written
    /// there.
    pub fn apply(&self, arena: &mut ShardArena) -> Result<(), ArenaError> {
        if self.len != arena.len as u64 || self.shard_len as usize != arena.shard_len {
            return Err(ArenaError::ShapeMismatch);
        }
        if self.keyframe {
            if self.epoch == 0 {
                return Err(ArenaError::EpochMismatch {
                    expected: 1,
                    found: 0,
                });
            }
            arena.epoch = self.epoch - 1;
        }
        let expected = arena.epoch + 1;
        if self.epoch != expected {
            return Err(ArenaError::EpochMismatch {
                expected,
                found: self.epoch,
            });
        }
        for (i, bytes) in &self.shards {
            arena
                .shard_mut(*i as usize)
                .ok_or(ArenaError::BadShard(*i))?
                .copy_from_slice(bytes);
        }
        Ok(())
    }
}

/// Store `checkpoint` as a snapshot epoch; returns the epoch id.
///
/// Deltas depend on every image before them, and the recorder overwrites
/// its oldest snapshot slot once all are used. So every `keyframe_every`-th
/// arena epoch (epochs 1, 1 + k, 1 + 2k, …) is stored as a full
/// [`ArenaCheckpoint::keyframe`] instead, and [`rebuild_arena`] rebuilds
/// the newest state from the newest retained keyframe. A keyframe stays
/// retained when every arena epoch is captured, `keyframe_every` is at most
/// the recorder's `snapshot_slots`, and no other snapshots share the
/// recorder; with other snapshots in the same recorder, lower the interval
/// accordingly.
pub fn capture_arena_epoch(
    recorder: &mut FlightRecorder,
    time_ns: u64,
    checkpoint: &ArenaCheckpoint,
    keyframe_every: u64,
) -> Result<u32, ArenaError> {
    if keyframe_every == 0 {
        return Err(ArenaError::NoKeyframe);
    }
    let image = if checkpoint
        .epoch
        .saturating_sub(1)
        .is_multiple_of(keyframe_every)
    {
        checkpoint.keyframe()
    } else {
        checkpoint.delta()
    };
    recorder
        .capture_snapshot(time_ns, &image.encode())
        .map_err(ArenaError::Record)
}

/// Rebuild an arena from stored images, oldest first (for example the
/// arena snapshots of an opened bundle): starts at the newest keyframe and
/// replays the deltas after it. Images before that keyframe are not needed.
pub fn rebuild_arena<'a>(
    images: impl IntoIterator<Item = &'a [u8]>,
    max_shards: usize,
) -> Result<ShardArena, ArenaError> {
    let deltas = images
        .into_iter()
        .map(|b| ArenaDelta::decode(b, max_shards))
        .collect::<Result<Vec<_>, _>>()?;
    let start = deltas
        .iter()
        .rposition(|d| d.keyframe)
        .ok_or(ArenaError::NoKeyframe)?;
    let base = &deltas[start];
    let mut arena = ShardArena::new(base.len as usize, base.shard_len as usize)?;
    for d in &deltas[start..] {
        d.apply(&mut arena)?;
        arena.checkpoint();
    }
    Ok(arena)
}

/// A kernel write-tracking mechanism the design names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WriteTracking {
    /// Linux `userfaultfd` with `UFFDIO_WRITEPROTECT`.
    UserfaultfdWriteProtect,
    /// `mprotect(PROT_READ)` plus a `SIGSEGV` handler that records and
    /// re-enables the page.
    MprotectFaults,
    /// Linux soft-dirty page bits (`/proc/self/clear_refs`, pagemap bit 55).
    SoftDirty,
    /// Mach `vm_copy` copy-on-write snapshots (macOS).
    MachVmCopy,
    /// `GetWriteWatch` / `MEM_RESET` page tracking (Windows).
    WindowsWriteWatch,
}

/// What [`probe_write_tracking`] found for one mechanism.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackingProbe {
    pub mechanism: WriteTracking,
    /// Whether the host would permit it (`None`: not determinable without
    /// using it).
    pub host_permits: Option<bool>,
    /// Whether the probe exercised the mechanism itself.
    pub exercised: bool,
    /// Whether `TrackedArena` uses it on this host.
    pub used: bool,
    pub detail: String,
}

/// Report each kernel write-tracking mechanism on this host. On Linux the
/// soft-dirty bit is exercised for real on one page: the probe clears the
/// process's soft-dirty bits, writes the page and reads its pagemap entry
/// back. (Clearing soft-dirty bits is process-wide; nothing else in Vesper
/// uses them.)
pub fn probe_write_tracking() -> Vec<TrackingProbe> {
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    let host = host_tracking();
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    let host = TrackingProbe {
        mechanism: WriteTracking::MprotectFaults,
        host_permits: None,
        exercised: false,
        used: false,
        detail: "no write-tracking backend for this OS".into(),
    };
    #[allow(unused_mut)]
    let mut out = vec![host];
    #[cfg(target_os = "linux")]
    {
        out.push(linux::userfaultfd());
        out.push(linux::soft_dirty());
    }
    #[cfg(target_os = "macos")]
    out.push(TrackingProbe {
        mechanism: WriteTracking::MachVmCopy,
        host_permits: Some(true),
        exercised: false,
        used: false,
        detail: "vm_copy is available on macOS; TrackedArena copies dirty pages found by mprotect faults instead".into(),
    });
    out
}

/// Exercise the mechanism [`crate::tracked_arena::TrackedArena`] uses: arm
/// a one-page region, store through a raw pointer, and check the page is
/// reported dirty.
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn host_tracking() -> TrackingProbe {
    use vesper_fiber_abi::write_watch::{Mechanism, TrackedRegion};
    let mechanism = match Mechanism::HOST {
        Mechanism::MprotectFaults => WriteTracking::MprotectFaults,
        Mechanism::WindowsWriteWatch => WriteTracking::WindowsWriteWatch,
    };
    let result = (|| -> std::io::Result<Vec<usize>> {
        let mut region = TrackedRegion::new(1)?;
        region.arm()?;
        region.write(0, &[1])?;
        region.take_dirty()
    })();
    let (host_permits, detail) = match result {
        Ok(dirty) if dirty == [0] => (
            Some(true),
            format!(
                "{}: a store to an armed page was recorded",
                Mechanism::HOST.name()
            ),
        ),
        Ok(dirty) => (
            Some(false),
            format!(
                "{}: expected page 0 dirty, got {dirty:?}",
                Mechanism::HOST.name()
            ),
        ),
        Err(e) => (Some(false), format!("{}: {e}", Mechanism::HOST.name())),
    };
    TrackingProbe {
        mechanism,
        used: host_permits == Some(true),
        host_permits,
        exercised: true,
        detail,
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{TrackingProbe, WriteTracking};
    use std::io::{Read, Seek, SeekFrom, Write};

    fn cap_sys_ptrace() -> Option<bool> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find(|l| l.starts_with("CapEff:"))?;
        let caps = u64::from_str_radix(line.split_whitespace().nth(1)?, 16).ok()?;
        Some(caps & (1 << 19) != 0)
    }

    pub(super) fn userfaultfd() -> TrackingProbe {
        let unprivileged = std::fs::read_to_string("/proc/sys/vm/unprivileged_userfaultfd")
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok());
        let ptrace = cap_sys_ptrace();
        let device = std::fs::File::open("/dev/userfaultfd").is_ok();
        let host_permits = match (unprivileged, ptrace) {
            (Some(1), _) | (_, Some(true)) => Some(true),
            _ if device => Some(true),
            (Some(0), Some(false)) => Some(false),
            _ => None,
        };
        TrackingProbe {
            mechanism: WriteTracking::UserfaultfdWriteProtect,
            host_permits,
            exercised: false,
            used: false,
            detail: format!(
                "unprivileged_userfaultfd={unprivileged:?}, CAP_SYS_PTRACE={ptrace:?}, /dev/userfaultfd openable={device}; not used: TrackedArena uses mprotect faults, which need no privilege; CI runners usually lack CAP_SYS_PTRACE"
            ),
        }
    }

    #[repr(C, align(4096))]
    struct Page([u8; 4096]);

    fn pagemap_entry(address: usize) -> std::io::Result<u64> {
        let mut f = std::fs::File::open("/proc/self/pagemap")?;
        f.seek(SeekFrom::Start((address / 4096 * 8) as u64))?;
        let mut b = [0u8; 8];
        f.read_exact(&mut b)?;
        Ok(u64::from_le_bytes(b))
    }

    pub(super) fn soft_dirty() -> TrackingProbe {
        let mut page = Box::new(Page([0; 4096]));
        page.0[0] = 1;
        let address = page.0.as_ptr() as usize;
        let result = (|| -> std::io::Result<(bool, bool)> {
            std::fs::OpenOptions::new()
                .write(true)
                .open("/proc/self/clear_refs")?
                .write_all(b"4")?;
            let cleared = pagemap_entry(address)? >> 55 & 1;
            page.0[1] = std::hint::black_box(2);
            let written = pagemap_entry(address)? >> 55 & 1;
            Ok((cleared == 0, written == 1))
        })();
        std::hint::black_box(&page);
        let page_size_ok = std::fs::read_to_string("/proc/self/smaps")
            .map(|s| s.contains("KernelPageSize:        4 kB"))
            .unwrap_or(false);
        let (host_permits, detail) = match result {
            Ok((true, true)) => (
                Some(true),
                "a write after clear_refs set the page's soft-dirty bit".to_string(),
            ),
            Ok((cleared, set)) => (
                Some(false),
                format!(
                    "soft-dirty bit did not track the write (cleared={cleared}, set after write={set}); the kernel is likely built without CONFIG_MEM_SOFT_DIRTY{}",
                    if page_size_ok {
                        ""
                    } else {
                        " (or uses pages larger than 4 KiB)"
                    }
                ),
            ),
            Err(e) => (Some(false), format!("clear_refs/pagemap not usable: {e}")),
        };
        TrackingProbe {
            mechanism: WriteTracking::SoftDirty,
            host_permits,
            exercised: true,
            used: false,
            detail: format!(
                "{detail}; soft-dirty bits are process-wide and need pagemap scans, so TrackedArena uses mprotect faults instead"
            ),
        }
    }
}
