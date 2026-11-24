//! Checked 16 KiB slotted-page codec, backed by `@vesper/quarry::page`.
use crate::port::binding;
use std::{fmt, ops::Range};

pub const PAGE_SIZE: usize = 16 * 1024;
pub const HEADER_SIZE: usize = 32;
pub const SLOT_SIZE: usize = 4;
pub const MAX_SLOTS: usize = (PAGE_SIZE - HEADER_SIZE) / (SLOT_SIZE + 1);
pub const MAX_RECORD_SIZE: usize = PAGE_SIZE - HEADER_SIZE - SLOT_SIZE;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageError {
    Size,
    Checksum,
    UnsupportedHeader,
    SlotLayout,
    SlotOutOfBounds,
    RecordSize,
    Full,
    Overflow,
}
impl fmt::Display for PageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "page {self:?}")
    }
}
impl std::error::Error for PageError {}

fn page_error(code: u64) -> PageError {
    match code {
        1 => PageError::Size,
        2 => PageError::Checksum,
        3 => PageError::UnsupportedHeader,
        4 => PageError::SlotLayout,
        5 => PageError::SlotOutOfBounds,
        6 => PageError::RecordSize,
        7 => PageError::Full,
        _ => PageError::Overflow,
    }
}

/// Exactly these fields are encoded at offsets 0,8,16,20,22,24,25,26,28.
/// The Rust struct itself is never cast to wire bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageHeader {
    pub page_lsn: u64,
    pub page_id: u64,
    pub checksum_crc32c: u32,
    pub slot_count: u16,
    pub free_space_offset: u16,
    pub page_kind_tag: u8,
    pub level: u8,
    pub flags: u16,
    pub right_sibling_page: u32,
}
impl PageHeader {
    fn decode(bytes: &[u8]) -> Self {
        Self {
            page_lsn: u64::from_le_bytes(bytes[0..8].try_into().expect("fixed field")),
            page_id: u64::from_le_bytes(bytes[8..16].try_into().expect("fixed field")),
            checksum_crc32c: read_u32(bytes, 16),
            slot_count: read_u16(bytes, 20),
            free_space_offset: read_u16(bytes, 22),
            page_kind_tag: bytes[24],
            level: bytes[25],
            flags: read_u16(bytes, 26),
            right_sibling_page: read_u32(bytes, 28),
        }
    }
}
fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}
fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}
fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

const CRC_RANGE: Range<usize> = 16..20;

const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = (crc >> 1) ^ if crc & 1 == 0 { 0 } else { 0x82f6_3b78 };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

/// Castagnoli CRC32C (reflected 0x82f63b78, init/final XOR all ones).
/// The checksum field itself is treated as four zero bytes.
pub fn page_crc32c(bytes: &[u8]) -> Result<u32, PageError> {
    if bytes.len() != PAGE_SIZE {
        return Err(PageError::Size);
    }
    let mut crc = crc32c_update(!0, &bytes[..CRC_RANGE.start]);
    crc = crc32c_update(crc, &[0; 4]);
    Ok(!crc32c_update(crc, &bytes[CRC_RANGE.end..]))
}

/// Table-driven CRC32C state update; callers apply the initial and final XOR.
pub(crate) fn crc32c_update(mut crc: u32, bytes: &[u8]) -> u32 {
    for &byte in bytes {
        crc = (crc >> 8) ^ CRC_TABLE[((crc ^ u32::from(byte)) & 0xff) as usize];
    }
    crc
}

/// Castagnoli CRC32C of a whole byte string (used by WAL frames and headers).
pub fn crc32c(bytes: &[u8]) -> u32 {
    !crc32c_update(!0, bytes)
}

