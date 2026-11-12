//! Real Linux AF_XDP backend (feature `af-xdp`, Linux only).
//!
//! This is the crate's only unsafe code. Every `unsafe` block is a system
//! call, an access to memory the kernel shares with us through `mmap`, or the
//! construction of a slice over the UMEM. The invariants they rely on:
//!
//! 1. Ring and UMEM mappings live exactly as long as their [`Mmap`] owners,
//!    which the backend drops only after the socket (field order), so no
//!    pointer outlives its mapping.
//! 2. Ring indices are read and written only through `AtomicU32`s placed at
//!    the kernel-reported offsets (4-byte aligned, inside the mapping), with
//!    acquire loads of the peer's index and release stores of our own,
//!    matching libxdp's barriers. Descriptor slots are accessed at
//!    `desc + (idx & mask)`, which is inside the mapping because the mapping
//!    length is `desc + size * entry_size`.
//! 3. UMEM slices are built only for ranges the driver has bounds-checked
//!    and that belong to chunks the application owns (free, app-held or being
//!    turned around). The kernel writes only chunks posted to the fill ring and
//!    reads only chunks posted to the tx ring, so it does not touch memory a
//!    live slice covers.
//!
//! Binding tries `XDP_ZEROCOPY` first and falls back to `XDP_COPY`. The
//! backend reports which mode the kernel granted; nothing here claims NIC
//! zero-copy unless `BindMode::ZeroCopy` was actually returned.
//!
//! [`XdpRedirect`] loads a minimal XDP program (an XSKMAP redirect filtered to
//! one UDP port) with `bpf(2)` and attaches it through a BPF link in generic
//! (skb) mode, so receive can be exercised on `lo`. The link detaches when the
//! value is dropped or the process exits.
#![allow(unsafe_code)]

use std::ffi::CString;
use std::fmt;
use std::mem::{size_of, zeroed};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::config::UmemConfig;
use crate::driver::XskBackend;
use crate::driver::sealed::BackendAccess;
use crate::error::{RingError, UmemError};
use crate::ring::{ConsumerRing, ProducerRing};
use crate::wire::XdpDesc;

/// A host refusal, reported as `unavailable: <step>: errno <n> (<text>)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AfXdpError {
    pub step: &'static str,
    pub errno: i32,
}

impl fmt::Display for AfXdpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = std::io::Error::from_raw_os_error(self.errno);
        write!(
            f,
            "unavailable: {}: errno {} ({text})",
            self.step, self.errno
        )
    }
}
impl std::error::Error for AfXdpError {}

impl From<AfXdpError> for UmemError {
    fn from(e: AfXdpError) -> Self {
        UmemError::Os {
            op: e.step,
            errno: e.errno,
        }
    }
}

fn last_error(step: &'static str) -> AfXdpError {
    AfXdpError {
        step,
        errno: std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
    }
}

/// Whether the host lets this process create an AF_XDP socket at all.
pub fn probe() -> Result<(), AfXdpError> {
    open_socket().map(drop)
}

fn open_socket() -> Result<OwnedFd, AfXdpError> {
    // SAFETY: plain system call; a non-negative result is a fresh descriptor we own.
    let fd = unsafe { libc::socket(libc::AF_XDP, libc::SOCK_RAW | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(last_error("socket(AF_XDP)"));
    }
    // SAFETY: `fd` is a valid descriptor nobody else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn setsockopt<T>(fd: RawFd, name: i32, value: &T, step: &'static str) -> Result<(), AfXdpError> {
    // SAFETY: `value` points to `size_of::<T>()` readable bytes for the call.
    let rc = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_XDP,
            name,
            (value as *const T).cast(),
            size_of::<T>() as libc::socklen_t,
        )
    };
    if rc != 0 {
        return Err(last_error(step));
    }
    Ok(())
}

/// An owned memory mapping.
struct Mmap {
    ptr: NonNull<u8>,
    len: usize,
}

