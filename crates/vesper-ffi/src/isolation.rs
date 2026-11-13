//! Hardware isolation domains (design §13.4): Intel MPK/PKU protection keys
//! on x86_64 Linux and the Arm Permission Overlay Extension (POE, `POR_EL0`)
//! on AArch64 Linux.
//!
//! An [`IsolationDomain`] owns one protection key and page-aligned anonymous
//! regions tagged with it. [`IsolationDomain::with_revoked`] runs a closure
//! with the current thread's rights to those regions reduced (write, or all
//! access, revoked) and restores the previous rights register on every exit,
//! including unwinding panics. A write that hardware forbids raises
//! `SIGSEGV` with `si_code == SEGV_PKUERR`; [`contain`] runs work in a forked
//! child so such a fault becomes a typed [`ChildExit::Fault`] instead of
//! killing the caller.
//!
//! What this is not:
//!
//! * Protection keys are per *thread* register state. Other threads keep
//!   their own rights; this is a same-address-space tripwire against stray
//!   writes, not a security boundary against code that can execute
//!   `WRPKRU`/`MSR POR_EL0` itself or call `pkey_mprotect`.
//! * Nothing here pivots stacks, separates a foreign heap (Domain 2) or emits
//!   a trampoline. Those belong to a code generator.
//! * When the CPU or kernel lacks PKU/POE, every constructor returns
//!   [`Unavailable`] with the reason. Nothing pretends isolation happened.
//!
//! Region memory is never exposed as Rust references; [`IsolationDomain::read`]
//! and [`IsolationDomain::write`] copy through raw pointers (like a `Cell`), so
//! revoking rights never invalidates a live reference. A write attempted
//! inside `with_revoked(Revoke::Write, ..)` faults by design.
use std::{cell::Cell, fmt, marker::PhantomData};

/// The hardware mechanism behind a domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mechanism {
    /// Intel/AMD protection keys: `PKRU` written with `WRPKRU`.
    IntelPku,
    /// Arm Permission Overlay Extension: `POR_EL0` written with `MSR`.
    ArmPoe,
}

/// Why hardware isolation is not available on this host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnavailableReason {
    /// Not Linux on x86_64 or AArch64.
    UnsupportedTarget,
    /// The CPU does not report PKU (x86_64) or the kernel does not report
    /// `HWCAP2_POE` (AArch64).
    CpuLacksFeature,
    /// The CPU has the feature but the kernel has not enabled it (`OSPKE`
    /// clear) or does not implement the pkey syscalls.
    KernelLacksSupport,
    /// `pkey_alloc` found no free protection key.
    KeysExhausted,
    /// Another operating-system error, with its `errno`.
    Os(i32),
}

/// Isolation is unavailable; [`Unavailable::detail`] says why in words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unavailable {
    pub reason: UnavailableReason,
    pub detail: String,
}
impl fmt::Display for Unavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "hardware isolation unavailable ({:?}): {}",
            self.reason, self.detail
        )
    }
}
impl std::error::Error for Unavailable {}

/// Typed failures of domain operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainError {
    Unavailable(Unavailable),
    /// A region length of zero, or above [`MAX_REGION_BYTES`].
    RegionLength {
        requested: usize,
    },
    /// More than [`MAX_REGIONS`] regions in one domain.
    RegionBudget,
    /// The region id does not belong to this domain.
    UnknownRegion,
    /// `offset + len` lies outside the region.
    OutOfBounds {
        offset: usize,
        len: usize,
        region: usize,
    },
    /// `mmap`, `pkey_mprotect` or `fork`/`pipe` failed with this `errno`.
    Os {
        operation: &'static str,
        errno: i32,
    },
}
impl fmt::Display for DomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(u) => u.fmt(f),
            other => write!(f, "{other:?}"),
        }
    }
}
impl std::error::Error for DomainError {}
impl From<Unavailable> for DomainError {
    fn from(u: Unavailable) -> Self {
        Self::Unavailable(u)
    }
}

/// Largest single region.
pub const MAX_REGION_BYTES: usize = 1 << 30;
/// Largest number of regions per domain.
pub const MAX_REGIONS: usize = 64;

