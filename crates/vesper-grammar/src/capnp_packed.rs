//! Bounded Cap'n Proto word packing, independent of message framing and schemas.
//!
//! [`pack_into`] chooses deterministic zero and all-nonzero runs; valid packed
//! streams can have other representations. [`unpack_into`] accepts those
//! alternatives, including zero bytes in raw runs, and consumes the complete
//! supplied slice. A packed stream has no intrinsic message boundary: callers
//! must supply one separately. Neither operation validates Cap'n Proto message
//! framing, pointers, canonicality, or authenticity.
//!
//! Both operations preflight the complete input, output capacity and logical
//! budgets before writing. Every error preserves the entire output buffer;
//! successful writes leave its suffix unchanged. No allocation is performed.
//! Work charges `4 * input_bytes + output_bytes` for packing and
//! `2 * input_bytes + output_bytes` for unpacking, independently per call.
//!
//! ```
//! use vesper_grammar::capnp_packed::{pack_into, unpack_into, Limits};
//! let word = [8, 0, 0, 0, 3, 0, 2, 0];
//! let mut packed = [0; 16];
//! let n = pack_into(&word, &mut packed, Limits::default())?;
//! assert_eq!(&packed[..n], &[0x51, 8, 3, 2]);
//! let mut decoded = [0; 8];
//! assert_eq!(unpack_into(&packed[..n], &mut decoded, Limits::default())?, 8);
//! assert_eq!(decoded, word);
//! # Ok::<(), vesper_grammar::capnp_packed::Error>(())
//! ```

use core::fmt;

#[cfg(vesper_port)]
use crate::port;

/// Cap'n Proto packing (encoding spec, "Packing"): input is 8-byte words; a
/// tag byte marks a word's nonzero bytes. Tag `ZERO_TAG` is followed by a
/// count of further all-zero words and `LITERAL_TAG` by a count of words
/// copied verbatim; a run holds at most `MAX_RUN` further words.
const WORD_BYTES: usize = 8;
const ZERO_TAG: u8 = 0;
const LITERAL_TAG: u8 = 0xff;
const MAX_RUN: usize = 255;
/// A run's tag byte and its count byte.
#[cfg_attr(vesper_port, allow(dead_code))]
const RUN_HEADER_BYTES: usize = 2;
const HARD_BYTES: usize = 64 * 1024 * 1024;
const HARD_WORDS: usize = HARD_BYTES / WORD_BYTES;
const HARD_WORK: usize = 512 * 1024 * 1024;