impl Mmap {
    fn new(
        len: usize,
        fd: Option<RawFd>,
        offset: u64,
        step: &'static str,
    ) -> Result<Self, AfXdpError> {
        let (flags, fd) = match fd {
            Some(fd) => (libc::MAP_SHARED | libc::MAP_POPULATE, fd),
            None => (
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_POPULATE,
                -1,
            ),
        };
        let offset = libc::off_t::try_from(offset).map_err(|_| AfXdpError {
            step,
            errno: libc::EOVERFLOW,
        })?;
        // SAFETY: requesting a fresh mapping; the kernel validates every argument.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                flags,
                fd,
                offset,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(last_error(step));
        }
        Ok(Self {
            ptr: NonNull::new(ptr.cast()).ok_or(AfXdpError {
                step,
                errno: libc::EFAULT,
            })?,
            len,
        })
    }
}

impl Drop for Mmap {
    fn drop(&mut self) {
        // SAFETY: unmapping exactly the range `new` mapped, once.
        unsafe {
            libc::munmap(self.ptr.as_ptr().cast(), self.len);
        }
    }
}

mod sealed {
    pub trait Sealed {}
}
impl sealed::Sealed for u64 {}
impl sealed::Sealed for XdpDesc {}

/// An entry type of a kernel ring (`u64` addresses or descriptors) and its
/// raw kernel representation. Sealed.
pub trait KernelEntry: Copy + sealed::Sealed {
    type Raw: Copy;
    fn from_raw(raw: Self::Raw) -> Self;
    fn to_raw(self) -> Self::Raw;
}
impl KernelEntry for u64 {
    type Raw = u64;
    fn from_raw(raw: u64) -> u64 {
        raw
    }
    fn to_raw(self) -> u64 {
        self
    }
}
impl KernelEntry for XdpDesc {
    type Raw = libc::xdp_desc;
    fn from_raw(raw: libc::xdp_desc) -> XdpDesc {
        XdpDesc {
            addr: raw.addr,
            len: raw.len,
            options: raw.options,
        }
    }
    fn to_raw(self) -> libc::xdp_desc {
        libc::xdp_desc {
            addr: self.addr,
            len: self.len,
            options: self.options,
        }
    }
}

/// One mmapped kernel ring.
struct KernelRing<T: KernelEntry> {
    /// Keeps the mapping alive for the pointers below.
    _map: Mmap,
    producer: NonNull<AtomicU32>,
    consumer: NonNull<AtomicU32>,
    flags: NonNull<AtomicU32>,
    desc: NonNull<T::Raw>,
    mask: u32,
    /// Producer role: our next index (published + reserved).
    /// Consumer role: our next index (released + peeked).
    cached_local: u32,
    /// Producer role: consumer + size. Consumer role: producer.
    cached_peer: u32,
}

impl<T: KernelEntry> KernelRing<T> {
    fn map(
        fd: RawFd,
        off: &libc::xdp_ring_offset,
        size: u32,
        pgoff: u64,
        producer_role: bool,
        step: &'static str,
    ) -> Result<Self, AfXdpError> {
        let too_big = AfXdpError {
            step,
            errno: libc::EOVERFLOW,
        };
        let len = usize::try_from(off.desc)
            .ok()
            .and_then(|d| d.checked_add((size as usize).checked_mul(size_of::<T::Raw>())?))
            .ok_or(too_big)?;
        let map = Mmap::new(len, Some(fd), pgoff, step)?;
        let at = |offset: u64, width: usize| -> Result<NonNull<u8>, AfXdpError> {
            let offset = usize::try_from(offset).map_err(|_| too_big)?;
            if offset.checked_add(width).is_none_or(|end| end > len) {
                return Err(too_big);
            }
            // SAFETY: `offset + width <= len`, inside the mapping.
            Ok(unsafe { map.ptr.add(offset) })
        };
        let producer = at(off.producer, 4)?.cast::<AtomicU32>();
        let consumer = at(off.consumer, 4)?.cast::<AtomicU32>();
        let flags = at(off.flags, 4)?.cast::<AtomicU32>();
        let desc = at(off.desc, 0)?.cast::<T::Raw>();
        for p in [
            producer.as_ptr() as usize,
            consumer.as_ptr() as usize,
            flags.as_ptr() as usize,
        ] {
            if p % 4 != 0 {
                return Err(AfXdpError {
                    step,
                    errno: libc::EFAULT,
                });
            }
        }
        let mut ring = Self {
            _map: map,
            producer,
            consumer,
            flags,
            desc,
            mask: size - 1,
            cached_local: 0,
            cached_peer: 0,
        };
        if producer_role {
            ring.cached_local = ring.producer().load(Ordering::Relaxed);
            ring.cached_peer = ring.consumer().load(Ordering::Acquire).wrapping_add(size);
        } else {
            ring.cached_local = ring.consumer().load(Ordering::Relaxed);
            ring.cached_peer = ring.producer().load(Ordering::Acquire);
        }
        Ok(ring)
    }

