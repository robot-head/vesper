//! Linear output leases (design §32.2 `RtOutputLease`, contract name
//! `RtBufferLease`).
//!
//! An [`RtOutputPort`] owns a fixed set of preallocated period buffers. The
//! realtime callback takes an [`RtBufferLease`] for the current period, must
//! write every frame of every channel in order, and then discharges the lease
//! with exactly one consuming call:
//!
//! * [`RtOutputPort::commit`] succeeds only when every frame was written;
//!   otherwise the lease is handed back unchanged;
//! * [`RtOutputPort::emergency_silence_and_commit`] zero-fills (safe-torques)
//!   whatever was not written and always commits.
//!
//! The driver side ([`RtOutputPort::complete`]) copies the committed period
//! out and recycles the buffer. Dropping a lease is detected: the port's
//! [`LeaseAuditor`] counts it as leaked. Nothing allocates after
//! [`RtOutputPort::new`].
//!
//! Rust ownership gives the linear discipline here; it is not the Vesper
//! source `linear struct` typing rule.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// How a period was committed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitKind {
    /// Every frame was rendered.
    Rendered,
    /// Unwritten frames were zero-filled.
    Silenced {
        /// Frames that had been written before silencing.
        written_frames: usize,
    },
}

/// Shared lease counters.
#[derive(Debug, Default)]
pub struct LeaseAuditor {
    issued: AtomicU64,
    committed: AtomicU64,
    silenced: AtomicU64,
    leaked: AtomicU64,
}

impl LeaseAuditor {
    /// Leases issued.
    pub fn issued(&self) -> u64 {
        self.issued.load(Ordering::Relaxed)
    }
    /// Leases committed fully rendered.
    pub fn committed(&self) -> u64 {
        self.committed.load(Ordering::Relaxed)
    }
    /// Leases committed through emergency silence.
    pub fn silenced(&self) -> u64 {
        self.silenced.load(Ordering::Relaxed)
    }
    /// Leases dropped without a consuming call.
    pub fn leaked(&self) -> u64 {
        self.leaked.load(Ordering::Relaxed)
    }
}

/// Lease write errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseError {
    /// Frames must be written in order; `expected` is the next frame.
    OutOfOrder {
        /// Next frame to write.
        expected: usize,
        /// Frame the caller tried to write.
        got: usize,
    },
    /// The frame has the wrong number of channel samples.
    ChannelCount {
        /// Channels in the lease.
        expected: usize,
        /// Samples supplied.
        got: usize,
    },
    /// The period is already full.
    Full,
    /// A sample was NaN or infinite.
    NonFinite,
}

impl fmt::Display for LeaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for LeaseError {}

/// A leased output period (linear: commit or silence it).
#[must_use = "an RtBufferLease must be committed or emergency-silenced"]
pub struct RtBufferLease {
    data: Option<Box<[f32]>>,
    channels: usize,
    frames: usize,
    written: usize,
    auditor: Arc<LeaseAuditor>,
}

impl RtBufferLease {
    /// Channels per frame.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Frames in the period.
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Frames written so far.
    pub fn written(&self) -> usize {
        self.written
    }

    /// Write frame `frame` (must be the next one), one sample per channel.
    pub fn write_frame(&mut self, frame: usize, samples: &[f32]) -> Result<(), LeaseError> {
        if self.written >= self.frames {
            return Err(LeaseError::Full);
        }
        if frame != self.written {
            return Err(LeaseError::OutOfOrder {
                expected: self.written,
                got: frame,
            });
        }
        if samples.len() != self.channels {
            return Err(LeaseError::ChannelCount {
                expected: self.channels,
                got: samples.len(),
            });
        }
        if samples.iter().any(|s| !s.is_finite()) {
            return Err(LeaseError::NonFinite);
        }
        let data = self.data.as_mut().expect("live lease holds its buffer");
        let base = frame * self.channels;
        data[base..base + self.channels].copy_from_slice(samples);
        self.written += 1;
        Ok(())
    }
}

