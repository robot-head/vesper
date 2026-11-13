//! §23.1.3 / §23.2 `ConstTime::scope`: hardware data-independent timing.
//!
//! On AArch64 with FEAT_DIT the scope sets `PSTATE.DIT` for the calling
//! thread and restores the previous value on exit, including on unwind.
//! `PSTATE.DIT` is per-thread state the kernel preserves across context
//! switches, so the closure must not hand the work to another thread.
//!
//! On x86_64, DOITM is controlled through `IA32_UARCH_MISC_CTL`, an MSR only
//! the kernel can write, and no stable Linux, macOS or Windows user-space
//! interface exposes it, so it is always reported as unavailable.

/// The hardware mode a scope ran under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HardwareCtMode {
    /// AArch64 `PSTATE.DIT` was set for the scope (`was_set` tells whether it
    /// was already on before entry).
    ArmDit {
        /// DIT was already set on entry.
        was_set: bool,
    },
    /// The CPU or OS offers no user-space control; nothing was changed.
    Unavailable {
        /// Why.
        reason: &'static str,
    },
}

impl HardwareCtMode {
    /// Was a hardware data-independent-timing mode active in the scope?
    pub fn is_active(self) -> bool {
        matches!(self, HardwareCtMode::ArmDit { .. })
    }
}

/// The stateless `copy effect ConstTime` capability.
#[derive(Clone, Copy, Debug)]
pub struct ConstTime {
    mode: HardwareCtMode,
}

impl ConstTime {
    /// The mode in force for this scope.
    pub fn mode(&self) -> HardwareCtMode {
        self.mode
    }

    /// Report what a scope would enable on this host, without changing it.
    pub fn probe() -> HardwareCtMode {
        probe_impl()
    }

    /// Run `f` with hardware constant-time mode enabled where available.
    pub fn scope<R>(f: impl FnOnce(&ConstTime) -> R) -> (R, HardwareCtMode) {
        let guard = enter();
        let ct = ConstTime { mode: guard.mode };
        let r = f(&ct);
        let mode = guard.mode;
        drop(guard);
        (r, mode)
    }

    /// Read the current hardware DIT flag (AArch64 with FEAT_DIT only).
    pub fn dit_flag() -> Option<bool> {
        dit_read()
    }
}

struct Guard {
    mode: HardwareCtMode,
    #[cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]
    restore: Option<bool>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        #[cfg(target_arch = "aarch64")]
        if let Some(prev) = self.restore {
            crate::sys::dit::write(prev);
        }
    }
}

#[cfg(target_arch = "aarch64")]
fn enter() -> Guard {
    match crate::sys::dit::read() {
        Some(prev) => {
            crate::sys::dit::write(true);
            Guard {
                mode: HardwareCtMode::ArmDit { was_set: prev },
                restore: Some(prev),
            }
        }
        None => Guard {
            mode: HardwareCtMode::Unavailable {
                reason: "CPU does not report FEAT_DIT",
            },
            restore: None,
        },
    }
}

#[cfg(target_arch = "aarch64")]
fn probe_impl() -> HardwareCtMode {
    match crate::sys::dit::read() {
        Some(prev) => HardwareCtMode::ArmDit { was_set: prev },
        None => HardwareCtMode::Unavailable {
            reason: "CPU does not report FEAT_DIT",
        },
    }
}

#[cfg(target_arch = "aarch64")]
fn dit_read() -> Option<bool> {
    crate::sys::dit::read()
}

#[cfg(target_arch = "x86_64")]
const UNAVAILABLE: &str =
    "x86_64 DOITM needs a kernel MSR write (IA32_UARCH_MISC_CTL); no user-space interface";

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
const UNAVAILABLE: &str = "no hardware data-independent-timing control on this architecture";

#[cfg(not(target_arch = "aarch64"))]
fn enter() -> Guard {
    Guard {
        mode: HardwareCtMode::Unavailable {
            reason: UNAVAILABLE,
        },
        restore: None,
    }
}

#[cfg(not(target_arch = "aarch64"))]
fn probe_impl() -> HardwareCtMode {
    HardwareCtMode::Unavailable {
        reason: UNAVAILABLE,
    }
}

#[cfg(not(target_arch = "aarch64"))]
fn dit_read() -> Option<bool> {
    None
}