/// Logical per-call limits, not process memory, CPU time, or caller allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_input_bytes: usize,
    pub max_output_bytes: usize,
    pub max_words: usize,
    pub max_work: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 16 * 1024 * 1024,
            max_output_bytes: 16 * 1024 * 1024,
            max_words: 2 * 1024 * 1024,
            max_work: 128 * 1024 * 1024,
        }
    }
}
impl Limits {
    fn validate(self) -> Result<()> {
        if self.max_input_bytes > HARD_BYTES
            || self.max_output_bytes > HARD_BYTES
            || self.max_words > HARD_WORDS
            || self.max_work > HARD_WORK
        {
            return Err(Error::new(ErrorKind::InvalidLimits, 0));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    InvalidLimits,
    InputLimit,
    OutputLimit,
    WordLimit,
    WorkLimit,
    Overflow,
    UnalignedInput,
    Truncated,
    OutputTooSmall,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    /// Packed-input end offset for truncation, input length for unaligned
    /// unpacked input, and zero for direct limits and capacity errors.
    pub offset: usize,
}
impl Error {
    const fn new(kind: ErrorKind, offset: usize) -> Self {
        Self { kind, offset }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "packed Cap'n Proto {:?} at byte {}",
            self.kind, self.offset
        )
    }
}
impl std::error::Error for Error {}
pub type Result<T> = core::result::Result<T, Error>;

fn add(a: usize, b: usize) -> Result<usize> {
    a.checked_add(b).ok_or(Error::new(ErrorKind::Overflow, 0))
}
fn mul(a: usize, b: usize) -> Result<usize> {
    a.checked_mul(b).ok_or(Error::new(ErrorKind::Overflow, 0))
}
fn bound(value: usize, max: usize, kind: ErrorKind) -> Result<()> {
    if value > max {
        Err(Error::new(kind, 0))
    } else {
        Ok(())
    }
}
fn input_preflight(input_len: usize, limits: Limits, passes: usize) -> Result<()> {
    limits.validate()?;
    bound(input_len, limits.max_input_bytes, ErrorKind::InputLimit)?;
    // Prepay the complete input scan before inspecting any token.
    bound(
        mul(input_len, passes)?,
        limits.max_work,
        ErrorKind::WorkLimit,
    )
}
fn work(input_len: usize, output_len: usize, passes: usize, limits: Limits) -> Result<()> {
    bound(
        add(mul(input_len, passes)?, output_len)?,
        limits.max_work,
        ErrorKind::WorkLimit,
    )
}
fn take(input: &[u8], pos: &mut usize, count: usize) -> Result<()> {
    let end = add(*pos, count)?;
    if end > input.len() {
        return Err(Error::new(ErrorKind::Truncated, input.len()));
    }
    *pos = end;
    Ok(())
}

/// Validates a complete packed byte sequence and returns its unpacked length.
/// This is a word codec only: even an empty sequence is accepted and is not
/// asserted to be a complete or valid Cap'n Proto message.
#[cfg(not(vesper_port))]
pub fn unpacked_len(input: &[u8], limits: Limits) -> Result<usize> {
    input_preflight(input.len(), limits, 2)?;
    let mut pos = 0usize;
    let mut words = 0usize;
    while pos < input.len() {
        let tag = input[pos];
        pos += 1;
        take(input, &mut pos, tag.count_ones() as usize)?;
        let mut group_words = 1usize;
        if tag == ZERO_TAG || tag == LITERAL_TAG {
            take(input, &mut pos, 1)?;
            let additional = usize::from(input[pos - 1]);
            group_words = add(group_words, additional)?;
            if tag == LITERAL_TAG {
                take(input, &mut pos, mul(additional, WORD_BYTES)?)?;
            }
        }
        words = add(words, group_words)?;
        bound(words, limits.max_words, ErrorKind::WordLimit)?;
        bound(
            mul(words, WORD_BYTES)?,
            limits.max_output_bytes,
            ErrorKind::OutputLimit,
        )?;
    }
    let size = mul(words, WORD_BYTES)?;
    work(input.len(), size, 2, limits)?;
    Ok(size)
}
#[cfg(vesper_port)]
pub fn unpacked_len(input: &[u8], limits: Limits) -> Result<usize> {
    Ok(
        port::vesper::pk_unpacked_len(input, &packed_limits(limits)).map_err(packed_error)?
            as usize,
    )
}
#[cfg(vesper_port)]
fn packed_limits(l: Limits) -> [u64; 4] {
    [
        l.max_input_bytes as u64,
        l.max_output_bytes as u64,
        l.max_words as u64,
        l.max_work as u64,
    ]
}
#[cfg(vesper_port)]
fn packed_error(e: port::vesper::GErr) -> Error {
    const KINDS: [ErrorKind; 9] = [
        ErrorKind::InvalidLimits,
        ErrorKind::InputLimit,
        ErrorKind::OutputLimit,
        ErrorKind::WordLimit,
        ErrorKind::WorkLimit,
        ErrorKind::Overflow,
        ErrorKind::UnalignedInput,
        ErrorKind::Truncated,
        ErrorKind::OutputTooSmall,
    ];
    Error::new(KINDS[e.kind as usize], e.offset as usize)
}

/// Decode into caller storage after full preflight; all failures preserve it.
#[cfg(not(vesper_port))]
pub fn unpack_into(input: &[u8], output: &mut [u8], limits: Limits) -> Result<usize> {
    let size = unpacked_len(input, limits)?;
    if output.len() < size {
        return Err(Error::new(ErrorKind::OutputTooSmall, 0));
    }
    let mut pos = 0usize;
    let mut written = 0usize;
    while pos < input.len() {
        let tag = input[pos];
        pos += 1;
        let word = &mut output[written..written + WORD_BYTES];
        for (i, byte) in word.iter_mut().enumerate() {
            *byte = if tag & (1 << i) == 0 {
                0
            } else {
                let b = input[pos];
                pos += 1;
                b
            };
        }
        written += WORD_BYTES;
        if tag == ZERO_TAG || tag == LITERAL_TAG {
            let additional = usize::from(input[pos]);
            pos += 1;
            let bytes = additional * WORD_BYTES;
            if tag == ZERO_TAG {
                output[written..written + bytes].fill(0);
            } else {
                output[written..written + bytes].copy_from_slice(&input[pos..pos + bytes]);
                pos += bytes;
            }
            written += bytes;
        }
    }
    debug_assert_eq!(written, size);
    Ok(size)
}
#[cfg(vesper_port)]
pub fn unpack_into(input: &[u8], output: &mut [u8], limits: Limits) -> Result<usize> {
    let n = port::vesper::pk_unpack_into(input, output, &packed_limits(limits))
        .map_err(packed_error)?;
    Ok(n as usize)
}

fn tag(word: &[u8]) -> u8 {
    word.iter()
        .enumerate()
        .fold(0, |bits, (i, b)| bits | (u8::from(*b != 0) << i))
}

/// Returns this group's tag and number of additional words in its run.
fn group(input: &[u8], index: usize) -> (u8, usize) {
    let first = tag(&input[index..index + WORD_BYTES]);
    if first != ZERO_TAG && first != LITERAL_TAG {
        return (first, 0);
    }
    let mut additional = 0;
    while additional < MAX_RUN {
        let next = index + (additional + 1) * WORD_BYTES;
        if next >= input.len() || tag(&input[next..next + WORD_BYTES]) != first {
            break;
        }
        additional += 1;
    }
    (first, additional)
}

/// Returns the encoded size after validating input alignment and budgets.
#[cfg(not(vesper_port))]
pub fn packed_len(input: &[u8], limits: Limits) -> Result<usize> {
    input_preflight(input.len(), limits, 4)?;
    if !input.len().is_multiple_of(WORD_BYTES) {
        return Err(Error::new(ErrorKind::UnalignedInput, input.len()));
    }
    bound(
        input.len() / WORD_BYTES,
        limits.max_words,
        ErrorKind::WordLimit,
    )?;
    let mut index = 0usize;
    let mut size = 0usize;
    while index < input.len() {
        let (tag, additional) = group(input, index);
        let group_size = match tag {
            ZERO_TAG => RUN_HEADER_BYTES,
            LITERAL_TAG => add(RUN_HEADER_BYTES + WORD_BYTES, mul(additional, WORD_BYTES)?)?,
            _ => 1 + tag.count_ones() as usize,
        };
        size = add(size, group_size)?;
        bound(size, limits.max_output_bytes, ErrorKind::OutputLimit)?;
        index += (additional + 1) * WORD_BYTES;
    }
    work(input.len(), size, 4, limits)?;
    Ok(size)
}
#[cfg(vesper_port)]
pub fn packed_len(input: &[u8], limits: Limits) -> Result<usize> {
    Ok(port::vesper::pk_packed_len(input, &packed_limits(limits)).map_err(packed_error)? as usize)
}

/// Encode complete words deterministically after full preflight. This encoder
/// is one legal representation, not a canonical Cap'n Proto encoding.
#[cfg(not(vesper_port))]
pub fn pack_into(input: &[u8], output: &mut [u8], limits: Limits) -> Result<usize> {
    let size = packed_len(input, limits)?;
    if output.len() < size {
        return Err(Error::new(ErrorKind::OutputTooSmall, 0));
    }
    let mut index = 0usize;
    let mut written = 0usize;
    while index < input.len() {
        let (tag, additional) = group(input, index);
        output[written] = tag;
        written += 1;
        for b in &input[index..index + WORD_BYTES] {
            if *b != 0 {
                output[written] = *b;
                written += 1;
            }
        }
        index += WORD_BYTES;
        if tag == ZERO_TAG || tag == LITERAL_TAG {
            output[written] = additional as u8;
            written += 1;
            let bytes = additional * WORD_BYTES;
            if tag == LITERAL_TAG {
                output[written..written + bytes].copy_from_slice(&input[index..index + bytes]);
                written += bytes;
            }
            index += bytes;
        }
    }
    debug_assert_eq!(written, size);
    Ok(size)
}
#[cfg(vesper_port)]
pub fn pack_into(input: &[u8], output: &mut [u8], limits: Limits) -> Result<usize> {
    let n =
        port::vesper::pk_pack_into(input, output, &packed_limits(limits)).map_err(packed_error)?;
    Ok(n as usize)
}
