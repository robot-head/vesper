//! Actual Arrow primitive signed-i64 values/validity validation. This accepts
//! explicit FieldNode/Buffer descriptors over a borrowed body, not IPC envelopes,
//! compression, dictionaries, child arrays, offsets, DMA or typed pointer casts.
use crate::{Error, ErrorKind, Result, add, bound, charge, mul};
use core::ops::Range;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BufferDesc {
    pub offset_bytes: i64,
    pub byte_length: i64,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FieldNode {
    pub row_length: i64,
    pub null_count: i64,
}
fn pair(bytes: &[u8]) -> Result<(i64, i64)> {
    if bytes.len() != 16 {
        return Err(Error::new(ErrorKind::Descriptor, 0));
    }
    Ok((
        i64::from_le_bytes(bytes[..8].try_into().expect("eight bytes")),
        i64::from_le_bytes(bytes[8..].try_into().expect("eight bytes")),
    ))
}
impl BufferDesc {
    pub fn from_wire(bytes: &[u8]) -> Result<Self> {
        let (offset_bytes, byte_length) = pair(bytes)?;
        Ok(Self {
            offset_bytes,
            byte_length,
        })
    }
}
impl FieldNode {
    pub fn from_wire(bytes: &[u8]) -> Result<Self> {
        let (row_length, null_count) = pair(bytes)?;
        Ok(Self {
            row_length,
            null_count,
        })
    }
}

/// Explicit host validation profiles. Relative offsets do not imply actual host
/// pointer alignment. HostAddress64 checks both, but establishes no DMA capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlignmentProfile {
    ByteCopy,
    Relative8,
    Relative64,
    HostAddress64,
}
impl AlignmentProfile {
    fn relative(self) -> usize {
        match self {
            Self::ByteCopy => 1,
            Self::Relative8 => 8,
            Self::Relative64 | Self::HostAddress64 => 64,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_body_bytes: usize,
    pub max_rows: usize,
    pub max_work: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_body_bytes: 16 * 1024 * 1024,
            max_rows: 1024 * 1024,
            max_work: 16 * 1024 * 1024,
        }
    }
}
fn nonnegative(n: i64) -> Result<usize> {
    let n = u64::try_from(n).map_err(|_| Error::new(ErrorKind::Descriptor, 0))?;
    usize::try_from(n).map_err(|_| Error::new(ErrorKind::Overflow, 0))
}
/// Descriptor arithmetic helper; it cannot publish a validated column.
pub fn checked_buffer_range(
    total_body_bytes: u64,
    descriptor: BufferDesc,
    alignment: usize,
) -> Result<Range<usize>> {
    if !alignment.is_power_of_two() {
        return Err(Error::new(ErrorKind::Alignment, 0));
    }
    let offset = nonnegative(descriptor.offset_bytes)?;
    let len = nonnegative(descriptor.byte_length)?;
    if !offset.is_multiple_of(alignment) {
        return Err(Error::new(ErrorKind::Alignment, offset));
    }
    let end = add(offset, len)?;
    let end_u64 = u64::try_from(end).map_err(|_| Error::new(ErrorKind::Overflow, offset))?;
    if end_u64 > total_body_bytes {
        return Err(Error::new(ErrorKind::Range, offset));
    }
    Ok(offset..end)
}

