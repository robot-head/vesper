//! Runtime obligation and secret leak auditing around test execution frames.
//!
//! [`ObligationLedger`] snapshots [`vesper_vault::LeakAuditor::this_thread()`]
//! plus any registered custom counters (arena linear leaks, chorus sessions,
//! `io_uring` entries) before and after running a closure in [`ObligationLedger::audit_frame`].

use std::fmt;

use serde::{Deserialize, Serialize};
use vesper_vault::{LeakAuditor, ObligationKind as VaultObligationKind};

/// All obligation kinds tracked by [`ObligationLedger`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LedgerObligationKind {
    /// A `vesper_vault::SecretSlot` dropped without `shred`.
    VaultSecretSlot,
    /// A `vesper_vault::NonceTicket` dropped without `seal_in_place` or `burn`.
    VaultNonceTicket,
    /// A `vesper_vault::AeadSealer` / `AeadOpener` dropped without `retire`.
    VaultAeadKey,
    /// A `vesper_vault::CiphertextFrame` dropped without `open_in_place` or `discard`.
    VaultCiphertextFrame,
    /// A `vesper_vault::HybridClientSecret` dropped without `decapsulate` or `abandon`.
    VaultKemSecret,
    /// An unconsumed linear allocation inside a resettable test arena.
    ArenaLinear,
    /// An unclosed `vesper-chorus` session channel.
    ChorusSession,
    /// An undrained `vesper-pulse` `io_uring` submission/completion entry.
    IoUringEntry,
    /// Any additional suite- or backend-specific obligation counter.
    Custom(String),
}

impl From<VaultObligationKind> for LedgerObligationKind {
    fn from(kind: VaultObligationKind) -> Self {
        match kind {
            VaultObligationKind::SecretSlot => Self::VaultSecretSlot,
            VaultObligationKind::NonceTicket => Self::VaultNonceTicket,
            VaultObligationKind::AeadKey => Self::VaultAeadKey,
            VaultObligationKind::CiphertextFrame => Self::VaultCiphertextFrame,
            VaultObligationKind::KemSecret => Self::VaultKemSecret,
        }
    }
}

impl fmt::Display for LedgerObligationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::VaultSecretSlot => f.write_str("vault::SecretSlot"),
            Self::VaultNonceTicket => f.write_str("vault::NonceTicket"),
            Self::VaultAeadKey => f.write_str("vault::AeadKey"),
            Self::VaultCiphertextFrame => f.write_str("vault::CiphertextFrame"),
            Self::VaultKemSecret => f.write_str("vault::KemSecret"),
            Self::ArenaLinear => f.write_str("arena::Linear"),
            Self::ChorusSession => f.write_str("chorus::Session"),
            Self::IoUringEntry => f.write_str("uring::Entry"),
            Self::Custom(name) => write!(f, "custom::{name}"),
        }
    }
}

/// A non-zero leak delta observed for one obligation kind during a test frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObligationDelta {
    /// Which obligation kind leaked.
    pub kind: LedgerObligationKind,
    /// Number of newly leaked obligations during the audited frame.
    pub leaked: u64,
}

/// Summary of obligation leaks observed across an audited test frame.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerReport {
    /// Non-zero obligation deltas recorded between frame entry and exit.
    pub deltas: Vec<ObligationDelta>,
}

impl LedgerReport {
    /// Returns `true` if any obligation kind had a non-zero leak delta.
    pub fn has_leaks(&self) -> bool {
        !self.deltas.is_empty()
    }

    /// Total number of leaked obligations summed across all kinds.
    pub fn total_leaks(&self) -> u64 {
        self.deltas
            .iter()
            .fold(0u64, |acc, d| acc.saturating_add(d.leaked))
    }

    /// Number of leaked obligations for a specific `kind`.
    pub fn leaks_for(&self, kind: &LedgerObligationKind) -> u64 {
        self.deltas
            .iter()
            .filter(|d| &d.kind == kind)
            .fold(0u64, |acc, d| acc.saturating_add(d.leaked))
    }
}

