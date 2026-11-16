//! Bounded zero-copy reader for Cap'n Proto's unpacked stream framing.
//!
//! This validates the segment table and byte extents only. It does not decode
//! pointers, validate schemas, unpack compressed streams, or claim canonical
//! message contents. Segment and remainder slices borrow the original input.

use crate::{Error, ErrorKind, Result, add, bound, mul};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub max_input_bytes: usize,
    pub max_segments: usize,
    pub max_words: u64,
    /// Number of 32-bit count/table entries read during validation.
    pub max_work: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 1024 * 1024,
            max_segments: 512,
            max_words: 131_072,
            max_work: 513,
        }
    }
}

/// A validated segment table and its borrowed data, without pointer decoding.
#[derive(Clone, Copy, Debug)]
pub struct Message<'a> {
    table: &'a [u8],
    payload: &'a [u8],
    count: usize,
}
impl<'a> Message<'a> {
    pub fn segment_count(self) -> usize {
        self.count
    }
    /// Segments in table order, each borrowing its exact bytes from the input.
    pub fn segments(self) -> Segments<'a> {
        Segments {
            table: self.table,
            payload: self.payload,
            index: 0,
            offset: 0,
            count: self.count,
        }
    }

    /// Decode the root pointer when it is a local struct pointer or null.
    pub fn root_struct(self) -> core::result::Result<Option<StructView<'a>>, PointerError> {
        // Stream framing always contains at least one segment.
        local_struct_pointer(self.segments().next().expect("validated segment count"), 0)
    }

    /// Decode the root pointer when it is a supported local list or null.
    pub fn root_list(self) -> core::result::Result<Option<ListView<'a>>, PointerError> {
        local_list_pointer(self.segments().next().expect("validated segment count"), 0)
    }
}

/// A shallow view of a local struct's two word-aligned sections.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StructView<'a> {
    pub data: &'a [u8],
    /// Raw pointer words; no pointers inside this section have been validated.
    pub pointers: &'a [u8],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PointerError {
    MisalignedSegment,
    Overflow,
    PointerOutOfBounds,
    UnsupportedKind(u8),
    UnsupportedCompositeList,
    TargetOutOfBounds,
}

/// Decode a single local struct pointer in a segment; do not follow children.
///
/// This checks only this pointer's type and section extents. It does not
/// validate aliases, cycles, far pointers, schemas, or canonical encoding.
pub fn local_struct_pointer(
    segment: &[u8],
    pointer_word: usize,
) -> core::result::Result<Option<StructView<'_>>, PointerError> {
    let Some((raw, target_byte)) = local_pointer(segment, pointer_word, 0)? else {
        return Ok(None);
    };
    let data_bytes = usize::from(((raw >> 32) & 0xffff) as u16)
        .checked_mul(8)
        .ok_or(PointerError::Overflow)?;
    let pointer_bytes = usize::from(((raw >> 48) & 0xffff) as u16)
        .checked_mul(8)
        .ok_or(PointerError::Overflow)?;
    let data_end = target_byte
        .checked_add(data_bytes)
        .ok_or(PointerError::Overflow)?;
    let pointer_end = data_end
        .checked_add(pointer_bytes)
        .ok_or(PointerError::Overflow)?;
    let data = segment
        .get(target_byte..data_end)
        .ok_or(PointerError::TargetOutOfBounds)?;
    let pointers = segment
        .get(data_end..pointer_end)
        .ok_or(PointerError::TargetOutOfBounds)?;
    Ok(Some(StructView { data, pointers }))
}

/// A non-composite list element's wire representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListKind {
    Void,
    Bit,
    Byte,
    TwoBytes,
    FourBytes,
    EightBytes,
    Pointer,
}

/// A shallow view of a local list, excluding word padding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ListView<'a> {
    pub kind: ListKind,
    pub element_count: u32,
    /// For bit lists the last byte may contain unused bits. Pointer words are
    /// raw and have not been validated or traversed.
    pub data: &'a [u8],
}

/// Decode one local primitive-list pointer without traversing its elements.
pub fn local_list_pointer(
    segment: &[u8],
    pointer_word: usize,
) -> core::result::Result<Option<ListView<'_>>, PointerError> {
    let Some((raw, target_byte)) = local_pointer(segment, pointer_word, 1)? else {
        return Ok(None);
    };
    let (kind, bits) = match (raw >> 32) & 7 {
        0 => (ListKind::Void, 0u64),
        1 => (ListKind::Bit, 1),
        2 => (ListKind::Byte, 8),
        3 => (ListKind::TwoBytes, 16),
        4 => (ListKind::FourBytes, 32),
        5 => (ListKind::EightBytes, 64),
        6 => (ListKind::Pointer, 64),
        _ => return Err(PointerError::UnsupportedCompositeList),
    };
    let count = (raw >> 35) as u32;
    // The 29-bit count and 64-bit element maximum fit in u64.
    let bit_count = u64::from(count) * bits;
    let logical_bytes =
        usize::try_from(bit_count.div_ceil(8)).map_err(|_| PointerError::Overflow)?;
    let padded_bytes =
        usize::try_from(bit_count.div_ceil(64) * 8).map_err(|_| PointerError::Overflow)?;
    let padded_end = target_byte
        .checked_add(padded_bytes)
        .ok_or(PointerError::Overflow)?;
    segment
        .get(target_byte..padded_end)
        .ok_or(PointerError::TargetOutOfBounds)?;
    let logical_end = target_byte
        .checked_add(logical_bytes)
        .ok_or(PointerError::Overflow)?;
    let data = segment
        .get(target_byte..logical_end)
        .ok_or(PointerError::TargetOutOfBounds)?;
    Ok(Some(ListView {
        kind,
        element_count: count,
        data,
    }))
}

