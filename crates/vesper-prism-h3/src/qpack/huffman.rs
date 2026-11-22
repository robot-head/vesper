//! The HPACK Huffman code (RFC 7541 §5.2, Appendix B).
//!
//! The code is canonical, so decoding needs only, per code length, the first
//! code and the index of its first symbol in length order. Those tables are
//! built at compile time from [`CODES`].

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HuffmanError {
    /// The EOS symbol appeared in the data (RFC 7541 §5.2: an error).
    Eos,
    /// More than 7 bits of padding, or padding that is not the EOS prefix.
    BadPadding,
    /// The decoded string is longer than the caller's limit.
    TooLong,
}

// The Vesper build: the same functions over the port's Huffman tables.

pub fn encoded_len(data: &[u8]) -> usize {
    super::port::huff().len(data) as usize
}

pub fn encode(data: &[u8], out: &mut Vec<u8>) {
    super::port::huff().append(data, out)
}

pub fn decode(data: &[u8], max_len: usize) -> Result<Vec<u8>, HuffmanError> {
    let mut out = Vec::new();
    match super::port::huff().unpack(data, max_len as u64, &mut out) {
        Some(e) => Err(e),
        None => Ok(out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    // Reads the Rust build's canonical tables.

    #[test]
    fn rfc7541_appendix_c4_and_c6_strings() {
        for (text, wire) in [
            ("www.example.com", "f1e3 c2e5 f23a 6ba0 ab90 f4ff"),
            ("no-cache", "a8eb 1064 9cbf"),
            ("custom-key", "25a8 49e9 5ba9 7d7f"),
            ("custom-value", "25a8 49e9 5bb8 e8b4 bf"),
            ("302", "6402"),
            ("private", "aec3 771a 4b"),
            (
                "Mon, 21 Oct 2013 20:13:21 GMT",
                "d07a be94 1054 d444 a820 0595 040b 8166 e082 a62d 1bff",
            ),
            (
                "https://www.example.com",
                "9d29 ad17 1863 c78f 0b97 c8e9 ae82 ae43 d3",
            ),
        ] {
            let mut out = Vec::new();
            encode(text.as_bytes(), &mut out);
            assert_eq!(out, hex(wire), "{text}");
            assert_eq!(encoded_len(text.as_bytes()), out.len());
            assert_eq!(decode(&out, 1024).unwrap(), text.as_bytes());
        }
    }

    #[test]
    fn every_byte_round_trips() {
        let all: Vec<u8> = (0..=255).collect();
        let mut out = Vec::new();
        encode(&all, &mut out);
        assert_eq!(decode(&out, 256).unwrap(), all);
        assert_eq!(decode(&out, 255), Err(HuffmanError::TooLong));
    }

    #[test]
    fn padding_and_eos_rules() {
        // 'a' is 00011 (5 bits): padded with 111.
        assert_eq!(decode(&[0b0001_1111], 8).unwrap(), b"a");
        // Padding not all ones.
        assert_eq!(decode(&[0b0001_1110], 8), Err(HuffmanError::BadPadding));
        // A full byte of padding (8 bits > 7).
        assert_eq!(
            decode(&[0b0001_1111, 0xff], 8),
            Err(HuffmanError::BadPadding)
        );
        // EOS (30 ones) encoded explicitly, then padding.
        assert_eq!(decode(&[0xff, 0xff, 0xff, 0xff], 8), Err(HuffmanError::Eos));
        assert_eq!(decode(&[], 8).unwrap(), b"");
    }
}
