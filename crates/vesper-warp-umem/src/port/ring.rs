//! The Vesper build of [`Ring`]: the ring runs in `vesper/src/ring.ves`
//! (`RingCore`). The public software ring holds `u64` entries; the software
//! NIC's descriptor rings live inside the Vesper device.

use std::marker::PhantomData;

use crate::binding::{self, RingCore};
use crate::error::RingError;

/// The producer half of a ring.
pub trait ProducerRing<T: Copy> {
    fn size(&self) -> u32;
    /// Slots the producer could reserve now (refreshing its cache if fewer than `wanted`).
    fn free_entries(&mut self, wanted: u32) -> u32;
    /// Reserves exactly `n` slots and returns the first index, or `None`.
    fn reserve(&mut self, n: u32) -> Option<u32>;
    fn write(&mut self, idx: u32, value: T) -> Result<(), RingError>;
    /// Gives back the last `n` reserved, unsubmitted slots.
    fn unreserve(&mut self, n: u32) -> Result<(), RingError>;
    /// Publishes the first `n` reserved slots.
    fn submit(&mut self, n: u32) -> Result<(), RingError>;
}

/// The consumer half of a ring.
pub trait ConsumerRing<T: Copy> {
    fn size(&self) -> u32;
    /// Peeks up to `n` entries; returns `(count, first_idx)`.
    fn peek(&mut self, n: u32) -> (u32, u32);
    fn read(&self, idx: u32) -> Result<T, RingError>;
    /// Gives back the last `n` peeked, unreleased entries.
    fn unpeek(&mut self, n: u32) -> Result<(), RingError>;
    /// Returns the first `n` peeked entries' slots to the producer.
    fn release(&mut self, n: u32) -> Result<(), RingError>;
}

fn done(e: Option<RingError>) -> Result<(), RingError> {
    e.map_or(Ok(()), Err)
}

/// The software ring model (`u64` entries in the Vesper build).
#[derive(Debug)]
pub struct Ring<T> {
    core: RingCore,
    _entry: PhantomData<T>,
}

impl Ring<u64> {
    pub fn new(size: u32) -> Result<Self, RingError> {
        Self::with_initial_index(size, 0)
    }

    /// Starts both indices at `start`, so tests can exercise `u32` wrap-around.
    pub fn with_initial_index(size: u32, start: u32) -> Result<Self, RingError> {
        Ok(Self {
            core: binding::ring_core_new(size, start)?,
            _entry: PhantomData,
        })
    }

    pub fn capacity(&self) -> u32 {
        binding::ring_core_capacity(&self.core)
    }
    /// Published, unreleased entries (the authoritative occupancy).
    pub fn len(&self) -> u32 {
        binding::ring_core_len(&self.core)
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn producer_index(&self) -> u32 {
        binding::ring_core_producer_index(&self.core)
    }
    pub fn consumer_index(&self) -> u32 {
        binding::ring_core_consumer_index(&self.core)
    }
    /// Published entries in ring order, for the auditor.
    pub fn committed(&self) -> impl Iterator<Item = u64> + '_ {
        binding::ring_core_committed(&self.core).into_iter()
    }
}

impl ProducerRing<u64> for Ring<u64> {
    fn size(&self) -> u32 {
        self.capacity()
    }
    fn free_entries(&mut self, wanted: u32) -> u32 {
        binding::ring_core_free_entries(&mut self.core, wanted)
    }
    fn reserve(&mut self, n: u32) -> Option<u32> {
        binding::ring_core_reserve(&mut self.core, n)
    }
    fn write(&mut self, idx: u32, value: u64) -> Result<(), RingError> {
        done(binding::ring_core_write(&mut self.core, idx, value))
    }
    fn unreserve(&mut self, n: u32) -> Result<(), RingError> {
        done(binding::ring_core_unreserve(&mut self.core, n))
    }
    fn submit(&mut self, n: u32) -> Result<(), RingError> {
        done(binding::ring_core_submit(&mut self.core, n))
    }
}

impl ConsumerRing<u64> for Ring<u64> {
    fn size(&self) -> u32 {
        self.capacity()
    }
    fn peek(&mut self, n: u32) -> (u32, u32) {
        binding::ring_core_peek(&mut self.core, n)
    }
    fn read(&self, idx: u32) -> Result<u64, RingError> {
        binding::ring_core_read(&self.core, idx)
    }
    fn unpeek(&mut self, n: u32) -> Result<(), RingError> {
        done(binding::ring_core_unpeek(&mut self.core, n))
    }
    fn release(&mut self, n: u32) -> Result<(), RingError> {
        done(binding::ring_core_release(&mut self.core, n))
    }
}

/// Producer-side helper: reserve, write and submit one entry.
pub fn push_one<T: Copy, R: ProducerRing<T> + ?Sized>(ring: &mut R, value: T) -> bool {
    let Some(idx) = ring.reserve(1) else {
        return false;
    };
    // The index was just reserved, so neither call can fail.
    ring.write(idx, value).is_ok() && ring.submit(1).is_ok()
}

/// Consumer-side helper: peek, read and release one entry.
pub fn pop_one<T: Copy, R: ConsumerRing<T> + ?Sized>(ring: &mut R) -> Option<T> {
    let (count, idx) = ring.peek(1);
    if count == 0 {
        return None;
    }
    let value = ring.read(idx).ok()?;
    ring.release(1).ok()?;
    Some(value)
}
