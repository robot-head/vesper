//! Kernel-assisted write tracking for checkpointable memory (§31.1).
//!
//! A [`TrackedRegion`] is a private, page-aligned anonymous mapping whose
//! writes are recorded per page by the memory-management unit, so stores
//! through raw pointers (generated code, FFI, `ptr::write`) are seen, not
//! only writes through a Rust API. [`TrackedRegion::take_dirty`] returns the
//! pages written since the previous call (or since [`TrackedRegion::arm`])
//! and re-arms the tracking.
//!
//! Mechanisms ([`Mechanism`]):
//! * Linux and macOS: `mprotect` faults. Arming makes the region read-only;
//!   the first store to a page raises `SIGSEGV` (Linux) or `SIGBUS`
//!   (macOS), and a process-wide handler that owns no locks marks the page
//!   dirty in an atomic bitmap and makes just that page writable again, so
//!   the faulting store is restarted and succeeds. Each page costs at most
//!   one fault per epoch. Faults outside every tracked region are passed to
//!   the previously installed handler (for example the Rust runtime's stack
//!   overflow handler) or, for the default action, re-raised with it.
//! * Windows: write watches. The region is allocated with
//!   `MEM_WRITE_WATCH` and every page is touched once (the first access to
//!   a demand-zero page, even a read, is otherwise reported as a write);
//!   the kernel records written pages and `GetWriteWatch` with
//!   `WRITE_WATCH_FLAG_RESET` reads and clears them. No handler runs.
//!
//! Not used: Linux `userfaultfd` write-protect (needs `CAP_SYS_PTRACE` or
//! `vm.unprivileged_userfaultfd=1`, which CI runners usually lack, and a
//! fault-servicing thread or a 6.7+ kernel for asynchronous mode), the
//! soft-dirty bit (process-wide, absent on some kernels) and Mach
//! `vm_copy` snapshots (the region is checkpointed by copying only the dirty
//! pages instead).
//!
//! Limits: at most [`MAX_REGIONS`] regions are tracked at once per process
//! on Unix; memory must be written by the process itself (a `read(2)` into
//! an armed page fails with `EFAULT` on Unix instead of faulting, since the
//! kernel does not raise signals for its own accesses; on Windows the write
//! watch records such writes); the Unix handler runs on the faulting
//! thread's stack, or its alternate signal stack if one is installed. A
//! fault in a page that is already writable (two threads racing on the same
//! page) is benign: the bit is set again and the page stays writable.
use std::io;

/// Most tracked regions alive at once on Unix (slots of the fault handler's
/// lock-free table).
pub const MAX_REGIONS: usize = 64;

/// How a region's writes are recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mechanism {
    /// `mprotect(PROT_READ)` plus a `SIGSEGV`/`SIGBUS` handler (Linux,
    /// macOS).
    MprotectFaults,
    /// `MEM_WRITE_WATCH` and `GetWriteWatch` (Windows).
    WindowsWriteWatch,
}

impl Mechanism {
    /// The mechanism [`TrackedRegion`] uses on this host.
    pub const HOST: Mechanism = if cfg!(windows) {
        Mechanism::WindowsWriteWatch
    } else {
        Mechanism::MprotectFaults
    };
    pub fn name(self) -> &'static str {
        match self {
            Mechanism::MprotectFaults => "mprotect-faults",
            Mechanism::WindowsWriteWatch => "windows-write-watch",
        }
    }
}

/// The system page size.
pub fn page_size() -> usize {
    sys::page_size()
}

/// Counters of one region.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TrackingStats {
    /// Times the region was (re-)armed.
    pub arms: u64,
    /// Distinct dirty pages reported by [`TrackedRegion::take_dirty`].
    pub dirty_pages: u64,
}

/// A page-aligned mapping whose writes are tracked by the kernel. See the
/// module docs.
pub struct TrackedRegion {
    base: *mut u8,
    len: usize,
    page: usize,
    armed: bool,
    stats: TrackingStats,
    #[cfg(unix)]
    slot: usize,
}