    fn producer(&self) -> &AtomicU32 {
        // SAFETY: aligned, inside the live mapping (invariant 2).
        unsafe { self.producer.as_ref() }
    }
    fn consumer(&self) -> &AtomicU32 {
        // SAFETY: as above.
        unsafe { self.consumer.as_ref() }
    }
    fn needs_wakeup(&self) -> bool {
        // SAFETY: as above.
        let flags = unsafe { self.flags.as_ref() };
        flags.load(Ordering::Acquire) & libc::XDP_RING_NEED_WAKEUP != 0
    }
    fn slot(&self, idx: u32) -> *mut T::Raw {
        // SAFETY: `idx & mask < size`, so the slot is inside the mapping.
        unsafe { self.desc.as_ptr().add((idx & self.mask) as usize) }
    }
    fn size(&self) -> u32 {
        self.mask + 1
    }
}

/// A ring the application produces into (fill, tx).
pub struct ProdRing<T: KernelEntry>(KernelRing<T>);
/// A ring the application consumes from (rx, completion).
pub struct ConsRing<T: KernelEntry>(KernelRing<T>);

impl<T: KernelEntry> ProdRing<T> {
    fn published(&self) -> u32 {
        // We are the only writer of the producer index.
        self.0.producer().load(Ordering::Relaxed)
    }
    fn reserved(&self) -> u32 {
        self.0.cached_local.wrapping_sub(self.published())
    }
}

impl<T: KernelEntry> ProducerRing<T> for ProdRing<T> {
    fn size(&self) -> u32 {
        self.0.size()
    }
    fn free_entries(&mut self, wanted: u32) -> u32 {
        let r = &mut self.0;
        let free = r.cached_peer.wrapping_sub(r.cached_local);
        if free >= wanted {
            return free;
        }
        r.cached_peer = r.consumer().load(Ordering::Acquire).wrapping_add(r.size());
        r.cached_peer.wrapping_sub(r.cached_local)
    }
    fn reserve(&mut self, n: u32) -> Option<u32> {
        if n == 0 || self.free_entries(n) < n {
            return None;
        }
        let idx = self.0.cached_local;
        self.0.cached_local = idx.wrapping_add(n);
        Some(idx)
    }
    fn write(&mut self, idx: u32, value: T) -> Result<(), RingError> {
        if idx.wrapping_sub(self.published()) >= self.reserved() {
            return Err(RingError::IndexOutsideWindow { idx });
        }
        // SAFETY: the slot is inside the mapping and reserved for us; the
        // kernel does not read it before we publish it.
        unsafe { self.0.slot(idx).write(value.to_raw()) };
        Ok(())
    }
    fn unreserve(&mut self, n: u32) -> Result<(), RingError> {
        let outstanding = self.reserved();
        if n > outstanding {
            return Err(RingError::CountExceedsWindow {
                requested: n,
                outstanding,
            });
        }
        self.0.cached_local = self.0.cached_local.wrapping_sub(n);
        Ok(())
    }
    fn submit(&mut self, n: u32) -> Result<(), RingError> {
        let outstanding = self.reserved();
        if n > outstanding {
            return Err(RingError::CountExceedsWindow {
                requested: n,
                outstanding,
            });
        }
        let published = self.published();
        self.0
            .producer()
            .store(published.wrapping_add(n), Ordering::Release);
        Ok(())
    }
}

