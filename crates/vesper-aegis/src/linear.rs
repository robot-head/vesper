//! Leak ledger for linear host-library owners.
//!
//! Owners (`UnacceptedGpaRange`, `HostBounceGrant`, `NonceTicket`) are
//! non-Clone, non-Copy and `#[must_use]`; their only exits are documented
//! consuming calls. A dropped owner is not silently released: its `Drop`
//! increments the issuing ledger, which the issuer reports through an
//! auditor method. This is host-library discipline in Rust, not an
//! implementation of the Vesper source typing rules for `linear struct`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ISSUER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub(crate) struct Ledger {
    pub(crate) issuer: u64,
    leaked: AtomicU64,
}

impl Ledger {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Ledger {
            issuer: NEXT_ISSUER.fetch_add(1, Ordering::Relaxed),
            leaked: AtomicU64::new(0),
        })
    }

    pub(crate) fn leaked(&self) -> u64 {
        self.leaked.load(Ordering::Relaxed)
    }
}

/// Drop guard embedded in each linear owner.
#[derive(Debug)]
pub(crate) struct Obligation {
    ledger: Arc<Ledger>,
    armed: bool,
}

impl Obligation {
    pub(crate) fn new(ledger: &Arc<Ledger>) -> Self {
        Obligation {
            ledger: Arc::clone(ledger),
            armed: true,
        }
    }

    /// A new armed obligation on the same ledger (for split/merge).
    pub(crate) fn sibling(&self) -> Self {
        Obligation::new(&self.ledger)
    }

    pub(crate) fn issuer(&self) -> u64 {
        self.ledger.issuer
    }

    /// Mark the obligation discharged by a consuming call.
    pub(crate) fn discharge(mut self) {
        self.armed = false;
    }
}

impl Drop for Obligation {
    fn drop(&mut self) {
        if self.armed {
            self.ledger.leaked.fetch_add(1, Ordering::Relaxed);
        }
    }
}
