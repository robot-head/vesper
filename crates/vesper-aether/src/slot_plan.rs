//! KVM EPT / Stage-2 memory-slot plans (§37.5).
//!
//! A [`KvmSlotPlan`] is a list of `kvm_userspace_memory_region` records for one
//! VM. It can only reach an ioctl as a [`VerifiedKvmSlotPlan`], which exists
//! only after [`KvmSlotPlan::verify`] has checked, against the probed host
//! limits, that every region is non-empty, 4 KiB aligned, overflow-free,
//! inside the guest physical address width, uses a known flag set and a
//! distinct in-range slot id, and that no two regions overlap in guest
//! physical or host virtual address space. Shared CAS pages are read-only.
//!
//! This is a numerical check over the supplied host virtual addresses. It does
//! not prove anything about host physical pages or the EPT the kernel builds.
#![forbid(unsafe_code)]

use crate::{PAGE_SIZE_4K, Permission, SnapshotMap};

/// `KVM_MEM_LOG_DIRTY_PAGES` from the Linux UAPI.
pub const KVM_MEM_LOG_DIRTY_PAGES: u32 = 1;
/// `KVM_MEM_READONLY` from the Linux UAPI.
pub const KVM_MEM_READONLY: u32 = 2;
const KNOWN_FLAGS: u32 = KVM_MEM_LOG_DIRTY_PAGES | KVM_MEM_READONLY;
/// Hard ceiling on regions per plan, independent of the host limit.
pub const MAX_PLAN_REGIONS: usize = 4096;

/// One `kvm_userspace_memory_region`, with the same field names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlannedRegion {
    pub slot: u32,
    pub flags: u32,
    pub guest_phys_addr: u64,
    pub memory_size: u64,
    pub userspace_addr: u64,
}
impl PlannedRegion {
    pub fn read_only(&self) -> bool {
        self.flags & KVM_MEM_READONLY != 0
    }
}