impl std::fmt::Debug for TrackedRegion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrackedRegion")
            .field("base", &self.base)
            .field("len", &self.len)
            .field("page", &self.page)
            .field("armed", &self.armed)
            .field("mechanism", &Mechanism::HOST)
            .finish()
    }
}

impl TrackedRegion {
    /// Map `len` bytes (rounded up to whole pages) of zeroed memory. The
    /// region starts unarmed: call [`Self::arm`] to start an epoch.
    pub fn new(len: usize) -> io::Result<Self> {
        let page = page_size();
        if len == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "tracked region must not be empty",
            ));
        }
        let len = len
            .checked_next_multiple_of(page)
            .filter(|l| *l <= isize::MAX as usize)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "region too large"))?;
        sys::create(len, page)
    }
    /// The tracking mechanism in use.
    pub fn mechanism(&self) -> Mechanism {
        Mechanism::HOST
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn page_size(&self) -> usize {
        self.page
    }
    pub fn pages(&self) -> usize {
        self.len / self.page
    }
    pub fn stats(&self) -> TrackingStats {
        self.stats
    }
    /// The first byte. Stores through this pointer (in bounds, while `self`
    /// is alive and not borrowed) are tracked.
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.base
    }
    pub fn as_ptr(&self) -> *const u8 {
        self.base
    }
    /// The address range `[start, end)`.
    pub fn range(&self) -> (usize, usize) {
        (self.base as usize, self.base as usize + self.len)
    }
    /// Whether an epoch is running.
    pub fn is_armed(&self) -> bool {
        self.armed
    }
    fn check(&self, offset: usize, len: usize) -> io::Result<()> {
        match offset.checked_add(len) {
            Some(end) if end <= self.len => Ok(()),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "range {offset}+{len} outside a {}-byte tracked region",
                    self.len
                ),
            )),
        }
    }
    /// Copy bytes out of the region.
    pub fn read(&self, offset: usize, out: &mut [u8]) -> io::Result<()> {
        self.check(offset, out.len())?;
        // SAFETY: in bounds of a live mapping that is always readable;
        // `&self` excludes concurrent writes through `as_mut_ptr` by the
        // safe API (raw writers must uphold the same rule).
        unsafe {
            std::ptr::copy_nonoverlapping(self.base.add(offset), out.as_mut_ptr(), out.len())
        };
        Ok(())
    }
    /// Copy bytes into the region. The write is tracked exactly like a raw
    /// store (it goes through the same pages).
    pub fn write(&mut self, offset: usize, data: &[u8]) -> io::Result<()> {
        self.check(offset, data.len())?;
        // SAFETY: in bounds of a live mapping; an armed page faults and is
        // made writable by the handler (Unix) or recorded by the kernel
        // (Windows), so the copy completes.
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), self.base.add(offset), data.len()) };
        Ok(())
    }
    /// Start a new epoch: forget every recorded write and track from now.
    pub fn arm(&mut self) -> io::Result<()> {
        sys::arm(self)?;
        self.armed = true;
        self.stats.arms += 1;
        Ok(())
    }
    /// Stop tracking (every page writable, nothing recorded). Pages written
    /// since the last arm are returned.
    pub fn disarm(&mut self) -> io::Result<Vec<usize>> {
        let dirty = sys::collect(self, false)?;
        self.armed = false;
        self.stats.dirty_pages += dirty.len() as u64;
        Ok(dirty)
    }
    /// The page indices written since the epoch started, ascending, and
    /// start the next epoch. An unarmed region is armed and reports nothing.
    pub fn take_dirty(&mut self) -> io::Result<Vec<usize>> {
        if !self.armed {
            self.arm()?;
            return Ok(vec![]);
        }
        let dirty = sys::collect(self, true)?;
        self.stats.arms += 1;
        self.stats.dirty_pages += dirty.len() as u64;
        Ok(dirty)
    }
}

impl Drop for TrackedRegion {
    fn drop(&mut self) {
        sys::destroy(self);
    }
}

