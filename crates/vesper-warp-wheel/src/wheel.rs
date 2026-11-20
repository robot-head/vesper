//! Core-local hierarchical hashed timing wheel (Varghese–Lauck scheme 7).
//!
//! # Model
//!
//! Time is `u64` nanoseconds supplied by the caller. A deadline `d` belongs to
//! tick `d / tick_ns`. The wheel keeps a *cursor*: every tick strictly below
//! the cursor has been opened, meaning its entries were moved, in exact
//! `(deadline_ns, insertion sequence)` order, onto a sorted *ready* list.
//!
//! Level `k` (0-based) has `2^slot_bits` slots of width `2^(k * slot_bits)`
//! ticks. An entry is placed at the level holding the highest bit in which
//! its tick differs from the cursor; entries that differ above
//! `levels * slot_bits` bits live on an overflow list that is re-placed when
//! the cursor reaches the block holding its earliest member. Per-level
//! occupancy bitmaps let [`Wheel::advance`] jump straight to the next tick
//! that has work, so empty stretches cost O(levels), not O(ticks).
//!
//! # Firing guarantees
//!
//! * Exactness: polling at `now` yields precisely the pending entries with
//!   `deadline_ns <= now`; nothing fires early and nothing that is due is
//!   withheld (no tick-granularity lateness).
//! * Order: entries are yielded in non-decreasing deadline order across the
//!   whole wheel (so also within a tick), and entries with equal deadlines are
//!   yielded in insertion (FIFO) order. Cascading never reorders them because
//!   the order key is `(deadline_ns, sequence)`.
//!
//! # Costs
//!
//! * `insert`: O(1) for a deadline in a tick the cursor has not opened. A
//!   deadline in an already-opened tick (late or same-tick insert) is merged
//!   into the sorted ready list by scanning from its tail, O(r) in the number
//!   of entries still waiting in opened ticks (usually 0 or a handful).
//! * `cancel`: O(1) by handle (intrusive doubly linked lists).
//! * `advance`/`poll`: each entry is cascaded at most `levels` times plus a
//!   bounded number of overflow re-placements per overflow block, and the
//!   entries of one tick are merge-sorted in place (O(k log k), no
//!   allocation) when that tick opens. Skipping empty ticks costs
//!   O(levels) per visited tick with work.
//! * Memory: all entry storage, list heads and bitmaps are allocated in
//!   [`Wheel::new`]; no later operation allocates (verified by the counting
//!   allocator test in `tests/alloc.rs`).
//!
//! The tick counter is an absolute `u64` and never wraps inside the supported
//! range ([`crate::MAX_TIME_NS`]); slot indices wrap modulo the ring size on
//! every rotation, which the model test exercises together with start times
//! near `2^64`.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// Largest supported timestamp (deadline or `now`), in nanoseconds.
///
/// `u64::MAX` itself is reserved so that the cursor (`last opened tick + 1`)
/// can always be represented.
pub const MAX_TIME_NS: u64 = u64::MAX - 1;

/// Largest supported entry capacity (`u32::MAX` is the list terminator).
pub const MAX_CAPACITY: u32 = u32::MAX - 1;
/// Largest `slot_bits` (64 slots per level).
pub const MAX_SLOT_BITS: u8 = 6;
/// Largest `levels * slot_bits`: the wheel spans less than 2^63 ticks.
pub const MAX_SPAN_BITS: u32 = 63;

static NEXT_WHEEL_ID: AtomicU64 = AtomicU64::new(1);
static LEAKED_ENTRIES: AtomicU64 = AtomicU64::new(0);

/// A fresh wheel identity (handles minted by one wheel are foreign to every
/// other).
pub(crate) fn next_wheel_id() -> u64 {
    NEXT_WHEEL_ID.fetch_add(1, Ordering::Relaxed)
}

/// Count `pending` entries dropped with their owner; debug builds also
/// panic (unless already unwinding).
pub(crate) fn report_dropped(pending: u64) {
    if pending != 0 {
        LEAKED_ENTRIES.fetch_add(pending, Ordering::Relaxed);
        if cfg!(debug_assertions) && !std::thread::panicking() {
            panic!("timing wheel dropped with {pending} pending entries; drain it first");
        }
    }
}

/// Total entries that were still pending when a wheel was dropped, across the
/// whole process. Debug builds also panic on such a drop (unless already
/// unwinding); release builds only count. Drain a wheel with
/// [`Wheel::drain`] before dropping it to hand its payloads back.
pub fn leaked_timer_entries() -> u64 {
    LEAKED_ENTRIES.load(Ordering::Relaxed)
}

/// Static shape of a wheel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WheelConfig {
    /// Tick width in nanoseconds (> 0).
    pub tick_ns: u64,
    /// log2 of the slot count per level, `1..=6` (2 to 64 slots).
    pub slot_bits: u8,
    /// Number of levels, `1..`, with `levels * slot_bits <= 63`.
    pub levels: u8,
    /// Fixed number of entries; storage is preallocated.
    pub capacity: u32,
    /// Initial value of the wheel's clock, in nanoseconds.
    pub start_ns: u64,
}

