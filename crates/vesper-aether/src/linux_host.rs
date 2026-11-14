//! The crate's only unsafe code: userfaultfd and KVM ioctls on Linux.
//!
//! The userfaultfd ABI (structures and ioctl numbers) is the stable Linux UAPI
//! from `<linux/userfaultfd.h>`, declared here because the `userfaultfd` crate's
//! `-sys` crate runs bindgen against kernel headers at build time, which the
//! hermetic Bazel build cannot do. The numbers use the generic `_IOC` encoding
//! shared by x86_64 and AArch64; this module is built only there. KVM goes
//! through the `kvm-ioctls` crate. Every unsafe block states its invariant.
#![allow(unsafe_code)]

#[cfg(feature = "uffd")]
pub use uffd::*;
#[cfg(feature = "uffd")]
mod uffd {
    use crate::host::{UffdError, UffdProbe, UffdRoute, UffdStats, Unavailable};
    use crate::hotset::{HotSet, PREFETCH_BATCH_PAGES};
    use crate::page_source::PageSource;
    use crate::{Error, PAGE_BYTES};
    use std::marker::PhantomData;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::fs::FileExt;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

    const UFFD_API: u64 = 0xAA;
    const UFFDIO_API: u64 = 0xC018_AA3F;
    const UFFDIO_REGISTER: u64 = 0xC020_AA00;
    const UFFDIO_UNREGISTER: u64 = 0x8010_AA01;
    const UFFDIO_WAKE: u64 = 0x8010_AA02;
    const UFFDIO_COPY: u64 = 0xC028_AA03;
    const UFFDIO_ZEROPAGE: u64 = 0xC020_AA04;
    const UFFDIO_CONTINUE: u64 = 0xC020_AA07;
    const USERFAULTFD_IOC_NEW: u64 = 0xAA00;
    const UFFD_USER_MODE_ONLY: libc::c_int = 1;
    const UFFDIO_REGISTER_MODE_MISSING: u64 = 1;
    const UFFDIO_REGISTER_MODE_MINOR: u64 = 1 << 2;
    const UFFD_FEATURE_MINOR_SHMEM: u64 = 1 << 10;
    const UFFD_EVENT_PAGEFAULT: u8 = 0x12;
    const UFFD_PAGEFAULT_FLAG_MINOR: u64 = 1 << 2;
    const BIT_REGISTER: u64 = 1 << 0x00;
    const BIT_UNREGISTER: u64 = 1 << 0x01;
    const BIT_WAKE: u64 = 1 << 0x02;
    const BIT_COPY: u64 = 1 << 0x03;
    const BIT_ZEROPAGE: u64 = 1 << 0x04;
    const BIT_CONTINUE: u64 = 1 << 0x07;
    const CAP_SYS_PTRACE: u32 = 19;
    /// Every seal a shared snapshot memfd carries.
    pub const SNAPSHOT_SEALS: u32 =
        (libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE) as u32;
    /// 1 GiB per lazily restored region.
    pub const MAX_LAZY_PAGES: usize = 1 << 18;

    #[repr(C)]
    struct UffdioApi {
        api: u64,
        features: u64,
        ioctls: u64,
    }
    #[repr(C)]
    struct UffdioRange {
        start: u64,
        len: u64,
    }
    #[repr(C)]
    struct UffdioRegister {
        range: UffdioRange,
        mode: u64,
        ioctls: u64,
    }
    #[repr(C)]
    struct UffdioCopy {
        dst: u64,
        src: u64,
        len: u64,
        mode: u64,
        copy: i64,
    }
    #[repr(C)]
    struct UffdioZeropage {
        range: UffdioRange,
        mode: u64,
        zeropage: i64,
    }
    #[repr(C)]
    struct UffdioContinue {
        range: UffdioRange,
        mode: u64,
        mapped: i64,
    }
    /// `struct uffd_msg`: 8-byte header, then `pagefault.flags`,
    /// `pagefault.address` and the thread-id union.
    #[repr(C)]
    struct UffdMsg {
        event: u8,
        reserved1: u8,
        reserved2: u16,
        reserved3: u32,
        arg: [u64; 3],
    }
    const _: () = assert!(std::mem::size_of::<UffdMsg>() == 32);
    const _: () = assert!(std::mem::size_of::<UffdioApi>() == 24);
    const _: () = assert!(std::mem::size_of::<UffdioRegister>() == 32);
    const _: () = assert!(std::mem::size_of::<UffdioCopy>() == 40);
    const _: () = assert!(std::mem::size_of::<UffdioZeropage>() == 32);
    const _: () = assert!(std::mem::size_of::<UffdioContinue>() == 32);

    fn errno() -> i32 {
        std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
    }

    /// # Safety
    /// `arg` must point to a live, writable value of the structure that
    /// `request` names, for the duration of the call.
    unsafe fn ioctl_ptr<T>(fd: RawFd, request: u64, arg: *mut T) -> Result<(), i32> {
        // SAFETY: forwarded caller contract; the kernel reads/writes *arg only.
        let r = unsafe { libc::ioctl(fd, request as _, arg) };
        if r < 0 { Err(errno()) } else { Ok(()) }
    }

    fn adopt(fd: libc::c_long) -> Result<OwnedFd, i32> {
        if fd < 0 {
            return Err(errno());
        }
        let fd = RawFd::try_from(fd).map_err(|_| libc::EBADF)?;
        // SAFETY: the kernel just returned this new descriptor; we own it.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    fn open_uffd() -> Result<(OwnedFd, UffdRoute), i32> {
        let flags = libc::O_CLOEXEC | libc::O_NONBLOCK;
        // SAFETY: userfaultfd(2) takes one int and returns a descriptor or -1.
        let first = adopt(unsafe { libc::syscall(libc::SYS_userfaultfd, flags) });
        let first_err = match first {
            Ok(fd) => return Ok((fd, UffdRoute::Syscall)),
            Err(e) => e,
        };
        // SAFETY: as above, with UFFD_USER_MODE_ONLY (Linux 5.11+).
        let umo =
            adopt(unsafe { libc::syscall(libc::SYS_userfaultfd, flags | UFFD_USER_MODE_ONLY) });
        if let Ok(fd) = umo {
            return Ok((fd, UffdRoute::SyscallUserModeOnly));
        }
        // SAFETY: a valid NUL-terminated path; open returns a descriptor or -1.
        let dev = adopt(libc::c_long::from(unsafe {
            libc::open(c"/dev/userfaultfd".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC)
        }));
        if let Ok(dev) = dev {
            // SAFETY: USERFAULTFD_IOC_NEW takes the flags as a plain int.
            let fd = unsafe { libc::ioctl(dev.as_raw_fd(), USERFAULTFD_IOC_NEW as _, flags) };
            if let Ok(fd) = adopt(libc::c_long::from(fd)) {
                return Ok((fd, UffdRoute::DevNode));
            }
        }
        Err(first_err)
    }