#[derive(Clone, Copy, Debug)]
pub struct I64Column<'a> {
    values: &'a [u8],
    validity: Option<&'a [u8]>,
    rows: usize,
    nulls: usize,
}
impl<'a> I64Column<'a> {
    /// Requires known null count (negative/unknown counts unsupported). Values
    /// length is exactly rows*8. A present bitmap may have padding bytes/bits;
    /// only row bits count. Empty bitmap is permitted iff null_count is zero.
    pub fn validate(
        body: &'a [u8],
        node: FieldNode,
        validity: BufferDesc,
        values: BufferDesc,
        profile: AlignmentProfile,
        limits: Limits,
    ) -> Result<Self> {
        bound(body.len(), limits.max_body_bytes, ErrorKind::InputLimit)?;
        let rows = nonnegative(node.row_length)?;
        let nulls = nonnegative(node.null_count)?;
        if nulls > rows {
            return Err(Error::new(ErrorKind::NullCount, 0));
        }
        bound(rows, limits.max_rows, ErrorKind::CountLimit)?;
        let required = mul(rows, 8)?;
        let total = u64::try_from(body.len()).map_err(|_| Error::new(ErrorKind::Overflow, 0))?;
        let value_range = checked_buffer_range(total, values, profile.relative())?;
        let bitmap_range = checked_buffer_range(total, validity, profile.relative())?;
        if value_range.len() != required {
            return Err(Error::new(ErrorKind::Range, value_range.start));
        }
        if !value_range.is_empty()
            && !bitmap_range.is_empty()
            && value_range.start < bitmap_range.end
            && bitmap_range.start < value_range.end
        {
            return Err(Error::new(ErrorKind::Aliasing, bitmap_range.start));
        }
        if profile == AlignmentProfile::HostAddress64 {
            for range in [&value_range, &bitmap_range] {
                if !range.is_empty()
                    && !add(body.as_ptr() as usize, range.start)?.is_multiple_of(64)
                {
                    return Err(Error::new(ErrorKind::Alignment, range.start));
                }
            }
        }
        let mut work = limits.max_work;
        charge(&mut work, rows)?;
        let bitmap = if bitmap_range.is_empty() {
            if nulls != 0 {
                return Err(Error::new(ErrorKind::Bitmap, bitmap_range.start));
            }
            None
        } else {
            let minimum = add(rows / 8, usize::from(!rows.is_multiple_of(8)))?;
            if bitmap_range.len() < minimum {
                return Err(Error::new(ErrorKind::Bitmap, bitmap_range.start));
            }
            let bytes = &body[bitmap_range];
            let mut actual_nulls = 0usize;
            for row in 0..rows {
                if bytes[row / 8] & (1 << (row % 8)) == 0 {
                    actual_nulls += 1;
                }
            }
            if actual_nulls != nulls {
                return Err(Error::new(ErrorKind::NullCount, 0));
            }
            Some(bytes)
        };
        Ok(Self {
            values: &body[value_range],
            validity: bitmap,
            rows,
            nulls,
        })
    }
    pub fn len(self) -> usize {
        self.rows
    }
    pub fn is_empty(self) -> bool {
        self.rows == 0
    }
    pub fn null_count(self) -> usize {
        self.nulls
    }
    pub fn values_bytes(self) -> &'a [u8] {
        self.values
    }
    pub fn validity_bytes(self) -> Option<&'a [u8]> {
        self.validity
    }
    pub fn get(self, row: usize) -> Option<Option<i64>> {
        if row >= self.rows {
            return None;
        }
        if self
            .validity
            .is_some_and(|bits| bits[row / 8] & (1 << (row % 8)) == 0)
        {
            return Some(None);
        }
        let offset = row * 8;
        Some(Some(i64::from_le_bytes(
            self.values[offset..offset + 8]
                .try_into()
                .expect("validated i64 bytes"),
        )))
    }
    /// Sequential checked arithmetic; intermediate overflow fails even if a later
    /// value could cancel it. Empty/all-null columns produce None, not zero.
    pub fn checked_sum(self, max_work: usize) -> Result<Option<i64>> {
        bound(self.rows, max_work, ErrorKind::WorkLimit)?;
        let mut sum = 0i64;
        let mut any = false;
        for row in 0..self.rows {
            if let Some(value) = self.get(row).expect("row in bounds") {
                any = true;
                sum = sum
                    .checked_add(value)
                    .ok_or(Error::new(ErrorKind::Overflow, row * 8))?;
            }
        }
        Ok(any.then_some(sum))
    }
}
