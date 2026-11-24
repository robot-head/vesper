//! The frozen magic-bits Rust implementation replaced in `src/morton.rs`
//! (SH6 lane 9, #1404).

use crate::morton::{AXIS_MAX, CODE_MASK, MortonError};

// The magic-bits interleave: an axis value is spread so that two zero
// bits follow each of its bits, by shifting and masking with the pattern
// each step leaves (named by the shift); compacting runs the steps
// backwards. Bit 3i of SPREAD_MASK_2 is set for i < 10.
const AXIS_MASK: u32 = AXIS_MAX as u32;
const SPREAD_MASK_16: u32 = 0x0300_00ff;
const SPREAD_MASK_8: u32 = 0x0300_f00f;
const SPREAD_MASK_4: u32 = 0x030c_30c3;
const SPREAD_MASK_2: u32 = 0x0924_9249;

#[inline]
pub(crate) const fn spread(v: u32) -> u32 {
    let mut x = v & AXIS_MASK;
    x = (x | (x << 16)) & SPREAD_MASK_16;
    x = (x | (x << 8)) & SPREAD_MASK_8;
    x = (x | (x << 4)) & SPREAD_MASK_4;
    x = (x | (x << 2)) & SPREAD_MASK_2;
    x
}

#[inline]
pub(crate) const fn compact(v: u32) -> u32 {
    let mut x = v & SPREAD_MASK_2;
    x = (x | (x >> 2)) & SPREAD_MASK_4;
    x = (x | (x >> 4)) & SPREAD_MASK_8;
    x = (x | (x >> 8)) & SPREAD_MASK_16;
    x = (x | (x >> 16)) & AXIS_MASK;
    x
}

#[inline]
pub(crate) const fn encode_masked(x: u16, y: u16, z: u16) -> u32 {
    spread(x as u32) | (spread(y as u32) << 1) | (spread(z as u32) << 2)
}

pub(crate) fn encode(x: u16, y: u16, z: u16) -> Result<u32, MortonError> {
    for (axis, value) in [(0u8, x), (1, y), (2, z)] {
        if value > AXIS_MAX {
            return Err(MortonError::AxisOutOfRange { axis, value });
        }
    }
    Ok(encode_masked(x, y, z))
}

#[inline]
pub(crate) const fn decode_masked(code: u32) -> (u16, u16, u16) {
    (
        compact(code) as u16,
        compact(code >> 1) as u16,
        compact(code >> 2) as u16,
    )
}

pub(crate) fn decode(code: u32) -> Result<(u16, u16, u16), MortonError> {
    if code & !CODE_MASK != 0 {
        return Err(MortonError::CodeOutOfRange(code));
    }
    Ok(decode_masked(code))
}