    /// `UFFDIO_API` with `features` requested. A kernel without one of them
    /// answers `EINVAL`, reported as the missing feature.
    fn handshake(fd: RawFd, features: u64) -> Result<(), Unavailable> {
        let mut api = UffdioApi {
            api: UFFD_API,
            features,
            ioctls: 0,
        };
        // SAFETY: `api` is a live uffdio_api for the call.
        unsafe { ioctl_ptr(fd, UFFDIO_API, &mut api) }.map_err(|errno| {
            if errno == libc::EINVAL && features != 0 {
                Unavailable::MissingFeature("UFFD_FEATURE_MINOR_SHMEM")
            } else {
                Unavailable::KernelRefused {
                    op: "UFFDIO_API",
                    errno,
                }
            }
        })?;
        if api.ioctls & (BIT_REGISTER | BIT_UNREGISTER) != (BIT_REGISTER | BIT_UNREGISTER) {
            return Err(Unavailable::MissingFeature("UFFDIO_REGISTER"));
        }
        if api.features & features != features {
            return Err(Unavailable::MissingFeature("UFFD_FEATURE_MINOR_SHMEM"));
        }
        Ok(())
    }

    fn cap_sys_ptrace() -> bool {
        let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
            return false;
        };
        status
            .lines()
            .find_map(|l| l.strip_prefix("CapEff:"))
            .and_then(|v| u64::from_str_radix(v.trim(), 16).ok())
            .is_some_and(|caps| caps & (1 << CAP_SYS_PTRACE) != 0)
    }

    /// Reads the sysctl and capability, then actually opens a descriptor and
    /// runs the API handshake; `route` is `Ok` only if both succeeded.
    pub fn probe_userfaultfd() -> UffdProbe {
        let unprivileged_userfaultfd =
            std::fs::read_to_string("/proc/sys/vm/unprivileged_userfaultfd")
                .ok()
                .and_then(|s| s.trim().parse().ok());
        let cap_sys_ptrace = cap_sys_ptrace();
        let route = match open_uffd() {
            Ok((fd, route)) => handshake(fd.as_raw_fd(), 0).map(|()| route),
            Err(errno) if errno == libc::ENOSYS => {
                Err(Unavailable::MissingFeature("userfaultfd(2)"))
            }
            Err(errno) => Err(Unavailable::UserfaultfdDenied {
                unprivileged_userfaultfd,
                cap_sys_ptrace,
                errno,
            }),
        };
        UffdProbe {
            unprivileged_userfaultfd,
            cap_sys_ptrace,
            route,
        }
    }

    /// Opens a descriptor and handshakes with `features`.
    fn open_ready(features: u64) -> Result<(OwnedFd, UffdRoute), UffdError> {
        let probe = probe_userfaultfd();
        probe.route?;
        let (fd, route) = open_uffd().map_err(|errno| {
            UffdError::Unavailable(Unavailable::UserfaultfdDenied {
                unprivileged_userfaultfd: probe.unprivileged_userfaultfd,
                cap_sys_ptrace: probe.cap_sys_ptrace,
                errno,
            })
        })?;
        handshake(fd.as_raw_fd(), features)?;
        Ok((fd, route))
    }

    fn mmap(len: usize, flags: libc::c_int, fd: RawFd) -> Result<usize, UffdError> {
        // SAFETY: a fresh mapping at a kernel-chosen address; no existing
        // memory is affected. `fd` is -1 or a live descriptor we hold.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                flags,
                fd,
                0,
            )
        };
        if base == libc::MAP_FAILED {
            return Err(UffdError::Os {
                op: "mmap",
                errno: errno(),
            });
        }
        Ok(base as usize)
    }

    /// Physical frame number behind a virtual address, from
    /// `/proc/self/pagemap`. `None` if the page is not present or the kernel
    /// hides frame numbers (it does without `CAP_SYS_ADMIN`).
    fn frame_of(address: usize) -> Option<u64> {
        let f = std::fs::File::open("/proc/self/pagemap").ok()?;
        let mut e = [0u8; 8];
        f.read_exact_at(&mut e, (address / PAGE_BYTES) as u64 * 8)
            .ok()?;
        let entry = u64::from_le_bytes(e);
        let pfn = entry & ((1 << 55) - 1);
        (entry >> 63 == 1 && pfn != 0).then_some(pfn)
    }

    /// How one fault is resolved.
    trait Resolve: Sync {
        /// Resolves page `index` at `dst`. `Ok(Err(errno))` is an ioctl
        /// failure (EEXIST is a race the caller wakes); `Err` ends the session.
        fn resolve(
            &self,
            fd: RawFd,
            index: usize,
            dst: u64,
            stats: &mut UffdStats,
        ) -> Result<Result<(), i32>, UffdError>;
    }

    /// MISSING faults: fill from a page source with UFFDIO_COPY/ZEROPAGE.
    struct CopyFrom<'s, S>(&'s S);
    impl<S: PageSource + Sync> Resolve for CopyFrom<'_, S> {
        fn resolve(
            &self,
            fd: RawFd,
            index: usize,
            dst: u64,
            stats: &mut UffdStats,
        ) -> Result<Result<(), i32>, UffdError> {
            let fill = self
                .0
                .fill(index)
                .map_err(|error| UffdError::Source { index, error })?;
            let r = match fill.bytes() {
                None => {
                    let mut z = UffdioZeropage {
                        range: UffdioRange {
                            start: dst,
                            len: PAGE_BYTES as u64,
                        },
                        mode: 0,
                        zeropage: 0,
                    };
                    // SAFETY: live uffdio_zeropage over one page of the region.
                    unsafe { ioctl_ptr(fd, UFFDIO_ZEROPAGE, &mut z) }.map(|()| stats.zeroed += 1)
                }
                Some(bytes) => {
                    let mut c = UffdioCopy {
                        dst,
                        src: bytes.as_ptr() as u64,
                        len: PAGE_BYTES as u64,
                        mode: 0,
                        copy: 0,
                    };
                    // SAFETY: `src` is a live 4 KiB buffer (held by `fill`
                    // until after the call); `dst` is one page of the region.
                    unsafe { ioctl_ptr(fd, UFFDIO_COPY, &mut c) }.map(|()| stats.copied += 1)
                }
            };
            drop(fill);
            Ok(r)
        }
    }

    /// MINOR faults on a shared memfd: map the page-cache page in place.
    struct ContinueShared;
    fn continue_range(fd: RawFd, start: u64, len: u64) -> Result<(), i32> {
        let mut c = UffdioContinue {
            range: UffdioRange { start, len },
            mode: 0,
            mapped: 0,
        };
        // SAFETY: live uffdio_continue over pages of a MINOR-registered
        // mapping we own; the kernel maps existing page-cache pages only.
        unsafe { ioctl_ptr(fd, UFFDIO_CONTINUE, &mut c) }
    }
    impl Resolve for ContinueShared {
        fn resolve(
            &self,
            fd: RawFd,
            _index: usize,
            dst: u64,
            stats: &mut UffdStats,
        ) -> Result<Result<(), i32>, UffdError> {
            Ok(continue_range(fd, dst, PAGE_BYTES as u64).map(|()| stats.continued += 1))
        }
    }

    /// One registered mapping plus its userfaultfd.
    #[derive(Debug)]
    struct Registered {
        fd: OwnedFd,
        base: usize,
        len: usize,
        pages: usize,
        route: UffdRoute,
        minor: bool,
        unregistered: AtomicBool,
        serving: AtomicBool,
    }

    impl Registered {
        /// Maps (`map_fd` = -1: anonymous) and registers in `mode`, requiring `need`.
        fn new(
            pages: usize,
            features: u64,
            flags: libc::c_int,
            map_fd: RawFd,
            mode: u64,
            need: (u64, &'static str),
        ) -> Result<Self, UffdError> {
            if pages == 0 || pages > MAX_LAZY_PAGES {
                return Err(UffdError::InvalidRegion);
            }
            let len = pages * PAGE_BYTES;
            let (fd, route) = open_ready(features)?;
            let base = mmap(len, flags, map_fd)?;
            // From here Drop unmaps the region on every error path.
            let region = Self {
                fd,
                base,
                len,
                pages,
                route,
                minor: mode == UFFDIO_REGISTER_MODE_MINOR,
                unregistered: AtomicBool::new(false),
                serving: AtomicBool::new(false),
            };
            let mut reg = UffdioRegister {
                range: region.range(),
                mode,
                ioctls: 0,
            };
            // SAFETY: `reg` is a live uffdio_register; the range is our mapping.
            unsafe { ioctl_ptr(region.fd.as_raw_fd(), UFFDIO_REGISTER, &mut reg) }.map_err(
                |errno| UffdError::Os {
                    op: "UFFDIO_REGISTER",
                    errno,
                },
            )?;
            if reg.ioctls & need.0 != need.0 {
                return Err(UffdError::Unavailable(Unavailable::MissingFeature(need.1)));
            }
            Ok(region)
        }
        fn range(&self) -> UffdioRange {
            UffdioRange {
                start: self.base as u64,
                len: self.len as u64,
            }
        }
        fn page_address(&self, index: usize) -> u64 {
            self.base as u64 + (index * PAGE_BYTES) as u64
        }

        fn fail(&self, error: UffdError) -> UffdError {
            if !self.unregistered.swap(true, Ordering::SeqCst) {
                let mut range = self.range();
                // SAFETY: `range` is a live uffdio_range over our mapping.
                // Unregistering wakes threads blocked on MISSING faults only.
                let _ = unsafe { ioctl_ptr(self.fd.as_raw_fd(), UFFDIO_UNREGISTER, &mut range) };
                // The kernel does not wake MINOR waiters on unregister: wake
                // them explicitly (they then fault in the page normally).
                let _ = self.wake(self.base as u64, self.len as u64);
            }
            error
        }

        fn wake(&self, dst: u64, len: u64) -> Result<(), UffdError> {
            let mut range = UffdioRange { start: dst, len };
            // SAFETY: live uffdio_range over pages of our region.
            unsafe { ioctl_ptr(self.fd.as_raw_fd(), UFFDIO_WAKE, &mut range) }.map_err(|errno| {
                UffdError::Os {
                    op: "UFFDIO_WAKE",
                    errno,
                }
            })
        }

        /// Runs a fault-serving thread while `touch` runs on the calling
        /// thread. Returns `touch`'s result, the counts and the fault order.
        fn serve<Z, R, F>(
            &self,
            resolver: &Z,
            fault_budget: u64,
            touch: F,
        ) -> Result<(R, UffdStats, Vec<usize>), UffdError>
        where
            Z: Resolve,
            F: FnOnce(&Toucher<'_>) -> R,
        {
            if self.unregistered.load(Ordering::SeqCst) {
                return Err(UffdError::InvalidRegion);
            }
            // One session at a time: two servers would share one userfaultfd.
            if self.serving.swap(true, Ordering::SeqCst) {
                return Err(UffdError::Busy);
            }
            struct EndSession<'r>(&'r AtomicBool);
            impl Drop for EndSession<'_> {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::SeqCst);
                }
            }
            let _session = EndSession(&self.serving);
            // SAFETY: eventfd(2) returns a new descriptor or -1.
            let stop = adopt(libc::c_long::from(unsafe {
                libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK)
            }))
            .map_err(|errno| UffdError::Os {
                op: "eventfd",
                errno,
            })?;
            let stop_fd = stop.as_raw_fd();
            struct StopOnDrop(RawFd);
            impl Drop for StopOnDrop {
                fn drop(&mut self) {
                    let one: u64 = 1;
                    // SAFETY: writes 8 bytes from a live u64 to our eventfd.
                    let _ = unsafe { libc::write(self.0, (&raw const one).cast(), 8) };
                }
            }
            std::thread::scope(|scope| {
                let server = scope.spawn(|| {
                    // A panicking resolver must still release the toucher:
                    // unregister (waking it) before the panic propagates.
                    let served = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        self.server_loop(resolver, fault_budget, stop_fd)
                    }));
                    match served {
                        Ok(result) => result,
                        Err(panic) => {
                            self.fail(UffdError::InvalidRegion);
                            std::panic::resume_unwind(panic)
                        }
                    }
                });
                let result = {
                    let _stop = StopOnDrop(stop_fd);
                    touch(&self.toucher())
                };
                let (stats, order) = match server.join() {
                    Ok(r) => r?,
                    Err(panic) => std::panic::resume_unwind(panic),
                };
                Ok((result, stats, order))
            })
        }

        fn toucher(&self) -> Toucher<'_> {
            Toucher {
                base: self.base,
                len: self.len,
                _region: PhantomData,
            }
        }

        fn server_loop<Z: Resolve>(
            &self,
            resolver: &Z,
            budget: u64,
            stop_fd: RawFd,
        ) -> Result<(UffdStats, Vec<usize>), UffdError> {
            let fd = self.fd.as_raw_fd();
            let mut stats = UffdStats::default();
            let mut order = Vec::new();
            loop {
                let mut fds = [
                    libc::pollfd {
                        fd,
                        events: libc::POLLIN,
                        revents: 0,
                    },
                    libc::pollfd {
                        fd: stop_fd,
                        events: libc::POLLIN,
                        revents: 0,
                    },
                ];
                // `ppoll` rather than `poll`: the VMM seccomp profile allows
                // only `ppoll` (x86_64 glibc `poll` issues syscall 7).
                // SAFETY: `fds` is a live array of two pollfd records; a null
                // timeout and signal mask mean block indefinitely, unchanged mask.
                let n =
                    unsafe { libc::ppoll(fds.as_mut_ptr(), 2, std::ptr::null(), std::ptr::null()) };
                if n < 0 {
                    let e = errno();
                    if e == libc::EINTR {
                        continue;
                    }
                    return Err(self.fail(UffdError::Os {
                        op: "ppoll",
                        errno: e,
                    }));
                }
                if fds[0].revents != 0 {
                    self.drain(resolver, budget, &mut stats, &mut order)
                        .map_err(|e| self.fail(e))?;
                }
                if fds[1].revents != 0 {
                    return Ok((stats, order));
                }
            }
        }

        fn drain<Z: Resolve>(
            &self,
            resolver: &Z,
            budget: u64,
            stats: &mut UffdStats,
            order: &mut Vec<usize>,
        ) -> Result<(), UffdError> {
            let fd = self.fd.as_raw_fd();
            loop {
                let mut msg = UffdMsg {
                    event: 0,
                    reserved1: 0,
                    reserved2: 0,
                    reserved3: 0,
                    arg: [0; 3],
                };
                // SAFETY: reads at most 32 bytes into the live 32-byte `msg`.
                let n = unsafe { libc::read(fd, (&raw mut msg).cast(), 32) };
                if n < 0 {
                    let e = errno();
                    if e == libc::EAGAIN || e == libc::EINTR {
                        return Ok(());
                    }
                    return Err(UffdError::Os {
                        op: "read(userfaultfd)",
                        errno: e,
                    });
                }
                if n != 32 {
                    return Err(UffdError::Os {
                        op: "short read(userfaultfd)",
                        errno: 0,
                    });
                }
                if msg.event != UFFD_EVENT_PAGEFAULT {
                    return Err(UffdError::UnexpectedEvent(msg.event));
                }
                // A MINOR region reports only minor faults and vice versa.
                if (msg.arg[0] & UFFD_PAGEFAULT_FLAG_MINOR != 0) != self.minor {
                    return Err(UffdError::UnexpectedEvent(msg.event));
                }
                stats.faults += 1;
                if stats.faults > budget {
                    return Err(UffdError::FaultBudget);
                }
                let address = msg.arg[1];
                let offset = address
                    .checked_sub(self.base as u64)
                    .filter(|o| *o < self.len as u64)
                    .ok_or(UffdError::FaultOutsideRegion { address })?;
                let index = (offset / PAGE_BYTES as u64) as usize;
                order.push(index);
                let dst = self.page_address(index);
                match resolver.resolve(fd, index, dst, stats)? {
                    Ok(()) => {}
                    Err(e) if e == libc::EEXIST => {
                        stats.raced += 1;
                        self.wake(dst, PAGE_BYTES as u64)?;
                    }
                    Err(errno) => {
                        return Err(UffdError::Os {
                            op: "UFFDIO_COPY/ZEROPAGE/CONTINUE",
                            errno,
                        });
                    }
                }
            }
        }
    }

    impl Drop for Registered {
        fn drop(&mut self) {
            // SAFETY: unmaps exactly the mapping created in `new`; no Toucher
            // outlives the region (it borrows it).
            unsafe {
                libc::munmap(self.base as *mut libc::c_void, self.len);
            }
        }
    }

    /// An anonymous private mapping registered for missing-page faults. Each
    /// page is filled on first touch by [`LazyRegion::serve`].
    #[derive(Debug)]
    pub struct LazyRegion {
        reg: Registered,
    }

    impl LazyRegion {
        pub fn new(pages: usize) -> Result<Self, UffdError> {
            Ok(Self {
                reg: Registered::new(
                    pages,
                    0,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                    -1,
                    UFFDIO_REGISTER_MODE_MISSING,
                    (
                        BIT_WAKE | BIT_COPY | BIT_ZEROPAGE,
                        "UFFDIO_COPY/UFFDIO_ZEROPAGE",
                    ),
                )?,
            })
        }
        pub fn page_count(&self) -> usize {
            self.reg.pages
        }
        pub fn route(&self) -> UffdRoute {
            self.reg.route
        }

        /// Runs a fault-serving thread that fills pages from `source` while
        /// `touch` runs on the calling thread against the region. Returns
        /// `touch`'s result and the session counts. At most `fault_budget`
        /// faults are served; a source error, unexpected event or budget
        /// overrun unregisters the region (releasing the toucher) and fails.
        pub fn serve<S, R, F>(
            &self,
            source: &S,
            fault_budget: u64,
            touch: F,
        ) -> Result<(R, UffdStats), UffdError>
        where
            S: PageSource + Sync,
            F: FnOnce(&Toucher<'_>) -> R,
        {
            self.serve_recording(source, fault_budget, touch)
                .map(|(r, s, _)| (r, s))
        }

        /// [`LazyRegion::serve`], also returning the page index of every
        /// fault in arrival order (the input of a [`HotSet`]).
        pub fn serve_recording<S, R, F>(
            &self,
            source: &S,
            fault_budget: u64,
            touch: F,
        ) -> Result<(R, UffdStats, Vec<usize>), UffdError>
        where
            S: PageSource + Sync,
            F: FnOnce(&Toucher<'_>) -> R,
        {
            if source.page_count() != self.reg.pages {
                return Err(UffdError::InvalidRegion);
            }
            self.reg.serve(&CopyFrom(source), fault_budget, touch)
        }

        /// Resolves the hot set before any touch: one `UFFDIO_COPY` per run of
        /// up to 2 MiB. Pages already present (`EEXIST`) are retried one by
        /// one and counted as raced.
        pub fn prefetch<S: PageSource>(
            &self,
            source: &S,
            hot: &HotSet,
        ) -> Result<UffdStats, UffdError> {
            if source.page_count() != self.reg.pages || hot.page_count() != self.reg.pages {
                return Err(UffdError::InvalidRegion);
            }
            if self.reg.unregistered.load(Ordering::SeqCst) {
                return Err(UffdError::InvalidRegion);
            }
            let fd = self.reg.fd.as_raw_fd();
            let mut stats = UffdStats::default();
            let runs = hot
                .runs(PREFETCH_BATCH_PAGES)
                .map_err(|_| UffdError::InvalidRegion)?;
            let mut batch = vec![0u8; PREFETCH_BATCH_PAGES * PAGE_BYTES];
            for run in runs {
                for i in 0..run.pages {
                    let index = run.start + i;
                    let fill = source
                        .fill(index)
                        .map_err(|error| UffdError::Source { index, error })?;
                    let dst = &mut batch[i * PAGE_BYTES..(i + 1) * PAGE_BYTES];
                    match fill.bytes() {
                        Some(b) => dst.copy_from_slice(b),
                        None => dst.fill(0),
                    }
                }
                let len = (run.pages * PAGE_BYTES) as u64;
                let mut c = UffdioCopy {
                    dst: self.reg.page_address(run.start),
                    src: batch.as_ptr() as u64,
                    len,
                    mode: 0,
                    copy: 0,
                };
                // SAFETY: `src` is our live batch buffer of at least `len`
                // bytes; `dst..dst+len` lies in the registered region.
                match unsafe { ioctl_ptr(fd, UFFDIO_COPY, &mut c) } {
                    Ok(()) => {
                        stats.copied += run.pages as u64;
                        stats.prefetched += run.pages as u64;
                    }
                    Err(e) if e == libc::EEXIST || e == libc::EAGAIN => {
                        for i in 0..run.pages {
                            let mut c = UffdioCopy {
                                dst: self.reg.page_address(run.start + i),
                                src: batch[i * PAGE_BYTES..].as_ptr() as u64,
                                len: PAGE_BYTES as u64,
                                mode: 0,
                                copy: 0,
                            };
                            // SAFETY: as above, one page.
                            match unsafe { ioctl_ptr(fd, UFFDIO_COPY, &mut c) } {
                                Ok(()) => {
                                    stats.copied += 1;
                                    stats.prefetched += 1;
                                }
                                Err(e) if e == libc::EEXIST => stats.raced += 1,
                                Err(errno) => {
                                    return Err(UffdError::Os {
                                        op: "UFFDIO_COPY (prefetch)",
                                        errno,
                                    });
                                }
                            }
                        }
                    }
                    Err(errno) => {
                        return Err(UffdError::Os {
                            op: "UFFDIO_COPY (prefetch)",
                            errno,
                        });
                    }
                }
            }
            Ok(stats)
        }

        /// Physical frame of page `index` (see [`MinorRegion::frame_number`]).
        pub fn frame_number(&self, index: usize) -> Option<u64> {
            (index < self.reg.pages).then(|| frame_of(self.reg.page_address(index) as usize))?
        }
    }

    /// A memfd holding a frozen snapshot, sealed against writes, growth and
    /// shrinking (and against further sealing). Many [`MinorRegion`]s map it
    /// `MAP_PRIVATE` and share its page-cache pages until they write.
    #[derive(Debug)]
    pub struct SealedMemfd {
        file: std::fs::File,
        pages: usize,
    }

    impl SealedMemfd {
        /// Builds the memfd from `fill(index, page)`, which writes page
        /// `index` into the buffer (zeroed beforehand) or fails.
        pub fn build(
            pages: usize,
            mut fill: impl FnMut(usize, &mut [u8; PAGE_BYTES]) -> Result<bool, Error>,
        ) -> Result<Self, UffdError> {
            if pages == 0 || pages > MAX_LAZY_PAGES {
                return Err(UffdError::InvalidRegion);
            }
            let len = (pages * PAGE_BYTES) as u64;
            // SAFETY: a valid NUL-terminated name; returns a new fd or -1.
            let fd = adopt(libc::c_long::from(unsafe {
                libc::memfd_create(
                    c"vesper-aether-snapshot".as_ptr(),
                    libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
                )
            }))
            .map_err(|errno| UffdError::Os {
                op: "memfd_create",
                errno,
            })?;
            let file = std::fs::File::from(fd);
            file.set_len(len).map_err(|e| UffdError::Os {
                op: "ftruncate(memfd)",
                errno: e.raw_os_error().unwrap_or(0),
            })?;
            // Allocate every page so that each one is in the page cache and a
            // first touch is a minor fault (holes would fault in silently).
            // SAFETY: fallocate on our memfd over its own length.
            if unsafe { libc::fallocate(file.as_raw_fd(), 0, 0, len as libc::off_t) } < 0 {
                return Err(UffdError::Os {
                    op: "fallocate(memfd)",
                    errno: errno(),
                });
            }
            let mut page = Box::new([0u8; PAGE_BYTES]);
            for index in 0..pages {
                page.fill(0);
                let nonzero =
                    fill(index, &mut page).map_err(|error| UffdError::Source { index, error })?;
                if nonzero {
                    file.write_all_at(&page[..], (index * PAGE_BYTES) as u64)
                        .map_err(|e| UffdError::Os {
                            op: "pwrite(memfd)",
                            errno: e.raw_os_error().unwrap_or(0),
                        })?;
                }
            }
            // SAFETY: F_ADD_SEALS on our memfd with a constant seal set.
            if unsafe {
                libc::fcntl(
                    file.as_raw_fd(),
                    libc::F_ADD_SEALS,
                    SNAPSHOT_SEALS as libc::c_int,
                )
            } < 0
            {
                return Err(UffdError::Os {
                    op: "F_ADD_SEALS",
                    errno: errno(),
                });
            }
            let sealed = Self { file, pages };
            if sealed.seals()? & SNAPSHOT_SEALS != SNAPSHOT_SEALS {
                return Err(UffdError::Os {
                    op: "F_GET_SEALS",
                    errno: 0,
                });
            }
            Ok(sealed)
        }
        /// A sealed copy of `source` (e.g. a CAS slot).
        pub fn from_source<S: PageSource>(source: &S) -> Result<Self, UffdError> {
            Self::build(source.page_count(), |i, page| {
                let fill = source.fill(i)?;
                Ok(match fill.bytes() {
                    Some(b) => {
                        page.copy_from_slice(b);
                        true
                    }
                    None => false,
                })
            })
        }
        pub fn page_count(&self) -> usize {
            self.pages
        }
        /// The seals the kernel reports (`F_GET_SEALS`).
        pub fn seals(&self) -> Result<u32, UffdError> {
            // SAFETY: F_GET_SEALS on our memfd takes no argument.
            let r = unsafe { libc::fcntl(self.file.as_raw_fd(), libc::F_GET_SEALS) };
            if r < 0 {
                return Err(UffdError::Os {
                    op: "F_GET_SEALS",
                    errno: errno(),
                });
            }
            Ok(r as u32)
        }
        /// Reads page `index` with `pread` (never through a mapping).
        pub fn read_page(&self, index: usize) -> Result<Box<[u8; PAGE_BYTES]>, UffdError> {
            if index >= self.pages {
                return Err(UffdError::InvalidRegion);
            }
            let mut out = Box::new([0u8; PAGE_BYTES]);
            self.file
                .read_exact_at(&mut out[..], (index * PAGE_BYTES) as u64)
                .map_err(|e| UffdError::Os {
                    op: "pread(memfd)",
                    errno: e.raw_os_error().unwrap_or(0),
                })?;
            Ok(out)
        }
        /// Attempts a write to the sealed file; returns the errno it gets
        /// (`EPERM` while sealed). For tests of the seal.
        pub fn try_write(&self) -> Option<i32> {
            self.file
                .write_at(&[1], 0)
                .err()
                .and_then(|e| e.raw_os_error())
        }
        /// Attempts a writable shared mapping; returns the errno (`EPERM`).
        pub fn try_map_shared_writable(&self) -> Option<i32> {
            match mmap(PAGE_BYTES, libc::MAP_SHARED, self.file.as_raw_fd()) {
                Ok(base) => {
                    // SAFETY: unmaps the page we just mapped.
                    unsafe { libc::munmap(base as *mut libc::c_void, PAGE_BYTES) };
                    None
                }
                Err(UffdError::Os { errno, .. }) => Some(errno),
                Err(_) => Some(0),
            }
        }
    }

    /// A `MAP_PRIVATE` mapping of a [`SealedMemfd`] registered for MINOR
    /// faults. A first touch of a page is resolved with `UFFDIO_CONTINUE`,
    /// which maps the memfd's existing page-cache page: no copy, and every
    /// region over the same memfd shares that physical page. A write then
    /// takes an ordinary copy-on-write fault and makes the page private to
    /// this region; the memfd (and every other region) is unchanged.
    #[derive(Debug)]
    pub struct MinorRegion {
        reg: Registered,
        memfd: Arc<SealedMemfd>,
    }

    impl MinorRegion {
        pub fn map_private(memfd: &Arc<SealedMemfd>) -> Result<Self, UffdError> {
            let reg = Registered::new(
                memfd.pages,
                UFFD_FEATURE_MINOR_SHMEM,
                libc::MAP_PRIVATE | libc::MAP_NORESERVE,
                memfd.file.as_raw_fd(),
                UFFDIO_REGISTER_MODE_MINOR,
                (BIT_WAKE | BIT_CONTINUE, "UFFDIO_CONTINUE"),
            )?;
            Ok(Self {
                reg,
                memfd: Arc::clone(memfd),
            })
        }
        pub fn page_count(&self) -> usize {
            self.reg.pages
        }
        pub fn route(&self) -> UffdRoute {
            self.reg.route
        }
        pub fn memfd(&self) -> &Arc<SealedMemfd> {
            &self.memfd
        }
        /// Serves minor faults with `UFFDIO_CONTINUE` while `touch` runs.
        pub fn serve<R, F>(&self, fault_budget: u64, touch: F) -> Result<(R, UffdStats), UffdError>
        where
            F: FnOnce(&Toucher<'_>) -> R,
        {
            self.serve_recording(fault_budget, touch)
                .map(|(r, s, _)| (r, s))
        }
        pub fn serve_recording<R, F>(
            &self,
            fault_budget: u64,
            touch: F,
        ) -> Result<(R, UffdStats, Vec<usize>), UffdError>
        where
            F: FnOnce(&Toucher<'_>) -> R,
        {
            self.reg.serve(&ContinueShared, fault_budget, touch)
        }
        /// Maps the hot set before any touch: one `UFFDIO_CONTINUE` per run of
        /// up to 2 MiB; pages already mapped are retried singly (raced).
        pub fn prefetch(&self, hot: &HotSet) -> Result<UffdStats, UffdError> {
            if hot.page_count() != self.reg.pages || self.reg.unregistered.load(Ordering::SeqCst) {
                return Err(UffdError::InvalidRegion);
            }
            let fd = self.reg.fd.as_raw_fd();
            let mut stats = UffdStats::default();
            for run in hot
                .runs(PREFETCH_BATCH_PAGES)
                .map_err(|_| UffdError::InvalidRegion)?
            {
                let start = self.reg.page_address(run.start);
                match continue_range(fd, start, (run.pages * PAGE_BYTES) as u64) {
                    Ok(()) => {
                        stats.continued += run.pages as u64;
                        stats.prefetched += run.pages as u64;
                    }
                    Err(e) if e == libc::EEXIST || e == libc::EAGAIN => {
                        for i in 0..run.pages {
                            match continue_range(
                                fd,
                                self.reg.page_address(run.start + i),
                                PAGE_BYTES as u64,
                            ) {
                                Ok(()) => {
                                    stats.continued += 1;
                                    stats.prefetched += 1;
                                }
                                Err(e) if e == libc::EEXIST => stats.raced += 1,
                                Err(errno) => {
                                    return Err(UffdError::Os {
                                        op: "UFFDIO_CONTINUE (prefetch)",
                                        errno,
                                    });
                                }
                            }
                        }
                    }
                    Err(errno) => {
                        return Err(UffdError::Os {
                            op: "UFFDIO_CONTINUE (prefetch)",
                            errno,
                        });
                    }
                }
            }
            Ok(stats)
        }
        /// Physical frame of page `index`, from `/proc/self/pagemap`. Two
        /// regions report the same frame for a page they share. `None` if the
        /// page is not mapped yet or the kernel hides frame numbers
        /// (unprivileged processes read zeros).
        pub fn frame_number(&self, index: usize) -> Option<u64> {
            (index < self.reg.pages).then(|| frame_of(self.reg.page_address(index) as usize))?
        }
    }

    /// Bounds-checked access to a registered region for the touching thread.
    /// All accesses are atomic byte operations, so a racing fault-serving copy
    /// can never be observed as a torn Rust value.
    #[derive(Debug)]
    pub struct Toucher<'a> {
        base: usize,
        len: usize,
        _region: PhantomData<&'a ()>,
    }
    impl Toucher<'_> {
        pub fn len(&self) -> usize {
            self.len
        }
        pub fn is_empty(&self) -> bool {
            self.len == 0
        }
        fn byte(&self, offset: usize) -> Option<&AtomicU8> {
            if offset >= self.len {
                return None;
            }
            // SAFETY: in bounds of the live mapping borrowed through 'a; u8 has
            // alignment 1; this module only accesses it atomically.
            Some(unsafe { AtomicU8::from_ptr((self.base + offset) as *mut u8) })
        }
        /// First touch of a missing page blocks until the server fills it.
        pub fn read_u8(&self, offset: usize) -> Option<u8> {
            self.byte(offset).map(|b| b.load(Ordering::Acquire))
        }
        pub fn write_u8(&self, offset: usize, value: u8) -> bool {
            self.byte(offset)
                .map(|b| b.store(value, Ordering::Release))
                .is_some()
        }
        /// Writes `bytes` at `offset`; false (and nothing written) if out of range.
        pub fn write_bytes(&self, offset: usize, bytes: &[u8]) -> bool {
            if offset.checked_add(bytes.len()).is_none_or(|e| e > self.len) {
                return false;
            }
            for (i, &b) in bytes.iter().enumerate() {
                self.write_u8(offset + i, b);
            }
            true
        }
        pub fn read_page(&self, index: usize) -> Option<Box<[u8; PAGE_BYTES]>> {
            let start = index.checked_mul(PAGE_BYTES)?;
            if start.checked_add(PAGE_BYTES)? > self.len {
                return None;
            }
            let mut out = Box::new([0u8; PAGE_BYTES]);
            for (i, slot) in out.iter_mut().enumerate() {
                *slot = self.read_u8(start + i)?;
            }
            Some(out)
        }
    }
}

#[cfg(feature = "seccomp")]
pub use seccomp_install::*;
#[cfg(feature = "seccomp")]
mod seccomp_install {
    use crate::host::Unavailable;
    use crate::seccomp::SockFilter;

    #[repr(C)]
    struct SockFprog {
        len: libc::c_ushort,
        filter: *const SockFilterC,
    }
    #[repr(C)]
    struct SockFilterC {
        code: u16,
        jt: u8,
        jf: u8,
        k: u32,
    }
    const SECCOMP_SET_MODE_FILTER: libc::c_ulong = 1;

    /// Installs `program` on the calling thread (inherited by threads it
    /// creates later) after setting `PR_SET_NO_NEW_PRIVS`. Irreversible for
    /// this thread.
    pub fn install_seccomp(program: &[SockFilter]) -> Result<(), Unavailable> {
        crate::seccomp::validate(program).map_err(|_| Unavailable::KernelRefused {
            op: "seccomp program validation",
            errno: libc::EINVAL,
        })?;
        let raw: Vec<SockFilterC> = program
            .iter()
            .map(|i| SockFilterC {
                code: i.code,
                jt: i.jt,
                jf: i.jf,
                k: i.k,
            })
            .collect();
        let prog = SockFprog {
            len: raw.len() as libc::c_ushort,
            filter: raw.as_ptr(),
        };
        // SAFETY: prctl with integer arguments only.
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } < 0 {
            return Err(Unavailable::KernelRefused {
                op: "PR_SET_NO_NEW_PRIVS",
                errno: std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
            });
        }
        // SAFETY: `prog` points to `raw`, a live array of `len` sock_filter
        // records, for the duration of the call; the kernel copies it.
        let r = unsafe {
            libc::syscall(
                libc::SYS_seccomp,
                SECCOMP_SET_MODE_FILTER,
                0 as libc::c_ulong,
                &raw const prog,
            )
        };
        if r < 0 {
            return Err(Unavailable::KernelRefused {
                op: "seccomp(SECCOMP_SET_MODE_FILTER)",
                errno: std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
            });
        }
        Ok(())
    }

    /// A system call for probing an installed filter.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum SeccompProbe {
        Getpid,
        /// `socket(AF_INET, SOCK_STREAM, 0)`; a returned descriptor is closed.
        Socket,
        /// `ioctl(-1, request, 0)`: reaches the filter, then fails `EBADF`.
        Ioctl(u32),
    }

    /// Runs `probe` and returns its result or `-errno`.
    pub fn seccomp_probe(probe: SeccompProbe) -> i64 {
        // SAFETY: each call takes plain integers and names no memory; a
        // socket descriptor it creates is closed below.
        let r = unsafe {
            match probe {
                SeccompProbe::Getpid => libc::syscall(libc::SYS_getpid),
                SeccompProbe::Socket => {
                    libc::syscall(libc::SYS_socket, libc::AF_INET, libc::SOCK_STREAM, 0)
                }
                SeccompProbe::Ioctl(request) => {
                    libc::syscall(libc::SYS_ioctl, -1, libc::c_ulong::from(request), 0)
                }
            }
        };
        if r < 0 {
            return -i64::from(std::io::Error::last_os_error().raw_os_error().unwrap_or(0));
        }
        if probe == SeccompProbe::Socket {
            // SAFETY: closes the descriptor socket(2) just returned to us.
            unsafe { libc::close(r as libc::c_int) };
        }
        r
    }
}

