//! Hardware random-number instructions: x86_64 `RDRAND` and AArch64 `RNDR`
//! (FEAT_RNG). The only unsafe code outside `linux_host`: one call of a
//! `#[target_feature]` function after runtime feature detection (x86_64), and
//! one `mrs` of the `RNDR` register after detection (AArch64). Each returns
//! `None` when the instruction is absent or reports failure after retries.
#![allow(unsafe_code)]

/// Retries per word, as Intel's DRNG guide recommends for RDRAND.
const RETRIES: usize = 10;

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "rdrand")]
fn rdrand64() -> Option<u64> {
    for _ in 0..RETRIES {
        let mut v = 0u64;
        if std::arch::x86_64::_rdrand64_step(&mut v) == 1 {
            return Some(v);
        }
    }
    None
}

/// Name of the instruction this build would use, if the CPU has it.
pub fn instruction() -> Option<&'static str> {
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("rdrand") {
            return Some("RDRAND");
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        if std::arch::is_aarch64_feature_detected!("rand") {
            return Some("RNDR");
        }
    }
    None
}

/// One 64-bit word from the CPU's hardware generator.
pub fn word() -> Option<u64> {
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("rdrand") {
            // SAFETY: the CPU supports RDRAND (checked just above), which is
            // the only precondition of this #[target_feature] function.
            return unsafe { rdrand64() };
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        if std::arch::is_aarch64_feature_detected!("rand") {
            for _ in 0..RETRIES {
                let v: u64;
                let ok: u64;
                // SAFETY: FEAT_RNG is present (checked above), so reading
                // RNDR (s3_3_c2_c4_0) is defined; it touches no memory and
                // only sets NZCV (Z=1 on failure), declared as clobbered.
                unsafe {
                    std::arch::asm!(
                        "mrs {v}, s3_3_c2_c4_0",
                        "cset {ok}, ne",
                        v = out(reg) v,
                        ok = out(reg) ok,
                        options(nomem, nostack),
                    );
                }
                if ok == 1 {
                    return Some(v);
                }
            }
        }
    }
    None
}