/// Which rights [`IsolationDomain::with_revoked`] removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revoke {
    /// Reads stay allowed; writes fault.
    Write,
    /// Reads and writes fault.
    Access,
}

/// The current thread's rights to a domain's regions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rights {
    pub read: bool,
    pub write: bool,
}

/// Identifies one region of one domain (keys are unique among live domains).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionId {
    key: i32,
    index: usize,
}

#[derive(Debug)]
struct Region {
    base: usize,
    len: usize,
    mapped: usize,
}

/// Whether this host can isolate, and with which mechanism. Allocates and
/// frees one protection key to confirm the kernel really supports it.
pub fn probe() -> Result<Mechanism, Unavailable> {
    let (mechanism, key) = hw::alloc_key()?;
    hw::free_key(key);
    Ok(mechanism)
}

/// One protection key plus the page-aligned regions tagged with it.
///
/// Not `Sync`: rights are per-thread state, and region contents are accessed
/// through `&self` like a `Cell`. Dropping the domain unmaps its regions and
/// then frees the key, so the key is never reused while pages carry it.
pub struct IsolationDomain {
    key: i32,
    mechanism: Mechanism,
    regions: Vec<Region>,
    _not_sync: PhantomData<Cell<()>>,
}
impl fmt::Debug for IsolationDomain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IsolationDomain")
            .field("key", &self.key)
            .field("mechanism", &self.mechanism)
            .field("regions", &self.regions.len())
            .finish()
    }
}
impl IsolationDomain {
    /// Allocate a protection key, or report why isolation is unavailable.
    pub fn new() -> Result<Self, Unavailable> {
        let (mechanism, key) = hw::alloc_key()?;
        Ok(Self {
            key,
            mechanism,
            regions: vec![],
            _not_sync: PhantomData,
        })
    }
    pub fn mechanism(&self) -> Mechanism {
        self.mechanism
    }
    /// The hardware protection key number.
    pub fn key(&self) -> u32 {
        self.key as u32
    }
    /// Map a zeroed, page-aligned region of at least `len` bytes tagged with
    /// this domain's key.
    pub fn add_region(&mut self, len: usize) -> Result<RegionId, DomainError> {
        if len == 0 || len > MAX_REGION_BYTES {
            return Err(DomainError::RegionLength { requested: len });
        }
        if self.regions.len() >= MAX_REGIONS {
            return Err(DomainError::RegionBudget);
        }
        let (base, mapped) = hw::map_tagged(len, self.key)?;
        self.regions.push(Region { base, len, mapped });
        Ok(RegionId {
            key: self.key,
            index: self.regions.len() - 1,
        })
    }
    fn region(&self, id: RegionId) -> Result<&Region, DomainError> {
        if id.key != self.key {
            return Err(DomainError::UnknownRegion);
        }
        self.regions.get(id.index).ok_or(DomainError::UnknownRegion)
    }
    /// Usable length of a region.
    pub fn region_len(&self, id: RegionId) -> Result<usize, DomainError> {
        Ok(self.region(id)?.len)
    }
    /// Base address of a region (page aligned), for passing to foreign code.
    /// An integer, not a reference or a permission.
    pub fn region_address(&self, id: RegionId) -> Result<usize, DomainError> {
        Ok(self.region(id)?.base)
    }
    fn bounds(&self, id: RegionId, offset: usize, len: usize) -> Result<usize, DomainError> {
        let r = self.region(id)?;
        if offset.checked_add(len).is_none_or(|end| end > r.len) {
            return Err(DomainError::OutOfBounds {
                offset,
                len,
                region: r.len,
            });
        }
        Ok(r.base + offset)
    }
    /// Copy region bytes out. Faults (SIGSEGV) inside
    /// `with_revoked(Revoke::Access, ..)`.
    pub fn read(&self, id: RegionId, offset: usize, out: &mut [u8]) -> Result<(), DomainError> {
        let at = self.bounds(id, offset, out.len())?;
        hw::copy_out(at, out);
        Ok(())
    }
    /// Copy bytes into a region. Faults (SIGSEGV) inside any `with_revoked`.
    pub fn write(&self, id: RegionId, offset: usize, bytes: &[u8]) -> Result<(), DomainError> {
        let at = self.bounds(id, offset, bytes.len())?;
        hw::copy_in(at, bytes);
        Ok(())
    }
    /// The current thread's rights to this domain's regions.
    pub fn rights(&self) -> Rights {
        hw::rights(hw::read_register(), self.key)
    }
    /// Run `f` with this thread's `revoke` rights to every region of the
    /// domain removed; the previous rights register value is restored when
    /// `f` returns or unwinds. Nested calls restore in order.
    pub fn with_revoked<R>(&self, revoke: Revoke, f: impl FnOnce() -> R) -> R {
        struct Restore(u64);
        impl Drop for Restore {
            fn drop(&mut self) {
                hw::write_register(self.0);
            }
        }
        let saved = hw::read_register();
        let _restore = Restore(saved);
        hw::write_register(hw::revoked(saved, self.key, revoke));
        f()
    }
}
impl Drop for IsolationDomain {
    fn drop(&mut self) {
        for r in self.regions.drain(..) {
            hw::unmap(r.base, r.mapped);
        }
        hw::free_key(self.key);
    }
}

