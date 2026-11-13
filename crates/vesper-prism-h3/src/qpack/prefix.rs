//! Prefixed integers (RFC 7541 §5.1, RFC 9204 §4.1.1) and string literals
//! (RFC 7541 §5.2, RFC 9204 §4.1.2).

use super::huffman;

/// The largest integer accepted: QUIC's varint range, which bounds stream
/// IDs, indices and lengths alike.
pub const MAX_INT: u64 = (1 << 62) - 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrefixError {
    /// More bytes are needed (only an error once the input is complete).
    Incomplete,
    /// The integer exceeds [`MAX_INT`] or uses more continuation bytes than
    /// any value in range needs.
    IntegerOverflow,
    /// A string literal is longer than the caller's limit.
    StringTooLong,
    /// Invalid Huffman data: EOS, over-long or non-1 padding.
    Huffman(huffman::HuffmanError),
}

/// Decodes an integer whose prefix is the low `bits` bits of `buf[0]`.
/// Returns the value and the bytes consumed.
pub fn decode_int(buf: &[u8], bits: u8) -> Result<(u64, usize), PrefixError> {
    debug_assert!((1..=8).contains(&bits));
    let first = *buf.first().ok_or(PrefixError::Incomplete)?;
    let mask = if bits == 8 {
        0xff
    } else {
        (1u16 << bits) as u8 - 1
    };
    let mut value = u64::from(first & mask);
    if value < u64::from(mask) {
        return Ok((value, 1));
    }
    let mut shift = 0u32;
    for (i, &byte) in buf.iter().enumerate().skip(1) {
        // 62-bit values need at most 9 continuation bytes after an 8-bit
        // prefix; anything longer (including zero-padded) is refused.
        if shift > 56 {
            return Err(PrefixError::IntegerOverflow);
        }
        let add = u64::from(byte & 0x7f) << shift;
        value = value
            .checked_add(add)
            .filter(|v| *v <= MAX_INT)
            .ok_or(PrefixError::IntegerOverflow)?;
        if byte & 0x80 == 0 {
            return Ok((value, i + 1));
        }
        shift += 7;
    }
    Err(PrefixError::Incomplete)
}

/// Appends an integer with an `bits`-bit prefix; `flags` fills the bits
/// above the prefix in the first byte.
pub fn encode_int(out: &mut Vec<u8>, flags: u8, bits: u8, mut value: u64) {
    debug_assert!((1..=8).contains(&bits));
    let mask: u64 = if bits == 8 { 0xff } else { (1 << bits) - 1 };
    if value < mask {
        out.push(flags | value as u8);
        return;
    }
    out.push(flags | mask as u8);
    value -= mask;
    while value >= 0x80 {
        out.push(0x80 | (value & 0x7f) as u8);
        value >>= 7;
    }
    out.push(value as u8);
}

/// Decodes a string literal whose Huffman flag is bit `bits` of `buf[0]`
/// and whose length has a `bits`-bit prefix. `max_len` bounds the decoded
/// length (and the encoded one, which is checked before any copying).
pub fn decode_string(
    buf: &[u8],
    bits: u8,
    max_len: usize,
) -> Result<(Vec<u8>, usize), PrefixError> {
    let first = *buf.first().ok_or(PrefixError::Incomplete)?;
    let huff = first & (1 << bits) != 0;
    let (len, used) = decode_int(buf, bits)?;
    // A Huffman string decodes to at most 8/5 of its length; bound the
    // wire length by the limit either way before touching the bytes.
    let len = usize::try_from(len).map_err(|_| PrefixError::StringTooLong)?;
    let limit = if huff {
        max_len.saturating_mul(8).div_ceil(5).max(max_len)
    } else {
        max_len
    };
    if len > limit {
        return Err(PrefixError::StringTooLong);
    }
    let end = used.checked_add(len).ok_or(PrefixError::StringTooLong)?;
    let raw = buf.get(used..end).ok_or(PrefixError::Incomplete)?;
    let value = if huff {
        huffman::decode(raw, max_len).map_err(|e| match e {
            huffman::HuffmanError::TooLong => PrefixError::StringTooLong,
            e => PrefixError::Huffman(e),
        })?
    } else {
        raw.to_vec()
    };
    Ok((value, end))
}

/// Appends a string literal, Huffman-coded when that is strictly shorter.
/// `flags` fills the bits above the H bit.
pub fn encode_string(out: &mut Vec<u8>, flags: u8, bits: u8, value: &[u8]) {
    let huff_len = huffman::encoded_len(value);
    if huff_len < value.len() {
        encode_int(out, flags | 1 << bits, bits, huff_len as u64);
        huffman::encode(value, out);
    } else {
        encode_int(out, flags, bits, value.len() as u64);
        out.extend_from_slice(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc7541_c1_integer_examples() {
        // C.1.1: 10 with a 5-bit prefix.
        let mut out = Vec::new();
        encode_int(&mut out, 0, 5, 10);
        assert_eq!(out, [0x0a]);
        // C.1.2: 1337 with a 5-bit prefix.
        out.clear();
        encode_int(&mut out, 0, 5, 1337);
        assert_eq!(out, [0x1f, 0x9a, 0x0a]);
        assert_eq!(decode_int(&out, 5), Ok((1337, 3)));
        // C.1.3: 42 at an octet boundary.
        out.clear();
        encode_int(&mut out, 0, 8, 42);
        assert_eq!(out, [0x2a]);
    }

    #[test]
    fn integers_round_trip_and_reject_overflow() {
        for bits in 1..=8 {
            for v in [0, 1, 30, 31, 127, 128, 255, 256, 1 << 20, MAX_INT] {
                let mut out = Vec::new();
                encode_int(&mut out, 0, bits, v);
                assert_eq!(decode_int(&out, bits), Ok((v, out.len())), "{bits} {v}");
                for cut in 0..out.len() {
                    assert_eq!(decode_int(&out[..cut], bits), Err(PrefixError::Incomplete));
                }
            }
        }
        let mut out = Vec::new();
        encode_int(&mut out, 0, 8, MAX_INT);
        // One more than the range.
        *out.last_mut().unwrap() += 1;
        assert_eq!(decode_int(&out, 8), Err(PrefixError::IntegerOverflow));
        // Endless continuation bytes stop, without overflow or panic.
        let long = [0xff; 64];
        assert_eq!(decode_int(&long, 8), Err(PrefixError::IntegerOverflow));
        // Zero-padded continuation (value in range, too many bytes).
        let padded = [
            0x1f, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x00,
        ];
        assert_eq!(decode_int(&padded, 5), Err(PrefixError::IntegerOverflow));
    }

    #[test]
    fn strings_respect_limits() {
        let mut out = Vec::new();
        encode_string(&mut out, 0, 7, b"custom-key");
        assert_eq!(decode_string(&out, 7, 10).unwrap().0, b"custom-key");
        assert_eq!(decode_string(&out, 7, 9), Err(PrefixError::StringTooLong));
        // A huge declared length is refused before any read.
        let mut bomb = Vec::new();
        encode_int(&mut bomb, 0, 7, MAX_INT);
        assert_eq!(
            decode_string(&bomb, 7, 4096),
            Err(PrefixError::StringTooLong)
        );
        let mut short = Vec::new();
        encode_int(&mut short, 0, 7, 5);
        short.extend_from_slice(b"abc");
        assert_eq!(decode_string(&short, 7, 4096), Err(PrefixError::Incomplete));
    }
}
