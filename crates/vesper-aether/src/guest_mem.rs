//! Bounded guest RAM model for the device models and PVH loader.
//!
//! Guest physical memory is a small set of page-aligned, non-overlapping
//! regions backed by ordinary host bytes. Every access must lie inside one
//! region; anything else is a typed [`MemError`], never a partial access.
//! This is a host model: it is not mapped into a KVM guest (the gated KVM path
//! copies it into its own anonymous mapping).
#![forbid(unsafe_code)]

use crate::PAGE_SIZE_4K;
use crate::virtio::{AuthorizedRange, DeviceAccess};

/// Total RAM ceiling for the model (1 GiB).
pub const MAX_GUEST_RAM_BYTES: u64 = 1 << 30;
/// Region count ceiling.
pub const MAX_GUEST_REGIONS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemError {
    NoRegions,
    TooManyRegions,
    EmptyRegion,
    Unaligned,
    Overflow,
    Overlap,
    TooLarge,
    /// The access is not contained in one RAM region.
    OutOfBounds {
        gpa: u64,
        len: u64,
    },
}
impl std::fmt::Display for MemError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for MemError {}

#[derive(Clone, Debug)]
struct Region {
    gpa: u64,
    bytes: Vec<u8>,
}

/// Guest RAM: sorted, page-aligned, non-overlapping regions.
#[derive(Clone, Debug)]
pub struct GuestMemory {
    regions: Vec<Region>,
}

impl GuestMemory {
    /// `layout` is `(guest_physical_address, byte_len)` per region.
    pub fn new(layout: &[(u64, u64)]) -> Result<Self, MemError> {
        if layout.is_empty() {
            return Err(MemError::NoRegions);
        }
        if layout.len() > MAX_GUEST_REGIONS {
            return Err(MemError::TooManyRegions);
        }
        let mut sorted = layout.to_vec();
        sorted.sort_unstable_by_key(|r| r.0);
        let mut total = 0u64;
        let mut prev_end = 0u64;
        let mut regions = Vec::with_capacity(sorted.len());
        for (i, &(gpa, len)) in sorted.iter().enumerate() {
            if len == 0 {
                return Err(MemError::EmptyRegion);
            }
            if !gpa.is_multiple_of(PAGE_SIZE_4K) || !len.is_multiple_of(PAGE_SIZE_4K) {
                return Err(MemError::Unaligned);
            }
            let end = gpa.checked_add(len).ok_or(MemError::Overflow)?;
            if i > 0 && gpa < prev_end {
                return Err(MemError::Overlap);
            }
            total = total.checked_add(len).ok_or(MemError::Overflow)?;
            if total > MAX_GUEST_RAM_BYTES {
                return Err(MemError::TooLarge);
            }
            prev_end = end;
            regions.push(Region {
                gpa,
                bytes: vec![0; usize::try_from(len).map_err(|_| MemError::TooLarge)?],
            });
        }
        Ok(Self { regions })
    }

    /// `(gpa, len)` of every region, ascending.
    pub fn layout(&self) -> Vec<(u64, u64)> {
        self.regions
            .iter()
            .map(|r| (r.gpa, r.bytes.len() as u64))
            .collect()
    }

    /// Read-write device authorizations covering exactly the guest RAM.
    pub fn ram_authorizations(&self) -> Vec<AuthorizedRange> {
        self.layout()
            .into_iter()
            .map(|(guest_start, byte_len)| AuthorizedRange {
                guest_start,
                byte_len,
                access: DeviceAccess::ReadWrite,
            })
            .collect()
    }

    fn locate(&self, gpa: u64, len: u64) -> Result<(usize, usize), MemError> {
        let oob = MemError::OutOfBounds { gpa, len };
        let end = gpa.checked_add(len).ok_or(oob)?;
        for (i, r) in self.regions.iter().enumerate() {
            let r_end = r.gpa + r.bytes.len() as u64;
            if r.gpa <= gpa && end <= r_end {
                return Ok((i, usize::try_from(gpa - r.gpa).map_err(|_| oob)?));
            }
        }
        Err(oob)
    }

    /// Whether `[gpa, gpa + len)` lies inside one region.
    pub fn contains(&self, gpa: u64, len: u64) -> bool {
        self.locate(gpa, len).is_ok()
    }

    pub fn read(&self, gpa: u64, out: &mut [u8]) -> Result<(), MemError> {
        let (i, off) = self.locate(gpa, out.len() as u64)?;
        out.copy_from_slice(&self.regions[i].bytes[off..off + out.len()]);
        Ok(())
    }

    pub fn write(&mut self, gpa: u64, bytes: &[u8]) -> Result<(), MemError> {
        let (i, off) = self.locate(gpa, bytes.len() as u64)?;
        self.regions[i].bytes[off..off + bytes.len()].copy_from_slice(bytes);
        Ok(())
    }

    /// Zero-fills `[gpa, gpa + len)`.
    pub fn fill_zero(&mut self, gpa: u64, len: u64) -> Result<(), MemError> {
        let (i, off) = self.locate(gpa, len)?;
        let n = usize::try_from(len).map_err(|_| MemError::TooLarge)?;
        self.regions[i].bytes[off..off + n].fill(0);
        Ok(())
    }

    pub fn read_u16(&self, gpa: u64) -> Result<u16, MemError> {
        let mut b = [0; 2];
        self.read(gpa, &mut b)?;
        Ok(u16::from_le_bytes(b))
    }
    pub fn read_u32(&self, gpa: u64) -> Result<u32, MemError> {
        let mut b = [0; 4];
        self.read(gpa, &mut b)?;
        Ok(u32::from_le_bytes(b))
    }
    pub fn read_u64(&self, gpa: u64) -> Result<u64, MemError> {
        let mut b = [0; 8];
        self.read(gpa, &mut b)?;
        Ok(u64::from_le_bytes(b))
    }

    /// Borrow of one whole region's bytes, for copying into a real mapping.
    pub fn region_bytes(&self, index: usize) -> Option<(u64, &[u8])> {
        self.regions.get(index).map(|r| (r.gpa, r.bytes.as_slice()))
    }
}
