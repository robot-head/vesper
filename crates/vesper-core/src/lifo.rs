//! Path-local LIFO obligation tracking for the rule in manuscript §3.3.
//!
//! The caller supplies stable binding identities and is responsible for mapping
//! errors to source diagnostics. This module does not integrate with the source
//! checker or determine which bindings carry the LIFO mode.

/// A compiler-assigned identity, distinct even for shadowed source names.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct BindingId(pub u32);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcquireError {
    AlreadyActive(BindingId),
    Limit { max: usize },
    AllocationFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReleaseError {
    /// The requested binding is not an active obligation on this path.
    NotActive(BindingId),
    /// A newer obligation must be released first.
    OutOfOrder {
        requested: BindingId,
        expected: BindingId,
    },
}

/// Ordered, path-local outstanding obligations. Clone at a control-flow fork.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LifoState {
    active: Vec<BindingId>,
    max_active: usize,
}

impl LifoState {
    pub fn new(max_active: usize) -> Self {
        Self {
            active: Vec::new(),
            max_active,
        }
    }

    /// The active identities in acquisition order, oldest first.
    pub fn active(&self) -> &[BindingId] {
        &self.active
    }

    /// Acquire an obligation without changing state on failure.
    pub fn acquire(&mut self, id: BindingId) -> Result<(), AcquireError> {
        if self.active.contains(&id) {
            return Err(AcquireError::AlreadyActive(id));
        }
        if self.active.len() >= self.max_active {
            return Err(AcquireError::Limit {
                max: self.max_active,
            });
        }
        self.active
            .try_reserve(1)
            .map_err(|_| AcquireError::AllocationFailed)?;
        self.active.push(id);
        Ok(())
    }

    /// Release only the newest outstanding obligation, preserving state on error.
    pub fn release(&mut self, id: BindingId) -> Result<(), ReleaseError> {
        match self.active.last().copied() {
            Some(top) if top == id => {
                self.active.pop();
                Ok(())
            }
            Some(top) if self.active.contains(&id) => Err(ReleaseError::OutOfOrder {
                requested: id,
                expected: top,
            }),
            _ => Err(ReleaseError::NotActive(id)),
        }
    }

    /// Live paths may join only with exactly the same outstanding stack.
    /// Limits must match so later acquisitions cannot depend on the branch.
    pub fn can_join(&self, other: &Self) -> bool {
        self == other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_release_and_failures_preserve_state() {
        let (outer, inner, unknown) = (BindingId(1), BindingId(2), BindingId(3));
        let mut state = LifoState::new(2);
        state.acquire(outer).unwrap();
        state.acquire(inner).unwrap();
        let snapshot = state.clone();
        assert_eq!(
            state.acquire(outer),
            Err(AcquireError::AlreadyActive(outer))
        );
        assert_eq!(state.acquire(unknown), Err(AcquireError::Limit { max: 2 }));
        assert_eq!(
            state.release(outer),
            Err(ReleaseError::OutOfOrder {
                requested: outer,
                expected: inner
            })
        );
        assert_eq!(
            state.release(unknown),
            Err(ReleaseError::NotActive(unknown))
        );
        assert_eq!(state, snapshot);
        state.release(inner).unwrap();
        state.release(outer).unwrap();
        assert_eq!(state.release(outer), Err(ReleaseError::NotActive(outer)));
        assert!(state.active().is_empty());
    }

    #[test]
    fn joins_require_identical_order_and_active_identities() {
        let (a, b) = (BindingId(1), BindingId(2));
        let mut left = LifoState::new(3);
        left.acquire(a).unwrap();
        let mut right = left.clone();
        left.acquire(b).unwrap();
        assert!(!left.can_join(&right));
        right.acquire(b).unwrap();
        assert!(left.can_join(&right));
        right.release(b).unwrap();
        right.release(a).unwrap();
        right.acquire(b).unwrap();
        right.acquire(a).unwrap();
        assert!(!left.can_join(&right));
        assert!(!LifoState::new(0).can_join(&LifoState::new(1)));
        assert_eq!(
            LifoState::new(0).acquire(a),
            Err(AcquireError::Limit { max: 0 })
        );
    }
}