#[cfg(unix)]
mod sys {
    use super::{MAX_REGIONS, TrackedRegion};
    use std::io;
    use std::sync::Once;
    use std::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering};

    /// One handler-visible region. `start == 0` is a free slot. A slot is
    /// claimed by CAS on `state` (0 free, 1 being set up, 2 live).
    struct Slot {
        state: AtomicUsize,
        start: AtomicUsize,
        len: AtomicUsize,
        page: AtomicUsize,
        bits: AtomicPtr<AtomicU64>,
        faults: AtomicU64,
        /// Handlers currently reading this slot. `destroy` unpublishes the
        /// slot and waits for this to drain before it unmaps, frees or
        /// lets the slot be reused, so a handler never mixes the fields of
        /// two regions or touches a freed bitmap.
        readers: AtomicUsize,
    }

    #[allow(clippy::declare_interior_mutable_const)]
    const FREE: Slot = Slot {
        state: AtomicUsize::new(0),
        start: AtomicUsize::new(0),
        len: AtomicUsize::new(0),
        page: AtomicUsize::new(0),
        bits: AtomicPtr::new(std::ptr::null_mut()),
        faults: AtomicU64::new(0),
        readers: AtomicUsize::new(0),
    };
    static SLOTS: [Slot; MAX_REGIONS] = [FREE; MAX_REGIONS];

    /// Previous dispositions of SIGSEGV and SIGBUS, saved once.
    static mut PREVIOUS: [std::mem::MaybeUninit<libc::sigaction>; 2] =
        [std::mem::MaybeUninit::uninit(); 2];
    static INSTALL: Once = Once::new();
    static INSTALL_ERROR: AtomicUsize = AtomicUsize::new(0);
    const SIGNALS: [libc::c_int; 2] = [libc::SIGSEGV, libc::SIGBUS];

    pub(super) fn page_size() -> usize {
        // SAFETY: sysconf has no preconditions.
        let p = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if p > 0 { p as usize } else { 4096 }
    }

    fn install() -> io::Result<()> {
        INSTALL.call_once(|| {
            for (i, &sig) in SIGNALS.iter().enumerate() {
                // SAFETY: a zeroed sigaction is a valid initial value; the
                // handler has the SA_SIGINFO signature. PREVIOUS is written
                // only here, under `Once`, before any handler can read it
                // (the handler is installed by this very call).
                unsafe {
                    let mut action: libc::sigaction = std::mem::zeroed();
                    action.sa_sigaction = handler as *const () as usize;
                    action.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
                    libc::sigemptyset(&mut action.sa_mask);
                    let previous = (&raw mut PREVIOUS[i]).cast::<libc::sigaction>();
                    if libc::sigaction(sig, &action, previous) != 0 {
                        INSTALL_ERROR.store(
                            io::Error::last_os_error().raw_os_error().unwrap_or(1) as usize,
                            Ordering::Relaxed,
                        );
                        return;
                    }
                }
            }
        });
        match INSTALL_ERROR.load(Ordering::Relaxed) {
            0 => Ok(()),
            code => Err(io::Error::from_raw_os_error(code as i32)),
        }
    }

    /// Async-signal-safe: atomics, `mprotect` and, for foreign faults, the
    /// previous handler or `sigaction` only.
    extern "C" fn handler(
        sig: libc::c_int,
        info: *mut libc::siginfo_t,
        context: *mut libc::c_void,
    ) {
        // SAFETY: the kernel passes a valid siginfo and ucontext for
        // SA_SIGINFO handlers.
        let (address, pc, protection) = unsafe {
            (
                fault_address(info),
                fault_pc(context),
                protection_fault(sig, info),
            )
        };
        // Only a store into a present, read-only page is ours. An
        // instruction fetch from the (never executable) region faults at
        // the instruction pointer, and unmapped-address faults are not
        // protection faults: making the page writable would not fix
        // either, so they go to the previous handler instead of looping.
        let executing = pc / 4096 == address / 4096;
        if protection && !executing && service(address) {
            return;
        }
        // Not ours (or the page could not be unprotected): hand it on.
        let i = if sig == libc::SIGSEGV { 0 } else { 1 };
        // SAFETY: PREVIOUS[i] was initialised by `install` before this
        // handler could run.
        unsafe {
            let previous = &*(&raw const PREVIOUS[i]).cast::<libc::sigaction>();
            let target = previous.sa_sigaction;
            if previous.sa_flags & libc::SA_SIGINFO != 0
                && target != libc::SIG_DFL
                && target != libc::SIG_IGN
            {
                let f: extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void) =
                    std::mem::transmute(target);
                f(sig, info, context);
            } else if target != libc::SIG_DFL && target != libc::SIG_IGN {
                let f: extern "C" fn(libc::c_int) = std::mem::transmute(target);
                f(sig);
            } else {
                // Restore the default action; returning re-executes the
                // faulting instruction, which now takes it.
                libc::sigaction(sig, previous, std::ptr::null_mut());
            }
        }
    }

    /// Record and unprotect the tracked page containing `address`; false if
    /// no live region contains it or the page cannot be unprotected.
    fn service(address: usize) -> bool {
        for slot in &SLOTS {
            slot.readers.fetch_add(1, Ordering::SeqCst);
            let mut done = None;
            if slot.state.load(Ordering::SeqCst) == 2 {
                let start = slot.start.load(Ordering::Relaxed);
                let len = slot.len.load(Ordering::Relaxed);
                if address >= start && address < start + len {
                    let page = slot.page.load(Ordering::Relaxed);
                    let index = (address - start) / page;
                    let bits = slot.bits.load(Ordering::Relaxed);
                    // SAFETY: the slot is live and `readers` keeps
                    // `destroy` from freeing the bitmap or unmapping the
                    // region; the page is inside the mapping.
                    unsafe {
                        (*bits.add(index / 64)).fetch_or(1 << (index % 64), Ordering::Relaxed);
                        slot.faults.fetch_add(1, Ordering::Relaxed);
                        let at = (start + index * page) as *mut libc::c_void;
                        done =
                            Some(libc::mprotect(at, page, libc::PROT_READ | libc::PROT_WRITE) == 0);
                    }
                }
            }
            slot.readers.fetch_sub(1, Ordering::SeqCst);
            if let Some(ok) = done {
                return ok;
            }
        }
        false
    }

    /// Whether the signal reports an access-permission fault (as opposed
    /// to an unmapped address or a bus error of another kind). macOS
    /// reports protection faults on mapped memory as `SIGBUS` with codes
    /// that vary by release, so only the Linux codes are checked.
    unsafe fn protection_fault(sig: libc::c_int, info: *mut libc::siginfo_t) -> bool {
        #[cfg(target_os = "linux")]
        {
            // SAFETY: caller passes a valid siginfo.
            let code = unsafe { (*info).si_code };
            sig == libc::SIGSEGV && code == SEGV_ACCERR
        }
        #[cfg(target_os = "macos")]
        {
            let _ = (sig, info);
            true
        }
    }
    #[cfg(target_os = "linux")]
    const SEGV_ACCERR: libc::c_int = 2;

    /// The interrupted instruction pointer.
    unsafe fn fault_pc(context: *mut libc::c_void) -> usize {
        let uc = context.cast::<libc::ucontext_t>();
        // SAFETY: the kernel passes the interrupted context.
        unsafe {
            #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
            {
                (*uc).uc_mcontext.gregs[libc::REG_RIP as usize] as usize
            }
            #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
            {
                (*uc).uc_mcontext.pc as usize
            }
            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
            {
                (*(*uc).uc_mcontext).__ss.__pc as usize
            }
            #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
            {
                (*(*uc).uc_mcontext).__ss.__rip as usize
            }
            #[cfg(not(any(
                all(
                    target_os = "linux",
                    any(target_arch = "x86_64", target_arch = "aarch64")
                ),
                target_os = "macos"
            )))]
            {
                let _ = uc;
                0
            }
        }
    }

    #[cfg(target_os = "linux")]
    unsafe fn fault_address(info: *mut libc::siginfo_t) -> usize {
        // SAFETY: caller passes a valid siginfo.
        unsafe { (*info).si_addr() as usize }
    }
    #[cfg(target_os = "macos")]
    unsafe fn fault_address(info: *mut libc::siginfo_t) -> usize {
        // SAFETY: caller passes a valid siginfo.
        unsafe { (*info).si_addr as usize }
    }

    pub(super) fn create(len: usize, page: usize) -> io::Result<TrackedRegion> {
        install()?;
        let words = (len / page).div_ceil(64);
        let bits: Box<[AtomicU64]> = (0..words).map(|_| AtomicU64::new(0)).collect();
        // SAFETY: anonymous private mapping; checked for MAP_FAILED.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        if base == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let Some(slot) = SLOTS.iter().position(|s| {
            s.state
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
        }) else {
            // SAFETY: unmapping the mapping created above.
            unsafe { libc::munmap(base, len) };
            return Err(io::Error::other(format!(
                "more than {MAX_REGIONS} tracked regions"
            )));
        };
        let s = &SLOTS[slot];
        s.start.store(base as usize, Ordering::Relaxed);
        s.len.store(len, Ordering::Relaxed);
        s.page.store(page, Ordering::Relaxed);
        s.faults.store(0, Ordering::Relaxed);
        s.bits
            .store(Box::into_raw(bits).cast::<AtomicU64>(), Ordering::Relaxed);
        s.state.store(2, Ordering::Release);
        Ok(TrackedRegion {
            base: base.cast(),
            len,
            page,
            armed: false,
            stats: Default::default(),
            slot,
        })
    }

    fn bits(region: &TrackedRegion) -> &'static [AtomicU64] {
        let words = region.pages().div_ceil(64);
        let ptr = SLOTS[region.slot].bits.load(Ordering::Relaxed);
        // SAFETY: allocated in `create` with `words` entries, freed only in
        // `destroy` after the slot is released.
        unsafe { std::slice::from_raw_parts(ptr, words) }
    }

    fn protect(region: &TrackedRegion, prot: libc::c_int) -> io::Result<()> {
        // SAFETY: the whole mapping owned by `region`.
        if unsafe { libc::mprotect(region.base.cast(), region.len, prot) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(super) fn arm(region: &mut TrackedRegion) -> io::Result<()> {
        // Clear, then protect: the epoch begins once every page is
        // read-only, so nothing written after `arm` returns is missed.
        for w in bits(region) {
            w.store(0, Ordering::Relaxed);
        }
        protect(region, libc::PROT_READ)
    }

    pub(super) fn collect(region: &mut TrackedRegion, rearm: bool) -> io::Result<Vec<usize>> {
        let mut dirty = vec![];
        for (i, w) in bits(region).iter().enumerate() {
            let mut word = w.swap(0, Ordering::AcqRel);
            while word != 0 {
                let bit = word.trailing_zeros() as usize;
                word &= word - 1;
                dirty.push(i * 64 + bit);
            }
        }
        if rearm {
            // Only written pages became writable; protect just those.
            for &p in &dirty {
                // SAFETY: page `p` lies inside the mapping.
                let at = unsafe { region.base.add(p * region.page) };
                if unsafe { libc::mprotect(at.cast(), region.page, libc::PROT_READ) } != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
        } else {
            protect(region, libc::PROT_READ | libc::PROT_WRITE)?;
        }
        Ok(dirty)
    }

    pub(super) fn destroy(region: &mut TrackedRegion) {
        let s = &SLOTS[region.slot];
        // Unpublish, then wait for handlers already reading the slot: after
        // that none can see this region's fields, so unmapping, freeing the
        // bitmap and reusing the slot are safe.
        s.state.store(1, Ordering::SeqCst);
        while s.readers.load(Ordering::SeqCst) != 0 {
            std::hint::spin_loop();
        }
        let bits = s.bits.swap(std::ptr::null_mut(), Ordering::AcqRel);
        s.start.store(0, Ordering::Relaxed);
        s.len.store(0, Ordering::Relaxed);
        // SAFETY: the mapping and bitmap were created by `create` for this
        // slot and nothing else refers to them now.
        unsafe {
            libc::munmap(region.base.cast(), region.len);
            let words = region.pages().div_ceil(64);
            drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                bits, words,
            )));
        }
        s.state.store(0, Ordering::Release);
    }

    /// Faults the handler serviced for `region` (for tests and probes).
    pub(super) fn faults(region: &TrackedRegion) -> u64 {
        SLOTS[region.slot].faults.load(Ordering::Relaxed)
    }
}