impl WheelConfig {
    /// Default shape for `capacity` entries: 1 µs ticks, 64 slots, 4 levels
    /// (about 16.7 s before the overflow list), starting at time 0.
    pub const fn with_capacity(capacity: u32) -> Self {
        Self {
            tick_ns: 1_000,
            slot_bits: 6,
            levels: 4,
            capacity,
            start_ns: 0,
        }
    }
}

/// Invalid [`WheelConfig`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WheelConfigError {
    /// `tick_ns` was zero.
    ZeroTick,
    /// `slot_bits` outside `1..=6`.
    SlotBits(u8),
    /// `levels` was zero or `levels * slot_bits > 63`.
    Levels(u8),
    /// `capacity` was zero or above [`MAX_CAPACITY`].
    Capacity(u32),
    /// `start_ns` above [`MAX_TIME_NS`].
    StartOutOfRange,
}

impl fmt::Display for WheelConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroTick => f.write_str("tick width must be non-zero"),
            Self::SlotBits(b) => write!(f, "slot_bits {b} is outside 1..=6"),
            Self::Levels(l) => write!(f, "{l} levels do not fit 1..=63 tick bits"),
            Self::Capacity(c) => write!(f, "capacity {c} is outside 1..={MAX_CAPACITY}"),
            Self::StartOutOfRange => f.write_str("start time exceeds MAX_TIME_NS"),
        }
    }
}

impl std::error::Error for WheelConfigError {}

/// Why an insert was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertErrorKind {
    /// Every preallocated entry is in use.
    Full,
    /// The deadline exceeds [`MAX_TIME_NS`].
    DeadlineOutOfRange,
}

/// A refused insert; the payload is handed back unchanged.
#[must_use = "the refused payload is returned and must be handled"]
pub struct InsertError<T> {
    /// The reason.
    pub kind: InsertErrorKind,
    /// The payload that was not stored.
    pub payload: T,
}

impl<T> InsertError<T> {
    /// Recover the payload.
    pub fn into_payload(self) -> T {
        self.payload
    }
}

impl<T> fmt::Debug for InsertError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InsertError")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl<T> fmt::Display for InsertError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            InsertErrorKind::Full => f.write_str("timing wheel is at capacity"),
            InsertErrorKind::DeadlineOutOfRange => f.write_str("deadline exceeds MAX_TIME_NS"),
        }
    }
}

impl<T> std::error::Error for InsertError<T> {}

/// A handle that does not name a pending entry of this wheel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandleError {
    /// The handle was minted by a different wheel.
    Foreign,
    /// The entry already fired, was cancelled or was drained.
    Stale,
}

impl fmt::Display for HandleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Foreign => f.write_str("timer handle belongs to another wheel"),
            Self::Stale => f.write_str("timer handle no longer names a pending entry"),
        }
    }
}

impl std::error::Error for HandleError {}

/// Rejected clock movement on a wheel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdvanceError {
    /// `now_ns` is earlier than the wheel's clock.
    Backwards {
        /// The wheel's current time.
        now_ns: u64,
        /// The requested, earlier time.
        requested_ns: u64,
    },
    /// `now_ns` exceeds [`MAX_TIME_NS`].
    OutOfRange,
}

impl fmt::Display for AdvanceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backwards {
                now_ns,
                requested_ns,
            } => write!(
                f,
                "wheel clock cannot move backwards from {now_ns} ns to {requested_ns} ns"
            ),
            Self::OutOfRange => f.write_str("time exceeds MAX_TIME_NS"),
        }
    }
}

impl std::error::Error for AdvanceError {}

/// Generation-checked reference to one pending entry.
///
/// Not `Clone`/`Copy`: there is at most one handle per insertion. Dropping a
/// handle does not cancel the entry (the payload is still yielded when it
/// fires); it only gives up the ability to cancel it.
#[must_use = "dropping the handle gives up the ability to cancel the timer"]
#[derive(Debug, PartialEq, Eq)]
pub struct TimerHandle {
    pub(crate) wheel: u64,
    pub(crate) index: u32,
    pub(crate) generation: u64,
}

/// A fired entry.
#[must_use = "a fired timer carries its payload"]
#[derive(Debug, PartialEq, Eq)]
pub struct Expired<T> {
    /// The entry's deadline (`<=` the `now` it was polled at).
    pub deadline_ns: u64,
    /// The owned payload.
    pub payload: T,
}

// The Rust build (the frozen reference) or the Vesper build (a thin wrapper
// over the Vesper port in vesper/src/: token slab, sink adaptor).
#[cfg(not(feature = "vesper-port"))]
#[path = "reference/wheel.rs"]
mod imp;
#[cfg(feature = "vesper-port")]
#[path = "port/wheel.rs"]
mod imp;

pub use imp::Wheel;

/// The Rust build, compiled beside the Vesper build only so that
/// `tests/abba.rs` can compare the two in one process. Not part of the API.
#[cfg(feature = "vesper-port")]
#[doc(hidden)]
#[path = "reference/wheel.rs"]
pub mod rust_reference;
