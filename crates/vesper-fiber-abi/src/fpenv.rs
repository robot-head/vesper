//! Floating-point control state that the target C ABI requires callees to
//! preserve, and which ABI v2 therefore saves in every switch frame.
//!
//! * x86_64 System V: the MXCSR control bits (rounding, exception masks,
//!   FTZ/DAZ) and the x87 control word. The switch saves the whole MXCSR, so
//!   the sticky status flags (bits 0–5) are context-local too.
//! * AArch64 AAPCS64: FPCR (rounding, FZ, DN, AHP and trap enables). The
//!   switch also saves FPSR (cumulative exception flags).
//!
//! Changing the floating-point environment from Rust is outside the language's
//! default floating-point semantics (the optimizer assumes round-to-nearest),
//! so [`install`] and [`set_rounding`] are `unsafe`. They exist for runtime
//! tests and for generated code's own helpers.

/// IEEE rounding direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rounding {
    Nearest,
    Down,
    Up,
    TowardZero,
}

/// The preserved floating-point environment of one context.
#[cfg(target_arch = "x86_64")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FpEnv {
    pub mxcsr: u32,
    pub x87_control: u16,
}

/// The preserved floating-point environment of one context.
#[cfg(target_arch = "aarch64")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FpEnv {
    pub fpcr: u64,
}

#[cfg(target_arch = "x86_64")]
impl FpEnv {
    /// Power-on/ABI default: round to nearest, all exceptions masked, no
    /// FTZ/DAZ; x87 extended precision, all exceptions masked.
    pub const DEFAULT: Self = Self {
        mxcsr: 0x1F80,
        x87_control: 0x037F,
    };
    const MXCSR_STATUS: u32 = 0x3F;
    /// The callee-saved part only (status flags cleared).
    pub fn control(self) -> Self {
        Self {
            mxcsr: self.mxcsr & !Self::MXCSR_STATUS,
            x87_control: self.x87_control,
        }
    }
    pub fn rounding(self) -> Rounding {
        match (self.mxcsr >> 13) & 3 {
            0 => Rounding::Nearest,
            1 => Rounding::Down,
            2 => Rounding::Up,
            _ => Rounding::TowardZero,
        }
    }
    /// The same environment with SSE and x87 rounding set to `r`.
    pub fn with_rounding(self, r: Rounding) -> Self {
        let bits = match r {
            Rounding::Nearest => 0,
            Rounding::Down => 1,
            Rounding::Up => 2,
            Rounding::TowardZero => 3,
        };
        Self {
            mxcsr: (self.mxcsr & !(3 << 13)) | (bits << 13),
            x87_control: (self.x87_control & !(3 << 10)) | ((bits as u16) << 10),
        }
    }
}

#[cfg(target_arch = "aarch64")]
impl FpEnv {
    /// ABI default: round to nearest, no flush-to-zero, no traps.
    pub const DEFAULT: Self = Self { fpcr: 0 };
    pub fn control(self) -> Self {
        self
    }
    pub fn rounding(self) -> Rounding {
        match (self.fpcr >> 22) & 3 {
            0 => Rounding::Nearest,
            1 => Rounding::Up,
            2 => Rounding::Down,
            _ => Rounding::TowardZero,
        }
    }
    pub fn with_rounding(self, r: Rounding) -> Self {
        let bits: u64 = match r {
            Rounding::Nearest => 0,
            Rounding::Up => 1,
            Rounding::Down => 2,
            Rounding::TowardZero => 3,
        };
        Self {
            fpcr: (self.fpcr & !(3 << 22)) | (bits << 22),
        }
    }
}

