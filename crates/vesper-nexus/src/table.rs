//! §30.5 O(1) generational WIT resource table.
//!
//! Each slot carries a generation, an owner tenant tag and the Component
//! Model `num_lends` counter (`active_borrows` here). A guest-visible handle
//! is the pair `(idx, generation)`; it resolves only to a live slot with the
//! same generation and the same owner tenant, so a forged, stale or
//! cross-tenant integer is a typed error rather than access.
//!
//! Host code holds `own<T>` as [`WitOwn`] and a borrow as [`WitBorrow`]:
//! non-`Clone`, non-`Copy`, `#[must_use]` owners whose only exits are the
//! documented consuming calls. Dropping one without consuming it is counted
//! as a leak ([`WitResourceTable::leaked_obligations`]); the slot itself is
//! never silently released. This is host-library discipline only; it is not
//! an implementation of the Vesper source typing rules for `Mode::Linear`.
//!
//! Corrections to the §30.5 sketch, recorded in `docs/parallel-results/nexus.md`:
//! * a generation that would wrap retires its slot instead of reusing
//!   generation 1 (the sketch's `wrapping_add(1).max(1)` permits ABA reuse);
//! * `end_borrow` checks the borrowing tenant, so tenant B cannot end tenant
//!   A's lend and unblock a drop;
//! * cross-tenant failures do not report the owner's tenant id;
//! * fuel errors live in [`crate::fuel::FuelError`], not the table error;
//! * the table has a fixed capacity, so inserts never allocate after
//!   construction.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// A tenant tag. Tenants are host-assigned; guests never choose them.
pub type TenantId = u64;

/// A guest-visible resource handle: slot index plus generation.
///
/// This is a plain integer pair because guests hold it as one. Possessing a
/// `WitHandle` grants nothing: every use is checked against the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WitHandle {
    pub idx: u32,
    pub generation: u32,
}

impl WitHandle {
    /// The 64-bit guest integer form: generation in the high half.
    pub const fn to_bits(self) -> u64 {
        ((self.generation as u64) << 32) | self.idx as u64
    }
    /// Any 64-bit integer decodes to some handle; validity is the table's job.
    pub const fn from_bits(bits: u64) -> Self {
        Self {
            idx: bits as u32,
            generation: (bits >> 32) as u32,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WitTableError {
    /// No live slot with this index and generation (forged, stale, dropped).
    InvalidOrStaleHandle(WitHandle),
    /// The slot is live but owned by another tenant. The owner is not named.
    UnauthorizedTenant { actual: TenantId },
    /// `drop`/`transfer` of an `own` while lends are outstanding.
    ResourceHasActiveBorrows { idx: u32, active_borrows: u32 },
    /// `end_borrow` on a slot with no outstanding lend by this tenant.
    NoActiveBorrow(WitHandle),
    /// `num_lends` would overflow `u32`.
    BorrowOverflow(WitHandle),
    /// Every slot is live or retired.
    TableFull { capacity: u32 },
    /// The owner passed to a consuming call belongs to a different table.
    ForeignOwner,
    /// The slot's generation counter is exhausted; it cannot be re-tagged.
    GenerationExhausted(WitHandle),
    /// `forget_reclaimed` on a handle that is still live.
    StillLive(WitHandle),
}

impl fmt::Display for WitTableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOrStaleHandle(h) => {
                write!(f, "invalid or stale WIT handle {}:{}", h.idx, h.generation)
            }
            Self::UnauthorizedTenant { actual } => {
                write!(f, "tenant {actual} does not own this WIT resource")
            }
            Self::ResourceHasActiveBorrows {
                idx,
                active_borrows,
            } => write!(
                f,
                "WIT resource {idx} has {active_borrows} active borrows and cannot be dropped"
            ),
            Self::NoActiveBorrow(h) => {
                write!(
                    f,
                    "no active borrow of WIT handle {}:{}",
                    h.idx, h.generation
                )
            }
            Self::BorrowOverflow(h) => {
                write!(f, "borrow count overflow on {}:{}", h.idx, h.generation)
            }
            Self::TableFull { capacity } => write!(f, "WIT table full ({capacity} slots)"),
            Self::ForeignOwner => write!(f, "owner belongs to a different WIT table"),
            Self::GenerationExhausted(h) => write!(f, "generation exhausted on slot {}", h.idx),
            Self::StillLive(h) => write!(f, "WIT handle {}:{} is still live", h.idx, h.generation),
        }
    }
}
impl std::error::Error for WitTableError {}

