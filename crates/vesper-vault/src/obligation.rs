//! Leak auditing for linear host-library obligations.
//!
//! `SecretSlot`, `NonceTicket`, `AeadSealer`, `AeadOpener`, `CiphertextFrame`
//! and `HybridClientSecret` are non-Clone, non-Copy `#[must_use]` owners whose
//! only exits are documented consuming calls. Rust cannot forbid dropping a
//! value, so a drop that bypasses those exits is counted here and reported by
//! [`LeakAuditor`]; secret memory is still wiped on that path, but the leak is
//! never silent. This is host-library discipline, not an implementation of
//! the Vesper source rule that rejects such programs at compile time.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

/// The kinds of obligation this crate audits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ObligationKind {
    /// A `SecretSlot` dropped without `shred`.
    SecretSlot,
    /// A `NonceTicket` dropped without `seal_in_place` or `burn`.
    NonceTicket,
    /// An `AeadSealer` or `AeadOpener` dropped without `retire`.
    AeadKey,
    /// A `CiphertextFrame` dropped without `open_in_place` or `discard`.
    CiphertextFrame,
    /// A `HybridClientSecret` dropped without `decapsulate` or `abandon`.
    KemSecret,
}

const KINDS: usize = 5;

impl ObligationKind {
    const ALL: [ObligationKind; KINDS] = [
        ObligationKind::SecretSlot,
        ObligationKind::NonceTicket,
        ObligationKind::AeadKey,
        ObligationKind::CiphertextFrame,
        ObligationKind::KemSecret,
    ];

    fn index(self) -> usize {
        match self {
            ObligationKind::SecretSlot => 0,
            ObligationKind::NonceTicket => 1,
            ObligationKind::AeadKey => 2,
            ObligationKind::CiphertextFrame => 3,
            ObligationKind::KemSecret => 4,
        }
    }
}

static GLOBAL: [AtomicU64; KINDS] = [const { AtomicU64::new(0) }; KINDS];

thread_local! {
    static LOCAL: [Cell<u64>; KINDS] = const { [const { Cell::new(0) }; KINDS] };
}

/// Record that an obligation of `kind` was dropped without its consuming exit.
pub(crate) fn record_leak(kind: ObligationKind) {
    GLOBAL[kind.index()].fetch_add(1, Ordering::Relaxed);
    // `try_with` so a drop during thread teardown still counts globally.
    let _ = LOCAL.try_with(|l| {
        let c = &l[kind.index()];
        c.set(c.get().saturating_add(1));
    });
}

/// Reads the leak counters.
#[derive(Clone, Copy, Debug, Default)]
pub struct LeakAuditor;

/// Counts of leaked obligations, per kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeakReport {
    counts: [u64; KINDS],
}

impl LeakReport {
    /// Leaks of one kind.
    pub fn count(&self, kind: ObligationKind) -> u64 {
        self.counts[kind.index()]
    }

    /// Total leaks across all kinds.
    pub fn total(&self) -> u64 {
        self.counts.iter().copied().fold(0u64, u64::saturating_add)
    }

    /// Kinds with at least one leak.
    pub fn leaked_kinds(&self) -> impl Iterator<Item = (ObligationKind, u64)> + '_ {
        ObligationKind::ALL
            .into_iter()
            .map(|k| (k, self.count(k)))
            .filter(|&(_, n)| n > 0)
    }
}

impl LeakAuditor {
    /// Process-wide leak counts since start-up.
    pub fn global() -> LeakReport {
        let mut counts = [0u64; KINDS];
        for (c, g) in counts.iter_mut().zip(GLOBAL.iter()) {
            *c = g.load(Ordering::Relaxed);
        }
        LeakReport { counts }
    }

    /// Leaks recorded by drops on the calling thread. Tests use this so that
    /// concurrently running tests do not disturb each other's counts.
    pub fn this_thread() -> LeakReport {
        let mut counts = [0u64; KINDS];
        LOCAL.with(|l| {
            for (c, cell) in counts.iter_mut().zip(l.iter()) {
                *c = cell.get();
            }
        });
        LeakReport { counts }
    }
}