/// How a [`contain`]ed child ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChildExit {
    /// The closure returned; the child exited 0.
    Returned,
    /// The closure panicked.
    Panicked,
    /// A synchronous memory fault. `protection_key` is true when the kernel
    /// reported `SEGV_PKUERR`, a protection-key (PKU/POE) violation.
    Fault {
        signal: i32,
        code: i32,
        address: u64,
        protection_key: bool,
    },
    /// Killed by another signal.
    Signaled(i32),
    /// Exited with an unexpected status.
    Exited(i32),
    /// Still running at the timeout; it was killed.
    TimedOut,
}

/// Result of a [`contain`]ed run: every value the child sent, in order, and
/// how it ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contained {
    pub messages: Vec<u64>,
    pub exit: ChildExit,
}

/// Sends progress values from a contained child to the parent.
pub struct Reporter {
    fd: i32,
}
impl Reporter {
    /// Send one value. Values sent before a fault are still delivered.
    pub fn send(&mut self, value: u64) {
        hw::send(self.fd, value);
    }
}

/// Run `f` in a forked child process so a hardware fault ends the child, not
/// the caller. Linux only; requires isolation to be available, because its
/// purpose is containing protection-key faults. A child still running after
/// `timeout` is killed and reported as [`ChildExit::TimedOut`].
///
/// The child is a single-threaded copy of the caller. `f` must not wait on
/// locks or threads of the parent, print, or use file descriptors: the child
/// keeps only stdio and the report pipe, and leaves with `_exit` without
/// running destructors. Nothing it changes is visible to the parent except
/// the values sent through the [`Reporter`]. A panic in the child runs the
/// panic hook, which takes std's stderr and backtrace locks; if another
/// parent thread held them at the instant of the fork (it was itself
/// printing a panic), the child hangs until the timeout.
pub fn contain(
    timeout: std::time::Duration,
    f: impl FnOnce(&mut Reporter),
) -> Result<Contained, DomainError> {
    hw::contain(timeout, f)
}

/// The single module allowed to use `unsafe` in this crate. It compiles only
/// on Linux x86_64 (PKU) and Linux AArch64 (POE); every other target gets
/// the safe `hw` stub below, which reports [`UnavailableReason::UnsupportedTarget`].
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[allow(unsafe_code)]
mod hw {
    use super::*;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::atomic::{AtomicI32, Ordering};

    /// `si_code` for a protection-key fault (Linux uapi `SEGV_PKUERR`).
    const SEGV_PKUERR: i32 = 4;
    /// Child exit status after the fault handler reported a fault.
    const FAULT_EXIT: i32 = 86;
    const PANIC_EXIT: i32 = 101;
    /// Message tag preceding a fault record (never a progress value the
    /// shadow harness sends, which are case numbers below 2^63).
    const FAULT_TAG: u64 = u64::MAX;

    fn errno() -> i32 {
        std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
    }

