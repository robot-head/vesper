//! Host kernel backends: userfaultfd lazy restore and KVM execution.
//!
//! The ioctls live in one gated module (`linux_host`, the crate's only unsafe
//! code), built on Linux x86_64/AArch64 with the `uffd` and/or `kvm` features.
//! Everywhere else, and wherever the kernel or privileges refuse, these calls
//! return [`Unavailable`] with the reason; nothing here claims a feature ran
//! unless it actually did.

use crate::Error;
use crate::slot_plan::{PlanError, VerifiedKvmSlotPlan};

/// Why a host backend cannot be used here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// Not Linux, or an architecture whose ioctl encoding is not supported.
    UnsupportedPlatform,
    /// The crate was built without this Cargo feature.
    FeatureDisabled(&'static str),
    DeviceAbsent {
        path: &'static str,
    },
    /// `vm.unprivileged_userfaultfd` is 0, the process lacks `CAP_SYS_PTRACE`,
    /// and the user-mode-only and `/dev/userfaultfd` routes were refused too.
    UserfaultfdDenied {
        unprivileged_userfaultfd: Option<u8>,
        cap_sys_ptrace: bool,
        errno: i32,
    },
    /// A kernel call failed with this errno before any work was done.
    KernelRefused {
        op: &'static str,
        errno: i32,
    },
    /// The kernel lacks a required userfaultfd ioctl or KVM capability.
    MissingFeature(&'static str),
}

/// How the userfaultfd descriptor was obtained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UffdRoute {
    /// `userfaultfd(2)` without restrictions (privileged or sysctl 1).
    Syscall,
    /// `userfaultfd(2)` with `UFFD_USER_MODE_ONLY`: only user-mode faults are
    /// reported, which is enough for guest memory touched by user code.
    SyscallUserModeOnly,
    /// `/dev/userfaultfd` (`USERFAULTFD_IOC_NEW`), gated by file permissions.
    DevNode,
}

/// What the host allows; see [`probe_userfaultfd`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UffdProbe {
    /// `/proc/sys/vm/unprivileged_userfaultfd`, if readable.
    pub unprivileged_userfaultfd: Option<u8>,
    /// `CAP_SYS_PTRACE` in the effective set (from `/proc/self/status`).
    pub cap_sys_ptrace: bool,
    pub route: Result<UffdRoute, Unavailable>,
}

/// Counts from one lazy-restore session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UffdStats {
    pub faults: u64,
    pub copied: u64,
    pub zeroed: u64,
    /// Faults whose page another fault had already resolved (`EEXIST`).
    pub raced: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UffdError {
    Unavailable(Unavailable),
    InvalidRegion,
    Os {
        op: &'static str,
        errno: i32,
    },
    /// The page source refused a page. The region is unregistered so that the
    /// faulting thread is released (it then reads zeros); the session fails.
    Source {
        index: usize,
        error: Error,
    },
    FaultOutsideRegion {
        address: u64,
    },
    UnexpectedEvent(u8),
    /// More faults than the caller's budget.
    FaultBudget,
}
impl From<Unavailable> for UffdError {
    fn from(value: Unavailable) -> Self {
        Self::Unavailable(value)
    }
}

/// Result of the gated KVM guest run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuestRun {
    pub port: u16,
    pub value: u8,
    pub exits: u32,
    pub plan: VerifiedKvmSlotPlan,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KvmError {
    Unavailable(Unavailable),
    Plan(PlanError),
    Kvm { op: &'static str, errno: i32 },
    UnexpectedExit(String),
    ExitBudget,
}
impl From<Unavailable> for KvmError {
    fn from(value: Unavailable) -> Self {
        Self::Unavailable(value)
    }
}
impl From<PlanError> for KvmError {
    fn from(value: PlanError) -> Self {
        Self::Plan(value)
    }
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    any(feature = "uffd", feature = "kvm")
))]
pub use crate::linux_host::*;

#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    feature = "uffd"
)))]
mod no_uffd {
    use super::*;
    use crate::page_source::PageSource;

    fn reason() -> Unavailable {
        if cfg!(all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )) {
            Unavailable::FeatureDisabled("uffd")
        } else {
            Unavailable::UnsupportedPlatform
        }
    }
    pub fn probe_userfaultfd() -> UffdProbe {
        UffdProbe {
            unprivileged_userfaultfd: None,
            cap_sys_ptrace: false,
            route: Err(reason()),
        }
    }
    /// Placeholder; [`LazyRegion::new`] always reports [`Unavailable`] here.
    #[derive(Debug)]
    pub struct LazyRegion(());
    /// Placeholder accessor; never constructed on this platform.
    #[derive(Debug)]
    pub struct Toucher<'a>(std::marker::PhantomData<&'a ()>);
    impl Toucher<'_> {
        pub fn len(&self) -> usize {
            0
        }
        pub fn is_empty(&self) -> bool {
            true
        }
        pub fn read_u8(&self, _offset: usize) -> Option<u8> {
            None
        }
        pub fn write_u8(&self, _offset: usize, _value: u8) -> bool {
            false
        }
        pub fn read_page(&self, _index: usize) -> Option<Box<[u8; crate::PAGE_BYTES]>> {
            None
        }
    }
    impl LazyRegion {
        pub fn new(_pages: usize) -> Result<Self, UffdError> {
            Err(UffdError::Unavailable(reason()))
        }
        pub fn page_count(&self) -> usize {
            0
        }
        pub fn route(&self) -> UffdRoute {
            UffdRoute::Syscall
        }
        pub fn serve<S, R, F>(
            &self,
            _source: &S,
            _fault_budget: u64,
            _touch: F,
        ) -> Result<(R, UffdStats), UffdError>
        where
            S: PageSource + Sync,
            R: Send,
            F: FnOnce(&Toucher<'_>) -> R + Send,
        {
            Err(UffdError::Unavailable(reason()))
        }
    }
}
#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    feature = "uffd"
)))]
pub use no_uffd::*;

#[cfg(not(all(target_os = "linux", target_arch = "x86_64", feature = "kvm")))]
mod no_kvm {
    use super::*;
    use crate::slot_plan::KvmHostLimits;

    fn reason() -> Unavailable {
        if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            Unavailable::FeatureDisabled("kvm")
        } else {
            Unavailable::UnsupportedPlatform
        }
    }
    pub fn probe_kvm() -> Result<KvmHostLimits, Unavailable> {
        Err(reason())
    }
    pub fn run_port_write_guest(_value: u8) -> Result<GuestRun, KvmError> {
        Err(KvmError::Unavailable(reason()))
    }
}
#[cfg(not(all(target_os = "linux", target_arch = "x86_64", feature = "kvm")))]
pub use no_kvm::*;