#[cfg(windows)]
mod sys {
    use super::TrackedRegion;
    use std::io;
    use windows_sys::Win32::System::Memory::{
        GetWriteWatch, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, ResetWriteWatch,
        VirtualAlloc, VirtualFree,
    };
    use windows_sys::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};

    // winnt.h values; windows-sys exports them from `SystemServices`, a
    // feature this crate does not otherwise need.
    const MEM_WRITE_WATCH: u32 = 0x0020_0000;
    const WRITE_WATCH_FLAG_RESET: u32 = 1;

    pub(super) fn page_size() -> usize {
        // SAFETY: GetSystemInfo fills the struct.
        unsafe {
            let mut info: SYSTEM_INFO = std::mem::zeroed();
            GetSystemInfo(&mut info);
            info.dwPageSize as usize
        }
    }

    pub(super) fn create(len: usize, page: usize) -> io::Result<TrackedRegion> {
        // SAFETY: a fresh committed reservation; checked for null.
        let base = unsafe {
            VirtualAlloc(
                std::ptr::null(),
                len,
                MEM_RESERVE | MEM_COMMIT | MEM_WRITE_WATCH,
                PAGE_READWRITE,
            )
        };
        if base.is_null() {
            return Err(io::Error::last_os_error());
        }
        // Touch every page once now. Windows materialises a demand-zero
        // page on its first access, a read included, and the write watch
        // then reports that page as written; pre-faulting keeps later reads
        // from showing up as writes (`arm` resets the watch afterwards).
        for p in (0..len).step_by(page) {
            // SAFETY: inside the committed allocation.
            unsafe { base.cast::<u8>().add(p).write_volatile(0) };
        }
        Ok(TrackedRegion {
            base: base.cast(),
            len,
            page,
            armed: false,
            stats: Default::default(),
        })
    }

    pub(super) fn arm(region: &mut TrackedRegion) -> io::Result<()> {
        // SAFETY: the whole MEM_WRITE_WATCH allocation.
        if unsafe { ResetWriteWatch(region.base.cast(), region.len) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(super) fn collect(region: &mut TrackedRegion, _rearm: bool) -> io::Result<Vec<usize>> {
        let mut addresses: Vec<*mut core::ffi::c_void> = vec![std::ptr::null_mut(); region.pages()];
        let mut count = addresses.len();
        let mut granularity = 0u32;
        // SAFETY: `addresses` has `count` entries; the range is our
        // allocation. The reset makes this read the start of the next epoch
        // (when disarming, a later arm resets again anyway).
        let status = unsafe {
            GetWriteWatch(
                WRITE_WATCH_FLAG_RESET,
                region.base.cast(),
                region.len,
                addresses.as_mut_ptr(),
                &mut count,
                &mut granularity,
            )
        };
        if status != 0 {
            return Err(io::Error::last_os_error());
        }
        let page = granularity.max(1) as usize;
        let mut dirty: Vec<usize> = addresses[..count]
            .iter()
            .map(|a| (*a as usize - region.base as usize) / page * page / region.page)
            .collect();
        dirty.sort_unstable();
        dirty.dedup();
        Ok(dirty)
    }

    pub(super) fn destroy(region: &mut TrackedRegion) {
        // SAFETY: releasing the allocation made in `create`.
        unsafe { VirtualFree(region.base.cast(), 0, MEM_RELEASE) };
    }

    pub(super) fn faults(_region: &TrackedRegion) -> u64 {
        0
    }
}

impl TrackedRegion {
    /// Write faults the Unix handler serviced for this region since it was
    /// created (always 0 on Windows, where the kernel records writes
    /// without a fault the process sees).
    pub fn serviced_faults(&self) -> u64 {
        sys::faults(self)
    }
}
