//! The Vesper port's boundary types: the crate's own where they match, and
//! port-only ones the binding converts.

pub(crate) use crate::slot_plan::{KvmHostLimits, PlannedRegion};

/// `PlanError` as the Vesper verifier returns it (counts as u64).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SlotError {
    InvalidLimits,
    TooManyRegions {
        count: u64,
        max: u64,
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
    DuplicateVm {
        vm_id: u64,
    },
    CrossVmHostOverlap {
        vm_a: u64,
        slot_a: u32,
        vm_b: u64,
        slot_b: u32,
    },
}

impl From<SlotError> for crate::slot_plan::PlanError {
    fn from(e: SlotError) -> Self {
        use crate::slot_plan::PlanError as P;
        let count = |n: u64| usize::try_from(n).unwrap_or(usize::MAX);
        match e {
            SlotError::InvalidLimits => P::InvalidLimits,
            SlotError::TooManyRegions { count: c, max } => P::TooManyRegions {
                count: count(c),
                max: count(max),
            },
            SlotError::ZeroSized { slot } => P::ZeroSized { slot },
            SlotError::Unaligned { slot } => P::Unaligned { slot },
            SlotError::Overflow { slot } => P::Overflow { slot },
            SlotError::NullHost { slot } => P::NullHost { slot },
            SlotError::UnknownFlags { slot, flags } => P::UnknownFlags { slot, flags },
            SlotError::ReadOnlyUnsupported { slot } => P::ReadOnlyUnsupported { slot },
            SlotError::DirtyLogReadOnly { slot } => P::DirtyLogReadOnly { slot },
            SlotError::SlotIdOutOfRange { slot, max } => P::SlotIdOutOfRange { slot, max },
            SlotError::GuestAddressTooWide { slot, bits } => P::GuestAddressTooWide { slot, bits },
            SlotError::DuplicateSlot { slot } => P::DuplicateSlot { slot },
            SlotError::GuestOverlap { a, b } => P::GuestOverlap { a, b },
            SlotError::HostOverlap { a, b } => P::HostOverlap { a, b },
            SlotError::DuplicateVm { vm_id } => P::DuplicateVm { vm_id },
            SlotError::CrossVmHostOverlap {
                vm_a,
                slot_a,
                vm_b,
                slot_b,
            } => P::CrossVmHostOverlap {
                vm_a,
                slot_a,
                vm_b,
                slot_b,
            },
        }
    }
}
