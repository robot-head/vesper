//! `PmuSampleWire` (§41.2): one hardware/TMA sample record as an exact
//! 64-byte, padding-free, little-endian wire struct (one L1 cache line).
//!
//! The record has no room for an availability flag, so this module keeps
//! availability next to it: a [`PmuSample`] holds every field as measured or
//! unavailable, [`PmuSample::to_wire`] refuses to encode a sample with any
//! unavailable field, and [`PmuSample::to_wire_masked`] encodes unavailable
//! fields as zero together with an explicit [`AvailabilityMask`]. A zero in
//! the wire bytes is therefore never, on its own, a claim that an event did
//! not happen. Decoding rejects wrong lengths and out-of-range basis points or
//! layout offsets with typed errors.
use std::fmt;

/// Size of the encoded record: one 64-byte cache line.
pub const PMU_SAMPLE_WIRE_BYTES: usize = 64;

/// Fields of [`PmuSampleWire`] in wire order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PmuField {
    RefCycles,
    InstructionsRetired,
    L1dReadMisses,
    LlcMisses,
    BranchMispredicts,
    SimdVecUopsRetired,
    ArenaBumpBytes,
    TmaBackendBoundSlotsBp,
    IcacheRandOffsetBytes,
}
impl PmuField {
    pub const ALL: [PmuField; 9] = [
        Self::RefCycles,
        Self::InstructionsRetired,
        Self::L1dReadMisses,
        Self::LlcMisses,
        Self::BranchMispredicts,
        Self::SimdVecUopsRetired,
        Self::ArenaBumpBytes,
        Self::TmaBackendBoundSlotsBp,
        Self::IcacheRandOffsetBytes,
    ];
    pub const fn name(self) -> &'static str {
        match self {
            Self::RefCycles => "ref_cycles",
            Self::InstructionsRetired => "instructions_retired",
            Self::L1dReadMisses => "l1d_read_misses",
            Self::LlcMisses => "llc_misses",
            Self::BranchMispredicts => "branch_mispredicts",
            Self::SimdVecUopsRetired => "simd_vec_uops_retired",
            Self::ArenaBumpBytes => "arena_bump_bytes",
            Self::TmaBackendBoundSlotsBp => "tma_backend_bound_slots_bp",
            Self::IcacheRandOffsetBytes => "icache_rand_offset_bytes",
        }
    }
    /// Byte offset and width in the wire record.
    pub const fn span(self) -> (usize, usize) {
        match self {
            Self::RefCycles => (0, 8),
            Self::InstructionsRetired => (8, 8),
            Self::L1dReadMisses => (16, 8),
            Self::LlcMisses => (24, 8),
            Self::BranchMispredicts => (32, 8),
            Self::SimdVecUopsRetired => (40, 8),
            Self::ArenaBumpBytes => (48, 8),
            Self::TmaBackendBoundSlotsBp => (56, 4),
            Self::IcacheRandOffsetBytes => (60, 4),
        }
    }
    const fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WireError {
    /// Decoding input is not exactly 64 bytes.
    Length(usize),
    /// Top-Down basis points above 10000.
    BasisPointsOutOfRange(u64),
    /// Stabilizer offset not a multiple of 64 below 4096.
    LayoutOffsetOutOfRange(u64),
    /// A 32-bit field was given a larger value.
    FieldOverflow(PmuField),
    /// [`PmuSample::to_wire`] with fields that were not measured.
    Unavailable(Vec<PmuField>),
    /// An availability mask with bits outside the nine fields.
    InvalidMask(u16),
}
impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Length(n) => write!(f, "PmuSampleWire must be 64 bytes, got {n}"),
            Self::BasisPointsOutOfRange(v) => write!(f, "TMA basis points {v} exceed 10000"),
            Self::LayoutOffsetOutOfRange(v) => {
                write!(f, "icache offset {v} is not a multiple of 64 below 4096")
            }
            Self::FieldOverflow(field) => write!(f, "{} does not fit 32 bits", field.name()),
            Self::Unavailable(fields) => {
                write!(f, "unavailable PMU fields:")?;
                for field in fields {
                    write!(f, " {}", field.name())?;
                }
                Ok(())
            }
            Self::InvalidMask(m) => write!(f, "invalid availability mask {m:#x}"),
        }
    }
}
impl std::error::Error for WireError {}

/// Exact 64-byte record: seven `u64le` then two `u32le`, no padding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PmuSampleWire {
    pub ref_cycles: u64,
    pub instructions_retired: u64,
    pub l1d_read_misses: u64,
    pub llc_misses: u64,
    pub branch_mispredicts: u64,
    pub simd_vec_uops_retired: u64,
    pub arena_bump_bytes: u64,
    /// Basis points (0..=10000) of Top-Down backend-bound slots.
    pub tma_backend_bound_slots_bp: u32,
    /// Stabilizer 64-byte-aligned entry jitter offset in [0, 4096).
    pub icache_rand_offset_bytes: u32,
}

fn check_ranges(bp: u64, offset: u64) -> Result<(), WireError> {
    if bp > 10_000 {
        return Err(WireError::BasisPointsOutOfRange(bp));
    }
    if offset >= 4096 || !offset.is_multiple_of(64) {
        return Err(WireError::LayoutOffsetOutOfRange(offset));
    }
    Ok(())
}

