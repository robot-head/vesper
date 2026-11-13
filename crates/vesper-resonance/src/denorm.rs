//! The `ConstDenorm` scope (design §32.1 item 2): flush subnormal floats to
//! zero in hardware around a realtime callback and restore the caller's
//! floating-point control state on exit.
//!
//! * x86_64: sets MXCSR.FTZ (bit 15) and MXCSR.DAZ (bit 6) with
//!   `stmxcsr`/`ldmxcsr`. Every x86_64 CPU implements both bits (x86_64
//!   implies SSE2, and DAZ predates the first x86_64 parts). This affects SSE
//!   arithmetic, which is what Rust uses for `f32`/`f64` on x86_64; x87 is
//!   not touched.
//! * AArch64: sets FPCR.FZ (bit 24) with `mrs`/`msr fpcr`. FZ flushes both
//!   subnormal inputs and outputs of single and double precision.
//! * Elsewhere: [`DenormUnavailable`]; nothing is changed.
//!
//! The control register is per thread. The scope is entered and left on the
//! calling thread only.
//!
//! This is the only module in the crate allowed to use `unsafe`: the inline
//! assembly that reads and writes the control register.
//!
//! Compiler caveat: LLVM assumes the default floating-point environment, so
//! it may constant-fold float arithmetic on compile-time-known values as if
//! subnormals were preserved. Values whose flushing matters must not be
//! compile-time constants (tests use `std::hint::black_box`).

#![allow(unsafe_code)]

use std::fmt;

/// Which hardware mode a scope enables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DenormMode {
    /// x86_64 MXCSR flush-to-zero and denormals-are-zero.
    MxcsrFtzDaz,
    /// AArch64 FPCR flush-to-zero.
    FpcrFz,
}

/// The host cannot set a flush-to-zero mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DenormUnavailable {
    /// Target architecture name.
    pub arch: &'static str,
}

impl fmt::Display for DenormUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "flush-to-zero control is not available on {}", self.arch)
    }
}

impl std::error::Error for DenormUnavailable {}

#[cfg(target_arch = "x86_64")]
mod hw {
    use super::DenormMode;
    pub const MODE: DenormMode = DenormMode::MxcsrFtzDaz;
    /// FTZ (bit 15) | DAZ (bit 6).
    pub const FLUSH_BITS: u32 = (1 << 15) | (1 << 6);

    pub fn read() -> u32 {
        let mut csr: u32 = 0;
        // SAFETY: `stmxcsr` stores the 32-bit MXCSR into the pointed-to,
        // properly aligned, writable u32; it has no other effect.
        unsafe {
            std::arch::asm!("stmxcsr [{}]", in(reg) &mut csr, options(nostack, preserves_flags));
        }
        csr
    }

    pub fn write(csr: u32) {
        // SAFETY: `csr` is either a value read from MXCSR or such a value
        // with only FTZ/DAZ added; both bits exist on every x86_64 CPU, and
        // reserved bits are never set, so `ldmxcsr` cannot fault. The asm
        // block is not `nomem`, so the compiler does not move memory
        // operations across it.
        unsafe {
            std::arch::asm!("ldmxcsr [{}]", in(reg) &csr, options(nostack, preserves_flags));
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod hw {
    use super::DenormMode;
    pub const MODE: DenormMode = DenormMode::FpcrFz;
    /// FPCR.FZ (bit 24).
    pub const FLUSH_BITS: u32 = 1 << 24;

    pub fn read() -> u32 {
        let fpcr: u64;
        // SAFETY: reading FPCR is permitted at EL0 and has no side effects.
        unsafe {
            std::arch::asm!("mrs {}, fpcr", out(reg) fpcr, options(nomem, nostack, preserves_flags));
        }
        // FPCR's defined bits all lie in the low 32 bits.
        fpcr as u32
    }

    pub fn write(fpcr: u32) {
        // SAFETY: writing FPCR is permitted at EL0; the value is a previously
        // read FPCR with at most FZ added, so no reserved bits are set.
        // Without `nomem` the compiler does not move memory operations
        // across it.
        unsafe {
            std::arch::asm!("msr fpcr, {}", in(reg) u64::from(fpcr), options(nostack, preserves_flags));
        }
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
mod hw {
    pub const ARCH: &str = std::env::consts::ARCH;
}

/// The mode this host supports, or why not.
pub fn availability() -> Result<DenormMode, DenormUnavailable> {
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    {
        Ok(hw::MODE)
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        Err(DenormUnavailable { arch: hw::ARCH })
    }
}

/// Whether the flush bits are currently set on this thread (`false` where
/// unavailable).
pub fn flush_active() -> bool {
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    {
        hw::read() & hw::FLUSH_BITS == hw::FLUSH_BITS
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        false
    }
}

/// Raw control-register value (MXCSR or low 32 bits of FPCR), for tests and
/// diagnostics; `None` where unavailable.
pub fn control_word() -> Option<u32> {
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    {
        Some(hw::read())
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        None
    }
}

/// Restores the saved control word on drop (including during unwinding).
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
struct Restore(u32);

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
impl Drop for Restore {
    fn drop(&mut self) {
        hw::write(self.0);
    }
}

/// Run `f` with subnormals flushed to zero, restoring the caller's exact
/// control word afterwards. Returns [`DenormUnavailable`] (without running
/// `f`) where the mode cannot be set.
pub fn with_const_denorm<R>(f: impl FnOnce() -> R) -> Result<R, DenormUnavailable> {
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    {
        let saved = hw::read();
        let _restore = Restore(saved);
        hw::write(saved | hw::FLUSH_BITS);
        if hw::read() & hw::FLUSH_BITS != hw::FLUSH_BITS {
            return Err(DenormUnavailable {
                arch: std::env::consts::ARCH,
            });
        }
        Ok(f())
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        let _ = f;
        Err(DenormUnavailable { arch: hw::ARCH })
    }
}
