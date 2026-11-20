//! Canonical unsigned LEB128 and bounded opaque length-prefixed frames.
//! Prefix parsing explicitly returns a recoverable nonclone remainder owner.
//! Rust still permits dropping/forgetting it: no source linearity is claimed.
#[cfg(vesper_port)]
use crate::port;
use crate::{Error, ErrorKind, Result, add, bound, charge};

/// Unsigned LEB128: seven payload bits per byte, bit 8 set while more bytes
/// follow; a `u64` takes at most ten bytes, the last holding one bit.
pub(crate) const LEB128_MAX_BYTES: usize = 10;
pub(crate) const LEB128_PAYLOAD: u8 = 0x7f;
pub(crate) const LEB128_MORE: u8 = 0x80;
pub(crate) const LEB128_SHIFT: u32 = 7;

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
            max_output_bytes: 1024 * 1024 + LEB128_MAX_BYTES,
            max_work: 4 * 1024 * 1024,
        }
    }
}

/// Prefix decoder: at most ten bytes inspected; returns consumed length. This
/// alone does not assert whole-input canonical uniqueness or exact EOF.
#[cfg(not(vesper_port))]
pub fn decode_canonical_u64_leb128(input: &[u8]) -> Result<(u64, usize)> {
    let mut remaining = LEB128_MAX_BYTES;
    decode(input, &mut remaining)
}
#[cfg(vesper_port)]
pub fn decode_canonical_u64_leb128(input: &[u8]) -> Result<(u64, usize)> {
    let p = port::vesper::fr_decode(input, LEB128_MAX_BYTES as u64).map_err(port::error)?;
    Ok((p.a, p.b as usize))
}
pub(crate) fn decode(input: &[u8], remaining: &mut usize) -> Result<(u64, usize)> {
    let mut value = 0;
    const LAST: usize = LEB128_MAX_BYTES - 1;
    for i in 0..LEB128_MAX_BYTES {
        charge(remaining, 1)?;
        let byte = *input
            .get(i)
            .ok_or(Error::new(ErrorKind::UnexpectedEof, i))?;
        if i == LAST && byte > 1 {
            return Err(Error::new(ErrorKind::Overflow, i));
        }
        value |= u64::from(byte & LEB128_PAYLOAD) << (i as u32 * LEB128_SHIFT);
        if byte & LEB128_MORE == 0 {
            if i != 0 && byte == 0 {
                return Err(Error::new(ErrorKind::NonCanonical, i));
            }
            return Ok((value, i + 1));
        }
    }
    Err(Error::new(ErrorKind::Overflow, LAST))
}
pub const fn encoded_u64_len(mut value: u64) -> usize {
    let mut count = 1;
    while value >= LEB128_MORE as u64 {
        value >>= LEB128_SHIFT;
        count += 1;
    }
    count
}
#[cfg(not(vesper_port))]
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
#[cfg(vesper_port)]
pub fn encode_u64_into(value: u64, output: &mut [u8], limits: Limits) -> Result<usize> {
    let lim = port::framing_limits(limits);
    Ok(port::vesper::fr_encode_u64_into(value, output, &lim).map_err(port::error)? as usize)
}
fn write_u64(mut value: u64, output: &mut [u8]) {
    for byte in output {
        *byte = (value as u8 & LEB128_PAYLOAD)
            | if value >= u64::from(LEB128_MORE) {
                LEB128_MORE
            } else {
                0
            };
        value >>= LEB128_SHIFT;
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
    /// Parse a three-byte big-endian length frame; on failure retain the tail.
    pub fn next_u24_frame(self, limits: Limits) -> core::result::Result<Frame<'a>, TailError<'a>> {
        match u24_frame_parts(self.remaining_bytes(), limits) {
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
#[cfg(not(vesper_port))]
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
#[cfg(vesper_port)]
fn frame_parts(input: &[u8], limits: Limits) -> Result<(&[u8], usize)> {
    let lim = port::framing_limits(limits);
    let p = port::vesper::fr_frame_parts(input, &lim).map_err(port::error)?;
    Ok((&input[p.a as usize..p.b as usize], p.b as usize))
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

/// Largest payload representable in a three-byte unsigned length.
pub const MAX_U24_PAYLOAD: usize = 0x00ff_ffff;
/// Bytes in a u24 frame's big-endian length prefix.
pub(crate) const U24_HEADER_BYTES: usize = 3;

#[cfg(not(vesper_port))]
fn u24_frame_parts(input: &[u8], limits: Limits) -> Result<(&[u8], usize)> {
    bound(input.len(), limits.max_input_bytes, ErrorKind::InputLimit)?;
    let mut work = limits.max_work;
    let mut len = 0usize;
    for i in 0..U24_HEADER_BYTES {
        charge(&mut work, 1)?;
        let byte = *input
            .get(i)
            .ok_or(Error::new(ErrorKind::UnexpectedEof, i))?;
        len = (len << 8) | usize::from(byte);
    }
    bound(len, limits.max_payload_bytes, ErrorKind::PayloadLimit)?;
    let end = add(U24_HEADER_BYTES, len)?;
    let payload = input
        .get(U24_HEADER_BYTES..end)
        .ok_or(Error::new(ErrorKind::UnexpectedEof, input.len()))?;
    charge(&mut work, len)?;
    Ok((payload, end))
}
#[cfg(vesper_port)]
fn u24_frame_parts(input: &[u8], limits: Limits) -> Result<(&[u8], usize)> {
    let lim = port::framing_limits(limits);
    let p = port::vesper::fr_u24_frame_parts(input, &lim).map_err(port::error)?;
    Ok((&input[p.a as usize..p.b as usize], p.b as usize))
}

/// Parse one opaque frame with a three-byte big-endian payload length.
pub fn parse_u24_frame(input: &[u8], limits: Limits) -> Result<Frame<'_>> {
    let (payload, end) = u24_frame_parts(input, limits)?;
    Ok(Frame {
        payload,
        tail: Remainder::at(input, end),
    })
}

/// Parse one frame and require that no bytes follow it.
pub fn parse_u24_frame_exact(input: &[u8], limits: Limits) -> Result<&[u8]> {
    let frame = parse_u24_frame(input, limits)?;
    frame.tail.assert_exact_eof().map_err(|e| e.error)?;
    Ok(frame.payload)
}

/// Encode a frame transactionally, preserving the output on every error.
#[cfg(not(vesper_port))]
pub fn encode_u24_frame_into(payload: &[u8], output: &mut [u8], limits: Limits) -> Result<usize> {
    if payload.len() > MAX_U24_PAYLOAD {
        return Err(Error::new(ErrorKind::Overflow, 0));
    }
    bound(
        payload.len(),
        limits.max_payload_bytes,
        ErrorKind::PayloadLimit,
    )?;
    let size = add(U24_HEADER_BYTES, payload.len())?;
    bound(size, limits.max_output_bytes, ErrorKind::OutputLimit)?;
    bound(size, limits.max_work, ErrorKind::WorkLimit)?;
    if output.len() < size {
        return Err(Error::new(ErrorKind::OutputTooSmall, 0));
    }
    let length = payload.len() as u32;
    output[..U24_HEADER_BYTES].copy_from_slice(&length.to_be_bytes()[1..]);
    output[U24_HEADER_BYTES..size].copy_from_slice(payload);
    Ok(size)
}
#[cfg(vesper_port)]
pub fn encode_u24_frame_into(payload: &[u8], output: &mut [u8], limits: Limits) -> Result<usize> {
    let lim = port::framing_limits(limits);
    let n = port::vesper::fr_encode_u24_frame_into(payload, output, &lim).map_err(port::error)?;
    Ok(n as usize)
}
#[cfg(not(vesper_port))]
pub fn parse_u64(input: &[u8], limits: Limits) -> Result<(u64, Remainder<'_>)> {
    bound(input.len(), limits.max_input_bytes, ErrorKind::InputLimit)?;
    let mut work = limits.max_work;
    let (value, count) = decode(input, &mut work)?;
    Ok((value, Remainder::at(input, count)))
}
#[cfg(vesper_port)]
pub fn parse_u64(input: &[u8], limits: Limits) -> Result<(u64, Remainder<'_>)> {
    let lim = port::framing_limits(limits);
    let p = port::vesper::fr_parse_u64(input, &lim).map_err(port::error)?;
    Ok((p.a, Remainder::at(input, p.b as usize)))
}
pub fn parse_u64_exact(input: &[u8], limits: Limits) -> Result<u64> {
    let (value, tail) = parse_u64(input, limits)?;
    tail.assert_exact_eof().map_err(|e| e.error)?;
    Ok(value)
}
#[cfg(not(vesper_port))]
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
#[cfg(vesper_port)]
pub fn frame_encoded_len(payload: &[u8], limits: Limits) -> Result<usize> {
    let lim = port::framing_limits(limits);
    let n = port::vesper::fr_frame_encoded_len(payload.len() as u64, &lim).map_err(port::error)?;
    Ok(n as usize)
}
#[cfg(not(vesper_port))]
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
#[cfg(vesper_port)]
pub fn encode_frame_into(payload: &[u8], output: &mut [u8], limits: Limits) -> Result<usize> {
    let lim = port::framing_limits(limits);
    let n = port::vesper::fr_encode_frame_into(payload, output, &lim).map_err(port::error)?;
    Ok(n as usize)
}
