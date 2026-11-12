//! Wait-free single-producer single-consumer handoff (design §32.1 item 3).
//!
//! The ring is [`rtrb`], a widely used wait-free SPSC ring buffer: after
//! construction, `push` and `pop` are a bounded number of atomic loads and
//! stores on preallocated memory, with no locks, no syscalls and no
//! allocation. Its index protocol (monotone read/write positions, readable
//! region `[read, write)`, at most `capacity` apart) is the one modelled in
//! `lean/ResonanceSpsc.lean`; that proof is about the model, not about
//! `rtrb`'s code.
//!
//! * [`retirement_channel`] gives the realtime thread an
//!   [`RtRetirementSink`]: it moves a retired value (an old graph, a patch
//!   bank `Box`) into the ring in O(1), so its destructor runs later on the
//!   non-realtime [`RtJanitor`], never on the realtime core.
//! * [`mailbox`] carries new state from a UI or planner thread to the
//!   realtime thread; [`MailboxReceiver::try_swap_in_latest`] swaps it in and
//!   retires the superseded value through a sink, only while the sink has
//!   room, so the realtime thread never drops state itself.
//!
//! The design writes `RtRetirementSink` as a `copy struct`. A copyable
//! producer would allow two producers on one SPSC ring; here the sink is a
//! unique (non-Clone) handle.

use rtrb::{Consumer, Producer, RingBuffer};
use std::fmt;

/// Channel construction errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpscError {
    /// Capacity 0.
    ZeroCapacity,
}

impl fmt::Display for SpscError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SPSC capacity must be at least 1")
    }
}

impl std::error::Error for SpscError {}

/// Realtime-side producer that retires values to a janitor thread.
pub struct RtRetirementSink<T> {
    producer: Producer<T>,
    retired: u64,
}

/// Non-realtime consumer that drops retired values.
pub struct RtJanitor<T> {
    consumer: Consumer<T>,
    dropped: u64,
}

/// Create a retirement channel with room for `capacity` pending values.
/// Allocates once, here, on the calling (non-realtime) thread.
pub fn retirement_channel<T>(
    capacity: usize,
) -> Result<(RtRetirementSink<T>, RtJanitor<T>), SpscError> {
    if capacity == 0 {
        return Err(SpscError::ZeroCapacity);
    }
    let (producer, consumer) = RingBuffer::new(capacity);
    Ok((
        RtRetirementSink {
            producer,
            retired: 0,
        },
        RtJanitor {
            consumer,
            dropped: 0,
        },
    ))
}

impl<T> RtRetirementSink<T> {
    /// Retire `old` in O(1) without running its destructor here. When the
    /// ring is full the value is handed back, so the caller keeps ownership
    /// and can retry next period.
    pub fn retire(&mut self, old: T) -> Result<(), T> {
        match self.producer.push(old) {
            Ok(()) => {
                self.retired += 1;
                Ok(())
            }
            Err(rtrb::PushError::Full(v)) => Err(v),
        }
    }

    /// Free slots now. A lower bound: the janitor can only add slots.
    pub fn free_slots(&self) -> usize {
        self.producer.slots()
    }

    /// Values retired so far.
    pub fn retired(&self) -> u64 {
        self.retired
    }

    /// `true` once the janitor is gone (retired values would never be dropped).
    pub fn janitor_gone(&self) -> bool {
        self.producer.is_abandoned()
    }
}

impl<T> RtJanitor<T> {
    /// Drop every value retired so far; returns how many.
    pub fn drain(&mut self) -> usize {
        let mut n = 0;
        while let Ok(v) = self.consumer.pop() {
            drop(v);
            n += 1;
        }
        self.dropped += n as u64;
        n
    }

    /// Pop one retired value (for callers that want to inspect it).
    pub fn pop(&mut self) -> Option<T> {
        let v = self.consumer.pop().ok();
        if v.is_some() {
            self.dropped += 1;
        }
        v
    }

    /// Values dropped (or popped) so far.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Values waiting to be dropped.
    pub fn pending(&self) -> usize {
        self.consumer.slots()
    }

    /// `true` once the realtime sink is gone.
    pub fn sink_gone(&self) -> bool {
        self.consumer.is_abandoned()
    }
}

/// Sending side of a state mailbox (UI / planner thread).
pub struct MailboxSender<T> {
    producer: Producer<T>,
}

/// Receiving side of a state mailbox (realtime thread).
pub struct MailboxReceiver<T> {
    consumer: Consumer<T>,
    stash: Option<T>,
}

/// Create a mailbox holding up to `capacity` pending states.
pub fn mailbox<T>(capacity: usize) -> Result<(MailboxSender<T>, MailboxReceiver<T>), SpscError> {
    if capacity == 0 {
        return Err(SpscError::ZeroCapacity);
    }
    let (producer, consumer) = RingBuffer::new(capacity);
    Ok((
        MailboxSender { producer },
        MailboxReceiver {
            consumer,
            stash: None,
        },
    ))
}

impl<T> MailboxSender<T> {
    /// Offer a new state; handed back when the mailbox is full.
    pub fn send(&mut self, state: T) -> Result<(), T> {
        self.producer
            .push(state)
            .map_err(|rtrb::PushError::Full(v)| v)
    }
}

/// Result of [`MailboxReceiver::try_swap_in_latest`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SwapOutcome {
    /// States swapped in (the last one is now current).
    pub swapped: usize,
    /// States left in the mailbox because the sink had no room.
    pub deferred: usize,
}

impl<T> MailboxReceiver<T> {
    /// Swap in pending states, newest last, retiring each superseded value
    /// to `sink`. Only takes a state while the sink has a free slot, so the
    /// realtime thread never has to drop one; states that do not fit stay
    /// queued for the next period.
    pub fn try_swap_in_latest(
        &mut self,
        current: &mut T,
        sink: &mut RtRetirementSink<T>,
    ) -> SwapOutcome {
        let mut out = SwapOutcome::default();
        if let Some(old) = self.stash.take()
            && let Err(old) = sink.retire(old)
        {
            self.stash = Some(old);
            out.deferred = self.consumer.slots();
            return out;
        }
        while sink.free_slots() > 0 {
            let Ok(new) = self.consumer.pop() else { break };
            let old = std::mem::replace(current, new);
            // Cannot fail: this thread is the only producer and saw a free
            // slot; the janitor can only free more. Should it ever fail, the
            // old value is stashed (never dropped here) and retried first
            // next period.
            if let Err(old) = sink.retire(old) {
                self.stash = Some(old);
                out.swapped += 1;
                break;
            }
            out.swapped += 1;
        }
        out.deferred = self.consumer.slots();
        out
    }

    /// States waiting.
    pub fn pending(&self) -> usize {
        self.consumer.slots()
    }
}