fn local_pointer(
    segment: &[u8],
    pointer_word: usize,
    expected_kind: u8,
) -> core::result::Result<Option<(u64, usize)>, PointerError> {
    if !segment.len().is_multiple_of(8) {
        return Err(PointerError::MisalignedSegment);
    }
    let start = pointer_word.checked_mul(8).ok_or(PointerError::Overflow)?;
    let end = start.checked_add(8).ok_or(PointerError::Overflow)?;
    let bytes = segment
        .get(start..end)
        .ok_or(PointerError::PointerOutOfBounds)?;
    let raw = u64::from_le_bytes(bytes.try_into().expect("eight-byte pointer"));
    if raw == 0 {
        return Ok(None);
    }
    let kind = (raw & 3) as u8;
    if kind != expected_kind {
        return Err(PointerError::UnsupportedKind(kind));
    }
    // Sign-extend the 30-bit displacement from the word after the pointer.
    let offset = (((raw >> 2) & 0x3fff_ffff) as i32) << 2 >> 2;
    let base = pointer_word.checked_add(1).ok_or(PointerError::Overflow)?;
    let distance = usize::try_from(offset.unsigned_abs()).map_err(|_| PointerError::Overflow)?;
    let target = if offset < 0 {
        base.checked_sub(distance)
    } else {
        base.checked_add(distance)
    }
    .ok_or(PointerError::TargetOutOfBounds)?;
    let target_byte = target.checked_mul(8).ok_or(PointerError::Overflow)?;
    Ok(Some((raw, target_byte)))
}

pub struct Segments<'a> {
    table: &'a [u8],
    payload: &'a [u8],
    index: usize,
    offset: usize,
    count: usize,
}
impl<'a> Iterator for Segments<'a> {
    type Item = &'a [u8];
    fn next(&mut self) -> Option<Self::Item> {
        if self.index == self.count {
            return None;
        }
        let start = self.index * 4;
        let words = u32::from_le_bytes(
            self.table[start..start + 4]
                .try_into()
                .expect("validated table"),
        );
        let len = (words as usize) * 8; // The total byte count was checked at parse time.
        let end = self.offset + len;
        let result = &self.payload[self.offset..end];
        self.offset = end;
        self.index += 1;
        Some(result)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = self.count - self.index;
        (left, Some(left))
    }
}
impl ExactSizeIterator for Segments<'_> {}

/// Parse one unpacked stream message, returning the exact unconsumed suffix.
/// The framing padding is ignored as specified; it is not a canonicality check.
pub fn parse_prefix(input: &[u8], limits: Limits) -> Result<(Message<'_>, &[u8])> {
    bound(input.len(), limits.max_input_bytes, ErrorKind::InputLimit)?;
    bound(1, limits.max_work, ErrorKind::WorkLimit)?;
    let count_bytes = input
        .get(..4)
        .ok_or(Error::new(ErrorKind::UnexpectedEof, input.len()))?;
    let minus_one = u32::from_le_bytes(count_bytes.try_into().expect("four bytes"));
    let count = usize::try_from(u64::from(minus_one) + 1)
        .map_err(|_| Error::new(ErrorKind::Overflow, 0))?;
    bound(count, limits.max_segments, ErrorKind::CountLimit)?;
    bound(add(1, count)?, limits.max_work, ErrorKind::WorkLimit)?;
    let table_end = add(4, mul(count, 4)?)?;
    let header_end = add(table_end, if count.is_multiple_of(2) { 4 } else { 0 })?;
    let header = input
        .get(..header_end)
        .ok_or(Error::new(ErrorKind::UnexpectedEof, input.len()))?;
    let table = &header[4..table_end];
    let mut words = 0u64;
    for &entry in table.as_chunks::<4>().0 {
        let size = u32::from_le_bytes(entry);
        words = words
            .checked_add(u64::from(size))
            .ok_or(Error::new(ErrorKind::Overflow, 0))?;
        if words > limits.max_words {
            return Err(Error::new(ErrorKind::PayloadLimit, 0));
        }
    }
    let bytes = usize::try_from(words).map_err(|_| Error::new(ErrorKind::Overflow, 0))?;
    let end = add(header_end, mul(bytes, 8)?)?;
    let payload = input
        .get(header_end..end)
        .ok_or(Error::new(ErrorKind::UnexpectedEof, input.len()))?;
    Ok((
        Message {
            table,
            payload,
            count,
        },
        &input[end..],
    ))
}

/// Parse one complete unpacked stream message with no trailing data.
pub fn parse_exact(input: &[u8], limits: Limits) -> Result<Message<'_>> {
    let (message, rest) = parse_prefix(input, limits)?;
    if !rest.is_empty() {
        return Err(Error::new(
            ErrorKind::TrailingBytes,
            input.len() - rest.len(),
        ));
    }
    Ok(message)
}
