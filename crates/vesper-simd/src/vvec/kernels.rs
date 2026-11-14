//! Whole-loop kernels: each runs a strip-mined loop inside one `asm!` block,
//! so the vector state stays in registers (the per-op [`VVec`](super::VVec)
//! API round-trips through memory). The scalar arm is the reference.
use super::IsaProfile;
use crate::ScanResult;

/// Delimiter count and XOR of `data` (design §14.3's scanner) on profile
/// `P`. SVE strips advance by `incb` under `whilelo`; RVV strips advance by
/// the `vl` that `vsetvli` grants. Neither has a scalar remainder.
pub fn scan<P: IsaProfile>(profile: &P, data: &[u8], delim: u8) -> ScanResult {
    let _ = profile;
    let (matches, xor) = dispatch!(P::ISA,
        sve => super::sve::scan(data.as_ptr(), data.len(), delim),
        rvv => super::rvv::scan(data.as_ptr(), data.len(), delim),
        scalar => scalar_scan(data, delim),
    );
    ScanResult { matches, xor }
}

fn scalar_scan(data: &[u8], delim: u8) -> (u64, u8) {
    data.iter().fold((0, 0), |(count, xor), byte| {
        (count + u64::from(*byte == delim), xor ^ byte)
    })
}
