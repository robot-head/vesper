//! The five "Patterns for Rust APIs" of `docs/maintenance/porting-a-crate.md`,
//! written once against the kit API, so they run on both builds.
//!
//! (a) Owned handles: [`super::Counter`] and [`super::Tree`] are used as
//!     ordinary Rust values (methods, `Drop`); see `tests/kit.rs`.
//! (b) Token slabs: [`TokenSlab`] keeps payloads in Rust and only `u64`
//!     tokens in Vesper.
//! (c) Sink closures: [`for_each_word`] turns a returned sequence into
//!     callbacks.
//! (d) Traits: [`is_expired`] passes the trait's answer as a value, and
//!     [`broadcast`] applies a command plan to a trait object.
//! (e) Errors: [`super::mean`] returns the crate's own error enum through
//!     `Fail[E]`; [`mean_or_zero`] uses it like any `Result`.
use super::{Command, KitError, Tokens};

/// A FIFO of arbitrary Rust payloads whose order Vesper keeps (pattern b).
pub struct TokenSlab<T> {
    order: Tokens,
    slots: Vec<Option<T>>,
    free: Vec<usize>,
}

impl<T> Default for TokenSlab<T> {
    fn default() -> Self {
        Self {
            order: Tokens::new(),
            slots: Vec::new(),
            free: Vec::new(),
        }
    }
}

impl<T> TokenSlab<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue `value`.
    pub fn push(&mut self, value: T) {
        let slot = match self.free.pop() {
            Some(slot) => {
                self.slots[slot] = Some(value);
                slot
            }
            None => {
                self.slots.push(Some(value));
                self.slots.len() - 1
            }
        };
        self.order.push(slot as u64);
    }

    /// The oldest queued value.
    pub fn pop(&mut self) -> Option<T> {
        let slot = self.order.pop()? as usize;
        self.free.push(slot);
        self.slots[slot].take()
    }
}

/// Call `f` with every space-separated word of `text` (pattern c): Vesper
/// returns the boundaries as one sequence, the closure stays in Rust.
pub fn for_each_word(text: &[u8], mut f: impl FnMut(&[u8])) {
    for [start, end] in super::word_bounds(text).as_chunks::<2>().0 {
        f(&text[*start as usize..*end as usize]);
    }
}

/// A time source (pattern d).
pub trait Clock {
    fn now(&self) -> u64;
}

/// Ask the clock first, then pass its answer to Vesper as a value.
pub fn is_expired(clock: &impl Clock, deadline: u64) -> bool {
    super::expired(clock.now(), deadline)
}

/// Where [`broadcast`] sends (pattern d).
pub trait Transport {
    fn send(&mut self, to: u32, bytes: u64);
    fn wait(&mut self, ticks: u64);
}

/// Vesper plans the steps, the wrapper applies them to `transport`.
pub fn broadcast(transport: &mut impl Transport, peers: u32, bytes: u64, pause: u64) {
    for step in super::plan_sends(peers, bytes, pause) {
        match step {
            Command::Send { to, bytes } => transport.send(to, bytes),
            Command::Wait(ticks) => transport.wait(ticks),
        }
    }
}

/// The mean, or zero for no values (pattern e).
pub fn mean_or_zero(xs: &[u64]) -> Result<u64, KitError> {
    match super::mean(xs) {
        Err(KitError::Empty) => Ok(0),
        other => other,
    }
}
