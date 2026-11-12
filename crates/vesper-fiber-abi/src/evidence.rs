//! Handler-installation identities and owned evidence vectors.
use crate::abi::{ABI_VERSION, EvidenceId, EvidenceSlot, EvidenceVectorHeader, HandlerFn};
use std::fmt;

/// Allocates evidence identities. Identities are monotonic and never reused,
/// so a continuation parked under a removed installation cannot be confused
/// with a later installation of the same effect at the same depth.
#[derive(Debug)]
pub struct EvidenceRegistry {
    next: u64,
}

impl Default for EvidenceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl EvidenceRegistry {
    pub fn new() -> Self {
        Self { next: 1 }
    }
    pub fn fresh(&mut self) -> Result<EvidenceId, EvidenceError> {
        let id = self.next;
        self.next = id.checked_add(1).ok_or(EvidenceError::Exhausted)?;
        Ok(id)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvidenceError {
    Exhausted,
    TooDeep,
    DuplicateIdentity(EvidenceId),
}

impl fmt::Display for EvidenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for EvidenceError {}

/// Description of one handler installation, outermost first.
#[derive(Clone, Copy, Debug)]
pub struct Installation {
    pub effect: u32,
    pub handler_fn: Option<HandlerFn>,
    pub state_ptr: *mut u8,
}

impl Installation {
    /// A handler whose operations suspend the performing fiber.
    pub fn parking(effect: u32) -> Self {
        Self {
            effect,
            handler_fn: None,
            state_ptr: std::ptr::null_mut(),
        }
    }
}

/// An owned, immutable evidence vector. The header and slot storage are heap
/// allocated and never move while the vector lives, so the `header()` pointer
/// remains valid for every fiber started with it. A fiber that borrows a vector
/// must not outlive it; [`crate::fiber::Fiber`] enforces this by sharing
/// ownership through `Rc`.
pub struct EvidenceVector {
    slots: Box<[EvidenceSlot]>,
    header: Box<EvidenceVectorHeader>,
    /// Handler state referenced by `state_ptr`s; freed with the vector.
    owners: Vec<Box<dyn std::any::Any>>,
}

impl fmt::Debug for EvidenceVector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.slots.iter()).finish()
    }
}

impl EvidenceVector {
    /// Install `handlers` (outermost first). Depth `i` is slot `i`.
    pub fn install(
        registry: &mut EvidenceRegistry,
        handlers: &[Installation],
    ) -> Result<Self, EvidenceError> {
        let len = u32::try_from(handlers.len()).map_err(|_| EvidenceError::TooDeep)?;
        let mut slots = Vec::with_capacity(handlers.len());
        for (depth, h) in handlers.iter().enumerate() {
            slots.push(EvidenceSlot {
                identity: registry.fresh()?,
                effect: h.effect,
                depth: depth as u32,
                handler_fn: h.handler_fn,
                state_ptr: h.state_ptr,
            });
        }
        let slots = slots.into_boxed_slice();
        let header = Box::new(EvidenceVectorHeader {
            abi_version: ABI_VERSION,
            len,
            slots: slots.as_ptr(),
        });
        Ok(Self {
            slots,
            header,
            owners: vec![],
        })
    }
    /// Keep handler state alive for as long as this vector (and therefore
    /// every fiber sharing it) lives. Box contents never move.
    pub fn keep_alive(&mut self, owner: Box<dyn std::any::Any>) {
        self.owners.push(owner);
    }
    pub fn header(&self) -> *const EvidenceVectorHeader {
        &*self.header
    }
    pub fn slots(&self) -> &[EvidenceSlot] {
        &self.slots
    }
    pub fn identity(&self, depth: usize) -> Option<EvidenceId> {
        self.slots.get(depth).map(|s| s.identity)
    }
    /// Find the depth of an identity. Used by the scheduler to route a park
    /// request to the handler installation that owns it.
    pub fn depth_of(&self, identity: EvidenceId) -> Option<usize> {
        self.slots.iter().position(|s| s.identity == identity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_installations_of_one_effect_get_distinct_identities() {
        let mut registry = EvidenceRegistry::new();
        let v = EvidenceVector::install(
            &mut registry,
            &[Installation::parking(7), Installation::parking(7)],
        )
        .unwrap();
        assert_ne!(v.identity(0), v.identity(1));
        assert_eq!(v.depth_of(v.identity(1).unwrap()), Some(1));
        // A reinstallation at the same depth never reuses an identity.
        let again = EvidenceVector::install(&mut registry, &[Installation::parking(7)]).unwrap();
        assert!(
            !v.slots()
                .iter()
                .any(|s| s.identity == again.identity(0).unwrap())
        );
        assert_eq!(v.identity(2), None);
    }
    #[test]
    fn exhausted_registry_is_an_error() {
        let mut registry = EvidenceRegistry { next: u64::MAX };
        assert_eq!(registry.fresh(), Err(EvidenceError::Exhausted));
    }
}
