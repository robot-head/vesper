//! Canonical unsigned LEB128 and length-LEB128 + opaque byte payload frames.
//! Prefix parsing explicitly returns a recoverable nonclone remainder owner.
//! Rust still permits dropping/forgetting it: no source linearity is claimed.
use crate::{Error, ErrorKind, Result, add, bound, charge};

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_input_bytes: usize,
    pub max_payload_bytes: usize,
    pub max_output_bytes: usize,
    pub max_work: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 1024 * 1024,
            max_payload_bytes: 1024 * 1024,
            max_output_bytes: 1024 * 1024 + 10,
            max_work: 4 * 1024 * 1024,
        }
    }
}

/// Prefix decoder: at most ten bytes inspected; returns consumed length. This
/// alone does not assert whole-input canonical uniqueness or exact EOF.
pub fn decode_canonical_u64_leb128(input: &[u8]) -> Result<(u64, usize)> {
    let mut remaining = 10;
    decode(input, &mut remaining)
}
pub(crate) fn decode(input: &[u8], remaining: &mut usize) -> Result<(u64, usize)> {
    let mut value = 0;
    for i in 0..10 {
        charge(remaining, 1)?;
        let byte = *input
            .get(i)
            .ok_or(Error::new(ErrorKind::UnexpectedEof, i))?;
        if i == 9 && byte > 1 {
            return Err(Error::new(ErrorKind::Overflow, i));
        }
        value |= u64::from(byte & 127) << (i * 7);
        if byte & 128 == 0 {
            if i != 0 && byte == 0 {
                return Err(Error::new(ErrorKind::NonCanonical, i));
            }
            return Ok((value, i + 1));
        }
    }
    Err(Error::new(ErrorKind::Overflow, 9))
}
pub const fn encoded_u64_len(mut value: u64) -> usize {
    let mut count = 1;
    while value >= 128 {
        value >>= 7;
        count += 1;
    }
    count
}
pub fn encode_u64_into(value: u64, output: &mut [u8], limits: Limits) -> Result<usize> {
    let len = encoded_u64_len(value);
    bound(len, limits.max_output_bytes, ErrorKind::OutputLimit)?;
    bound(len, limits.max_work, ErrorKind::WorkLimit)?;
    if output.len() < len {
        return Err(Error::new(ErrorKind::OutputTooSmall, 0));
    }
    write_u64(value, &mut output[..len]);
    Ok(len)
}
fn write_u64(mut value: u64, output: &mut [u8]) {
    for byte in output {
        *byte = (value as u8 & 127) | if value >= 128 { 128 } else { 0 };
        value >>= 7;
    }
}

/// Explicit remainder owner; safe callers cannot clone the token.
/// ```compile_fail
/// use vesper_grammar::framing::Remainder;
/// fn duplicate(tail: Remainder<'_>) { let _copy = tail.clone(); }
/// ```
#[must_use = "explicitly check EOF or parse the next frame"]
#[derive(Debug)]
pub struct Remainder<'a> {
    input: &'a [u8],
    offset: usize,
}
#[derive(Debug)]
pub struct TailError<'a> {
    pub error: Error,
    pub tail: Remainder<'a>,
}
impl core::fmt::Display for TailError<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.error.fmt(f)
    }
}
impl std::error::Error for TailError<'_> {}
impl<'a> Remainder<'a> {
    pub(crate) fn at(input: &'a [u8], offset: usize) -> Self {
        Self { input, offset }
    }
    pub fn remaining_bytes(&self) -> &'a [u8] {
        &self.input[self.offset..]
    }
    pub fn offset(&self) -> usize {
        self.offset
    }
    pub fn assert_exact_eof(self) -> core::result::Result<(), TailError<'a>> {
        if self.offset == self.input.len() {
            Ok(())
        } else {
            Err(TailError {
                error: Error::new(ErrorKind::TrailingBytes, self.offset),
                tail: self,
            })
        }
    }
    /// Failure returns the original remainder owner and position for retry.
    pub fn next_frame(self, limits: Limits) -> core::result::Result<Frame<'a>, TailError<'a>> {
        match frame_parts(self.remaining_bytes(), limits) {
            Ok((payload, consumed)) => Ok(Frame {
                payload,
                tail: Self::at(self.input, self.offset + consumed),
            }),
            Err(mut error) => {
                error.offset += self.offset;
                Err(TailError { error, tail: self })
            }
        }
    }
}
#[derive(Debug)]
pub struct Frame<'a> {
    pub payload: &'a [u8],
    pub tail: Remainder<'a>,
}
fn frame_parts(input: &[u8], limits: Limits) -> Result<(&[u8], usize)> {
    bound(input.len(), limits.max_input_bytes, ErrorKind::InputLimit)?;
    let mut work = limits.max_work;
    let (len, prefix) = decode(input, &mut work)?;
    let len = usize::try_from(len).map_err(|_| Error::new(ErrorKind::Overflow, 0))?;
    bound(len, limits.max_payload_bytes, ErrorKind::PayloadLimit)?;
    let end = add(prefix, len)?;
    let payload = input
        .get(prefix..end)
        .ok_or(Error::new(ErrorKind::UnexpectedEof, input.len()))?;
    charge(&mut work, len)?;
    Ok((payload, end))
}
pub fn parse_frame(input: &[u8], limits: Limits) -> Result<Frame<'_>> {
    let (payload, end) = frame_parts(input, limits)?;
    Ok(Frame {
        payload,
        tail: Remainder::at(input, end),
    })
}
pub fn parse_frame_exact(input: &[u8], limits: Limits) -> Result<&[u8]> {
    let frame = parse_frame(input, limits)?;
    frame.tail.assert_exact_eof().map_err(|e| e.error)?;
    Ok(frame.payload)
}
pub fn parse_u64(input: &[u8], limits: Limits) -> Result<(u64, Remainder<'_>)> {
    bound(input.len(), limits.max_input_bytes, ErrorKind::InputLimit)?;
    let mut work = limits.max_work;
    let (value, count) = decode(input, &mut work)?;
    Ok((value, Remainder::at(input, count)))
}
pub fn parse_u64_exact(input: &[u8], limits: Limits) -> Result<u64> {
    let (value, tail) = parse_u64(input, limits)?;
    tail.assert_exact_eof().map_err(|e| e.error)?;
    Ok(value)
}
pub fn frame_encoded_len(payload: &[u8], limits: Limits) -> Result<usize> {
    bound(
        payload.len(),
        limits.max_payload_bytes,
        ErrorKind::PayloadLimit,
    )?;
    let len = u64::try_from(payload.len()).map_err(|_| Error::new(ErrorKind::Overflow, 0))?;
    let size = add(encoded_u64_len(len), payload.len())?;
    bound(size, limits.max_output_bytes, ErrorKind::OutputLimit)?;
    bound(size, limits.max_work, ErrorKind::WorkLimit)?;
    Ok(size)
}
pub fn encode_frame_into(payload: &[u8], output: &mut [u8], limits: Limits) -> Result<usize> {
    let size = frame_encoded_len(payload, limits)?;
    if output.len() < size {
        return Err(Error::new(ErrorKind::OutputTooSmall, 0));
    }
    let prefix = size - payload.len();
    write_u64(payload.len() as u64, &mut output[..prefix]);
    output[prefix..size].copy_from_slice(payload);
    Ok(size)
}
