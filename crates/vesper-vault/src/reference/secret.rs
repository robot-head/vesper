//! Frozen Rust §23.2 branchless 16-byte tag comparison over `SecU64`.

use crate::secret::SecU64;

pub(crate) fn verify_tag_16_ct(expected: &[u8; 16], computed: &[u8; 16]) -> bool {
    let e_lo = SecU64::new(u64::from_le_bytes(
        expected[0..8].try_into().unwrap_or([0; 8]),
    ));
    let e_hi = SecU64::new(u64::from_le_bytes(
        expected[8..16].try_into().unwrap_or([0; 8]),
    ));
    let c_lo = SecU64::new(u64::from_le_bytes(
        computed[0..8].try_into().unwrap_or([0; 8]),
    ));
    let c_hi = SecU64::new(u64::from_le_bytes(
        computed[8..16].try_into().unwrap_or([0; 8]),
    ));
    let combined = e_lo.bit_xor(c_lo).bit_or(e_hi.bit_xor(c_hi));
    combined.ct_is_zero().declassify_bool()
}
