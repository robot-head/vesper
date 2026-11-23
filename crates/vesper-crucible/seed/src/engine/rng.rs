//! Frozen Rust SplitMix64 fallback for `vesper-crucible-seed`.

pub(crate) fn next_u64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

pub(crate) fn below_inclusive(state: &mut u64, max_inclusive: u64) -> u64 {
    let Some(bound) = max_inclusive.checked_add(1) else {
        return next_u64(state);
    };
    // Values below `threshold` would bias the residue; reject them.
    let threshold = bound.wrapping_neg() % bound;
    loop {
        let value = next_u64(state);
        if value >= threshold {
            return value % bound;
        }
    }
}
