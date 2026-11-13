//! The one module where this crate allows `unsafe` (wave-five contracts,
//! lane 1): page-aligned locked secret memory and the AArch64 DIT register.
//!
//! Page allocation and `mlock`/`VirtualLock` come from the mature `region`
//! crate; `madvise(MADV_DONTDUMP)` comes from `libc` on Linux and Android.
//! Every `unsafe` block below states the invariant it relies on.
#![allow(unsafe_code)]

use zeroize::Zeroize;

/// Whether a platform protection was applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protection {
    /// The protection is in force for this allocation.
    Applied,
    /// The platform call was made and failed (for example `RLIMIT_MEMLOCK`).
    Failed {
        /// OS error code, when the OS reported one.
        os_error: Option<i32>,
    },
    /// This platform has no such protection, so none was attempted.
    Unavailable,
}

impl Protection {
    /// Is the protection in force?
    pub fn is_applied(self) -> bool {
        matches!(self, Protection::Applied)
    }
}

fn os_error(e: &region::Error) -> Option<i32> {
    match e {
        region::Error::SystemCall(io) => io.raw_os_error(),
        _ => None,
    }
}

/// Page-aligned read-write memory, optionally locked and excluded from core
/// dumps, wiped with volatile writes before it is unlocked and freed.
pub(crate) struct LockedPages {
    // Field order matters for the implicit drops after `Drop::drop` runs:
    // unlock (guard) before free (alloc).
    guard: Option<region::LockGuard>,
    alloc: region::Allocation,
    len: usize,
    pub(crate) mlock: Protection,
    pub(crate) dontdump: Protection,
}

// SAFETY: `region::Allocation` is `!Send`/`!Sync` only because it holds a raw
// pointer. `LockedPages` uniquely owns its mapping, hands out references only
// through `&self`/`&mut self` borrows, and the OS calls it makes (munlock,
// munmap / VirtualUnlock, VirtualFree) are valid from any thread.
unsafe impl Send for LockedPages {}
// SAFETY: shared access only yields `&[u8]`, which is `Sync`.
unsafe impl Sync for LockedPages {}

impl LockedPages {
    /// Map at least `len` bytes (rounded up to whole pages), zero-filled by
    /// the OS. `len` must be non-zero.
    pub(crate) fn new(len: usize) -> Result<Self, region::Error> {
        let alloc = region::alloc(len, region::Protection::READ_WRITE)?;
        let (guard, mlock) = match region::lock(alloc.as_ptr::<u8>(), alloc.len()) {
            Ok(g) => (Some(g), Protection::Applied),
            Err(e) => (
                None,
                Protection::Failed {
                    os_error: os_error(&e),
                },
            ),
        };
        let dontdump = dontdump(&alloc);
        Ok(Self {
            guard,
            alloc,
            len,
            mlock,
            dontdump,
        })
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        // SAFETY: `alloc` is a live READ_WRITE mapping of `alloc.len() >= len`
        // bytes owned by `self`; the returned borrow is tied to `&self`, so no
        // `&mut` alias can exist at the same time.
        unsafe { std::slice::from_raw_parts(self.alloc.as_ptr::<u8>(), self.len) }
    }

    pub(crate) fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as in `bytes`, and `&mut self` guarantees exclusivity.
        unsafe { std::slice::from_raw_parts_mut(self.alloc.as_mut_ptr::<u8>(), self.len) }
    }

    /// Volatile zero-fill of the whole mapping (not only `len`), followed by
    /// a compiler fence (inside `zeroize`) and a hardware fence.
    pub(crate) fn wipe(&mut self) {
        let whole = self.alloc.len();
        // SAFETY: the whole mapping is READ_WRITE and exclusively borrowed.
        let all = unsafe { std::slice::from_raw_parts_mut(self.alloc.as_mut_ptr::<u8>(), whole) };
        all.zeroize();
        std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
    }

    /// Volatile read-back used to confirm a wipe.
    pub(crate) fn all_zero(&self) -> bool {
        let base = self.alloc.as_ptr::<u8>();
        let mut acc = 0u8;
        for i in 0..self.alloc.len() {
            // SAFETY: `i < alloc.len()`, inside the live mapping; volatile so
            // the read-back is not elided after the volatile wipe.
            acc |= unsafe { std::ptr::read_volatile(base.add(i)) };
        }
        acc == 0
    }

    pub(crate) fn mapped_len(&self) -> usize {
        self.alloc.len()
    }

    pub(crate) fn is_page_aligned(&self) -> bool {
        (self.alloc.as_ptr::<u8>() as usize).is_multiple_of(region::page::size())
    }
}

impl Drop for LockedPages {
    fn drop(&mut self) {
        // Always wipe before unlocking and unmapping, whatever path led here.
        self.wipe();
        drop(self.guard.take());
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn dontdump(alloc: &region::Allocation) -> Protection {
    // SAFETY: the range is exactly a live, page-aligned mapping we own;
    // MADV_DONTDUMP changes only core-dump inclusion, not its contents.
    let rc = unsafe {
        libc::madvise(
            alloc.as_ptr::<u8>() as *mut libc::c_void,
            alloc.len(),
            libc::MADV_DONTDUMP,
        )
    };
    if rc == 0 {
        Protection::Applied
    } else {
        Protection::Failed {
            os_error: std::io::Error::last_os_error().raw_os_error(),
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn dontdump(_alloc: &region::Allocation) -> Protection {
    Protection::Unavailable
}

/// AArch64 `PSTATE.DIT` access through the generic system-register name of
/// `DIT` (op0=3, op1=3, CRn=4, CRm=2, op2=5), so no assembler extension flag
/// is needed. Callers must have checked FEAT_DIT first.
#[cfg(target_arch = "aarch64")]
pub(crate) mod dit {
    const DIT_BIT: u64 = 1 << 24;

    pub(crate) fn supported() -> bool {
        std::arch::is_aarch64_feature_detected!("dit")
    }

    /// # Panics
    /// Never; returns `None` when FEAT_DIT is absent.
    pub(crate) fn read() -> Option<bool> {
        if !supported() {
            return None;
        }
        let v: u64;
        // SAFETY: FEAT_DIT is present (checked above), so reading DIT is a
        // defined EL0 access; it touches no memory and no flags.
        unsafe {
            core::arch::asm!("mrs {v}, s3_3_c4_c2_5", v = out(reg) v,
                options(nomem, nostack, preserves_flags));
        }
        Some(v & DIT_BIT != 0)
    }

    /// Returns `false` (and does nothing) when FEAT_DIT is absent.
    pub(crate) fn write(on: bool) -> bool {
        if !supported() {
            return false;
        }
        let v: u64 = if on { DIT_BIT } else { 0 };
        // SAFETY: FEAT_DIT is present; writing DIT at EL0 only changes the
        // timing mode of this thread's data-processing instructions.
        unsafe {
            core::arch::asm!("msr s3_3_c4_c2_5, {v}", v = in(reg) v,
                options(nomem, nostack, preserves_flags));
        }
        true
    }
}