impl fmt::Display for LedgerReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.deltas.is_empty() {
            return f.write_str("no obligation leaks");
        }
        f.write_str("leaked obligations: ")?;
        for (idx, delta) in self.deltas.iter().enumerate() {
            if idx > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{}={}", delta.kind, delta.leaked)?;
        }
        Ok(())
    }
}

/// Pluggable monotonic leak counter for subsystems outside `vesper-vault`.
pub trait LeakCounter: Send + Sync {
    /// Obligation kind reported by this counter.
    fn kind(&self) -> LedgerObligationKind;
    /// Current cumulative leak count on the calling thread or subsystem.
    fn current_count(&self) -> u64;
}

/// Closure-backed [`LeakCounter`] adapter.
pub struct ClosureCounter<F> {
    kind: LedgerObligationKind,
    read: F,
}

impl<F> ClosureCounter<F>
where
    F: Fn() -> u64 + Send + Sync,
{
    /// Create a new closure-backed leak counter for `kind`.
    pub fn new(kind: LedgerObligationKind, read: F) -> Self {
        Self { kind, read }
    }
}

impl<F> LeakCounter for ClosureCounter<F>
where
    F: Fn() -> u64 + Send + Sync,
{
    fn kind(&self) -> LedgerObligationKind {
        self.kind.clone()
    }

    fn current_count(&self) -> u64 {
        (self.read)()
    }
}

const VAULT_KINDS: [VaultObligationKind; 5] = [
    VaultObligationKind::SecretSlot,
    VaultObligationKind::NonceTicket,
    VaultObligationKind::AeadKey,
    VaultObligationKind::CiphertextFrame,
    VaultObligationKind::KemSecret,
];

/// Point-in-time snapshot of all audited leak counters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerSnapshot {
    vault_counts: [u64; 5],
    custom_counts: Vec<(LedgerObligationKind, u64)>,
}

/// Runtime obligation ledger wrapping `vesper_vault::LeakAuditor::this_thread()`
/// and pluggable subsystem counters.
#[derive(Default)]
pub struct ObligationLedger {
    counters: Vec<Box<dyn LeakCounter>>,
}

impl ObligationLedger {
    /// Create a new ledger auditing `vesper_vault::LeakAuditor::this_thread()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a custom [`LeakCounter`] (e.g. arena, chorus, or `io_uring`).
    pub fn register_counter(&mut self, counter: Box<dyn LeakCounter>) {
        self.counters.push(counter);
    }