    #[cfg(target_arch = "x86_64")]
    mod arch {
        use super::*;
        pub const MECHANISM: Mechanism = Mechanism::IntelPku;
        /// CPUID.(EAX=7,ECX=0):ECX bit 3 is PKU, bit 4 is OSPKE (the OS set
        /// CR4.PKE, so RDPKRU/WRPKRU do not fault with #UD).
        pub fn feature() -> Result<(), Unavailable> {
            #[allow(unused_unsafe)]
            // SAFETY: CPUID is available on every x86_64 CPU and has no side effects.
            let (max, ecx) = unsafe {
                let max = std::arch::x86_64::__cpuid(0).eax;
                let ecx = if max >= 7 {
                    std::arch::x86_64::__cpuid_count(7, 0).ecx
                } else {
                    0
                };
                (max, ecx)
            };
            if max < 7 || ecx & (1 << 3) == 0 {
                return Err(Unavailable {
                    reason: UnavailableReason::CpuLacksFeature,
                    detail: "CPUID reports no PKU (memory protection keys)".into(),
                });
            }
            if ecx & (1 << 4) == 0 {
                return Err(Unavailable {
                    reason: UnavailableReason::KernelLacksSupport,
                    detail: "CPU has PKU but the kernel has not enabled it (OSPKE clear)".into(),
                });
            }
            Ok(())
        }
        pub fn read() -> u64 {
            let eax: u32;
            // SAFETY: only reached after `feature()` confirmed OSPKE, so
            // RDPKRU is enabled; it reads the PKRU register (ECX must be 0).
            unsafe {
                std::arch::asm!(
                    "rdpkru",
                    in("ecx") 0u32,
                    lateout("eax") eax,
                    lateout("edx") _,
                    options(nomem, nostack, preserves_flags)
                );
            }
            u64::from(eax)
        }
        pub fn write(value: u64) {
            // SAFETY: OSPKE confirmed. WRPKRU only changes this thread's
            // access rights to keyed pages; no `nomem`, so the compiler
            // keeps memory accesses ordered around it. ECX and EDX must be 0.
            unsafe {
                std::arch::asm!(
                    "wrpkru",
                    in("eax") value as u32,
                    in("ecx") 0u32,
                    in("edx") 0u32,
                    options(nostack, preserves_flags)
                );
            }
        }
        /// PKRU: bit 2k is access-disable, bit 2k+1 write-disable.
        pub fn revoked(saved: u64, key: i32, revoke: Revoke) -> u64 {
            let shift = 2 * key as u64;
            saved
                | match revoke {
                    Revoke::Write => 0b10 << shift,
                    Revoke::Access => 0b11 << shift,
                }
        }
        pub fn rights(value: u64, key: i32) -> Rights {
            let bits = (value >> (2 * key as u64)) & 0b11;
            Rights {
                read: bits & 0b01 == 0,
                write: bits == 0,
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    mod arch {
        use super::*;
        pub const MECHANISM: Mechanism = Mechanism::ArmPoe;
        /// Linux uapi `HWCAP2_POE`.
        const HWCAP2_POE: u64 = 1 << 63;
        const POE_R: u64 = 0x1;
        const POE_W: u64 = 0x4;
        pub fn feature() -> Result<(), Unavailable> {
            // SAFETY: getauxval only reads the process auxiliary vector.
            let hwcap2 = unsafe { libc::getauxval(libc::AT_HWCAP2) } as u64;
            if hwcap2 & HWCAP2_POE == 0 {
                return Err(Unavailable {
                    reason: UnavailableReason::CpuLacksFeature,
                    detail: "the kernel reports no HWCAP2_POE (Permission Overlay Extension)"
                        .into(),
                });
            }
            Ok(())
        }
        pub fn read() -> u64 {
            let value: u64;
            // SAFETY: HWCAP2_POE confirmed, so EL0 access to POR_EL0
            // (S3_3_C10_C2_4) is enabled by the kernel.
            unsafe {
                std::arch::asm!(
                    "mrs {v}, S3_3_C10_C2_4",
                    v = out(reg) value,
                    options(nomem, nostack, preserves_flags)
                );
            }
            value
        }
        pub fn write(value: u64) {
            // SAFETY: HWCAP2_POE confirmed. Changes only this thread's
            // overlay permissions; ISB makes them effective before any
            // later access. No `nomem`, so accesses stay ordered.
            unsafe {
                std::arch::asm!(
                    "msr S3_3_C10_C2_4, {v}",
                    "isb",
                    v = in(reg) value,
                    options(nostack, preserves_flags)
                );
            }
        }
        /// POR_EL0: 4 bits per key (R = 1, X = 2, W = 4).
        pub fn revoked(saved: u64, key: i32, revoke: Revoke) -> u64 {
            let shift = 4 * key as u64;
            saved
                & !(match revoke {
                    Revoke::Write => POE_W,
                    Revoke::Access => POE_W | POE_R,
                } << shift)
        }
        pub fn rights(value: u64, key: i32) -> Rights {
            let bits = (value >> (4 * key as u64)) & 0xf;
            Rights {
                read: bits & POE_R != 0,
                write: bits & POE_W != 0,
            }
        }
    }

    pub fn alloc_key() -> Result<(Mechanism, i32), Unavailable> {
        arch::feature()?;
        // SAFETY: pkey_alloc(flags = 0, access_rights = 0) takes no pointers.
        let key = unsafe { libc::syscall(libc::SYS_pkey_alloc, 0, 0) };
        if key < 0 {
            let e = errno();
            return Err(match e {
                libc::ENOSYS => Unavailable {
                    reason: UnavailableReason::KernelLacksSupport,
                    detail: "the kernel does not implement pkey_alloc".into(),
                },
                libc::EINVAL => Unavailable {
                    reason: UnavailableReason::KernelLacksSupport,
                    detail: "pkey_alloc rejected the request; protection keys are disabled".into(),
                },
                libc::ENOSPC => Unavailable {
                    reason: UnavailableReason::KeysExhausted,
                    detail: "no free protection key (or keys disabled by the kernel)".into(),
                },
                e => Unavailable {
                    reason: UnavailableReason::Os(e),
                    detail: format!("pkey_alloc failed with errno {e}"),
                },
            });
        }
        Ok((arch::MECHANISM, key as i32))
    }
    pub fn free_key(key: i32) {
        // SAFETY: frees a key this process allocated; callers unmap every
        // region tagged with it first.
        unsafe { libc::syscall(libc::SYS_pkey_free, key) };
    }
    pub fn map_tagged(len: usize, key: i32) -> Result<(usize, usize), DomainError> {
        // SAFETY: sysconf has no memory effects.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        let page = usize::try_from(page)
            .ok()
            .filter(|p| *p > 0)
            .unwrap_or(4096);
        let mapped = len.div_ceil(page) * page;
        // SAFETY: a fresh private anonymous mapping; it aliases nothing.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                mapped,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if base == libc::MAP_FAILED {
            return Err(DomainError::Os {
                operation: "mmap",
                errno: errno(),
            });
        }
        // SAFETY: tags exactly the mapping created above.
        let r = unsafe {
            libc::syscall(
                libc::SYS_pkey_mprotect,
                base,
                mapped,
                libc::PROT_READ | libc::PROT_WRITE,
                key,
            )
        };
        if r != 0 {
            let e = errno();
            unmap(base as usize, mapped);
            return Err(DomainError::Os {
                operation: "pkey_mprotect",
                errno: e,
            });
        }
        Ok((base as usize, mapped))
    }
    pub fn unmap(base: usize, mapped: usize) {
        // SAFETY: unmaps a mapping this module created; no references into
        // it exist because region memory is only copied through.
        unsafe { libc::munmap(base as *mut libc::c_void, mapped) };
    }
    pub fn copy_out(at: usize, out: &mut [u8]) {
        // SAFETY: `at..at + out.len()` was bounds-checked against a live
        // mapping owned by the domain; the mapping is never referenced as a
        // Rust slice, so the copy aliases no reference.
        unsafe { std::ptr::copy_nonoverlapping(at as *const u8, out.as_mut_ptr(), out.len()) }
    }
    pub fn copy_in(at: usize, bytes: &[u8]) {
        // SAFETY: as in `copy_out`; the domain is !Sync, so no other thread
        // copies concurrently through the same domain.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), at as *mut u8, bytes.len()) }
    }
    pub fn read_register() -> u64 {
        arch::read()
    }
    pub fn write_register(value: u64) {
        arch::write(value)
    }
    pub fn revoked(saved: u64, key: i32, revoke: Revoke) -> u64 {
        arch::revoked(saved, key, revoke)
    }
    pub fn rights(value: u64, key: i32) -> Rights {
        arch::rights(value, key)
    }

    pub fn send(fd: i32, value: u64) {
        let bytes = value.to_le_bytes();
        // SAFETY: writes 8 bytes from a live local array to the pipe the
        // parent reads. A short write cannot happen for <= PIPE_BUF bytes.
        unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
    }

    /// The fault-report pipe of the current contained child (-1 outside one).
    static FAULT_FD: AtomicI32 = AtomicI32::new(-1);

    extern "C" fn on_fault(signal: i32, info: *mut libc::siginfo_t, _: *mut libc::c_void) {
        // Async-signal-safe only: atomics, write(2) and _exit(2).
        let fd = FAULT_FD.load(Ordering::Relaxed);
        // SAFETY: the kernel passes a valid siginfo for SA_SIGINFO handlers.
        let (code, address) = unsafe { ((*info).si_code, (*info).si_addr() as u64) };
        for v in [FAULT_TAG, signal as u64, code as u32 as u64, address] {
            send(fd, v);
        }
        // SAFETY: terminates the child immediately without unwinding.
        unsafe { libc::_exit(FAULT_EXIT) };
    }

    pub fn contain(
        timeout: std::time::Duration,
        f: impl FnOnce(&mut Reporter),
    ) -> Result<Contained, DomainError> {
        // Containment exists for protection-key faults.
        probe()?;
        let os = |operation| DomainError::Os {
            operation,
            errno: errno(),
        };
        let mut fds = [0i32; 2];
        // SAFETY: pipe2 writes two descriptors into the local array.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return Err(os("pipe2"));
        }
        let [read_fd, write_fd] = fds;
        // SAFETY: the child only runs `f` (documented to avoid parent
        // locks, threads and descriptors), async-signal-safe reporting, and
        // `_exit`.
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            let e = os("fork");
            // SAFETY: closing descriptors created above.
            unsafe {
                libc::close(read_fd);
                libc::close(write_fd);
            }
            return Err(e);
        }
        if pid == 0 {
            child(read_fd, write_fd, f);
        }
        // Parent.
        // SAFETY: closes the child's write end in the parent.
        unsafe { libc::close(write_fd) };
        let deadline = std::time::Instant::now() + timeout;
        let mut bytes = vec![];
        let mut buf = [0u8; 4096];
        let mut timed_out = false;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                timed_out = true;
                break;
            }
            let mut poll = libc::pollfd {
                fd: read_fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let ms = i32::try_from(left.as_millis().max(1)).unwrap_or(i32::MAX);
            // SAFETY: polls one live descriptor described by a local struct.
            let ready = unsafe { libc::poll(&mut poll, 1, ms) };
            if ready < 0 {
                if errno() == libc::EINTR {
                    continue;
                }
                break;
            }
            if ready == 0 {
                continue;
            }
            // SAFETY: reads into a live local buffer of the stated length.
            let n = unsafe { libc::read(read_fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n > 0 {
                bytes.extend_from_slice(&buf[..n as usize]);
            } else if n == 0 || errno() != libc::EINTR {
                break;
            }
        }
        if timed_out {
            // SAFETY: kills the child forked above (it may already be gone).
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        // SAFETY: closing the read end created above.
        unsafe { libc::close(read_fd) };
        let mut status = 0i32;
        loop {
            // SAFETY: waits for the child forked above.
            let r = unsafe { libc::waitpid(pid, &mut status, 0) };
            if r == pid {
                break;
            }
            if r < 0 && errno() != libc::EINTR {
                return Err(os("waitpid"));
            }
        }
        let mut messages: Vec<u64> = bytes
            .chunks_exact(8)
            .map(|c| u64::from_le_bytes(c.try_into().expect("8-byte chunk")))
            .collect();
        let fault = messages
            .len()
            .checked_sub(4)
            .filter(|i| messages[*i] == FAULT_TAG)
            .map(|i| {
                let record = messages.split_off(i);
                (record[1] as i32, record[2] as u32 as i32, record[3])
            });
        let exit = if timed_out {
            ChildExit::TimedOut
        } else if libc::WIFEXITED(status) {
            match (libc::WEXITSTATUS(status), fault) {
                (FAULT_EXIT, Some((signal, code, address))) => ChildExit::Fault {
                    signal,
                    code,
                    address,
                    protection_key: code == SEGV_PKUERR,
                },
                (0, _) => ChildExit::Returned,
                (PANIC_EXIT, _) => ChildExit::Panicked,
                (code, _) => ChildExit::Exited(code),
            }
        } else if libc::WIFSIGNALED(status) {
            ChildExit::Signaled(libc::WTERMSIG(status))
        } else {
            ChildExit::Exited(-1)
        };
        Ok(Contained { messages, exit })
    }

    fn child(read_fd: i32, write_fd: i32, f: impl FnOnce(&mut Reporter)) -> ! {
        // SAFETY: in the forked child: drop inherited descriptors except
        // stdio and the report pipe (so other contained runs see EOF), and
        // install SIGSEGV/SIGBUS handlers that use only async-signal-safe
        // calls. close_range errors (old kernels) are harmless.
        unsafe {
            libc::close(read_fd);
            if write_fd > 3 {
                libc::syscall(libc::SYS_close_range, 3u32, (write_fd - 1) as u32, 0u32);
            }
            libc::syscall(libc::SYS_close_range, (write_fd + 1) as u32, u32::MAX, 0u32);
            FAULT_FD.store(write_fd, Ordering::Relaxed);
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_fault as *const () as usize;
            action.sa_flags = libc::SA_SIGINFO;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(libc::SIGSEGV, &action, std::ptr::null_mut());
            libc::sigaction(libc::SIGBUS, &action, std::ptr::null_mut());
        }
        let mut reporter = Reporter { fd: write_fd };
        let status = match catch_unwind(AssertUnwindSafe(|| f(&mut reporter))) {
            Ok(()) => 0,
            Err(_) => PANIC_EXIT,
        };
        // SAFETY: leave without running parent-owned destructors.
        unsafe { libc::_exit(status) }
    }
}

/// Safe stub for targets without PKU/POE support in this crate.
#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
mod hw {
    use super::*;
    fn unsupported() -> Unavailable {
        Unavailable {
            reason: UnavailableReason::UnsupportedTarget,
            detail: "hardware isolation domains are implemented for Linux x86_64 (PKU) and Linux AArch64 (POE) only".into(),
        }
    }
    pub fn alloc_key() -> Result<(Mechanism, i32), Unavailable> {
        Err(unsupported())
    }
    // A domain can never be constructed here, so the rest is unreachable.
    pub fn free_key(_: i32) {}
    pub fn map_tagged(_: usize, _: i32) -> Result<(usize, usize), DomainError> {
        Err(unsupported().into())
    }
    pub fn unmap(_: usize, _: usize) {}
    pub fn copy_out(_: usize, _: &mut [u8]) {
        unreachable!("no isolation domain exists on this target")
    }
    pub fn copy_in(_: usize, _: &[u8]) {
        unreachable!("no isolation domain exists on this target")
    }
    pub fn read_register() -> u64 {
        0
    }
    pub fn write_register(_: u64) {}
    pub fn revoked(saved: u64, _: i32, _: Revoke) -> u64 {
        saved
    }
    pub fn rights(_: u64, _: i32) -> Rights {
        Rights {
            read: true,
            write: true,
        }
    }
    pub fn send(_: i32, _: u64) {}
    pub fn contain(
        _: std::time::Duration,
        _: impl FnOnce(&mut Reporter),
    ) -> Result<Contained, DomainError> {
        Err(unsupported().into())
    }
}