impl<T: KernelEntry> ConsRing<T> {
    fn released(&self) -> u32 {
        // We are the only writer of the consumer index.
        self.0.consumer().load(Ordering::Relaxed)
    }
    fn peeked(&self) -> u32 {
        self.0.cached_local.wrapping_sub(self.released())
    }
}

impl<T: KernelEntry> ConsumerRing<T> for ConsRing<T> {
    fn size(&self) -> u32 {
        self.0.size()
    }
    fn peek(&mut self, n: u32) -> (u32, u32) {
        let r = &mut self.0;
        let mut entries = r.cached_peer.wrapping_sub(r.cached_local);
        if entries == 0 {
            r.cached_peer = r.producer().load(Ordering::Acquire);
            entries = r.cached_peer.wrapping_sub(r.cached_local);
        }
        let count = entries.min(n);
        let idx = r.cached_local;
        r.cached_local = idx.wrapping_add(count);
        (count, idx)
    }
    fn read(&self, idx: u32) -> Result<T, RingError> {
        if idx.wrapping_sub(self.released()) >= self.peeked() {
            return Err(RingError::IndexOutsideWindow { idx });
        }
        // SAFETY: the slot is inside the mapping and was published by the
        // kernel before the acquire load in `peek`.
        Ok(T::from_raw(unsafe { self.0.slot(idx).read() }))
    }
    fn unpeek(&mut self, n: u32) -> Result<(), RingError> {
        let outstanding = self.peeked();
        if n > outstanding {
            return Err(RingError::CountExceedsWindow {
                requested: n,
                outstanding,
            });
        }
        self.0.cached_local = self.0.cached_local.wrapping_sub(n);
        Ok(())
    }
    fn release(&mut self, n: u32) -> Result<(), RingError> {
        let outstanding = self.peeked();
        if n > outstanding {
            return Err(RingError::CountExceedsWindow {
                requested: n,
                outstanding,
            });
        }
        let released = self.released();
        self.0
            .consumer()
            .store(released.wrapping_add(n), Ordering::Release);
        Ok(())
    }
}

/// Which copy mode the kernel accepted at bind time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindMode {
    ZeroCopy,
    Copy,
}

/// An AF_XDP socket bound to one interface queue, with its UMEM and rings.
pub struct AfXdpBackend {
    // Field order is drop order: rings and UMEM are unmapped after the
    // socket is closed (invariant 1).
    fd: OwnedFd,
    fill: ProdRing<u64>,
    completion: ConsRing<u64>,
    rx: ConsRing<XdpDesc>,
    tx: ProdRing<XdpDesc>,
    umem: Mmap,
    umem_len: usize,
    frame_size: u32,
    headroom: u32,
    bind_mode: BindMode,
    zero_copy_error: Option<AfXdpError>,
    ifindex: u32,
    queue_id: u32,
}