#[cfg(all(feature = "kvm", target_arch = "x86_64"))]
pub use kvm::*;
#[cfg(all(feature = "kvm", target_arch = "x86_64"))]
mod kvm {
    use crate::host::{GuestRun, KvmError, PvhGuestRun, Unavailable};
    use crate::pvh::{BootCpuState, SegmentState};
    use crate::slot_plan::{KVM_MEM_READONLY, KvmHostLimits, KvmSlotPlan, PlannedRegion};
    use kvm_bindings::kvm_userspace_memory_region;
    use kvm_ioctls::{Cap, Kvm, VcpuExit};

    /// I/O port the test guest writes to.
    pub const GUEST_PORT: u16 = 0x10;
    const CODE_GPA: u64 = 0x1000;
    const DATA_GPA: u64 = 0x2000;
    const EXIT_BUDGET: u32 = 16;

    fn open() -> Result<Kvm, Unavailable> {
        if !std::path::Path::new("/dev/kvm").exists() {
            return Err(Unavailable::DeviceAbsent { path: "/dev/kvm" });
        }
        let kvm = Kvm::new().map_err(|e| Unavailable::KernelRefused {
            op: "open /dev/kvm",
            errno: e.errno(),
        })?;
        if kvm.get_api_version() != 12 {
            return Err(Unavailable::MissingFeature("KVM API version 12"));
        }
        Ok(kvm)
    }
    fn limits(kvm: &Kvm) -> KvmHostLimits {
        KvmHostLimits {
            max_slots: u32::try_from(kvm.get_nr_memslots()).unwrap_or(u32::MAX),
            readonly_mem: kvm.check_extension(Cap::ReadonlyMem),
            // Every x86_64 CPU has MAXPHYADDR >= 36.
            guest_phys_bits: 36,
        }
    }

