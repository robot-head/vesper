//! The HPACK Huffman code (RFC 7541 §5.2, Appendix B).
//!
//! The code is canonical, so decoding needs only, per code length, the first
//! code and the index of its first symbol in length order. Those tables are
//! built at compile time from [`CODES`].

use super::huffman_table::CODES;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HuffmanError {
    /// The EOS symbol appeared in the data (RFC 7541 §5.2: an error).
    Eos,
    /// More than 7 bits of padding, or padding that is not the EOS prefix.
    BadPadding,
    /// The decoded string is longer than the caller's limit.
    TooLong,
}

const EOS: u16 = 256;
const MAX_LEN: usize = 30;

struct Canonical {
    /// First code of each length (index = length).
    first: [u32; MAX_LEN + 1],
    /// Number of codes of each length.
    count: [u16; MAX_LEN + 1],
    /// Index into `symbols` of each length's first code.
    offset: [u16; MAX_LEN + 1],
    /// Symbols sorted by (length, code).
    symbols: [u16; 257],
}

const fn canonical() -> Canonical {
    let mut count = [0u16; MAX_LEN + 1];
    let mut s = 0;
    while s < 257 {
        count[CODES[s].0 as usize] += 1;
        s += 1;
    }
    let mut offset = [0u16; MAX_LEN + 1];
    let mut first = [0u32; MAX_LEN + 1];
    let mut len = 1;
    let mut code = 0u32;
    let mut index = 0u16;
    while len <= MAX_LEN {
        first[len] = code;
        offset[len] = index;
        index += count[len];
        code = (code + count[len] as u32) << 1;
        len += 1;
    }
    // Place symbols: within a length, codes are consecutive in symbol order.
    let mut symbols = [0u16; 257];
    let mut len = 1;
    while len <= MAX_LEN {
        let mut s = 0;
        while s < 257 {
            if CODES[s].0 as usize == len {
                let slot = offset[len] as u32 + (CODES[s].1 - first[len]);
                symbols[slot as usize] = s as u16;
            }
            s += 1;
        }
        len += 1;
    }
    Canonical {
        first,
        count,
        offset,
        symbols,
    }
}

static TABLE: Canonical = canonical();

/// Encoded length in bytes.
pub fn encoded_len(data: &[u8]) -> usize {
    let bits: usize = data.iter().map(|&b| usize::from(CODES[b as usize].0)).sum();
    bits.div_ceil(8)
}

/// Appends the Huffman coding of `data`, padded with the EOS prefix.
pub fn encode(data: &[u8], out: &mut Vec<u8>) {
    let mut acc: u64 = 0;
    let mut nbits: u32 = 0;
    for &b in data {
        let (len, code) = CODES[b as usize];
        acc = (acc << len) | u64::from(code);
        nbits += u32::from(len);
        while nbits >= 8 {
            nbits -= 8;
            out.push((acc >> nbits) as u8);
        }
        acc &= (1 << nbits) - 1;
    }
    if nbits > 0 {
        let pad = 8 - nbits;
        out.push(((acc << pad) | ((1 << pad) - 1)) as u8);
    }
}

/// Decodes Huffman data into at most `max_len` bytes.
pub fn decode(data: &[u8], max_len: usize) -> Result<Vec<u8>, HuffmanError> {
    let mut out = Vec::with_capacity(data.len().saturating_mul(8).div_ceil(5).min(max_len));
    let mut code: u32 = 0;
    let mut len: usize = 0;
    // Bits since the last complete symbol, and whether they were all 1.
    let mut all_ones = true;
    for &byte in data {
        for i in (0..8).rev() {
            let bit = u32::from(byte >> i & 1);
            code = code << 1 | bit;
            len += 1;
            all_ones &= bit == 1;
            if len > MAX_LEN {
                // Every 30-bit prefix is a code (the code is complete), so
                // this cannot happen for valid tables; refuse defensively.
                return Err(HuffmanError::BadPadding);
            }
            let rel = code.wrapping_sub(TABLE.first[len]);
            if code >= TABLE.first[len] && rel < u32::from(TABLE.count[len]) {
                let sym = TABLE.symbols[(u32::from(TABLE.offset[len]) + rel) as usize];
                if sym == EOS {
                    return Err(HuffmanError::Eos);
                }
                if out.len() == max_len {
                    return Err(HuffmanError::TooLong);
                }
                out.push(sym as u8);
                code = 0;
                len = 0;
                all_ones = true;
            }
        }
    }
    // Padding: fewer than 8 bits, all 1 (a prefix of EOS).
    if len > 7 || !all_ones {
        return Err(HuffmanError::BadPadding);
    }
    Ok(out)
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

    #[test]
    fn code_is_canonical_and_complete() {
        // Kraft equality: the code is complete (sum 2^-len == 1).
        let kraft: u64 = CODES
            .iter()
            .map(|(l, _)| 1u64 << (MAX_LEN - *l as usize))
            .sum();
        assert_eq!(kraft, 1 << MAX_LEN);
        for (s, &(len, code)) in CODES.iter().enumerate() {
            let len = len as usize;
            let rel = code - TABLE.first[len];
            assert!(rel < u32::from(TABLE.count[len]));
            assert_eq!(
                TABLE.symbols[(u32::from(TABLE.offset[len]) + rel) as usize] as usize,
                s
            );
        }
        // EOS is thirty 1 bits.
        assert_eq!(CODES[256], (30, 0x3fff_ffff));
    }

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
