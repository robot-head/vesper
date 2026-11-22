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

// The Vesper build: the same functions forwarded to the port.

pub fn decode_int(buf: &[u8], bits: u8) -> Result<(u64, usize), PrefixError> {
    let n = super::port::huff().decode_int(buf, u64::from(bits))?;
    Ok((n.value, n.used as usize))
}

pub fn encode_int(out: &mut Vec<u8>, flags: u8, bits: u8, value: u64) {
    super::port::huff().encode_int(out, flags, u64::from(bits), value)
}

pub fn decode_string(
    buf: &[u8],
    bits: u8,
    max_len: usize,
) -> Result<(Vec<u8>, usize), PrefixError> {
    let mut out = Vec::new();
    let end = super::port::huff().decode_string(buf, u64::from(bits), max_len as u64, &mut out)?;
    Ok((out, end as usize))
}

pub fn encode_string(out: &mut Vec<u8>, flags: u8, bits: u8, value: &[u8]) {
    super::port::huff().encode_string(out, flags, u64::from(bits), value)
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
