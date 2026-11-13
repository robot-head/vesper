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
    use crate::PAGE_BYTES;
    use crate::host::{UffdError, UffdProbe, UffdRoute, UffdStats, Unavailable};
    use crate::page_source::PageSource;
    use std::marker::PhantomData;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

    const UFFD_API: u64 = 0xAA;
    const UFFDIO_API: u64 = 0xC018_AA3F;
    const UFFDIO_REGISTER: u64 = 0xC020_AA00;
    const UFFDIO_UNREGISTER: u64 = 0x8010_AA01;
    const UFFDIO_WAKE: u64 = 0x8010_AA02;
    const UFFDIO_COPY: u64 = 0xC028_AA03;
    const UFFDIO_ZEROPAGE: u64 = 0xC020_AA04;
    const USERFAULTFD_IOC_NEW: u64 = 0xAA00;
    const UFFD_USER_MODE_ONLY: libc::c_int = 1;
    const UFFDIO_REGISTER_MODE_MISSING: u64 = 1;
    const UFFD_EVENT_PAGEFAULT: u8 = 0x12;
    const BIT_REGISTER: u64 = 1 << 0x00;
    const BIT_UNREGISTER: u64 = 1 << 0x01;
    const BIT_WAKE: u64 = 1 << 0x02;
    const BIT_COPY: u64 = 1 << 0x03;
    const BIT_ZEROPAGE: u64 = 1 << 0x04;
    const CAP_SYS_PTRACE: u32 = 19;
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

    fn handshake(fd: RawFd) -> Result<(), Unavailable> {
        let mut api = UffdioApi {
            api: UFFD_API,
            features: 0,
            ioctls: 0,
        };
        // SAFETY: `api` is a live uffdio_api for the call.
        unsafe { ioctl_ptr(fd, UFFDIO_API, &mut api) }.map_err(|errno| {
            Unavailable::KernelRefused {
                op: "UFFDIO_API",
                errno,
            }
        })?;
        if api.ioctls & (BIT_REGISTER | BIT_UNREGISTER) != (BIT_REGISTER | BIT_UNREGISTER) {
            return Err(Unavailable::MissingFeature("UFFDIO_REGISTER"));
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
            Ok((fd, route)) => handshake(fd.as_raw_fd()).map(|()| route),
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

    /// An anonymous private mapping registered for missing-page faults. Each
    /// page is filled on first touch by [`LazyRegion::serve`].
    #[derive(Debug)]
    pub struct LazyRegion {
        fd: OwnedFd,
        base: usize,
        len: usize,
        pages: usize,
        route: UffdRoute,
        unregistered: AtomicBool,
    }

    impl LazyRegion {
        pub fn new(pages: usize) -> Result<Self, UffdError> {
            if pages == 0 || pages > MAX_LAZY_PAGES {
                return Err(UffdError::InvalidRegion);
            }
            let len = pages * PAGE_BYTES;
            let probe = probe_userfaultfd();
            probe.route?;
            let (fd, route) = open_uffd().map_err(|errno| {
                UffdError::Unavailable(Unavailable::UserfaultfdDenied {
                    unprivileged_userfaultfd: probe.unprivileged_userfaultfd,
                    cap_sys_ptrace: probe.cap_sys_ptrace,
                    errno,
                })
            })?;
            handshake(fd.as_raw_fd())?;
            // SAFETY: a fresh anonymous mapping; no existing memory is affected.
            let base = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    len,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                    -1,
                    0,
                )
            };
            if base == libc::MAP_FAILED {
                return Err(UffdError::Os {
                    op: "mmap",
                    errno: errno(),
                });
            }
            // From here Drop unmaps the region on every error path.
            let region = Self {
                fd,
                base: base as usize,
                len,
                pages,
                route,
                unregistered: AtomicBool::new(false),
            };
            let mut reg = UffdioRegister {
                range: region.range(),
                mode: UFFDIO_REGISTER_MODE_MISSING,
                ioctls: 0,
            };
            // SAFETY: `reg` is a live uffdio_register; the range is our mapping.
            unsafe { ioctl_ptr(region.fd.as_raw_fd(), UFFDIO_REGISTER, &mut reg) }.map_err(
                |errno| UffdError::Os {
                    op: "UFFDIO_REGISTER",
                    errno,
                },
            )?;
            let need = BIT_WAKE | BIT_COPY | BIT_ZEROPAGE;
            if reg.ioctls & need != need {
                return Err(UffdError::Unavailable(Unavailable::MissingFeature(
                    "UFFDIO_COPY/UFFDIO_ZEROPAGE",
                )));
            }
            Ok(region)
        }
        fn range(&self) -> UffdioRange {
            UffdioRange {
                start: self.base as u64,
                len: self.len as u64,
            }
        }
        pub fn page_count(&self) -> usize {
            self.pages
        }
        pub fn route(&self) -> UffdRoute {
            self.route
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
            if source.page_count() != self.pages || self.unregistered.load(Ordering::SeqCst) {
                return Err(UffdError::InvalidRegion);
            }
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
                let server = scope.spawn(|| self.server_loop(source, fault_budget, stop_fd));
                let result = {
                    let _stop = StopOnDrop(stop_fd);
                    touch(&Toucher {
                        base: self.base,
                        len: self.len,
                        _region: PhantomData,
                    })
                };
                let stats = match server.join() {
                    Ok(stats) => stats?,
                    Err(panic) => std::panic::resume_unwind(panic),
                };
                Ok((result, stats))
            })
        }

        fn fail(&self, error: UffdError) -> UffdError {
            if !self.unregistered.swap(true, Ordering::SeqCst) {
                let mut range = self.range();
                // SAFETY: `range` is a live uffdio_range over our mapping.
                // Unregistering wakes every thread blocked on this range.
                let _ = unsafe { ioctl_ptr(self.fd.as_raw_fd(), UFFDIO_UNREGISTER, &mut range) };
            }
            error
        }

        fn server_loop<S: PageSource>(
            &self,
            source: &S,
            budget: u64,
            stop_fd: RawFd,
        ) -> Result<UffdStats, UffdError> {
            let fd = self.fd.as_raw_fd();
            let mut stats = UffdStats::default();
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
                // SAFETY: `fds` is a live array of two pollfd records.
                let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
                if n < 0 {
                    let e = errno();
                    if e == libc::EINTR {
                        continue;
                    }
                    return Err(self.fail(UffdError::Os {
                        op: "poll",
                        errno: e,
                    }));
                }
                if fds[0].revents != 0 {
                    self.drain(source, budget, &mut stats)
                        .map_err(|e| self.fail(e))?;
                }
                if fds[1].revents != 0 {
                    return Ok(stats);
                }
            }
        }

        fn drain<S: PageSource>(
            &self,
            source: &S,
            budget: u64,
            stats: &mut UffdStats,
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
                let dst = self.base as u64 + index as u64 * PAGE_BYTES as u64;
                let fill = source
                    .fill(index)
                    .map_err(|error| UffdError::Source { index, error })?;
                let outcome = match fill.bytes() {
                    None => {
                        let mut z = UffdioZeropage {
                            range: UffdioRange {
                                start: dst,
                                len: PAGE_BYTES as u64,
                            },
                            mode: 0,
                            zeropage: 0,
                        };
                        // SAFETY: live uffdio_zeropage over one page of our region.
                        let r = unsafe { ioctl_ptr(fd, UFFDIO_ZEROPAGE, &mut z) };
                        r.map(|()| stats.zeroed += 1)
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
                        // until after the call); `dst` is one page of our region.
                        let r = unsafe { ioctl_ptr(fd, UFFDIO_COPY, &mut c) };
                        r.map(|()| stats.copied += 1)
                    }
                };
                drop(fill);
                match outcome {
                    Ok(()) => {}
                    Err(e) if e == libc::EEXIST => {
                        stats.raced += 1;
                        let mut range = UffdioRange {
                            start: dst,
                            len: PAGE_BYTES as u64,
                        };
                        // SAFETY: live uffdio_range over one page of our region.
                        unsafe { ioctl_ptr(fd, UFFDIO_WAKE, &mut range) }.map_err(|errno| {
                            UffdError::Os {
                                op: "UFFDIO_WAKE",
                                errno,
                            }
                        })?;
                    }
                    Err(errno) => {
                        return Err(UffdError::Os {
                            op: "UFFDIO_COPY/ZEROPAGE",
                            errno,
                        });
                    }
                }
            }
        }
    }

    impl Drop for LazyRegion {
        fn drop(&mut self) {
            // SAFETY: unmaps exactly the mapping created in `new`; no Toucher
            // outlives the region (it borrows it).
            unsafe {
                libc::munmap(self.base as *mut libc::c_void, self.len);
            }
        }
    }

    /// Bounds-checked access to a [`LazyRegion`] for the touching thread. All
    /// accesses are atomic byte operations, so a racing fault-serving copy can
    /// never be observed as a torn Rust value.
    #[derive(Debug)]
    pub struct Toucher<'a> {
        base: usize,
        len: usize,
        _region: PhantomData<&'a LazyRegion>,
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

#[cfg(all(feature = "kvm", target_arch = "x86_64"))]
pub use kvm::*;
#[cfg(all(feature = "kvm", target_arch = "x86_64"))]
mod kvm {
    use crate::host::{GuestRun, KvmError, Unavailable};
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
}
