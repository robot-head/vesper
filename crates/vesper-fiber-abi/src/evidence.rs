//! Handler-installation identities and owned evidence vectors.
use crate::abi::{ABI_VERSION, EvidenceId, EvidenceSlot, EvidenceVectorHeader, HandlerFn};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// Process-wide identity counter shared by every registry, so recreating a
/// registry can never reissue an identity that a live vector or parked
/// continuation still carries.
static NEXT_EVIDENCE: AtomicU64 = AtomicU64::new(1);

fn allocate(counter: &AtomicU64) -> Result<EvidenceId, EvidenceError> {
    let mut id = counter.load(Ordering::Relaxed);
    loop {
        let next = id.checked_add(1).ok_or(EvidenceError::Exhausted)?;
        match counter.compare_exchange_weak(id, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return Ok(id),
            Err(current) => id = current,
        }
    }
}

/// Allocates evidence identities. Identities come from one process-wide
/// monotonic counter and are never reused, across all registries, so a
/// continuation parked under a removed installation cannot be confused with a
/// later installation of the same effect at the same depth.
#[derive(Debug)]
pub struct EvidenceRegistry {
    _private: (),
}

impl Default for EvidenceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl EvidenceRegistry {
    pub fn new() -> Self {
        Self { _private: () }
    }
    pub fn fresh(&mut self) -> Result<EvidenceId, EvidenceError> {
        allocate(&NEXT_EVIDENCE)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvidenceError {
    Exhausted,
    TooDeep,
}

impl fmt::Display for EvidenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for EvidenceError {}

/// Description of one handler installation, outermost first. Fields are
/// private: a raw handler/state pair can only be built through the `unsafe`
/// [`Installation::direct`] constructor, so safe code cannot install a handler
/// with invalid or dangling state.
///
/// ```compile_fail
/// let _ = vesper_fiber_abi::evidence::Installation {
///     effect: 1,
///     handler_fn: Some(vesper_fiber_abi::prototype::clock_handler),
///     state_ptr: std::ptr::null_mut(), // error[E0451]: private fields
/// };
/// ```
#[derive(Clone, Copy, Debug)]
pub struct Installation {
    effect: u32,
    handler_fn: Option<HandlerFn>,
    state_ptr: *mut u8,
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
    /// A tail-resumptive handler that generated code calls directly as
    /// `handler(state, op, arg)` on the performing fiber's stack.
    ///
    /// # Safety
    /// For as long as the resulting [`EvidenceVector`] (and so every fiber
    /// sharing it) lives, `state` must be valid for every access `handler`
    /// makes, and no other code may access it while a fiber runs `handler`.
    /// Ownership is usually given to the vector with
    /// [`EvidenceVector::keep_alive`]. `handler` must accept every `op` and
    /// `arg` the generated code passes, and must not unwind.
    pub unsafe fn direct(effect: u32, handler: HandlerFn, state: *mut u8) -> Self {
        Self {
            effect,
            handler_fn: Some(handler),
            state_ptr: state,
        }
    }
    pub fn effect(&self) -> u32 {
        self.effect
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
    fn identities_are_unique_across_registries() {
        let old = {
            let mut first = EvidenceRegistry::new();
            EvidenceVector::install(&mut first, &[Installation::parking(7)]).unwrap()
        };
        let mut second = EvidenceRegistry::new();
        let new = EvidenceVector::install(&mut second, &[Installation::parking(7)]).unwrap();
        assert_ne!(old.identity(0), new.identity(0));
    }
    #[test]
    fn exhausted_counter_is_an_error() {
        let counter = AtomicU64::new(u64::MAX);
        assert_eq!(allocate(&counter), Err(EvidenceError::Exhausted));
        let counter = AtomicU64::new(5);
        assert_eq!(allocate(&counter), Ok(5));
    }
}
