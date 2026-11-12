#![allow(dead_code)]

pub mod vectors;

/// Decodes hex, ignoring ASCII whitespace.
pub fn hex(s: &str) -> Vec<u8> {
    let digits: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    assert!(digits.len().is_multiple_of(2), "odd hex length");
    digits
        .chunks(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}

/// RFC 9000 Appendix A.3 packet-number reconstruction (the packet core's
/// job, reproduced here only to drive the receive boundary in tests).
pub fn decode_pn(largest: Option<u64>, truncated: u64, pn_len: usize) -> u64 {
    let expected = largest.map_or(0, |l| l + 1);
    let bits = 8 * pn_len as u32;
    let win = 1u64 << bits;
    let hwin = win / 2;
    let mask = win - 1;
    let candidate = (expected & !mask) | truncated;
    if candidate + hwin <= expected && candidate < (1 << 62) - win {
        candidate + win
    } else if candidate > expected + hwin && candidate >= win {
        candidate - win
    } else {
        candidate
    }
}
