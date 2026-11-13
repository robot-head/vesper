//! Leak accounting for the linear owners of this crate.
//!
//! Every linear owner (`IrqGuard`, `DmaTransfer`, `PhysFrame4K`,
//! `PendingTlbShootdown`) registers with a [`Ledger`] when it is created and
//! deregisters through its documented consuming call. Dropping an owner
//! without consuming it is never a silent release: the drop is counted as a
//! leak of that kind, and auditors report it. Rust ownership here is
//! host-library discipline; it is not the Vesper source typing rule for
//! `linear` values.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

/// Kind of linear owner tracked by the ledger.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OwnerKind {
    IrqGuard,
    DmaTransfer,
    PhysFrame4K,
    PendingTlbShootdown,
}

#[derive(Debug, Default)]
struct Counts {
    live: BTreeMap<OwnerKind, u64>,
    leaked: BTreeMap<OwnerKind, u64>,
}

/// Shared leak ledger. Cloning shares the same counters.
#[derive(Clone, Debug, Default)]
pub struct Ledger(Rc<RefCell<Counts>>);

impl Ledger {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn open(&self, kind: OwnerKind) -> Token {
        *self.0.borrow_mut().live.entry(kind).or_default() += 1;
        Token {
            ledger: self.clone(),
            kind,
            armed: true,
        }
    }

    /// Whether two handles share the same counters.
    pub fn same(&self, other: &Ledger) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }

    /// Owners of `kind` that exist right now.
    pub fn live(&self, kind: OwnerKind) -> u64 {
        self.0.borrow().live.get(&kind).copied().unwrap_or(0)
    }

    /// Owners of `kind` dropped without their consuming call.
    pub fn leaked(&self, kind: OwnerKind) -> u64 {
        self.0.borrow().leaked.get(&kind).copied().unwrap_or(0)
    }

    pub fn total_leaked(&self) -> u64 {
        self.0.borrow().leaked.values().sum()
    }

    fn close(&self, kind: OwnerKind, leaked: bool) {
        let mut c = self.0.borrow_mut();
        if let Some(n) = c.live.get_mut(&kind) {
            *n = n.saturating_sub(1);
        }
        if leaked {
            *c.leaked.entry(kind).or_default() += 1;
        }
    }
}

/// Registration held inside a linear owner. Not `Clone`.
#[derive(Debug)]
pub(crate) struct Token {
    ledger: Ledger,
    kind: OwnerKind,
    armed: bool,
}

impl Token {
    /// The owner's consuming call: deregister without a leak.
    pub(crate) fn discharge(mut self) {
        self.armed = false;
        self.ledger.close(self.kind, false);
    }

    pub(crate) fn ledger(&self) -> &Ledger {
        &self.ledger
    }
}

impl Drop for Token {
    fn drop(&mut self) {
        if self.armed {
            self.ledger.close(self.kind, true);
        }
    }
}
