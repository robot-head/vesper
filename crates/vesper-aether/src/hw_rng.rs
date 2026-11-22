//! Hardware random-number instructions: x86_64 `RDRAND` and AArch64 `RNDR`
//! (FEAT_RNG), detected and executed in Vesper (`vesper-rng/`, through
//! `@vesper-std/cpu`; SH6 lane 14, #1409). build.rs builds the package for
//! the target: `x86_64` (CPUID), `aarch64-linux` (AT_HWCAP2) or
//! `aarch64-macos` (the `hw.optional.arm.FEAT_RNG` sysctl). Each returns
//! `None` when the instruction is absent or reports failure after retries.

/// Name of the instruction this build would use, if the CPU has it.
pub fn instruction() -> Option<&'static str> {
    match crate::rng_binding::rng_instruction() {
        1 => Some("RDRAND"),
        2 => Some("RNDR"),
        _ => None,
    }
}

/// One 64-bit word from the CPU's hardware generator.
pub fn word() -> Option<u64> {
    let mut w = 0;
    crate::rng_binding::rng_word(&mut w).then_some(w)
}

#[cfg(test)]
mod tests {
    /// The Vesper detection agrees with Rust's.
    #[test]
    fn detection_matches_the_standard_library() {
        #[cfg(target_arch = "x86_64")]
        let expected = std::is_x86_feature_detected!("rdrand").then_some("RDRAND");
        #[cfg(target_arch = "aarch64")]
        let expected = std::arch::is_aarch64_feature_detected!("rand").then_some("RNDR");
        assert_eq!(super::instruction(), expected);
    }

    #[test]
    fn words_come_only_from_a_present_instruction() {
        let words: Vec<_> = (0..4).map(|_| super::word()).collect();
        if super::instruction().is_none() {
            assert!(words.iter().all(Option::is_none));
        } else {
            // A healthy generator delivers within the retries and does not
            // repeat itself.
            let words: Vec<u64> = words.into_iter().map(Option::unwrap).collect();
            assert!(words.windows(2).any(|w| w[0] != w[1]), "{words:x?}");
        }
    }
}