    /// Builder-style helper to register any [`LeakCounter`].
    pub fn with_counter<C: LeakCounter + 'static>(mut self, counter: C) -> Self {
        self.counters.push(Box::new(counter));
        self
    }

    /// Register an arena linear leak counter (`ARENA_LINEAR_LEAK_KIND`).
    pub fn with_arena_counter<F>(self, read: F) -> Self
    where
        F: Fn() -> u64 + Send + Sync + 'static,
    {
        self.with_counter(ClosureCounter::new(LedgerObligationKind::ArenaLinear, read))
    }

    /// Register a `vesper-chorus` session leak counter.
    pub fn with_chorus_counter<F>(self, read: F) -> Self
    where
        F: Fn() -> u64 + Send + Sync + 'static,
    {
        self.with_counter(ClosureCounter::new(
            LedgerObligationKind::ChorusSession,
            read,
        ))
    }

    /// Register a `vesper-pulse` `io_uring` undrained-entry counter.
    pub fn with_uring_counter<F>(self, read: F) -> Self
    where
        F: Fn() -> u64 + Send + Sync + 'static,
    {
        self.with_counter(ClosureCounter::new(
            LedgerObligationKind::IoUringEntry,
            read,
        ))
    }

    /// Register a named custom obligation leak counter.
    pub fn with_custom_counter<F>(self, name: impl Into<String>, read: F) -> Self
    where
        F: Fn() -> u64 + Send + Sync + 'static,
    {
        self.with_counter(ClosureCounter::new(
            LedgerObligationKind::Custom(name.into()),
            read,
        ))
    }

    /// Capture a snapshot of all vault and registered custom counters.
    pub fn snapshot(&self) -> LedgerSnapshot {
        let report = LeakAuditor::this_thread();
        let mut vault_counts = [0u64; 5];
        for (slot, kind) in vault_counts.iter_mut().zip(VAULT_KINDS.iter().copied()) {
            *slot = report.count(kind);
        }
        let custom_counts = self
            .counters
            .iter()
            .map(|c| (c.kind(), c.current_count()))
            .collect();
        LedgerSnapshot {
            vault_counts,
            custom_counts,
        }
    }

    /// Compute the [`LedgerReport`] of non-zero leak deltas between `before` and `after`.
    pub fn diff(&self, before: &LedgerSnapshot, after: &LedgerSnapshot) -> LedgerReport {
        let mut deltas = Vec::new();
        for (idx, kind) in VAULT_KINDS.iter().copied().enumerate() {
            let leaked = after.vault_counts[idx].saturating_sub(before.vault_counts[idx]);
            if leaked > 0 {
                deltas.push(ObligationDelta {
                    kind: LedgerObligationKind::from(kind),
                    leaked,
                });
            }
        }
        for ((_, before_count), (kind, after_count)) in before
            .custom_counts
            .iter()
            .zip(after.custom_counts.iter())
        {
            let leaked = after_count.saturating_sub(*before_count);
            if leaked > 0 {
                deltas.push(ObligationDelta {
                    kind: kind.clone(),
                    leaked,
                });
            }
        }
        LedgerReport { deltas }
    }

    /// Execute `f` inside an audited frame, returning both its result and any
    /// obligation leak deltas recorded while `f` ran.
    pub fn audit_frame<T, F>(&self, f: F) -> (T, LedgerReport)
    where
        F: FnOnce() -> T,
    {
        let before = self.snapshot();
        let result = f();
        let after = self.snapshot();
        let report = self.diff(&before, &after);
        (result, report)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use vesper_vault::{LockPolicy, SecretSlot};

    use super::*;

    #[test]
    fn clean_frame_reports_no_leaks() {
        let ledger = ObligationLedger::new();
        let (val, report) = ledger.audit_frame(|| {
            let slot = SecretSlot::<32>::new(LockPolicy::BestEffort).expect("alloc slot");
            let _receipt = slot.shred();
            42
        });
        assert_eq!(val, 42);
        assert!(!report.has_leaks());
        assert_eq!(report.total_leaks(), 0);
    }

    #[test]
    fn dropped_secret_slot_is_flagged_by_ledger() {
        let ledger = ObligationLedger::new();
        let ((), report) = ledger.audit_frame(|| {
            let _leaked = SecretSlot::<32>::new(LockPolicy::BestEffort).expect("alloc slot");
            // Dropped without calling `.shred()`.
        });
        assert!(report.has_leaks());
        assert_eq!(
            report.leaks_for(&LedgerObligationKind::VaultSecretSlot),
            1
        );
        assert!(report.to_string().contains("vault::SecretSlot=1"));
    }

    #[test]
    fn pluggable_custom_counters_report_exact_deltas() {
        let arena_leaks = Arc::new(AtomicU64::new(5));
        let chorus_leaks = Arc::new(AtomicU64::new(0));
        let uring_leaks = Arc::new(AtomicU64::new(2));

        let a = Arc::clone(&arena_leaks);
        let c = Arc::clone(&chorus_leaks);
        let u = Arc::clone(&uring_leaks);

        let ledger = ObligationLedger::new()
            .with_arena_counter(move || a.load(Ordering::Relaxed))
            .with_chorus_counter(move || c.load(Ordering::Relaxed))
            .with_uring_counter(move || u.load(Ordering::Relaxed));

        let (res, report) = ledger.audit_frame(|| {
            arena_leaks.fetch_add(2, Ordering::Relaxed);
            uring_leaks.fetch_add(1, Ordering::Relaxed);
            "done"
        });

        assert_eq!(res, "done");
        assert!(report.has_leaks());
        assert_eq!(report.total_leaks(), 3);
        assert_eq!(report.leaks_for(&LedgerObligationKind::ArenaLinear), 2);
        assert_eq!(report.leaks_for(&LedgerObligationKind::ChorusSession), 0);
        assert_eq!(report.leaks_for(&LedgerObligationKind::IoUringEntry), 1);
    }
}
