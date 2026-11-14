//! Kernel-tracked arena checkpoints (§31.1 item 2).
//!
//! A [`TrackedArena`] is checkpointable memory whose writes are recorded by
//! the MMU, not by an API: it lives in a
//! [`vesper_fiber_abi::write_watch::TrackedRegion`] (`mprotect` faults on
//! Linux and macOS, write watches on Windows), so stores through raw
//! pointers into it, from generated code, FFI or `unsafe` Rust, are seen
//! by the next [`TrackedArena::checkpoint`]. A checkpoint copies only the
//! shards whose pages were written in the epoch and returns them as an
//! [`ArenaDelta`] in the same versioned format as [`crate::ShardArena`], so
//! [`ArenaDelta::apply`], [`crate::arena::rebuild_arena`] and flight-recorder
//! snapshot epochs work unchanged.
//!
//! Unlike [`crate::ShardArena`] this arena does not share shards with its
//! checkpoints copy-on-write: the delta owns copies of the dirty shards
//! (its cost is one copy per written shard per epoch, the same bytes the
//! delta must hold anyway). Shards are whole pages (a power of two, at least
//! the page size and at most 1 MiB).
//!
//! This crate contains no unsafe code; it hands out a raw pointer
//! ([`TrackedArena::as_mut_ptr`]) and the caller's own unsafe stores are what
//! the tracking catches. Rust ownership of the arena is not evidence that
//! the Vesper source rules for region writes are implemented.
use std::io;

use vesper_fiber_abi::write_watch::{Mechanism, TrackedRegion};

use crate::arena::{ArenaDelta, ArenaError};
use crate::ring::{FlightRecorder, RecordError};

/// Errors of a [`TrackedArena`].
#[derive(Debug)]
pub enum TrackedArenaError {
    Arena(ArenaError),
    /// The kernel refused to map or (re-)arm the region.
    Os(io::Error),
    /// [`TrackedArena::capture`] ended the epoch but the recorder rejected
    /// its image. The image is handed back so it can still be recorded
    /// (for example `recorder.capture_snapshot(time, &image.encode())` on a
    /// larger recorder); later deltas build on it.
    NotRecorded {
        image: Box<ArenaDelta>,
        error: RecordError,
    },
}

impl std::fmt::Display for TrackedArenaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Arena(e) => write!(f, "{e}"),
            Self::Os(e) => write!(f, "chrono tracked arena: {e}"),
            Self::NotRecorded { image, error } => write!(
                f,
                "chrono tracked arena: epoch {} image not recorded: {error}",
                image.epoch
            ),
        }
    }
}
impl std::error::Error for TrackedArenaError {}
impl From<ArenaError> for TrackedArenaError {
    fn from(e: ArenaError) -> Self {
        Self::Arena(e)
    }
}
impl From<io::Error> for TrackedArenaError {
    fn from(e: io::Error) -> Self {
        Self::Os(e)
    }
}

/// Checkpointable memory tracked by the kernel. See the module docs.
#[derive(Debug)]
pub struct TrackedArena {
    region: TrackedRegion,
    len: usize,
    shard_len: usize,
    epoch: u64,
    shards: usize,
}

