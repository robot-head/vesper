//! Branchless 30-bit 3D Morton (Z-order) codes, 10 bits per axis (§36.4).
//!
//! Bit `3i` of a code is bit `i` of `x`, bit `3i + 1` is bit `i` of `y` and
//! bit `3i + 2` is bit `i` of `z`. [`spec_encode`] is the bit-by-bit
//! specification that `lean/KineticColoring.lean` models; [`encode`] uses
//! the usual magic-mask spreading and is tested against it.

/// Largest coordinate an axis can carry (`2^10 - 1`).
pub const AXIS_MAX: u16 = 1023;
/// Mask of the 30 bits a code can use.
pub const CODE_MASK: u32 = (1 << 30) - 1;

/// Typed rejection of out-of-range Morton inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MortonError {
    /// A coordinate does not fit in 10 bits.
    AxisOutOfRange { axis: u8, value: u16 },
    /// A code uses bits above bit 29.
    CodeOutOfRange(u32),
}

impl std::fmt::Display for MortonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AxisOutOfRange { axis, value } => {
                write!(f, "Morton axis {axis} value {value} exceeds 10 bits")
            }
            Self::CodeOutOfRange(code) => write!(f, "Morton code {code:#x} exceeds 30 bits"),
        }
    }
}

impl std::error::Error for MortonError {}

/// Spread the low 10 bits of `v` so bit `i` lands at bit `3i`.
#[inline]
pub const fn spread(v: u32) -> u32 {
    let mut x = v & 0x0000_03ff;
    x = (x | (x << 16)) & 0x0300_00ff;
    x = (x | (x << 8)) & 0x0300_f00f;
    x = (x | (x << 4)) & 0x030c_30c3;
    x = (x | (x << 2)) & 0x0924_9249;
    x
}

/// Inverse of [`spread`]: gather bits `0, 3, 6, ...` into the low 10 bits.
#[inline]
pub const fn compact(v: u32) -> u32 {
    let mut x = v & 0x0924_9249;
    x = (x | (x >> 2)) & 0x030c_30c3;
    x = (x | (x >> 4)) & 0x0300_f00f;
    x = (x | (x >> 8)) & 0x0300_00ff;
    x = (x | (x >> 16)) & 0x0000_03ff;
    x
}

/// Branchless encode of three coordinates that are already known to be in
/// range; higher bits are masked off, never carried into other axes.
#[inline]
pub const fn encode_masked(x: u16, y: u16, z: u16) -> u32 {
    spread(x as u32) | (spread(y as u32) << 1) | (spread(z as u32) << 2)
}

/// Encode with range checking.
pub fn encode(x: u16, y: u16, z: u16) -> Result<u32, MortonError> {
    for (axis, value) in [(0u8, x), (1, y), (2, z)] {
        if value > AXIS_MAX {
            return Err(MortonError::AxisOutOfRange { axis, value });
        }
    }
    Ok(encode_masked(x, y, z))
}

/// Branchless decode of the low 30 bits.
#[inline]
pub const fn decode_masked(code: u32) -> (u16, u16, u16) {
    (
        compact(code) as u16,
        compact(code >> 1) as u16,
        compact(code >> 2) as u16,
    )
}

/// Decode with range checking.
pub fn decode(code: u32) -> Result<(u16, u16, u16), MortonError> {
    if code & !CODE_MASK != 0 {
        return Err(MortonError::CodeOutOfRange(code));
    }
    Ok(decode_masked(code))
}

/// Bit-by-bit specification (the Lean model's `interleave`).
pub fn spec_encode(x: u16, y: u16, z: u16) -> u32 {
    let mut code = 0u32;
    for i in 0..10 {
        code |= ((u32::from(x) >> i) & 1) << (3 * i);
        code |= ((u32::from(y) >> i) & 1) << (3 * i + 1);
        code |= ((u32::from(z) >> i) & 1) << (3 * i + 2);
    }
    code
}

/// Encode using BMI2 `pdep` when the running CPU has it, else the scalar path.
/// Both give the same result for every input (tested).
pub fn encode_fast(x: u16, y: u16, z: u16) -> Result<u32, MortonError> {
    encode(x, y, z)?;
    #[cfg(target_arch = "x86_64")]
    if arch::bmi2_available() {
        return Ok(arch::encode_bmi2(x, y, z));
    }
    Ok(encode_masked(x, y, z))
}

/// Decode using BMI2 `pext` when available, else the scalar path.
pub fn decode_fast(code: u32) -> Result<(u16, u16, u16), MortonError> {
    decode(code)?;
    #[cfg(target_arch = "x86_64")]
    if arch::bmi2_available() {
        return Ok(arch::decode_bmi2(code));
    }
    Ok(decode_masked(code))
}

/// The only module in this crate that uses `unsafe` (allowed by the wave-five
/// contracts for lane 10: optional `std::arch` SIMD). Each intrinsic call is
/// guarded by runtime detection of the BMI2 feature it requires.
#[cfg(target_arch = "x86_64")]
#[allow(unsafe_code)]
pub mod arch {
    use std::arch::x86_64::{_pdep_u32, _pext_u32};

    const X_MASK: u32 = 0x0924_9249;

    /// Whether the running CPU supports BMI2.
    pub fn bmi2_available() -> bool {
        std::arch::is_x86_feature_detected!("bmi2")
    }

    #[target_feature(enable = "bmi2")]
    fn encode_impl(x: u16, y: u16, z: u16) -> u32 {
        _pdep_u32(u32::from(x) & 0x3ff, X_MASK)
            | _pdep_u32(u32::from(y) & 0x3ff, X_MASK << 1)
            | _pdep_u32(u32::from(z) & 0x3ff, X_MASK << 2)
    }

    #[target_feature(enable = "bmi2")]
    fn decode_impl(code: u32) -> (u16, u16, u16) {
        (
            _pext_u32(code, X_MASK) as u16,
            _pext_u32(code, X_MASK << 1) as u16,
            _pext_u32(code, X_MASK << 2) as u16,
        )
    }

    /// `pdep` encode; panics if BMI2 is unavailable (never executes the
    /// instruction on a CPU without it).
    pub fn encode_bmi2(x: u16, y: u16, z: u16) -> u32 {
        assert!(bmi2_available(), "BMI2 unavailable");
        // SAFETY: the assertion above established that the CPU supports
        // BMI2, the only target feature `encode_impl` enables.
        unsafe { encode_impl(x, y, z) }
    }

    /// `pext` decode of the low 30 bits; panics if BMI2 is unavailable.
    pub fn decode_bmi2(code: u32) -> (u16, u16, u16) {
        assert!(bmi2_available(), "BMI2 unavailable");
        // SAFETY: as in `encode_bmi2`.
        unsafe { decode_impl(code) }
    }
}
