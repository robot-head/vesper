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