impl TrackedArena {
    /// `len` bytes in shards of `shard_len` (a power of two in
    /// `page..=1 MiB`), zeroed and armed at epoch 0.
    pub fn new(len: usize, shard_len: usize) -> Result<Self, TrackedArenaError> {
        let page = vesper_fiber_abi::write_watch::page_size();
        if !shard_len.is_power_of_two()
            || shard_len < page.max(64)
            || shard_len > 1 << 20
            || len == 0
            || u32::try_from(len.div_ceil(shard_len)).is_err()
        {
            return Err(ArenaError::BadGeometry { len, shard_len }.into());
        }
        let count = len.div_ceil(shard_len);
        let mut region = TrackedRegion::new(count * shard_len)?;
        region.arm()?;
        Ok(Self {
            region,
            len,
            shard_len,
            epoch: 0,
            shards: count,
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
        self.shards
    }
    /// Checkpoints taken so far.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    /// How writes are tracked on this host.
    pub fn mechanism(&self) -> Mechanism {
        self.region.mechanism()
    }
    /// Write faults serviced for this arena (Unix; 0 on Windows).
    pub fn serviced_faults(&self) -> u64 {
        self.region.serviced_faults()
    }
    /// The first byte. In-bounds stores through it are tracked (the arena's
    /// mapping spans `shard_count() * shard_len()` bytes).
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.region.as_mut_ptr()
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
    pub fn read(&self, offset: usize, out: &mut [u8]) -> Result<(), TrackedArenaError> {
        self.check(offset, out.len())?;
        Ok(self.region.read(offset, out)?)
    }
    /// A safe write; tracked by the same pages as a raw store.
    pub fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), TrackedArenaError> {
        self.check(offset, data.len())?;
        Ok(self.region.write(offset, data)?)
    }
    /// The shards written since the last checkpoint, without ending the
    /// epoch (the region keeps tracking).
    fn dirty_shards(&mut self) -> Result<Vec<u32>, TrackedArenaError> {
        let per_shard = self.shard_len / self.region.page_size();
        let mut dirty = vec![false; self.shards];
        for page in self.region.take_dirty()? {
            if let Some(d) = dirty.get_mut(page / per_shard) {
                *d = true;
            }
        }
        Ok(dirty
            .iter()
            .enumerate()
            .filter(|(_, d)| **d)
            .map(|(i, _)| i as u32)
            .collect())
    }
    fn shard_bytes(&self, index: u32) -> Result<Vec<u8>, TrackedArenaError> {
        let mut bytes = vec![0; self.shard_len];
        self.region
            .read(index as usize * self.shard_len, &mut bytes)?;
        Ok(bytes)
    }
    /// End the epoch: the delta of every shard written (by any means) since
    /// the previous checkpoint, with its bytes now, ascending by shard.
    pub fn checkpoint(&mut self) -> Result<ArenaDelta, TrackedArenaError> {
        let dirty = self.dirty_shards()?;
        self.epoch += 1;
        let shards = dirty
            .into_iter()
            .map(|i| Ok((i, self.shard_bytes(i)?)))
            .collect::<Result<Vec<_>, TrackedArenaError>>()?;
        Ok(ArenaDelta {
            keyframe: false,
            epoch: self.epoch,
            len: self.len as u64,
            shard_len: self.shard_len as u32,
            shards,
        })
    }
    /// End the epoch with a self-contained image of every shard.
    pub fn keyframe(&mut self) -> Result<ArenaDelta, TrackedArenaError> {
        let mut delta = self.checkpoint()?;
        delta.keyframe = true;
        delta.shards = (0..self.shard_count() as u32)
            .map(|i| Ok((i, self.shard_bytes(i)?)))
            .collect::<Result<Vec<_>, TrackedArenaError>>()?;
        Ok(delta)
    }
    /// Replay a delta (or keyframe) onto this arena, as
    /// [`ArenaDelta::apply`] does for a `ShardArena`: a delta must follow
    /// the arena's epoch, a keyframe applies at any epoch. Writes made in
    /// the current epoch are kept unless the delta overwrites their shard.
    /// The epoch then becomes the delta's.
    pub fn apply(&mut self, delta: &ArenaDelta) -> Result<(), TrackedArenaError> {
        if delta.len != self.len as u64 || delta.shard_len as usize != self.shard_len {
            return Err(ArenaError::ShapeMismatch.into());
        }
        let expected = self.epoch + 1;
        if !delta.keyframe && delta.epoch != expected {
            return Err(ArenaError::EpochMismatch {
                expected,
                found: delta.epoch,
            }
            .into());
        }
        if delta.keyframe && delta.epoch == 0 {
            return Err(ArenaError::EpochMismatch {
                expected: 1,
                found: 0,
            }
            .into());
        }
        for (i, bytes) in &delta.shards {
            if *i as usize >= self.shard_count() || bytes.len() != self.shard_len {
                return Err(ArenaError::BadShard(*i).into());
            }
        }
        for (i, bytes) in &delta.shards {
            self.region.write(*i as usize * self.shard_len, bytes)?;
        }
        // The replayed state is the delta's epoch; what replay wrote is
        // already in `delta`, so the next delta must not repeat it. Writes
        // of the abandoned epoch that the delta did not overwrite are lost
        // from tracking as well, so a replica should not mix local writes
        // with replay.
        self.region.take_dirty()?;
        self.epoch = delta.epoch;
        Ok(())
    }
    /// Return to the state of `image`, a keyframe, discarding every write
    /// since. Only shards that differ from it are rewritten. The epoch
    /// becomes the image's, so the next delta follows it and carries only
    /// writes made after the restore.
    pub fn restore(&mut self, image: &ArenaDelta) -> Result<(), TrackedArenaError> {
        if !image.keyframe {
            return Err(ArenaError::NoKeyframe.into());
        }
        if image.len != self.len as u64 || image.shard_len as usize != self.shard_len {
            return Err(ArenaError::ShapeMismatch.into());
        }
        for (i, bytes) in &image.shards {
            if *i as usize >= self.shard_count() || bytes.len() != self.shard_len {
                return Err(ArenaError::BadShard(*i).into());
            }
        }
        for (i, bytes) in &image.shards {
            if self.shard_bytes(*i)? != *bytes {
                self.region.write(*i as usize * self.shard_len, bytes)?;
            }
        }
        self.region.take_dirty()?;
        self.epoch = image.epoch;
        Ok(())
    }
    /// Store this epoch's checkpoint in `recorder` as a snapshot epoch:
    /// every `keyframe_every`-th epoch (1, 1 + k, …) as a keyframe, the
    /// others as deltas (see [`crate::arena::capture_arena_epoch`]).
    /// Returns the epoch id and the image. If the recorder rejects the
    /// image, the epoch has still ended and the image comes back in
    /// [`TrackedArenaError::NotRecorded`].
    pub fn capture(
        &mut self,
        recorder: &mut FlightRecorder,
        time_ns: u64,
        keyframe_every: u64,
    ) -> Result<(u32, ArenaDelta), TrackedArenaError> {
        if keyframe_every == 0 {
            return Err(ArenaError::NoKeyframe.into());
        }
        let image = if self.epoch.is_multiple_of(keyframe_every) {
            self.keyframe()?
        } else {
            self.checkpoint()?
        };
        match recorder.capture_snapshot(time_ns, &image.encode()) {
            Ok(id) => Ok((id, image)),
            Err(error) => Err(TrackedArenaError::NotRecorded {
                image: Box::new(image),
                error,
            }),
        }
    }
}