/// Host facts a plan is verified against; normally from a KVM probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KvmHostLimits {
    /// `KVM_CAP_NR_MEMSLOTS`: slot ids must be below this.
    pub max_slots: u32,
    /// `KVM_CAP_READONLY_MEM`.
    pub readonly_mem: bool,
    /// Guest physical address width; every region must end at or below 2^bits.
    pub guest_phys_bits: u8,
}
impl Default for KvmHostLimits {
    /// Conservative defaults that any x86_64 or AArch64 KVM host satisfies.
    fn default() -> Self {
        Self {
            max_slots: 32,
            readonly_mem: true,
            guest_phys_bits: 36,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanError {
    Empty,
    TooManyRegions {
        count: usize,
        max: usize,
    },
    UnknownVm {
        vm_id: u64,
    },
    ZeroSized {
        slot: u32,
    },
    Unaligned {
        slot: u32,
    },
    Overflow {
        slot: u32,
    },
    NullHost {
        slot: u32,
    },
    UnknownFlags {
        slot: u32,
        flags: u32,
    },
    ReadOnlyUnsupported {
        slot: u32,
    },
    /// Dirty logging a read-only CAS slot is refused: it can never be dirty.
    DirtyLogReadOnly {
        slot: u32,
    },
    SlotIdOutOfRange {
        slot: u32,
        max: u32,
    },
    GuestAddressTooWide {
        slot: u32,
        bits: u8,
    },
    DuplicateSlot {
        slot: u32,
    },
    GuestOverlap {
        a: u32,
        b: u32,
    },
    HostOverlap {
        a: u32,
        b: u32,
    },
    InvalidLimits,
    DuplicateVm {
        vm_id: u64,
    },
    /// Two VMs alias host memory that is not an identical read-only range.
    CrossVmHostOverlap {
        vm_a: u64,
        slot_a: u32,
        vm_b: u64,
        slot_b: u32,
    },
}
impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for PlanError {}

/// Unverified plan; build it from a [`SnapshotMap`] or explicit regions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KvmSlotPlan {
    vm_id: u64,
    regions: Vec<PlannedRegion>,
}
impl KvmSlotPlan {
    pub fn from_regions(vm_id: u64, regions: Vec<PlannedRegion>) -> Result<Self, PlanError> {
        if regions.is_empty() {
            return Err(PlanError::Empty);
        }
        if regions.len() > MAX_PLAN_REGIONS {
            return Err(PlanError::TooManyRegions {
                count: regions.len(),
                max: MAX_PLAN_REGIONS,
            });
        }
        Ok(Self { vm_id, regions })
    }
    pub fn vm_id(&self) -> u64 {
        self.vm_id
    }
    pub fn regions(&self) -> &[PlannedRegion] {
        &self.regions
    }

    /// Checks every rule in the module docs in O(n log n).
    pub fn verify(self, limits: &KvmHostLimits) -> Result<VerifiedKvmSlotPlan, PlanError> {
        if limits.max_slots == 0 || limits.guest_phys_bits == 0 || limits.guest_phys_bits > 64 {
            return Err(PlanError::InvalidLimits);
        }
        if self.regions.len() > limits.max_slots as usize {
            return Err(PlanError::TooManyRegions {
                count: self.regions.len(),
                max: limits.max_slots as usize,
            });
        }
        let gpa_limit: u128 = 1u128 << limits.guest_phys_bits;
        for r in &self.regions {
            let slot = r.slot;
            if r.memory_size == 0 {
                return Err(PlanError::ZeroSized { slot });
            }
            if !r.guest_phys_addr.is_multiple_of(PAGE_SIZE_4K)
                || !r.userspace_addr.is_multiple_of(PAGE_SIZE_4K)
                || !r.memory_size.is_multiple_of(PAGE_SIZE_4K)
            {
                return Err(PlanError::Unaligned { slot });
            }
            let gpa_end = r
                .guest_phys_addr
                .checked_add(r.memory_size)
                .ok_or(PlanError::Overflow { slot })?;
            r.userspace_addr
                .checked_add(r.memory_size)
                .ok_or(PlanError::Overflow { slot })?;
            if r.userspace_addr == 0 {
                return Err(PlanError::NullHost { slot });
            }
            if r.flags & !KNOWN_FLAGS != 0 {
                return Err(PlanError::UnknownFlags {
                    slot,
                    flags: r.flags,
                });
            }
            if r.read_only() && !limits.readonly_mem {
                return Err(PlanError::ReadOnlyUnsupported { slot });
            }
            if r.read_only() && r.flags & KVM_MEM_LOG_DIRTY_PAGES != 0 {
                return Err(PlanError::DirtyLogReadOnly { slot });
            }
            if slot >= limits.max_slots {
                return Err(PlanError::SlotIdOutOfRange {
                    slot,
                    max: limits.max_slots,
                });
            }
            if u128::from(gpa_end) > gpa_limit {
                return Err(PlanError::GuestAddressTooWide {
                    slot,
                    bits: limits.guest_phys_bits,
                });
            }
        }
        let mut ids: Vec<u32> = self.regions.iter().map(|r| r.slot).collect();
        ids.sort_unstable();
        if let Some(w) = ids.windows(2).find(|w| w[0] == w[1]) {
            return Err(PlanError::DuplicateSlot { slot: w[0] });
        }
        let guest: Vec<(u64, u64, u32)> = self
            .regions
            .iter()
            .map(|r| (r.guest_phys_addr, r.guest_phys_addr + r.memory_size, r.slot))
            .collect();
        if let Some((a, b)) = first_overlap(guest) {
            return Err(PlanError::GuestOverlap { a, b });
        }
        let host: Vec<(u64, u64, u32)> = self
            .regions
            .iter()
            .map(|r| (r.userspace_addr, r.userspace_addr + r.memory_size, r.slot))
            .collect();
        if let Some((a, b)) = first_overlap(host) {
            return Err(PlanError::HostOverlap { a, b });
        }
        Ok(VerifiedKvmSlotPlan {
            vm_id: self.vm_id,
            regions: self.regions,
            limits: *limits,
        })
    }
}

/// Sort by start and keep the interval with the largest end seen so far: any
/// overlapping pair makes some later start fall below that running end.
fn first_overlap(mut spans: Vec<(u64, u64, u32)>) -> Option<(u32, u32)> {
    spans.sort_unstable();
    let mut reach: Option<(u64, u32)> = None;
    for (start, end, id) in spans {
        if let Some((reach_end, reach_id)) = reach {
            if start < reach_end {
                return Some((reach_id, id));
            }
            if end > reach_end {
                reach = Some((end, id));
            }
        } else {
            reach = Some((end, id));
        }
    }
    None
}

/// A plan that passed [`KvmSlotPlan::verify`]; fields cannot be changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedKvmSlotPlan {
    vm_id: u64,
    regions: Vec<PlannedRegion>,
    limits: KvmHostLimits,
}
impl VerifiedKvmSlotPlan {
    pub fn vm_id(&self) -> u64 {
        self.vm_id
    }
    pub fn regions(&self) -> &[PlannedRegion] {
        &self.regions
    }
    pub fn limits(&self) -> KvmHostLimits {
        self.limits
    }
}

/// Cross-VM host check for plans that will run on one host: VMs may share host
/// memory only as identical, read-only ranges (deduplicated CAS pages).
pub fn verify_cross_vm(plans: &[&VerifiedKvmSlotPlan]) -> Result<(), PlanError> {
    let mut vms: Vec<u64> = plans.iter().map(|p| p.vm_id).collect();
    vms.sort_unstable();
    if let Some(w) = vms.windows(2).find(|w| w[0] == w[1]) {
        return Err(PlanError::DuplicateVm { vm_id: w[0] });
    }
    let mut all: Vec<(u64, u64, bool, u64, u32)> = Vec::new();
    for plan in plans {
        for r in &plan.regions {
            all.push((
                r.userspace_addr,
                r.userspace_addr + r.memory_size,
                r.read_only(),
                plan.vm_id,
                r.slot,
            ));
        }
    }
    all.sort_unstable_by_key(|e| (e.0, e.1));
    let mut reach: Option<(u64, u64, bool, u64, u32)> = None;
    for e in all {
        // Sorted by start, a non-overlapping entry also has the largest end.
        if let Some(r) = reach
            && e.0 < r.1
        {
            let identical_shared = e.0 == r.0 && e.1 == r.1 && e.2 && r.2;
            if !identical_shared {
                return Err(PlanError::CrossVmHostOverlap {
                    vm_a: r.3,
                    slot_a: r.4,
                    vm_b: e.3,
                    slot_b: e.4,
                });
            }
            continue;
        }
        reach = Some(e);
    }
    Ok(())
}

impl SnapshotMap {
    /// Turns one VM's validated slots into a KVM slot plan. `ReadOnlyCas`
    /// slots become `KVM_MEM_READONLY`; private slots get dirty logging when
    /// `log_dirty_private` is set (needed for the next snapshot).
    pub fn kvm_slot_plan(
        &self,
        vm_id: u64,
        log_dirty_private: bool,
    ) -> Result<KvmSlotPlan, PlanError> {
        let regions: Vec<PlannedRegion> = self
            .slots
            .iter()
            .filter(|s| s.slot.vm_id == vm_id)
            .map(|s| PlannedRegion {
                slot: s.slot.slot_id,
                flags: match s.slot.permission {
                    Permission::ReadOnlyCas { .. } => KVM_MEM_READONLY,
                    Permission::Private if log_dirty_private => KVM_MEM_LOG_DIRTY_PAGES,
                    Permission::Private => 0,
                },
                guest_phys_addr: s.guest.start(),
                memory_size: s.guest.byte_len(),
                userspace_addr: s.host.start(),
            })
            .collect();
        if regions.is_empty() {
            return Err(PlanError::UnknownVm { vm_id });
        }
        KvmSlotPlan::from_regions(vm_id, regions)
    }
}
