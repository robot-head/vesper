//! Single-producer/single-consumer ring with Linux AF_XDP (libxdp `xsk.h`) semantics.
//!
//! * The size is a power of two; slots are addressed by `idx & (size - 1)`.
//! * `producer` and `consumer` are free-running `u32` indices that wrap modulo
//!   2^32. Every index difference is computed with `wrapping_sub`; that is
//!   correct because the ring never holds more than `size <= 2^31` entries, so
//!   the true difference always fits.
//! * Each side keeps cached copies of the other side's index and only re-reads
//!   the shared one when its cache says it cannot make progress, exactly as
//!   `xsk_prod_nb_free` / `xsk_cons_nb_avail` do. The producer's cached
//!   consumer is stored as `consumer + size`, like libxdp.
//! * Batches: the producer `reserve`s `n` slots (all or nothing), `write`s them
//!   and `submit`s; the consumer `peek`s up to `n`, `read`s and `release`s.
//!
//! This is a single-threaded model: the shared indices are plain fields, and
//! the publication order that libxdp enforces with release/acquire barriers is
//! represented by "submit/release is the only place a shared index moves".
//! The Linux backend implements the same traits over the kernel's mmapped
//! rings with real atomics.

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

/// The software ring model. Both halves live in one value; the software NIC
/// drives the half the application does not.
#[derive(Debug, Clone)]
pub struct Ring<T> {
    slots: Box<[T]>,
    mask: u32,
    producer: u32,
    consumer: u32,
    prod_cached_prod: u32,
    /// `consumer + size` as last seen by the producer.
    prod_cached_cons: u32,
    cons_cached_prod: u32,
    cons_cached_cons: u32,
}

impl<T: Copy + Default> Ring<T> {
    pub fn new(size: u32) -> Result<Self, RingError> {
        Self::with_initial_index(size, 0)
    }

    /// Starts both indices at `start`, so tests can exercise `u32` wrap-around.
    pub fn with_initial_index(size: u32, start: u32) -> Result<Self, RingError> {
        if !size.is_power_of_two() || size > 1 << 31 {
            return Err(RingError::InvalidSize);
        }
        let len = usize::try_from(size).map_err(|_| RingError::InvalidSize)?;
        Ok(Self {
            slots: vec![T::default(); len].into_boxed_slice(),
            mask: size - 1,
            producer: start,
            consumer: start,
            prod_cached_prod: start,
            prod_cached_cons: start.wrapping_add(size),
            cons_cached_prod: start,
            cons_cached_cons: start,
        })
    }
}

impl<T: Copy> Ring<T> {
    pub fn capacity(&self) -> u32 {
        self.mask + 1
    }
    /// Published, unreleased entries (the authoritative occupancy).
    pub fn len(&self) -> u32 {
        self.producer.wrapping_sub(self.consumer)
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn producer_index(&self) -> u32 {
        self.producer
    }
    pub fn consumer_index(&self) -> u32 {
        self.consumer
    }
    /// Published entries in ring order, for the auditor.
    pub fn committed(&self) -> impl Iterator<Item = T> + '_ {
        (0..self.len()).map(move |i| self.slot(self.consumer.wrapping_add(i)))
    }

    fn slot(&self, idx: u32) -> T {
        // `mask < 2^31` and `usize` is at least 32 bits on supported targets.
        self.slots[(idx & self.mask) as usize]
    }
    fn reserved(&self) -> u32 {
        self.prod_cached_prod.wrapping_sub(self.producer)
    }
    fn peeked(&self) -> u32 {
        self.cons_cached_cons.wrapping_sub(self.consumer)
    }
}

impl<T: Copy> ProducerRing<T> for Ring<T> {
    fn size(&self) -> u32 {
        self.capacity()
    }
    fn free_entries(&mut self, wanted: u32) -> u32 {
        let free = self.prod_cached_cons.wrapping_sub(self.prod_cached_prod);
        if free >= wanted {
            return free;
        }
        self.prod_cached_cons = self.consumer.wrapping_add(self.capacity());
        self.prod_cached_cons.wrapping_sub(self.prod_cached_prod)
    }
    fn reserve(&mut self, n: u32) -> Option<u32> {
        if n == 0 || self.free_entries(n) < n {
            return None;
        }
        let idx = self.prod_cached_prod;
        self.prod_cached_prod = self.prod_cached_prod.wrapping_add(n);
        Some(idx)
    }
    fn write(&mut self, idx: u32, value: T) -> Result<(), RingError> {
        if idx.wrapping_sub(self.producer) >= self.reserved() {
            return Err(RingError::IndexOutsideWindow { idx });
        }
        self.slots[(idx & self.mask) as usize] = value;
        Ok(())
    }
    fn unreserve(&mut self, n: u32) -> Result<(), RingError> {
        let outstanding = self.reserved();
        if n > outstanding {
            return Err(RingError::CountExceedsWindow {
                requested: n,
                outstanding,
            });
        }
        self.prod_cached_prod = self.prod_cached_prod.wrapping_sub(n);
        Ok(())
    }
    fn submit(&mut self, n: u32) -> Result<(), RingError> {
        let outstanding = self.reserved();
        if n > outstanding {
            return Err(RingError::CountExceedsWindow {
                requested: n,
                outstanding,
            });
        }
        self.producer = self.producer.wrapping_add(n);
        Ok(())
    }
}

impl<T: Copy> ConsumerRing<T> for Ring<T> {
    fn size(&self) -> u32 {
        self.capacity()
    }
    fn peek(&mut self, n: u32) -> (u32, u32) {
        let mut entries = self.cons_cached_prod.wrapping_sub(self.cons_cached_cons);
        if entries == 0 {
            self.cons_cached_prod = self.producer;
            entries = self.cons_cached_prod.wrapping_sub(self.cons_cached_cons);
        }
        let count = entries.min(n);
        let idx = self.cons_cached_cons;
        self.cons_cached_cons = self.cons_cached_cons.wrapping_add(count);
        (count, idx)
    }
    fn read(&self, idx: u32) -> Result<T, RingError> {
        if idx.wrapping_sub(self.consumer) >= self.peeked() {
            return Err(RingError::IndexOutsideWindow { idx });
        }
        Ok(self.slot(idx))
    }
    fn unpeek(&mut self, n: u32) -> Result<(), RingError> {
        let outstanding = self.peeked();
        if n > outstanding {
            return Err(RingError::CountExceedsWindow {
                requested: n,
                outstanding,
            });
        }
        self.cons_cached_cons = self.cons_cached_cons.wrapping_sub(n);
        Ok(())
    }
    fn release(&mut self, n: u32) -> Result<(), RingError> {
        let outstanding = self.peeked();
        if n > outstanding {
            return Err(RingError::CountExceedsWindow {
                requested: n,
                outstanding,
            });
        }
        self.consumer = self.consumer.wrapping_add(n);
        Ok(())
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