/// Read the current context's floating-point environment.
pub fn current() -> FpEnv {
    #[cfg(target_arch = "x86_64")]
    {
        let mut mxcsr = 0u32;
        let mut cw = 0u16;
        // SAFETY: stores only to the two locals.
        unsafe {
            std::arch::asm!(
                "stmxcsr [{m}]",
                "fnstcw [{c}]",
                m = in(reg) &mut mxcsr,
                c = in(reg) &mut cw,
                options(nostack, preserves_flags),
            );
        }
        FpEnv {
            mxcsr,
            x87_control: cw,
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        let fpcr: u64;
        // SAFETY: reading FPCR has no side effects.
        unsafe { std::arch::asm!("mrs {}, fpcr", out(reg) fpcr, options(nomem, nostack)) };
        FpEnv { fpcr }
    }
}

/// Install `env` in the current context.
///
/// # Safety
/// Code running afterwards in this context observes the new environment.
/// Rust and LLVM assume the default environment; the caller must restore it
/// before running Rust floating-point code whose results matter, or confine
/// such code to an `asm`/`black_box` boundary as the tests do. Enabling traps
/// makes later floating-point operations fault.
pub unsafe fn install(env: FpEnv) {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: the caller accepts the new environment.
    unsafe {
        std::arch::asm!(
            "ldmxcsr [{m}]",
            "fldcw [{c}]",
            m = in(reg) &env.mxcsr,
            c = in(reg) &env.x87_control,
            options(nostack, preserves_flags, readonly),
        );
    }
    #[cfg(target_arch = "aarch64")]
    // SAFETY: as above.
    unsafe {
        std::arch::asm!("msr fpcr, {}", in(reg) env.fpcr, options(nomem, nostack));
    }
}

/// Set only the rounding direction of the current context.
///
/// # Safety
/// As for [`install`].
pub unsafe fn set_rounding(r: Rounding) {
    // SAFETY: forwarded contract.
    unsafe { install(current().with_rounding(r)) }
}

/// `a / b` computed by the hardware under the current rounding mode, as raw
/// bits. The division is done in non-`pure` `asm` so the optimizer can neither
/// constant-fold it under its default-environment assumption nor reuse a
/// result computed before a rounding change.
pub fn div_bits(a: f64, b: f64) -> u64 {
    let q: f64;
    #[cfg(target_arch = "x86_64")]
    // SAFETY: pure register arithmetic.
    unsafe {
        std::arch::asm!(
            "divsd {a}, {b}",
            a = inout(xmm_reg) a => q,
            b = in(xmm_reg) b,
            options(nomem, nostack),
        );
    }
    #[cfg(target_arch = "aarch64")]
    // SAFETY: pure register arithmetic.
    unsafe {
        std::arch::asm!(
            "fdiv {q:d}, {a:d}, {b:d}",
            q = out(vreg) q,
            a = in(vreg) a,
            b = in(vreg) b,
            options(nomem, nostack),
        );
    }
    q.to_bits()
}

/// `(1/3, -1/3)` under the current rounding mode. Round-to-nearest truncates
/// both; rounding up bumps the first and rounding down the second, so the
/// pair identifies `Nearest`/`TowardZero`, `Up` and `Down` by computation
/// rather than by reading the control register.
pub fn rounding_fingerprint() -> (u64, u64) {
    (div_bits(1.0, 3.0), div_bits(-1.0, 3.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rounding_round_trips_and_changes_results() {
        let saved = current();
        assert_eq!(saved.control(), FpEnv::DEFAULT.control(), "test thread env");
        let nearest = rounding_fingerprint();
        for r in [Rounding::Down, Rounding::Up, Rounding::TowardZero] {
            // SAFETY: only asm arithmetic runs before the restore below.
            unsafe { set_rounding(r) };
            let observed = (current().rounding(), rounding_fingerprint());
            // SAFETY: restoring the saved environment.
            unsafe { install(saved) };
            assert_eq!(observed.0, r);
            let expected = match r {
                Rounding::Up => (nearest.0 + 1, nearest.1),
                Rounding::Down => (nearest.0, nearest.1 + 1),
                _ => nearest,
            };
            assert_eq!(observed.1, expected, "{r:?}");
        }
        assert_eq!(current().control(), saved.control());
    }
}