impl PmuSampleWire {
    pub fn get(&self, field: PmuField) -> u64 {
        match field {
            PmuField::RefCycles => self.ref_cycles,
            PmuField::InstructionsRetired => self.instructions_retired,
            PmuField::L1dReadMisses => self.l1d_read_misses,
            PmuField::LlcMisses => self.llc_misses,
            PmuField::BranchMispredicts => self.branch_mispredicts,
            PmuField::SimdVecUopsRetired => self.simd_vec_uops_retired,
            PmuField::ArenaBumpBytes => self.arena_bump_bytes,
            PmuField::TmaBackendBoundSlotsBp => u64::from(self.tma_backend_bound_slots_bp),
            PmuField::IcacheRandOffsetBytes => u64::from(self.icache_rand_offset_bytes),
        }
    }
    fn set(&mut self, field: PmuField, value: u64) -> Result<(), WireError> {
        let narrow = || u32::try_from(value).map_err(|_| WireError::FieldOverflow(field));
        match field {
            PmuField::RefCycles => self.ref_cycles = value,
            PmuField::InstructionsRetired => self.instructions_retired = value,
            PmuField::L1dReadMisses => self.l1d_read_misses = value,
            PmuField::LlcMisses => self.llc_misses = value,
            PmuField::BranchMispredicts => self.branch_mispredicts = value,
            PmuField::SimdVecUopsRetired => self.simd_vec_uops_retired = value,
            PmuField::ArenaBumpBytes => self.arena_bump_bytes = value,
            PmuField::TmaBackendBoundSlotsBp => self.tma_backend_bound_slots_bp = narrow()?,
            PmuField::IcacheRandOffsetBytes => self.icache_rand_offset_bytes = narrow()?,
        }
        Ok(())
    }
    pub fn validate(&self) -> Result<(), WireError> {
        check_ranges(
            u64::from(self.tma_backend_bound_slots_bp),
            u64::from(self.icache_rand_offset_bytes),
        )
    }
    pub fn encode(&self) -> Result<[u8; PMU_SAMPLE_WIRE_BYTES], WireError> {
        self.validate()?;
        let mut bytes = [0u8; PMU_SAMPLE_WIRE_BYTES];
        for field in PmuField::ALL {
            let (offset, width) = field.span();
            let value = self.get(field).to_le_bytes();
            bytes[offset..offset + width].copy_from_slice(&value[..width]);
        }
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        if bytes.len() != PMU_SAMPLE_WIRE_BYTES {
            return Err(WireError::Length(bytes.len()));
        }
        let mut wire = Self::default();
        for field in PmuField::ALL {
            let (offset, width) = field.span();
            let mut value = [0u8; 8];
            value[..width].copy_from_slice(&bytes[offset..offset + width]);
            wire.set(field, u64::from_le_bytes(value))?;
        }
        wire.validate()?;
        Ok(wire)
    }
}

/// Bit `i` set: field `PmuField::ALL[i]` was measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AvailabilityMask(u16);
impl AvailabilityMask {
    pub const ALL: u16 = (1 << 9) - 1;
    pub fn new(bits: u16) -> Result<Self, WireError> {
        if bits & !Self::ALL != 0 {
            return Err(WireError::InvalidMask(bits));
        }
        Ok(Self(bits))
    }
    pub fn bits(self) -> u16 {
        self.0
    }
    pub fn contains(self, field: PmuField) -> bool {
        self.0 & (1 << field.index()) != 0
    }
}

/// One sample with per-field availability. Fields start unavailable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PmuSample {
    values: [Option<u64>; 9],
}
impl PmuSample {
    pub fn new() -> Self {
        Self::default()
    }
    /// Records a measured value, rejecting values the wire cannot carry.
    pub fn set(&mut self, field: PmuField, value: u64) -> Result<(), WireError> {
        PmuSampleWire::default().set(field, value)?;
        match field {
            PmuField::TmaBackendBoundSlotsBp => check_ranges(value, 0)?,
            PmuField::IcacheRandOffsetBytes => check_ranges(0, value)?,
            _ => {}
        }
        self.values[field.index()] = Some(value);
        Ok(())
    }
    pub fn get(&self, field: PmuField) -> Option<u64> {
        self.values[field.index()]
    }
    pub fn unavailable(&self) -> Vec<PmuField> {
        PmuField::ALL
            .into_iter()
            .filter(|f| self.get(*f).is_none())
            .collect()
    }
    pub fn mask(&self) -> AvailabilityMask {
        let bits = PmuField::ALL
            .into_iter()
            .filter(|f| self.get(*f).is_some())
            .fold(0u16, |m, f| m | (1 << f.index()));
        AvailabilityMask(bits)
    }
    /// The complete record; an error lists every unavailable field.
    pub fn to_wire(&self) -> Result<PmuSampleWire, WireError> {
        let missing = self.unavailable();
        if !missing.is_empty() {
            return Err(WireError::Unavailable(missing));
        }
        self.to_wire_masked().map(|(wire, _)| wire)
    }
    /// The record with unavailable fields zeroed, and the mask that says so.
    pub fn to_wire_masked(&self) -> Result<(PmuSampleWire, AvailabilityMask), WireError> {
        let mut wire = PmuSampleWire::default();
        for field in PmuField::ALL {
            wire.set(field, self.get(field).unwrap_or(0))?;
        }
        wire.validate()?;
        Ok((wire, self.mask()))
    }
    /// Inverse of [`to_wire_masked`](Self::to_wire_masked).
    pub fn from_wire_masked(wire: &PmuSampleWire, mask: AvailabilityMask) -> Self {
        let mut sample = Self::new();
        for field in PmuField::ALL {
            if mask.contains(field) {
                sample.values[field.index()] = Some(wire.get(field));
            }
        }
        sample
    }
}

/// Top-Down backend-bound share in basis points, rounded down; `None` when
/// no slots were counted or the backend count exceeds the slot count.
pub fn backend_bound_bp(backend_slots: u64, total_slots: u64) -> Option<u64> {
    if total_slots == 0 || backend_slots > total_slots {
        return None;
    }
    Some((u128::from(backend_slots) * 10_000 / u128::from(total_slots)) as u64)
}