impl AfXdpBackend {
    /// Creates the socket, registers a fresh anonymous UMEM, sizes and maps
    /// the four rings and binds to `ifname` queue `queue_id` (zero-copy, then
    /// copy mode).
    pub fn open(config: &UmemConfig, ifname: &str, queue_id: u32) -> Result<Self, AfXdpError> {
        config.validate().map_err(|_| AfXdpError {
            step: "UmemConfig::validate",
            errno: libc::EINVAL,
        })?;
        let name = CString::new(ifname).map_err(|_| AfXdpError {
            step: "interface name",
            errno: libc::EINVAL,
        })?;
        // SAFETY: `name` is a valid NUL-terminated string.
        let ifindex = unsafe { libc::if_nametoindex(name.as_ptr()) };
        if ifindex == 0 {
            return Err(last_error("if_nametoindex"));
        }
        let fd = open_socket()?;
        let raw = fd.as_raw_fd();
        let umem_len = config.umem_len().map_err(|_| AfXdpError {
            step: "UMEM size",
            errno: libc::EOVERFLOW,
        })?;
        let umem = Mmap::new(umem_len, None, 0, "mmap(UMEM)")?;
        let reg = libc::xdp_umem_reg {
            addr: umem.ptr.as_ptr() as u64,
            len: umem_len as u64,
            chunk_size: config.frame_size,
            headroom: config.headroom,
            flags: 0,
            tx_metadata_len: 0,
        };
        setsockopt(raw, libc::XDP_UMEM_REG, &reg, "setsockopt(XDP_UMEM_REG)")?;
        setsockopt(
            raw,
            libc::XDP_UMEM_FILL_RING,
            &config.fill_size,
            "setsockopt(XDP_UMEM_FILL_RING)",
        )?;
        setsockopt(
            raw,
            libc::XDP_UMEM_COMPLETION_RING,
            &config.completion_size,
            "setsockopt(XDP_UMEM_COMPLETION_RING)",
        )?;
        setsockopt(
            raw,
            libc::XDP_RX_RING,
            &config.rx_size,
            "setsockopt(XDP_RX_RING)",
        )?;
        setsockopt(
            raw,
            libc::XDP_TX_RING,
            &config.tx_size,
            "setsockopt(XDP_TX_RING)",
        )?;

        // SAFETY: an all-zero xdp_mmap_offsets is a valid value.
        let mut off: libc::xdp_mmap_offsets = unsafe { zeroed() };
        let mut optlen = size_of::<libc::xdp_mmap_offsets>() as libc::socklen_t;
        // SAFETY: `off`/`optlen` are valid for writes of the sizes passed.
        let rc = unsafe {
            libc::getsockopt(
                raw,
                libc::SOL_XDP,
                libc::XDP_MMAP_OFFSETS,
                (&mut off as *mut libc::xdp_mmap_offsets).cast(),
                &mut optlen,
            )
        };
        if rc != 0 {
            return Err(last_error("getsockopt(XDP_MMAP_OFFSETS)"));
        }
        if optlen as usize != size_of::<libc::xdp_mmap_offsets>() {
            // Pre-5.4 kernels report offsets without the flags word.
            return Err(AfXdpError {
                step: "XDP_MMAP_OFFSETS (kernel without ring flags)",
                errno: libc::ENOTSUP,
            });
        }
        let fill = ProdRing(KernelRing::map(
            raw,
            &off.fr,
            config.fill_size,
            libc::XDP_UMEM_PGOFF_FILL_RING,
            true,
            "mmap(fill ring)",
        )?);
        let completion = ConsRing(KernelRing::map(
            raw,
            &off.cr,
            config.completion_size,
            libc::XDP_UMEM_PGOFF_COMPLETION_RING,
            false,
            "mmap(completion ring)",
        )?);
        let rx = ConsRing(KernelRing::map(
            raw,
            &off.rx,
            config.rx_size,
            libc::XDP_PGOFF_RX_RING as u64,
            false,
            "mmap(rx ring)",
        )?);
        let tx = ProdRing(KernelRing::map(
            raw,
            &off.tx,
            config.tx_size,
            libc::XDP_PGOFF_TX_RING as u64,
            true,
            "mmap(tx ring)",
        )?);

        let bind = |flags: u16, step| {
            let addr = libc::sockaddr_xdp {
                sxdp_family: libc::AF_XDP as u16,
                sxdp_flags: flags,
                sxdp_ifindex: ifindex,
                sxdp_queue_id: queue_id,
                sxdp_shared_umem_fd: 0,
            };
            // SAFETY: `addr` is a valid sockaddr_xdp of the size passed.
            let rc = unsafe {
                libc::bind(
                    raw,
                    (&addr as *const libc::sockaddr_xdp).cast(),
                    size_of::<libc::sockaddr_xdp>() as libc::socklen_t,
                )
            };
            if rc == 0 {
                Ok(())
            } else {
                Err(last_error(step))
            }
        };
        let (bind_mode, zero_copy_error) = match bind(libc::XDP_ZEROCOPY, "bind(XDP_ZEROCOPY)") {
            Ok(()) => (BindMode::ZeroCopy, None),
            Err(zc) => {
                bind(libc::XDP_COPY, "bind(XDP_COPY)")?;
                (BindMode::Copy, Some(zc))
            }
        };
        Ok(Self {
            fd,
            fill,
            completion,
            rx,
            tx,
            umem,
            umem_len,
            frame_size: config.frame_size,
            headroom: config.headroom,
            bind_mode,
            zero_copy_error,
            ifindex,
            queue_id,
        })
    }

