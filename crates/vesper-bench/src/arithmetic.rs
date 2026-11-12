use crate::Error;
/// Computes a residue, not hardware evidence. Values must fit the explicitly
/// supplied width (1..=64). This equals the true increment only if that increment
/// is strictly below 2^width; multiple wraps are not detectable from two reads.
/// Requires the same continuously-running counter with no reset/reconfiguration.
/// It does not validate register metadata, permissions, multiplexing or scaling.
pub fn modular_counter_delta(start: u64, end: u64, width_bits: u8) -> Result<u64, Error> {
    if !(1..=64).contains(&width_bits) {
        return Err(Error::InvalidCounterWidth);
    }
    let modulus = 1u128
        .checked_shl(u32::from(width_bits))
        .ok_or(Error::ArithmeticOverflow)?;
    if u128::from(start) >= modulus || u128::from(end) >= modulus {
        return Err(Error::CounterValueOutOfRange);
    }
    let value = u128::from(end)
        .checked_add(modulus)
        .and_then(|n| n.checked_sub(u128::from(start)))
        .ok_or(Error::ArithmeticOverflow)?
        % modulus;
    u64::try_from(value).map_err(|_| Error::ArithmeticOverflow)
}
/// Deterministic SplitMix-style generator of 64-byte offsets in [0,4096).
/// Explicit wrapping here is the generator's modular definition. This function
/// changes no executable code, entry padding, stack, linker, cache or scheduling.
pub fn layout_offset_bytes(seed: u64, epoch: u64) -> u32 {
    let mut z = seed.wrapping_add(epoch.wrapping_mul(0x9e37_79b9_7f4a_7c15));
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    (((z ^ (z >> 31)) & 63) as u32) * 64
}