/// Validated page bytes. Mutations never expose raw mutable bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct SlottedPage {
    bytes: Box<[u8; PAGE_SIZE]>,
}
impl fmt::Debug for SlottedPage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SlottedPage")
            .field("header", &self.header())
            .finish()
    }
}
impl SlottedPage {
    /// Supported kind 1: opaque slotted records at level 0, flags 0.
    /// The sibling ID is preserved metadata, not a tree traversal contract.
    pub fn new(page_id: u64, right_sibling_page: u32) -> Self {
        let mut page = Self {
            bytes: Box::new([0; PAGE_SIZE]),
        };
        page.bytes[8..16].copy_from_slice(&page_id.to_le_bytes());
        write_u16(page.bytes.as_mut(), 22, PAGE_SIZE as u16);
        page.bytes[24] = 1;
        page.bytes[28..32].copy_from_slice(&right_sibling_page.to_le_bytes());
        page.refresh_crc();
        page
    }
    /// Validates borrowed bytes before copying, so failures preserve the input.
    pub fn decode(bytes: &[u8]) -> Result<Self, PageError> {
        if bytes.len() != PAGE_SIZE {
            return Err(PageError::Size);
        }
        let h = PageHeader::decode(bytes);
        if page_crc32c(bytes)? != h.checksum_crc32c {
            return Err(PageError::Checksum);
        }
        let mut seen = [0u64; 256];
        let err = binding::page_validate(bytes, &mut seen);
        if err != 0 {
            return Err(page_error(err));
        }
        let mut page = Self {
            bytes: Box::new([0; PAGE_SIZE]),
        };
        page.bytes.copy_from_slice(bytes);
        Ok(page)
    }
    pub fn header(&self) -> PageHeader {
        PageHeader::decode(self.bytes.as_slice())
    }
    pub fn as_bytes(&self) -> &[u8; PAGE_SIZE] {
        &self.bytes
    }
    pub fn slot_count(&self) -> usize {
        usize::from(read_u16(self.bytes.as_slice(), 20))
    }
    pub fn free_bytes(&self) -> usize {
        usize::from(read_u16(self.bytes.as_slice(), 22))
            - (HEADER_SIZE + self.slot_count() * SLOT_SIZE)
    }
    /// Header, directory and live record bytes; `PAGE_SIZE - used_bytes()`
    /// is what an update (which compacts) can still use.
    pub fn used_bytes(&self) -> usize {
        binding::page_used_bytes(self.bytes.as_slice()) as usize
    }
    pub fn read(&self, slot: u16) -> Result<&[u8], PageError> {
        if usize::from(slot) >= self.slot_count() {
            return Err(PageError::SlotOutOfBounds);
        }
        let range = slot_range(self.bytes.as_slice(), usize::from(slot))?;
        Ok(&self.bytes[range])
    }
    pub fn insert(&mut self, record: &[u8]) -> Result<u16, PageError> {
        let res = binding::page_insert(self.bytes.as_mut(), record);
        if res >= 65536 {
            Err(page_error(res - 65536))
        } else {
            self.refresh_crc();
            Ok(res as u16)
        }
    }
    /// Rebuilds/compacts into a fresh page, retaining slot identities and metadata.
    /// A capacity or invalid-slot failure leaves the entire previous page intact.
    pub fn update(&mut self, slot: u16, record: &[u8]) -> Result<(), PageError> {
        let mut scratch = Box::new([0u8; PAGE_SIZE]);
        let err = binding::page_update(
            self.bytes.as_mut(),
            u64::from(slot),
            record,
            scratch.as_mut(),
        );
        if err != 0 {
            Err(page_error(err))
        } else {
            self.refresh_crc();
            Ok(())
        }
    }
    pub(crate) fn set_lsn(&mut self, lsn: u64) {
        self.bytes[..8].copy_from_slice(&lsn.to_le_bytes());
        self.refresh_crc();
    }
    fn refresh_crc(&mut self) {
        let checksum = page_crc32c(self.bytes.as_slice()).expect("fixed page length");
        self.bytes[CRC_RANGE].copy_from_slice(&checksum.to_le_bytes());
    }
}
fn slot_range(bytes: &[u8], slot: usize) -> Result<Range<usize>, PageError> {
    let offset = HEADER_SIZE
        .checked_add(slot.checked_mul(SLOT_SIZE).ok_or(PageError::Overflow)?)
        .ok_or(PageError::Overflow)?;
    if offset.checked_add(SLOT_SIZE).ok_or(PageError::Overflow)? > PAGE_SIZE {
        return Err(PageError::SlotLayout);
    }
    let start = usize::from(read_u16(bytes, offset));
    let len = usize::from(read_u16(bytes, offset + 2));
    let end = start.checked_add(len).ok_or(PageError::Overflow)?;
    if len == 0 || end > PAGE_SIZE {
        return Err(PageError::SlotLayout);
    }
    Ok(start..end)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn standard_castagnoli_check_vector() {
        assert_eq!(crc32c(b"123456789"), 0xe306_9283);
    }
}