impl fmt::Debug for RtBufferLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RtBufferLease")
            .field("channels", &self.channels)
            .field("frames", &self.frames)
            .field("written", &self.written)
            .finish_non_exhaustive()
    }
}

impl Drop for RtBufferLease {
    fn drop(&mut self) {
        if self.data.is_some() {
            self.auditor.leaked.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// A committed period waiting for the driver.
struct Committed {
    data: Box<[f32]>,
    kind: CommitKind,
}

/// Preallocated output periods for one device or actuator.
pub struct RtOutputPort {
    channels: usize,
    frames: usize,
    free: Vec<Box<[f32]>>,
    committed: Vec<Committed>,
    auditor: Arc<LeaseAuditor>,
}

/// Port construction errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortError {
    /// Zero channels, frames or periods, or a size overflow.
    BadShape,
}

impl RtOutputPort {
    /// `periods` interleaved buffers of `channels * frames` samples.
    pub fn new(channels: usize, frames: usize, periods: usize) -> Result<Self, PortError> {
        let len = channels.checked_mul(frames).ok_or(PortError::BadShape)?;
        if len == 0 || periods == 0 {
            return Err(PortError::BadShape);
        }
        Ok(RtOutputPort {
            channels,
            frames,
            free: (0..periods)
                .map(|_| vec![0.0f32; len].into_boxed_slice())
                .collect(),
            committed: Vec::with_capacity(periods),
            auditor: Arc::new(LeaseAuditor::default()),
        })
    }

    /// The port's auditor.
    pub fn auditor(&self) -> &Arc<LeaseAuditor> {
        &self.auditor
    }

    /// Lease the next free period, if the driver has returned one.
    pub fn lease(&mut self) -> Option<RtBufferLease> {
        let data = self.free.pop()?;
        self.auditor.issued.fetch_add(1, Ordering::Relaxed);
        Some(RtBufferLease {
            data: Some(data),
            channels: self.channels,
            frames: self.frames,
            written: 0,
            auditor: Arc::clone(&self.auditor),
        })
    }

    /// Commit a fully written period; an incomplete one is handed back.
    pub fn commit(&mut self, mut lease: RtBufferLease) -> Result<(), RtBufferLease> {
        if lease.written != lease.frames || !self.owns(&lease) {
            return Err(lease);
        }
        let data = lease.data.take().expect("live lease holds its buffer");
        self.auditor.committed.fetch_add(1, Ordering::Relaxed);
        self.committed.push(Committed {
            data,
            kind: CommitKind::Rendered,
        });
        Ok(())
    }

    /// Zero-fill every unwritten frame and commit. Handed back only when the
    /// lease belongs to another port.
    pub fn emergency_silence_and_commit(
        &mut self,
        mut lease: RtBufferLease,
    ) -> Result<(), RtBufferLease> {
        if !self.owns(&lease) {
            return Err(lease);
        }
        let mut data = lease.data.take().expect("live lease holds its buffer");
        data[lease.written * lease.channels..].fill(0.0);
        self.auditor.silenced.fetch_add(1, Ordering::Relaxed);
        self.committed.push(Committed {
            data,
            kind: CommitKind::Silenced {
                written_frames: lease.written,
            },
        });
        Ok(())
    }

    fn owns(&self, lease: &RtBufferLease) -> bool {
        Arc::ptr_eq(&self.auditor, &lease.auditor)
    }

    /// Driver side: copy the oldest committed period into `out` (interleaved,
    /// `channels * frames` samples) and recycle its buffer.
    pub fn complete(&mut self, out: &mut [f32]) -> Option<CommitKind> {
        if self.committed.is_empty() || out.len() != self.channels * self.frames {
            return None;
        }
        let c = self.committed.remove(0);
        out.copy_from_slice(&c.data);
        self.free.push(c.data);
        Some(c.kind)
    }
}