    pub fn bind_mode(&self) -> BindMode {
        self.bind_mode
    }
    /// Why zero-copy was refused, when the bind fell back to copy mode.
    pub fn zero_copy_error(&self) -> Option<AfXdpError> {
        self.zero_copy_error
    }
    pub fn ifindex(&self) -> u32 {
        self.ifindex
    }
    pub fn queue_id(&self) -> u32 {
        self.queue_id
    }
    pub fn raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
    /// Lets the kernel run its receive path for this socket (`recvfrom`
    /// with `MSG_DONTWAIT`), needed when the fill ring asks for a wakeup.
    pub fn wake_rx(&mut self) {
        if self.fill.0.needs_wakeup() {
            // SAFETY: a zero-length, non-blocking recvfrom with null buffers.
            unsafe {
                libc::recvfrom(
                    self.fd.as_raw_fd(),
                    std::ptr::null_mut(),
                    0,
                    libc::MSG_DONTWAIT,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                );
            }
        }
    }

    fn range(&self, addr: u64, len: usize) -> Option<usize> {
        let start = usize::try_from(addr).ok()?;
        (start.checked_add(len)? <= self.umem_len).then_some(start)
    }
}

impl XskBackend for AfXdpBackend {}

impl BackendAccess for AfXdpBackend {
    type Fill = ProdRing<u64>;
    type Rx = ConsRing<XdpDesc>;
    type Tx = ProdRing<XdpDesc>;
    type Completion = ConsRing<u64>;
    fn fill(&mut self) -> &mut ProdRing<u64> {
        &mut self.fill
    }
    fn rx(&mut self) -> &mut ConsRing<XdpDesc> {
        &mut self.rx
    }
    fn tx(&mut self) -> &mut ProdRing<XdpDesc> {
        &mut self.tx
    }
    fn completion(&mut self) -> &mut ConsRing<u64> {
        &mut self.completion
    }
    fn umem_len(&self) -> usize {
        self.umem_len
    }
    fn frame_geometry(&self) -> (u32, u32) {
        (self.frame_size, self.headroom)
    }
    fn frame(&self, addr: u64, len: usize) -> Option<&[u8]> {
        let start = self.range(addr, len)?;
        // SAFETY: in bounds of the live UMEM mapping; see invariant 3.
        Some(unsafe { std::slice::from_raw_parts(self.umem.ptr.as_ptr().add(start), len) })
    }
    fn frame_mut(&mut self, addr: u64, len: usize) -> Option<&mut [u8]> {
        let start = self.range(addr, len)?;
        // SAFETY: as above, and `&mut self` excludes other Rust borrows.
        Some(unsafe { std::slice::from_raw_parts_mut(self.umem.ptr.as_ptr().add(start), len) })
    }
    fn kick_tx(&mut self) -> Result<(), UmemError> {
        // Without XDP_USE_NEED_WAKEUP the flag is never set, so always kick.
        let _ = self.tx.0.needs_wakeup();
        // SAFETY: a zero-length, non-blocking sendto with null buffers.
        let rc = unsafe {
            libc::sendto(
                self.fd.as_raw_fd(),
                std::ptr::null(),
                0,
                libc::MSG_DONTWAIT,
                std::ptr::null(),
                0,
            )
        };
        if rc >= 0 {
            return Ok(());
        }
        match std::io::Error::last_os_error().raw_os_error() {
            // Transient: the kernel is busy or will make progress anyway.
            Some(libc::EAGAIN | libc::EBUSY | libc::ENOBUFS | libc::ENETDOWN) => Ok(()),
            errno => Err(UmemError::Os {
                op: "sendto(AF_XDP)",
                errno: errno.unwrap_or(0),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Minimal XDP redirect program, so that receive can be exercised on `lo`.

#[repr(C)]
#[derive(Clone, Copy)]
struct BpfInsn {
    code: u8,
    regs: u8,
    off: i16,
    imm: i32,
}

const fn insn(code: u8, dst: u8, src: u8, off: i16, imm: i32) -> BpfInsn {
    BpfInsn {
        code,
        regs: (src << 4) | dst,
        off,
        imm,
    }
}

const BPF_MAP_CREATE: i64 = 0;
const BPF_MAP_UPDATE_ELEM: i64 = 2;
const BPF_PROG_LOAD: i64 = 5;
const BPF_LINK_CREATE: i64 = 28;
const BPF_MAP_TYPE_XSKMAP: u32 = 17;
const BPF_PROG_TYPE_XDP: u32 = 6;
const BPF_XDP: u32 = 37;
const XDP_FLAGS_SKB_MODE: u32 = 1 << 1;
const BPF_FUNC_REDIRECT_MAP: i32 = 51;
const XDP_PASS: i32 = 2;

#[repr(C)]
#[derive(Default)]
struct MapCreateAttr {
    map_type: u32,
    key_size: u32,
    value_size: u32,
    max_entries: u32,
    map_flags: u32,
}

#[repr(C)]
#[derive(Default)]
struct MapUpdateAttr {
    map_fd: u32,
    _pad: u32,
    key: u64,
    value: u64,
    flags: u64,
}

#[repr(C)]
#[derive(Default)]
struct ProgLoadAttr {
    prog_type: u32,
    insn_cnt: u32,
    insns: u64,
    license: u64,
    log_level: u32,
    log_size: u32,
    log_buf: u64,
    kern_version: u32,
    prog_flags: u32,
    prog_name: [u8; 16],
    prog_ifindex: u32,
    expected_attach_type: u32,
}

#[repr(C)]
#[derive(Default)]
struct LinkCreateAttr {
    prog_fd: u32,
    target_ifindex: u32,
    attach_type: u32,
    flags: u32,
}

fn bpf_raw<T>(cmd: i64, attr: &mut T, step: &'static str) -> Result<i64, AfXdpError> {
    // SAFETY: `attr` is a properly sized, zero-initialised prefix of
    // `union bpf_attr` for `cmd`; the kernel zero-extends shorter attrs.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_bpf,
            cmd,
            (attr as *mut T).cast::<libc::c_void>(),
            size_of::<T>() as libc::c_uint,
        )
    };
    if rc < 0 {
        return Err(last_error(step));
    }
    Ok(rc)
}

fn bpf_fd<T>(cmd: i64, attr: &mut T, step: &'static str) -> Result<OwnedFd, AfXdpError> {
    let rc = bpf_raw(cmd, attr, step)?;
    let fd = RawFd::try_from(rc).map_err(|_| AfXdpError {
        step,
        errno: libc::EBADF,
    })?;
    // SAFETY: the kernel returned a fresh descriptor we own.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// An attached XDP program redirecting unfragmented, option-free IPv4/UDP
/// packets with one destination port to an AF_XDP socket; every other packet
/// (including IPv4 with options and any fragment) passes to the stack.
pub struct XdpRedirect {
    _link: OwnedFd,
    _prog: OwnedFd,
    _map: OwnedFd,
}

impl XdpRedirect {
    /// Attaches in generic (skb) mode to the backend's interface.
    pub fn attach_udp_port(xsk: &AfXdpBackend, udp_dst_port: u16) -> Result<Self, AfXdpError> {
        let mut create = MapCreateAttr {
            map_type: BPF_MAP_TYPE_XSKMAP,
            key_size: 4,
            value_size: 4,
            max_entries: 1,
            map_flags: 0,
        };
        // The program redirects with key = rx_queue_index and the map has
        // one entry, so only queue 0 is supported.
        if xsk.queue_id() != 0 {
            return Err(AfXdpError {
                step: "XdpRedirect supports queue 0 only",
                errno: libc::EINVAL,
            });
        }
        let map = bpf_fd(BPF_MAP_CREATE, &mut create, "bpf(BPF_MAP_CREATE, XSKMAP)")?;
        let key: u32 = 0;
        let value: u32 = xsk.raw_fd() as u32;
        let mut update = MapUpdateAttr {
            map_fd: map.as_raw_fd() as u32,
            key: &key as *const u32 as u64,
            value: &value as *const u32 as u64,
            ..MapUpdateAttr::default()
        };
        bpf_raw(BPF_MAP_UPDATE_ELEM, &mut update, "bpf(BPF_MAP_UPDATE_ELEM)")?;
        let port_le = i32::from(udp_dst_port.swap_bytes());
        let map_fd = map.as_raw_fd();
        // Unsupported layouts (IP options, fragments) pass to the stack
        // unchanged rather than being matched at a wrong offset.
        let program = [
            insn(0x61, 2, 1, 0, 0),        // r2 = ctx->data
            insn(0x61, 3, 1, 4, 0),        // r3 = ctx->data_end
            insn(0xbf, 4, 2, 0, 0),        // r4 = r2
            insn(0x07, 4, 0, 0, 38),       // r4 += 38 (eth + 20-byte ipv4 + udp dst port)
            insn(0x2d, 4, 3, 17, 0),       // if r4 > r3 goto pass
            insn(0x69, 5, 2, 12, 0),       // r5 = ethertype
            insn(0x55, 5, 0, 15, 0x0008),  // if r5 != htons(0x0800) goto pass
            insn(0x71, 5, 2, 14, 0),       // r5 = version/IHL
            insn(0x55, 5, 0, 13, 0x45),    // if r5 != (v4, IHL 5) goto pass
            insn(0x69, 5, 2, 20, 0),       // r5 = flags/fragment offset
            insn(0x57, 5, 0, 0, 0xff3f),   // r5 &= htons(MF | offset)
            insn(0x55, 5, 0, 10, 0),       // if fragmented goto pass
            insn(0x71, 5, 2, 23, 0),       // r5 = ip protocol
            insn(0x55, 5, 0, 8, 17),       // if r5 != UDP goto pass
            insn(0x69, 5, 2, 36, 0),       // r5 = udp dst port
            insn(0x55, 5, 0, 6, port_le),  // if r5 != port goto pass
            insn(0x61, 2, 1, 16, 0),       // r2 = ctx->rx_queue_index
            insn(0x18, 1, 1, 0, map_fd),   // r1 = map (pseudo map fd), low half
            insn(0, 0, 0, 0, 0),           //   high half
            insn(0xb7, 3, 0, 0, XDP_PASS), // r3 = XDP_PASS on miss
            insn(0x85, 0, 0, 0, BPF_FUNC_REDIRECT_MAP),
            insn(0x95, 0, 0, 0, 0),        // exit
            insn(0xb7, 0, 0, 0, XDP_PASS), // pass: r0 = XDP_PASS
            insn(0x95, 0, 0, 0, 0),        // exit
        ];
        let license = b"GPL\0";
        let mut name = [0u8; 16];
        name[..10].copy_from_slice(b"vesper_xsk");
        let mut load = ProgLoadAttr {
            prog_type: BPF_PROG_TYPE_XDP,
            insn_cnt: program.len() as u32,
            insns: program.as_ptr() as u64,
            license: license.as_ptr() as u64,
            prog_name: name,
            expected_attach_type: BPF_XDP,
            ..ProgLoadAttr::default()
        };
        let prog = bpf_fd(BPF_PROG_LOAD, &mut load, "bpf(BPF_PROG_LOAD, XDP)")?;
        let mut link = LinkCreateAttr {
            prog_fd: prog.as_raw_fd() as u32,
            target_ifindex: xsk.ifindex(),
            attach_type: BPF_XDP,
            flags: XDP_FLAGS_SKB_MODE,
        };
        let link = bpf_fd(
            BPF_LINK_CREATE,
            &mut link,
            "bpf(BPF_LINK_CREATE, XDP generic)",
        )?;
        Ok(Self {
            _link: link,
            _prog: prog,
            _map: map,
        })
    }
}
