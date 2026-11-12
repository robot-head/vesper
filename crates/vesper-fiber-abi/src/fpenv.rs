//! Floating-point control state that the target C ABI requires callees to
//! preserve, and which ABI v2 therefore saves in every switch frame.
//!
//! * x86_64 System V: the MXCSR control bits (rounding, exception masks,
//!   FTZ/DAZ) and the x87 control word. The switch saves the whole MXCSR, so
//!   the sticky status flags (bits 0–5) are context-local too.
//! * AArch64 AAPCS64 (Linux) and the Apple arm64 ABI (macOS, which keeps the
//!   AAPCS64 rule here): FPCR (rounding, FZ, DN, AHP and trap enables). The
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

/// Cumulative IEEE exception flags of the current context, in a portable
/// encoding: bit 0 invalid, 1 divide-by-zero, 2 overflow, 3 underflow,
/// 4 inexact, 5 denormal input (x86 DE, AArch64 IDC). ABI v2 switches these
/// with the context (x86 MXCSR status bits, AArch64 FPSR), so one context's
/// flags are never observed by another.
pub fn exception_flags() -> u32 {
    #[cfg(target_arch = "x86_64")]
    {
        let mut mxcsr = 0u32;
        // SAFETY: stores only to the local.
        unsafe {
            std::arch::asm!(
                "stmxcsr [{m}]",
                m = in(reg) &mut mxcsr,
                options(nostack, preserves_flags),
            );
        }
        // MXCSR: IE ZE OE UE PE in bits 0,2,3,4,5 and DE in bit 1.
        let bit = |b: u32| (mxcsr >> b) & 1;
        bit(0) | (bit(2) << 1) | (bit(3) << 2) | (bit(4) << 3) | (bit(5) << 4) | (bit(1) << 5)
    }
    #[cfg(target_arch = "aarch64")]
    {
        let fpsr: u64;
        // SAFETY: reading FPSR has no side effects.
        unsafe { std::arch::asm!("mrs {}, fpsr", out(reg) fpsr, options(nomem, nostack)) };
        // FPSR: IOC DZC OFC UFC IXC in bits 0..=4 and IDC in bit 7.
        ((fpsr & 0x1F) | (((fpsr >> 7) & 1) << 5)) as u32
    }
}

/// Clear the current context's cumulative exception flags.
pub fn clear_exception_flags() {
    #[cfg(target_arch = "x86_64")]
    {
        let mut mxcsr = 0u32;
        // SAFETY: only the sticky status bits change; the control bits are
        // written back unchanged.
        unsafe {
            std::arch::asm!(
                "stmxcsr [{m}]",
                m = in(reg) &mut mxcsr,
                options(nostack, preserves_flags),
            );
            mxcsr &= !0x3F;
            std::arch::asm!(
                "ldmxcsr [{m}]",
                m = in(reg) &mxcsr,
                options(nostack, preserves_flags, readonly),
            );
        }
    }
    #[cfg(target_arch = "aarch64")]
    // SAFETY: FPSR holds only status bits in AArch64 state (the cumulative
    // IEEE flags, QC saturation and AArch32-only NZCV), so zeroing it clears
    // status and changes no control state.
    unsafe {
        std::arch::asm!("msr fpsr, xzr", options(nomem, nostack));
    }
}

/// Inexact flag in the [`exception_flags`] encoding.
pub const FLAG_INEXACT: u32 = 1 << 4;
/// Divide-by-zero flag in the [`exception_flags`] encoding.
pub const FLAG_DIVIDE_BY_ZERO: u32 = 1 << 1;

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

    #[test]
    fn exception_flags_are_raised_and_cleared() {
        clear_exception_flags();
        assert_eq!(exception_flags(), 0);
        std::hint::black_box(div_bits(1.0, 3.0));
        assert_eq!(exception_flags() & FLAG_INEXACT, FLAG_INEXACT);
        std::hint::black_box(div_bits(1.0, 0.0));
        assert_ne!(exception_flags() & FLAG_DIVIDE_BY_ZERO, 0);
        clear_exception_flags();
        assert_eq!(exception_flags(), 0);
        assert_eq!(current().control(), FpEnv::DEFAULT.control());
    }
}
