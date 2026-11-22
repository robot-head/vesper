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

/// `PvhError` as the Vesper parser returns it (indices as u64, no
/// `Memory`: parsing never touches guest memory).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PvhFault {
    Truncated,
    BadMagic,
    NotElf64LittleEndian,
    NotX86_64Executable,
    BadProgramHeaders,
    NoLoadSegment,
    SegmentFileRange { index: u64 },
    SegmentOverflow { index: u64 },
    SegmentOutsideRam { index: u64 },
    SegmentsOverlap { a: u64, b: u64 },
    SegmentOverBootData { index: u64 },
    MalformedNote,
    MissingPvhNote,
    DuplicatePvhNote,
    EntryAbove4G,
    EntryNotExecutable { entry: u32 },
}

impl From<PvhFault> for crate::pvh::PvhError {
    fn from(e: PvhFault) -> Self {
        use crate::pvh::PvhError as P;
        // Indices are below MAX_PHDRS (64).
        let ix = |n: u64| usize::try_from(n).unwrap_or(usize::MAX);
        match e {
            PvhFault::Truncated => P::Truncated,
            PvhFault::BadMagic => P::BadMagic,
            PvhFault::NotElf64LittleEndian => P::NotElf64LittleEndian,
            PvhFault::NotX86_64Executable => P::NotX86_64Executable,
            PvhFault::BadProgramHeaders => P::BadProgramHeaders,
            PvhFault::NoLoadSegment => P::NoLoadSegment,
            PvhFault::SegmentFileRange { index } => P::SegmentFileRange { index: ix(index) },
            PvhFault::SegmentOverflow { index } => P::SegmentOverflow { index: ix(index) },
            PvhFault::SegmentOutsideRam { index } => P::SegmentOutsideRam { index: ix(index) },
            PvhFault::SegmentsOverlap { a, b } => P::SegmentsOverlap { a: ix(a), b: ix(b) },
            PvhFault::SegmentOverBootData { index } => P::SegmentOverBootData { index: ix(index) },
            PvhFault::MalformedNote => P::MalformedNote,
            PvhFault::MissingPvhNote => P::MissingPvhNote,
            PvhFault::DuplicatePvhNote => P::DuplicatePvhNote,
            PvhFault::EntryAbove4G => P::EntryAbove4G,
            PvhFault::EntryNotExecutable { entry } => P::EntryNotExecutable { entry },
        }
    }
}

/// `HotSetError` as the Vesper code returns it (the index as u64).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HotFault {
    OutOfRange { index: u64 },
    TooMany,
    BadBatch,
    Truncated,
    BadMagic,
    TrailingBytes,
    NotCanonical,
}

impl From<HotFault> for crate::hotset::HotSetError {
    fn from(e: HotFault) -> Self {
        use crate::hotset::HotSetError as H;
        match e {
            HotFault::OutOfRange { index } => H::OutOfRange {
                index: usize::try_from(index).unwrap_or(usize::MAX),
            },
            HotFault::TooMany => H::TooMany,
            HotFault::BadBatch => H::BadBatch,
            HotFault::Truncated => H::Truncated,
            HotFault::BadMagic => H::BadMagic,
            HotFault::TrailingBytes => H::TrailingBytes,
            HotFault::NotCanonical => H::NotCanonical,
        }
    }
}