    /// Opens `/dev/kvm` and reads the limits a slot plan is verified against.
    pub fn probe_kvm() -> Result<KvmHostLimits, Unavailable> {
        open().map(|kvm| limits(&kvm))
    }

    struct GuestMem {
        addr: *mut u8,
    }
    impl GuestMem {
        fn page() -> Result<Self, KvmError> {
            // SAFETY: a fresh anonymous mapping; no existing memory is affected.
            let addr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    crate::PAGE_BYTES,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                )
            };
            if addr == libc::MAP_FAILED {
                return Err(KvmError::Kvm {
                    op: "mmap",
                    errno: std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
                });
            }
            Ok(Self { addr: addr.cast() })
        }
        fn write(&mut self, bytes: &[u8]) {
            assert!(bytes.len() <= crate::PAGE_BYTES);
            // SAFETY: `addr` is a live, writable, page-sized mapping we own and
            // no guest is running against it yet.
            unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.addr, bytes.len()) };
        }
    }
    impl Drop for GuestMem {
        fn drop(&mut self) {
            // SAFETY: unmaps exactly the page mapped in `page`.
            unsafe { libc::munmap(self.addr.cast(), crate::PAGE_BYTES) };
        }
    }

    fn kvm_err(op: &'static str) -> impl Fn(kvm_ioctls::Error) -> KvmError {
        move |e| KvmError::Kvm {
            op,
            errno: e.errno(),
        }
    }

    /// Boots a one-vCPU real-mode guest whose code page is private RAM at GPA
    /// 0x1000 and whose data page is a read-only slot at 0x2000 holding
    /// `value`. The guest runs `mov al, [0x2000]; out 0x10, al; hlt`. The slot
    /// plan is verified against the probed limits before any memory ioctl.
    pub fn run_port_write_guest(value: u8) -> Result<GuestRun, KvmError> {
        // Guest memory is declared first so it outlives the VM and vCPU.
        let mut code = GuestMem::page()?;
        let mut data = GuestMem::page()?;
        let kvm = open()?;
        let host = limits(&kvm);
        if !host.readonly_mem {
            return Err(Unavailable::MissingFeature("KVM_CAP_READONLY_MEM").into());
        }
        code.write(&[0xA0, 0x00, 0x20, 0xE6, GUEST_PORT as u8, 0xF4]);
        data.write(&[value]);
        let plan = KvmSlotPlan::from_regions(
            1,
            vec![
                PlannedRegion {
                    slot: 0,
                    flags: 0,
                    guest_phys_addr: CODE_GPA,
                    memory_size: crate::PAGE_SIZE_4K,
                    userspace_addr: code.addr as u64,
                },
                PlannedRegion {
                    slot: 1,
                    flags: KVM_MEM_READONLY,
                    guest_phys_addr: DATA_GPA,
                    memory_size: crate::PAGE_SIZE_4K,
                    userspace_addr: data.addr as u64,
                },
            ],
        )?
        .verify(&host)?;
        let vm = kvm.create_vm().map_err(kvm_err("KVM_CREATE_VM"))?;
        for r in plan.regions() {
            let region = kvm_userspace_memory_region {
                slot: r.slot,
                flags: r.flags,
                guest_phys_addr: r.guest_phys_addr,
                memory_size: r.memory_size,
                userspace_addr: r.userspace_addr,
            };
            // SAFETY: the verified plan names only `code` and `data`, live
            // page mappings that outlive `vm` (declared before it).
            unsafe { vm.set_user_memory_region(region) }
                .map_err(kvm_err("KVM_SET_USER_MEMORY_REGION"))?;
        }
        let mut vcpu = vm.create_vcpu(0).map_err(kvm_err("KVM_CREATE_VCPU"))?;
        let mut sregs = vcpu.get_sregs().map_err(kvm_err("KVM_GET_SREGS"))?;
        sregs.cs.base = 0;
        sregs.cs.selector = 0;
        sregs.ds.base = 0;
        sregs.ds.selector = 0;
        vcpu.set_sregs(&sregs).map_err(kvm_err("KVM_SET_SREGS"))?;
        let mut regs = vcpu.get_regs().map_err(kvm_err("KVM_GET_REGS"))?;
        regs.rip = CODE_GPA;
        regs.rflags = 2;
        vcpu.set_regs(&regs).map_err(kvm_err("KVM_SET_REGS"))?;
        let mut seen: Option<(u16, u8)> = None;
        let mut exits = 0u32;
        loop {
            exits += 1;
            if exits > EXIT_BUDGET {
                return Err(KvmError::ExitBudget);
            }
            match vcpu.run() {
                Ok(VcpuExit::IoOut(port, bytes)) => {
                    let first = *bytes
                        .first()
                        .ok_or_else(|| KvmError::UnexpectedExit("empty IoOut".into()))?;
                    seen = Some((port, first));
                }
                Ok(VcpuExit::Hlt) => break,
                Ok(other) => return Err(KvmError::UnexpectedExit(format!("{other:?}"))),
                Err(e) if e.errno() == libc::EINTR => {}
                Err(e) => {
                    return Err(KvmError::Kvm {
                        op: "KVM_RUN",
                        errno: e.errno(),
                    });
                }
            }
        }
        let (port, value) =
            seen.ok_or_else(|| KvmError::UnexpectedExit("halted without port write".into()))?;
        Ok(GuestRun {
            port,
            value,
            exits,
            plan,
        })
    }

    fn kvm_segment(s: &SegmentState) -> kvm_bindings::kvm_segment {
        kvm_bindings::kvm_segment {
            base: s.base,
            limit: s.limit,
            selector: s.selector,
            type_: s.kind,
            present: u8::from(s.present),
            dpl: s.dpl,
            db: u8::from(s.db),
            s: u8::from(s.s),
            l: u8::from(s.l),
            g: u8::from(s.g),
            avl: 0,
            unusable: 0,
            padding: 0,
        }
    }

    /// Guest RAM for the PVH run: an anonymous mapping of `len` bytes.
    struct GuestRam {
        addr: *mut u8,
        len: usize,
    }
    impl GuestRam {
        fn new(bytes: &[u8]) -> Result<Self, KvmError> {
            let len = bytes.len();
            // SAFETY: a fresh anonymous mapping; no existing memory is affected.
            let addr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    len,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                    -1,
                    0,
                )
            };
            if addr == libc::MAP_FAILED {
                return Err(KvmError::Kvm {
                    op: "mmap",
                    errno: std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
                });
            }
            // SAFETY: `addr` is a live writable mapping of `len` bytes we own;
            // no guest runs against it yet.
            unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), addr.cast(), len) };
            Ok(Self {
                addr: addr.cast(),
                len,
            })
        }
    }
    impl Drop for GuestRam {
        fn drop(&mut self) {
            // SAFETY: unmaps exactly the mapping made in `new`.
            unsafe { libc::munmap(self.addr.cast(), self.len) };
        }
    }

    /// Boots a PVH ELF through [`crate::pvh`] on one vCPU: 2 MiB of RAM at
    /// GPA 0, the entry code at 1 MiB reads `hvm_start_info.magic` and
    /// `.version` through `EBX` and writes their low bytes to port 0x10, then
    /// halts. Every port byte is returned, in order.
    pub fn run_pvh_guest() -> Result<PvhGuestRun, KvmError> {
        let kvm = open()?;
        let host = limits(&kvm);
        let mut memory = crate::guest_mem::GuestMemory::new(&[(0, 2 << 20)])
            .map_err(|e| KvmError::UnexpectedExit(format!("{e:?}")))?;
        // mov eax,[ebx]; out 0x10,al; mov eax,[ebx+4]; out 0x10,al; hlt
        let code = [0x8B, 0x03, 0xE6, 0x10, 0x8B, 0x43, 0x04, 0xE6, 0x10, 0xF4];
        let elf = crate::pvh::build_test_pvh_elf(0x10_0000, &code);
        let image = crate::pvh::parse_pvh_elf(&elf, &memory)
            .map_err(|e| KvmError::UnexpectedExit(format!("{e:?}")))?;
        let boot: BootCpuState = crate::pvh::load_pvh(&image, &elf, &mut memory, "console=none")
            .map_err(|e| KvmError::UnexpectedExit(format!("{e:?}")))?;
        let (gpa, bytes) = memory.region_bytes(0).expect("one region");
        let ram = GuestRam::new(bytes)?;
        let plan = KvmSlotPlan::from_regions(
            2,
            vec![PlannedRegion {
                slot: 0,
                flags: 0,
                guest_phys_addr: gpa,
                memory_size: ram.len as u64,
                userspace_addr: ram.addr as u64,
            }],
        )?
        .verify(&host)?;
        let vm = kvm.create_vm().map_err(kvm_err("KVM_CREATE_VM"))?;
        for r in plan.regions() {
            let region = kvm_userspace_memory_region {
                slot: r.slot,
                flags: r.flags,
                guest_phys_addr: r.guest_phys_addr,
                memory_size: r.memory_size,
                userspace_addr: r.userspace_addr,
            };
            // SAFETY: the verified plan names only `ram`, a live mapping that
            // outlives `vm` (declared before it).
            unsafe { vm.set_user_memory_region(region) }
                .map_err(kvm_err("KVM_SET_USER_MEMORY_REGION"))?;
        }
        let mut vcpu = vm.create_vcpu(0).map_err(kvm_err("KVM_CREATE_VCPU"))?;
        let mut sregs = vcpu.get_sregs().map_err(kvm_err("KVM_GET_SREGS"))?;
        sregs.cs = kvm_segment(&boot.cs);
        sregs.ds = kvm_segment(&boot.data);
        sregs.es = kvm_segment(&boot.data);
        sregs.fs = kvm_segment(&boot.data);
        sregs.gs = kvm_segment(&boot.data);
        sregs.ss = kvm_segment(&boot.data);
        sregs.tr = kvm_segment(&boot.tr);
        sregs.gdt.base = boot.gdt_base;
        sregs.gdt.limit = boot.gdt_limit;
        sregs.cr0 = boot.cr0;
        sregs.cr4 = boot.cr4;
        sregs.efer = boot.efer;
        vcpu.set_sregs(&sregs).map_err(kvm_err("KVM_SET_SREGS"))?;
        let mut regs = vcpu.get_regs().map_err(kvm_err("KVM_GET_REGS"))?;
        regs.rip = boot.rip;
        regs.rbx = boot.rbx;
        regs.rflags = boot.rflags;
        vcpu.set_regs(&regs).map_err(kvm_err("KVM_SET_REGS"))?;
        let mut port_bytes = Vec::new();
        let mut exits = 0u32;
        loop {
            exits += 1;
            if exits > EXIT_BUDGET {
                return Err(KvmError::ExitBudget);
            }
            match vcpu.run() {
                Ok(VcpuExit::IoOut(GUEST_PORT, bytes)) => {
                    port_bytes.push(
                        *bytes
                            .first()
                            .ok_or_else(|| KvmError::UnexpectedExit("empty IoOut".into()))?,
                    );
                }
                Ok(VcpuExit::Hlt) => break,
                Ok(other) => return Err(KvmError::UnexpectedExit(format!("{other:?}"))),
                Err(e) if e.errno() == libc::EINTR => {}
                Err(e) => {
                    return Err(KvmError::Kvm {
                        op: "KVM_RUN",
                        errno: e.errno(),
                    });
                }
            }
        }
        drop(vcpu);
        drop(vm);
        drop(ram);
        Ok(PvhGuestRun {
            port_bytes,
            exits,
            plan,
        })
    }
}