/// Shared leak counter for linear owners of one table.
#[derive(Clone, Debug, Default)]
pub struct LeakAuditor {
    leaked: Arc<AtomicU64>,
}
impl LeakAuditor {
    pub fn leaked(&self) -> u64 {
        self.leaked.load(Ordering::Relaxed)
    }
    fn same(&self, other: &LeakAuditor) -> bool {
        Arc::ptr_eq(&self.leaked, &other.leaked)
    }
}

/// Armed while the linear owner is alive; counts a leak if dropped armed.
#[derive(Debug)]
pub(crate) struct Obligation {
    auditor: LeakAuditor,
    armed: bool,
}
impl Obligation {
    pub(crate) fn new(auditor: &LeakAuditor) -> Self {
        Self {
            auditor: auditor.clone(),
            armed: true,
        }
    }
    pub(crate) fn same_auditor(&self, auditor: &LeakAuditor) -> bool {
        self.auditor.same(auditor)
    }
    pub(crate) fn discharge(&mut self) {
        self.armed = false;
    }
}
impl Drop for Obligation {
    fn drop(&mut self) {
        if self.armed {
            self.auditor.leaked.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Host-held WIT `own<T>`: exits only through [`WitResourceTable::drop_own`],
/// [`WitResourceTable::transfer`] or [`WitResourceTable::lower_to_guest`].
#[must_use = "a WIT own<T> must be dropped, transferred or lowered exactly once"]
#[derive(Debug)]
pub struct WitOwn {
    handle: WitHandle,
    tenant: TenantId,
    obligation: Obligation,
}
impl WitOwn {
    pub fn handle(&self) -> WitHandle {
        self.handle
    }
    pub fn tenant(&self) -> TenantId {
        self.tenant
    }
}

/// Host-held WIT `borrow<T>` lend: exits only through
/// [`WitResourceTable::end_borrow`].
#[must_use = "a WIT borrow<T> must be returned with end_borrow before its own<T> can drop"]
#[derive(Debug)]
pub struct WitBorrow {
    handle: WitHandle,
    tenant: TenantId,
    obligation: Obligation,
}
impl WitBorrow {
    pub fn handle(&self) -> WitHandle {
        self.handle
    }
    pub fn tenant(&self) -> TenantId {
        self.tenant
    }
}

#[derive(Clone, Debug)]
struct ResourceSlot<T> {
    generation: u32,
    owner_tenant: TenantId,
    active_borrows: u32,
    payload: Option<T>,
}

/// Snapshot of one slot for audits and the Lean differential.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotState {
    pub generation: u32,
    pub owner_tenant: TenantId,
    pub active_borrows: u32,
    pub live: bool,
}

/// Fixed-capacity generational WIT resource table.
pub struct WitResourceTable<T> {
    slots: Vec<ResourceSlot<T>>,
    free_indices: Vec<u32>,
    capacity: u32,
    max_generation: u32,
    retired: u32,
    auditor: LeakAuditor,
}

impl<T> WitResourceTable<T> {
    /// Allocates all bookkeeping up front; no later operation allocates.
    pub fn with_capacity(capacity: u32) -> Self {
        Self::with_generation_limit(capacity, u32::MAX)
    }

    /// Like [`Self::with_capacity`] with a lower generation ceiling, so
    /// slot retirement can be exercised (and modelled) without 2^32 cycles.
    /// `max_generation` is clamped to at least 1.
    pub fn with_generation_limit(capacity: u32, max_generation: u32) -> Self {
        Self {
            slots: Vec::with_capacity(capacity as usize),
            free_indices: Vec::with_capacity(capacity as usize),
            capacity,
            max_generation: max_generation.max(1),
            retired: 0,
            auditor: LeakAuditor::default(),
        }
    }

    pub fn capacity(&self) -> u32 {
        self.capacity
    }
    /// Slots permanently retired because their generation was exhausted.
    pub fn retired_slots(&self) -> u32 {
        self.retired
    }
    /// Linear owners dropped without a consuming call.
    pub fn leaked_obligations(&self) -> u64 {
        self.auditor.leaked()
    }
    pub fn auditor(&self) -> &LeakAuditor {
        &self.auditor
    }
    pub fn live_count(&self) -> usize {
        self.slots.iter().filter(|s| s.payload.is_some()).count()
    }
    pub fn live_count_for(&self, tenant: TenantId) -> usize {
        self.slots
            .iter()
            .filter(|s| s.payload.is_some() && s.owner_tenant == tenant)
            .count()
    }
    pub fn slot_state(&self, idx: u32) -> Option<SlotState> {
        self.slots.get(idx as usize).map(|s| SlotState {
            generation: s.generation,
            owner_tenant: s.owner_tenant,
            active_borrows: s.active_borrows,
            live: s.payload.is_some(),
        })
    }

    /// Mint a new `own<T>` for `tenant`. O(1). On failure the value is
    /// handed back (it may itself be a linear owner).
    pub fn insert_own(&mut self, tenant: TenantId, val: T) -> Result<WitOwn, (T, WitTableError)> {
        let handle = self.insert_raw(tenant, val)?;
        Ok(WitOwn {
            handle,
            tenant,
            obligation: Obligation::new(&self.auditor),
        })
    }

    fn insert_raw(&mut self, tenant: TenantId, val: T) -> Result<WitHandle, (T, WitTableError)> {
        if let Some(idx) = self.free_indices.pop() {
            let slot = &mut self.slots[idx as usize];
            slot.owner_tenant = tenant;
            slot.active_borrows = 0;
            slot.payload = Some(val);
            return Ok(WitHandle {
                idx,
                generation: slot.generation,
            });
        }
        let full = WitTableError::TableFull {
            capacity: self.capacity,
        };
        let idx = match u32::try_from(self.slots.len()) {
            Ok(idx) if idx < self.capacity => idx,
            _ => return Err((val, full)),
        };
        self.slots.push(ResourceSlot {
            generation: 1,
            owner_tenant: tenant,
            active_borrows: 0,
            payload: Some(val),
        });
        Ok(WitHandle { idx, generation: 1 })
    }

    fn live_slot(&self, h: WitHandle) -> Result<&ResourceSlot<T>, WitTableError> {
        self.slots
            .get(h.idx as usize)
            .filter(|s| s.generation == h.generation && s.payload.is_some())
            .ok_or(WitTableError::InvalidOrStaleHandle(h))
    }
    fn live_slot_mut(&mut self, h: WitHandle) -> Result<&mut ResourceSlot<T>, WitTableError> {
        self.slots
            .get_mut(h.idx as usize)
            .filter(|s| s.generation == h.generation && s.payload.is_some())
            .ok_or(WitTableError::InvalidOrStaleHandle(h))
    }
    fn owned_slot_mut(
        &mut self,
        tenant: TenantId,
        h: WitHandle,
    ) -> Result<&mut ResourceSlot<T>, WitTableError> {
        let slot = self.live_slot_mut(h)?;
        if slot.owner_tenant != tenant {
            return Err(WitTableError::UnauthorizedTenant { actual: tenant });
        }
        Ok(slot)
    }

    /// Resolve a guest integer for `tenant` without lending. This is the
    /// non-forgery check: success implies a live slot, matching generation
    /// and matching owner.
    pub fn lookup(&self, tenant: TenantId, h: WitHandle) -> Result<&T, WitTableError> {
        let slot = self.live_slot(h)?;
        if slot.owner_tenant != tenant {
            return Err(WitTableError::UnauthorizedTenant { actual: tenant });
        }
        Ok(slot.payload.as_ref().expect("live slot has a payload"))
    }

    /// Begin a lend (`num_lends += 1`).
    pub fn begin_borrow(
        &mut self,
        tenant: TenantId,
        h: WitHandle,
    ) -> Result<WitBorrow, WitTableError> {
        self.begin_borrow_raw(tenant, h)?;
        Ok(WitBorrow {
            handle: h,
            tenant,
            obligation: Obligation::new(&self.auditor),
        })
    }

    /// Raw lend for guest-boundary bookkeeping that tracks lends itself.
    pub fn begin_borrow_raw(
        &mut self,
        tenant: TenantId,
        h: WitHandle,
    ) -> Result<(), WitTableError> {
        let slot = self.owned_slot_mut(tenant, h)?;
        slot.active_borrows = slot
            .active_borrows
            .checked_add(1)
            .ok_or(WitTableError::BorrowOverflow(h))?;
        Ok(())
    }

    /// Read through a live lend.
    pub fn get(&self, borrow: &WitBorrow) -> Result<&T, WitTableError> {
        self.lookup(borrow.tenant, borrow.handle)
    }

    /// Return a lend. On failure the lend is handed back.
    pub fn end_borrow(&mut self, mut borrow: WitBorrow) -> Result<(), (WitBorrow, WitTableError)> {
        if !self.auditor.same(&borrow.obligation.auditor) {
            return Err((borrow, WitTableError::ForeignOwner));
        }
        match self.end_borrow_raw(borrow.tenant, borrow.handle) {
            Ok(()) => {
                borrow.obligation.discharge();
                Ok(())
            }
            Err(e) => Err((borrow, e)),
        }
    }

    pub fn end_borrow_raw(&mut self, tenant: TenantId, h: WitHandle) -> Result<(), WitTableError> {
        let slot = self.owned_slot_mut(tenant, h)?;
        if slot.active_borrows == 0 {
            return Err(WitTableError::NoActiveBorrow(h));
        }
        slot.active_borrows -= 1;
        Ok(())
    }

    /// Drop an `own<T>`: requires `num_lends == 0`. On failure the owner is
    /// handed back so it can be retried after the lends end.
    pub fn drop_own(&mut self, mut own: WitOwn) -> Result<T, (WitOwn, WitTableError)> {
        if !self.auditor.same(&own.obligation.auditor) {
            return Err((own, WitTableError::ForeignOwner));
        }
        match self.drop_own_raw(own.tenant, own.handle) {
            Ok(v) => {
                own.obligation.discharge();
                Ok(v)
            }
            Err(e) => Err((own, e)),
        }
    }

    /// Guest-initiated `resource.drop` on an integer handle.
    pub fn drop_own_raw(&mut self, tenant: TenantId, h: WitHandle) -> Result<T, WitTableError> {
        let slot = self.owned_slot_mut(tenant, h)?;
        if slot.active_borrows > 0 {
            return Err(WitTableError::ResourceHasActiveBorrows {
                idx: h.idx,
                active_borrows: slot.active_borrows,
            });
        }
        let val = slot.payload.take().expect("live slot has a payload");
        self.release_slot(h.idx);
        Ok(val)
    }

    fn release_slot(&mut self, idx: u32) {
        let slot = &mut self.slots[idx as usize];
        if slot.generation < self.max_generation {
            slot.generation += 1;
            // Capacity was reserved for every slot, so this never allocates.
            self.free_indices.push(idx);
        } else {
            self.retired += 1;
        }
    }

    /// Move an `own<T>` to another tenant. The old handle becomes stale; the
    /// returned owner carries the new handle. Requires `num_lends == 0`.
    pub fn transfer(
        &mut self,
        mut own: WitOwn,
        to: TenantId,
    ) -> Result<WitOwn, (WitOwn, WitTableError)> {
        if !self.auditor.same(&own.obligation.auditor) {
            return Err((own, WitTableError::ForeignOwner));
        }
        match self.transfer_raw(own.tenant, own.handle, to) {
            Ok(handle) => {
                own.obligation.discharge();
                Ok(WitOwn {
                    handle,
                    tenant: to,
                    obligation: Obligation::new(&self.auditor),
                })
            }
            Err(e) => Err((own, e)),
        }
    }

    pub fn transfer_raw(
        &mut self,
        from: TenantId,
        h: WitHandle,
        to: TenantId,
    ) -> Result<WitHandle, WitTableError> {
        let max = self.max_generation;
        let slot = self.owned_slot_mut(from, h)?;
        if slot.active_borrows > 0 {
            return Err(WitTableError::ResourceHasActiveBorrows {
                idx: h.idx,
                active_borrows: slot.active_borrows,
            });
        }
        if slot.generation >= max {
            return Err(WitTableError::GenerationExhausted(h));
        }
        let next = slot.generation + 1;
        slot.generation = next;
        slot.owner_tenant = to;
        Ok(WitHandle {
            idx: h.idx,
            generation: next,
        })
    }

    /// Hand an `own<T>` to a guest: the obligation moves into the guest's
    /// handle table and is reclaimed by [`Self::reclaim_tenant`] at instance
    /// teardown if the guest never drops it.
    pub fn lower_to_guest(&self, mut own: WitOwn) -> WitHandle {
        own.obligation.discharge();
        own.handle
    }

    /// Re-wrap a guest-held handle as a host `own<T>` after checking it.
    pub fn lift_from_guest(&self, tenant: TenantId, h: WitHandle) -> Result<WitOwn, WitTableError> {
        self.lookup(tenant, h)?;
        Ok(WitOwn {
            handle: h,
            tenant,
            obligation: Obligation::new(&self.auditor),
        })
    }

    /// Instance teardown: release every resource `tenant` still owns,
    /// regardless of outstanding lends (the lenders died with the instance).
    /// Returns the payloads in slot order. Host-held [`WitOwn`]/[`WitBorrow`]
    /// values for these slots become stale.
    pub fn reclaim_tenant(&mut self, tenant: TenantId, mut sink: impl FnMut(T)) -> usize {
        let mut n = 0;
        for idx in 0..self.slots.len() {
            let slot = &mut self.slots[idx];
            if slot.owner_tenant == tenant
                && let Some(v) = slot.payload.take()
            {
                slot.active_borrows = 0;
                sink(v);
                self.release_slot(idx as u32);
                n += 1;
            }
        }
        n
    }

    /// Discharge a host owner whose slot was reclaimed by teardown.
    pub fn forget_reclaimed(&self, mut own: WitOwn) -> Result<(), (WitOwn, WitTableError)> {
        match self.live_slot(own.handle) {
            Ok(_) => {
                let h = own.handle;
                Err((own, WitTableError::StillLive(h)))
            }
            Err(_) => {
                own.obligation.discharge();
                Ok(())
            }
        }
    }

    /// Discharge a host lend whose slot was reclaimed by teardown.
    pub fn forget_reclaimed_borrow(
        &self,
        mut borrow: WitBorrow,
    ) -> Result<(), (WitBorrow, WitTableError)> {
        match self.live_slot(borrow.handle) {
            Ok(_) => {
                let h = borrow.handle;
                Err((borrow, WitTableError::StillLive(h)))
            }
            Err(_) => {
                borrow.obligation.discharge();
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhausted_generation_retires_the_slot_instead_of_wrapping() {
        let mut t = WitResourceTable::with_generation_limit(2, 2);
        let h1 = t.insert_own(1, 'a').unwrap();
        let h1 = t.lower_to_guest(h1);
        t.drop_own_raw(1, h1).unwrap();
        let o2 = t.insert_own(1, 'b').unwrap();
        let h2 = t.lower_to_guest(o2);
        assert_eq!((h2.idx, h2.generation), (h1.idx, 2));
        assert_eq!(t.drop_own_raw(1, h2), Ok('b'));
        assert_eq!(t.retired_slots(), 1);
        // The retired slot is never reissued, so generation 1 cannot return.
        let next = t.insert_own(1, 'c').unwrap();
        assert_ne!(next.handle().idx, h1.idx);
        for stale in [h1, h2] {
            assert_eq!(
                t.lookup(1, stale),
                Err(WitTableError::InvalidOrStaleHandle(stale))
            );
        }
        t.drop_own(next).unwrap();
        assert_eq!(t.leaked_obligations(), 0);
    }

    #[test]
    fn transfer_with_exhausted_generation_is_refused() {
        let mut t = WitResourceTable::with_generation_limit(1, 1);
        let o = t.insert_own(1, 0u8).unwrap();
        let h = t.lower_to_guest(o);
        assert_eq!(
            t.transfer_raw(1, h, 2),
            Err(WitTableError::GenerationExhausted(h))
        );
        t.drop_own_raw(1, h).unwrap();
        assert_eq!(t.retired_slots(), 1);
        assert_eq!(
            t.insert_own(1, 1).unwrap_err().1,
            WitTableError::TableFull { capacity: 1 }
        );
    }

    #[test]
    fn default_ceiling_is_u32_max() {
        let t = WitResourceTable::<()>::with_capacity(1);
        assert_eq!(t.max_generation, u32::MAX);
    }
}
