//! Checked 16 KiB slotted-page codec. This is a host byte format, not a B+Tree.
use std::{fmt, ops::Range};

pub const PAGE_SIZE: usize = 16 * 1024;
pub const HEADER_SIZE: usize = 32;
pub const SLOT_SIZE: usize = 4;
pub const MAX_SLOTS: usize = (PAGE_SIZE - HEADER_SIZE) / (SLOT_SIZE + 1);
pub const MAX_RECORD_SIZE: usize = PAGE_SIZE - HEADER_SIZE - SLOT_SIZE;
const CRC_RANGE: Range<usize> = 16..20;

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

/// Castagnoli CRC32C (reflected 0x82f63b78, init/final XOR all ones).
/// The checksum field itself is treated as four zero bytes.
pub fn page_crc32c(bytes: &[u8]) -> Result<u32, PageError> {
    if bytes.len() != PAGE_SIZE {
        return Err(PageError::Size);
    }
    Ok(crc32c(bytes.iter().enumerate().map(|(i, b)| {
        if CRC_RANGE.contains(&i) {
            0
        } else {
            *b
        }
    })))
}
fn crc32c(bytes: impl IntoIterator<Item = u8>) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 == 0 { 0 } else { 0x82f6_3b78 };
        }
    }
    !crc
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
        validate(bytes)?;
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
    pub fn read(&self, slot: u16) -> Result<&[u8], PageError> {
        if usize::from(slot) >= self.slot_count() {
            return Err(PageError::SlotOutOfBounds);
        }
        let range = slot_range(self.bytes.as_slice(), usize::from(slot))?;
        Ok(&self.bytes[range])
    }
    pub fn insert(&mut self, record: &[u8]) -> Result<u16, PageError> {
        check_record(record)?;
        let count = self.slot_count();
        if count >= MAX_SLOTS {
            return Err(PageError::Full);
        }
        let directory_end = HEADER_SIZE
            .checked_add(
                (count + 1)
                    .checked_mul(SLOT_SIZE)
                    .ok_or(PageError::Overflow)?,
            )
            .ok_or(PageError::Overflow)?;
        let start = usize::from(self.header().free_space_offset)
            .checked_sub(record.len())
            .ok_or(PageError::Full)?;
        if start < directory_end {
            return Err(PageError::Full);
        }
        // All fallible checks precede the first write.
        self.bytes[start..start + record.len()].copy_from_slice(record);
        write_slot(self.bytes.as_mut(), count, start, record.len());
        write_u16(self.bytes.as_mut(), 20, (count + 1) as u16);
        write_u16(self.bytes.as_mut(), 22, start as u16);
        self.refresh_crc();
        Ok(count as u16)
    }
    /// Rebuilds/compacts into a fresh page, retaining slot identities and metadata.
    /// A capacity or invalid-slot failure leaves the entire previous page intact.
    pub fn update(&mut self, slot: u16, record: &[u8]) -> Result<(), PageError> {
        check_record(record)?;
        self.read(slot)?;
        let count = self.slot_count();
        let mut used = HEADER_SIZE + count * SLOT_SIZE;
        for i in 0..count {
            let len = if i == usize::from(slot) {
                record.len()
            } else {
                self.read(i as u16)?.len()
            };
            used = used.checked_add(len).ok_or(PageError::Overflow)?;
        }
        if used > PAGE_SIZE {
            return Err(PageError::Full);
        }
        let mut next = Self {
            bytes: Box::new([0; PAGE_SIZE]),
        };
        next.bytes[..HEADER_SIZE].copy_from_slice(&self.bytes[..HEADER_SIZE]);
        let mut cursor = PAGE_SIZE;
        for i in 0..count {
            let data = if i == usize::from(slot) {
                record
            } else {
                self.read(i as u16)?
            };
            cursor = cursor.checked_sub(data.len()).ok_or(PageError::Overflow)?;
            next.bytes[cursor..cursor + data.len()].copy_from_slice(data);
            write_slot(next.bytes.as_mut(), i, cursor, data.len());
        }
        write_u16(next.bytes.as_mut(), 22, cursor as u16);
        next.refresh_crc();
        *self = next;
        Ok(())
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
fn check_record(record: &[u8]) -> Result<(), PageError> {
    if record.is_empty() || record.len() > MAX_RECORD_SIZE {
        Err(PageError::RecordSize)
    } else {
        Ok(())
    }
}
fn write_slot(bytes: &mut [u8], slot: usize, start: usize, len: usize) {
    let offset = HEADER_SIZE + slot * SLOT_SIZE;
    write_u16(bytes, offset, start as u16);
    write_u16(bytes, offset + 2, len as u16);
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
fn validate(bytes: &[u8]) -> Result<(), PageError> {
    if bytes.len() != PAGE_SIZE {
        return Err(PageError::Size);
    }
    let h = PageHeader::decode(bytes);
    if page_crc32c(bytes)? != h.checksum_crc32c {
        return Err(PageError::Checksum);
    }
    if h.page_kind_tag != 1 || h.level != 0 || h.flags != 0 {
        return Err(PageError::UnsupportedHeader);
    }
    let count = usize::from(h.slot_count);
    if count > MAX_SLOTS {
        return Err(PageError::SlotLayout);
    }
    let directory_end = HEADER_SIZE
        .checked_add(count.checked_mul(SLOT_SIZE).ok_or(PageError::Overflow)?)
        .ok_or(PageError::Overflow)?;
    let free = usize::from(h.free_space_offset);
    if free < directory_end || free > PAGE_SIZE {
        return Err(PageError::SlotLayout);
    }
    let mut ranges = Vec::with_capacity(count);
    for i in 0..count {
        let range = slot_range(bytes, i)?;
        if range.start < free {
            return Err(PageError::SlotLayout);
        }
        ranges.push(range);
    }
    ranges.sort_unstable_by_key(|range| range.start);
    let first = ranges.first().map_or(PAGE_SIZE, |range| range.start);
    if first != free {
        return Err(PageError::SlotLayout);
    }
    for pair in ranges.windows(2) {
        if pair[0].end > pair[1].start {
            return Err(PageError::SlotLayout);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn standard_castagnoli_check_vector() {
        assert_eq!(crc32c(b"123456789".iter().copied()), 0xe306_9283);
    }
}
