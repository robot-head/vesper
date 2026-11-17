//! The Vesper build of [`Wheel`]: the scheduling runs in `vesper/src/wheel*.ves`
//! (`WheelCore`), and this wrapper keeps the payloads (token slab), the
//! wheel identity that makes handles of other wheels foreign, and the sink
//! adaptor for [`Wheel::drain`].

use std::fmt;

use super::*;
use crate::binding::WheelCore;
use crate::slab::Slab;

/// Fixed-capacity hierarchical hashed timing wheel owning payloads of `T`.
pub struct Wheel<T> {
    id: u64,
    core: WheelCore,
    payloads: Slab<T>,
}

impl<T> fmt::Debug for Wheel<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Wheel")
            .field("id", &self.id)
            .field("tick_ns", &self.core.tick_ns())
            .field("capacity", &self.capacity())
            .field("len", &self.len())
            .field("now_ns", &self.now_ns())
            .finish_non_exhaustive()
    }
}

impl<T> Wheel<T> {
    /// Build a wheel, preallocating all storage.
    pub fn new(config: WheelConfig) -> Result<Self, WheelConfigError> {
        let core = WheelCore::new(config)?;
        Ok(Self {
            id: next_wheel_id(),
            core,
            payloads: Slab::new(config.capacity),
        })
    }

    /// Number of pending entries.
    pub fn len(&self) -> usize {
        self.core.len() as usize
    }

    /// Whether no entry is pending.
    pub fn is_empty(&self) -> bool {
        self.core.len() == 0
    }

    /// Fixed capacity.
    pub fn capacity(&self) -> usize {
        self.payloads.capacity()
    }

    /// Tick width in nanoseconds.
    pub fn tick_ns(&self) -> u64 {
        self.core.tick_ns()
    }

    /// The wheel's clock: the latest `now` passed to `advance`/`poll`, or the
    /// configured start.
    pub fn now_ns(&self) -> u64 {
        self.core.now_ns()
    }

    /// Store `payload` to fire at `deadline_ns`. A deadline at or before the
    /// wheel's clock is accepted and fires on the next poll.
    pub fn insert(&mut self, deadline_ns: u64, payload: T) -> Result<TimerHandle, InsertError<T>> {
        match self.core.insert(deadline_ns) {
            Ok((index, generation)) => {
                self.payloads.put(index, payload);
                Ok(TimerHandle {
                    wheel: self.id,
                    index: index as u32,
                    generation,
                })
            }
            Err(kind) => Err(InsertError { kind, payload }),
        }
    }

    /// Deadline of a pending entry.
    pub fn deadline(&self, handle: &TimerHandle) -> Result<u64, HandleError> {
        if handle.wheel != self.id {
            return Err(HandleError::Foreign);
        }
        self.core
            .deadline(u64::from(handle.index), handle.generation)
    }

    /// Remove a pending entry and return its payload.
    pub fn cancel(&mut self, handle: TimerHandle) -> Result<T, HandleError> {
        if handle.wheel != self.id {
            return Err(HandleError::Foreign);
        }
        let index = u64::from(handle.index);
        match self.core.cancel(index, handle.generation) {
            Some(err) => Err(err),
            None => Ok(self.payloads.take(index)),
        }
    }

    /// Move the wheel's clock to `now_ns`, opening every tick that has work
    /// up to it. Does not yield entries; see [`Wheel::poll`].
    pub fn advance(&mut self, now_ns: u64) -> Result<(), AdvanceError> {
        match self.core.advance(now_ns) {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    /// Advance to `now_ns` and yield the earliest entry with
    /// `deadline_ns <= now_ns`, if any. Call repeatedly to drain all due
    /// entries in order.
    pub fn poll(&mut self, now_ns: u64) -> Result<Option<Expired<T>>, AdvanceError> {
        Ok(self.core.poll(now_ns)?.map(|(deadline_ns, index)| Expired {
            deadline_ns,
            payload: self.payloads.take(index),
        }))
    }

    /// Earliest time at which polling may yield an entry or must do
    /// bookkeeping. Never later than the true earliest pending deadline;
    /// `None` when the wheel is empty.
    pub fn next_wake_hint_ns(&self) -> Option<u64> {
        self.core.next_wake_hint_ns()
    }

    /// Remove every pending entry, handing each `(deadline_ns, payload)` to
    /// `sink` in slab order (deterministic but unrelated to deadlines). All
    /// outstanding handles become stale. Vesper returns the entries as one
    /// sequence, so this allocates once.
    pub fn drain(&mut self, mut sink: impl FnMut(u64, T)) {
        // Take every payload before calling the sink, so a panicking sink
        // cannot leave payloads in slots Vesper has already freed.
        let drained: Vec<_> = self
            .core
            .drain()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&[deadline, index]| (deadline, self.payloads.take(index)))
            .collect();
        for (deadline, value) in drained {
            sink(deadline, value);
        }
    }
}

impl<T> Drop for Wheel<T> {
    fn drop(&mut self) {
        report_dropped(self.core.len());
    }
}
