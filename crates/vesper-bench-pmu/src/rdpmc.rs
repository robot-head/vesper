//! User-space RDPMC through the Linux perf mmap metadata page (x86_64).
//!
//! Two pinned, enabled, user-only events (cycles leading, instructions as a
//! member) are opened for the current thread, and the first page of each is
//! mapped read-only. A read follows the kernel's documented protocol for
//! `struct perf_event_mmap_page`: under the page's sequence lock, check
//! `cap_user_rdpmc` and a nonzero `index`, execute `RDPMC(index - 1)`,
//! sign-extend the value from `pmc_width` bits and add the kernel `offset`.
//! No syscall runs between the two reads of a window. Hosts without a hardware
//! PMU, or with RDPMC denied, report a typed [`Unavailable`].
use crate::linux::{Attr, event_id, identity, next_origin, os_error};
use crate::{CounterDelta, Provider, Unavailable, UnavailableReason, checked_delta};
use std::{
    marker::PhantomData,
    mem,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    ptr::NonNull,
    rc::Rc,
    sync::atomic::{Ordering, compiler_fence},
    time::Instant,
};
use vesper_bench::{Invocation, WindowCompletion, WindowObserver};

// Field offsets in struct perf_event_mmap_page (include/uapi/linux/perf_event.h).
const LOCK: usize = 8;
const INDEX: usize = 12;
const OFFSET: usize = 16;
const TIME_ENABLED: usize = 24;
const TIME_RUNNING: usize = 32;
const CAPABILITIES: usize = 40;
const PMC_WIDTH: usize = 48;
const CAP_USER_RDPMC: u64 = 1 << 2;
const MAX_RETRIES: usize = 1024;

/// Fields the read protocol needs. Implemented by the real mapping and, in
/// tests, by scripted pages.
pub(crate) trait Page {
    fn u32_at(&self, offset: usize) -> u32;
    fn u64_at(&self, offset: usize) -> u64;
    fn u16_at(&self, offset: usize) -> u16;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Reading {
    pub(crate) count: u64,
    pub(crate) time_enabled: u64,
    pub(crate) time_running: u64,
}

pub(crate) fn sign_extend(raw: u64, width: u16) -> i64 {
    let shift = 64 - u32::from(width);
    ((raw << shift) as i64) >> shift
}

/// The kernel's user-space read loop. `rdpmc` receives the hardware counter
/// number (`index - 1`) and must only be called once the page grants RDPMC.
pub(crate) fn read_page(
    page: &impl Page,
    mut rdpmc: impl FnMut(u32) -> u64,
) -> Result<Reading, Unavailable> {
    for _ in 0..MAX_RETRIES {
        let seq = page.u32_at(LOCK);
        compiler_fence(Ordering::SeqCst);
        let caps = page.u64_at(CAPABILITIES);
        let index = page.u32_at(INDEX);
        let width = page.u16_at(PMC_WIDTH);
        let offset = page.u64_at(OFFSET) as i64;
        let enabled = page.u64_at(TIME_ENABLED);
        let running = page.u64_at(TIME_RUNNING);
        let result = if caps & CAP_USER_RDPMC == 0 {
            Err(UnavailableReason::RdpmcNotPermitted)
        } else if index == 0 {
            Err(UnavailableReason::NotScheduled)
        } else if width == 0 || width > 64 {
            Err(UnavailableReason::InvalidMetadata)
        } else {
            let pmc = sign_extend(rdpmc(index - 1), width);
            // The kernel defines the count as this two's-complement sum.
            Ok(Reading {
                count: offset.wrapping_add(pmc) as u64,
                time_enabled: enabled,
                time_running: running,
            })
        };
        compiler_fence(Ordering::SeqCst);
        if page.u32_at(LOCK) == seq {
            return result.map_err(|reason| Unavailable::new(reason, "rdpmc page"));
        }
    }
    Err(Unavailable::new(
        UnavailableReason::Contended,
        "rdpmc seqlock",
    ))
}

struct Mapping {
    base: NonNull<u8>,
    len: usize,
}
impl Page for Mapping {
    // SAFETY (all three): offsets are fixed, aligned fields inside the first
    // page, which stays mapped read-only until Drop; the kernel writes it
    // concurrently, hence volatile reads under the sequence lock.
    fn u32_at(&self, offset: usize) -> u32 {
        unsafe { self.base.as_ptr().add(offset).cast::<u32>().read_volatile() }
    }
    fn u64_at(&self, offset: usize) -> u64 {
        unsafe { self.base.as_ptr().add(offset).cast::<u64>().read_volatile() }
    }
    fn u16_at(&self, offset: usize) -> u16 {
        unsafe { self.base.as_ptr().add(offset).cast::<u16>().read_volatile() }
    }
}
impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: base/len are exactly the region returned by mmap.
        unsafe {
            libc::munmap(self.base.as_ptr().cast(), self.len);
        }
    }
}
fn map(fd: &OwnedFd) -> Result<Mapping, Unavailable> {
    // SAFETY: sysconf has no pointer arguments.
    let len = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if len < 4096 {
        return Err(Unavailable::new(UnavailableReason::OsError, "page size"));
    }
    let len = len as usize;
    // SAFETY: a fresh read-only shared mapping of the event's metadata page.
    let base = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ,
            libc::MAP_SHARED,
            fd.as_raw_fd(),
            0,
        )
    };
    if base == libc::MAP_FAILED {
        return Err(os_error("perf mmap"));
    }
    NonNull::new(base.cast::<u8>())
        .map(|base| Mapping { base, len })
        .ok_or_else(|| Unavailable::new(UnavailableReason::OsError, "perf mmap"))
}

fn open(config: u64, leader: Option<RawFd>) -> Result<OwnedFd, Unavailable> {
    let attr = Attr {
        size: mem::size_of::<Attr>() as u32,
        config,
        // exclude_kernel, exclude_hv; the leader is pinned so the group stays
        // on hardware counters or reports an error state, never multiplexes.
        flags: (1 << 5) | (1 << 6) | if leader.is_none() { 1 << 2 } else { 0 },
        ..Attr::default()
    };
    // SAFETY: as in the group-read provider: initialized v0 attr, current
    // task on any CPU, owned live leader FD, PERF_FLAG_FD_CLOEXEC.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_perf_event_open,
            &attr as *const Attr,
            0,
            -1,
            leader.unwrap_or(-1),
            8u64,
        )
    };
    if fd < 0 {
        let mut error = os_error("perf_event_open");
        // The kernel's answer when no PMU driver backs PERF_TYPE_HARDWARE.
        // (The group-read provider keeps reporting these as OsError.)
        if matches!(
            error.os_error,
            Some(libc::ENOENT | libc::ENODEV | libc::EOPNOTSUPP)
        ) {
            error.reason = UnavailableReason::NoHardwarePmu;
        }
        return Err(error);
    }
    // SAFETY: successful syscall returned a fresh, uniquely owned FD.
    Ok(unsafe { OwnedFd::from_raw_fd(fd as RawFd) })
}

fn rdpmc(counter: u32) -> u64 {
    let (lo, hi): (u32, u32);
    // RDPMC is not serializing. LFENCE before it waits for earlier
    // instructions to complete locally; LFENCE after it keeps later
    // instructions from starting before the read, so window boundaries do
    // not drift into or out of the measured body.
    // SAFETY: only reached after the kernel page granted cap_user_rdpmc with a
    // nonzero index for an event of this thread, so CR4.PCE permits RDPMC.
    unsafe {
        std::arch::asm!(
            "lfence",
            "rdpmc",
            "lfence",
            in("ecx") counter,
            out("eax") lo,
            out("edx") hi,
            options(nostack, preserves_flags)
        );
    }
    (u64::from(hi) << 32) | u64::from(lo)
}

/// Current-thread cycles and instructions read with RDPMC. Not Send/Sync.
pub struct RdpmcCounters {
    // Mappings drop (unmap) before the FDs close.
    pages: [Mapping; 2],
    _fds: [OwnedFd; 2],
    origin: u64,
    generation: u64,
    active: Option<u64>,
    process: libc::pid_t,
    thread: libc::pid_t,
    _thread_bound: PhantomData<Rc<()>>,
}
/// Authority for one window; neither Clone nor publicly minted.
pub struct RdpmcSnapshot {
    origin: u64,
    generation: u64,
    counts: [u64; 2],
    start: Instant,
    _thread_bound: PhantomData<Rc<()>>,
}

impl RdpmcCounters {
    /// Opens the events and verifies one real RDPMC read of each.
    pub fn open() -> Result<Self, Unavailable> {
        let origin = next_origin()?;
        let leader = open(0, None)?;
        let member = open(1, Some(leader.as_raw_fd()))?;
        let ids = [event_id(&leader)?, event_id(&member)?];
        if ids[0] == ids[1] {
            return Err(Unavailable::new(
                UnavailableReason::InvalidMetadata,
                "duplicate event IDs",
            ));
        }
        let pages = [map(&leader)?, map(&member)?];
        let (process, thread) = identity();
        let counters = Self {
            pages,
            _fds: [leader, member],
            origin,
            generation: 0,
            active: None,
            process,
            thread,
            _thread_bound: PhantomData,
        };
        counters.read()?;
        Ok(counters)
    }
    fn read(&self) -> Result<[Reading; 2], Unavailable> {
        Ok([
            read_page(&self.pages[0], rdpmc)?,
            read_page(&self.pages[1], rdpmc)?,
        ])
    }
    fn check_owner(&self) -> Result<(), Unavailable> {
        if identity() != (self.process, self.thread) {
            return Err(Unavailable::new(
                UnavailableReason::WrongProcessOrThread,
                "owning process/thread",
            ));
        }
        Ok(())
    }
}

/// The pinned group must never have been descheduled while enabled.
fn fully_scheduled(readings: &[Reading; 2]) -> Result<(), Unavailable> {
    if readings.iter().any(|r| r.time_enabled != r.time_running) {
        return Err(Unavailable::new(
            UnavailableReason::NotFullyScheduled,
            "rdpmc enabled/running",
        ));
    }
    Ok(())
}

impl WindowObserver for RdpmcCounters {
    type Token = RdpmcSnapshot;
    type Observation = CounterDelta;
    type Error = Unavailable;
    fn before_window(&mut self, _: Invocation, _: usize) -> Result<RdpmcSnapshot, Unavailable> {
        self.check_owner()?;
        if self.active.is_some() {
            return Err(Unavailable::new(
                UnavailableReason::InvalidState,
                "before_window",
            ));
        }
        let generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| Unavailable::new(UnavailableReason::Exhausted, "window generation"))?;
        let start = Instant::now();
        let readings = self.read()?;
        self.generation = generation;
        self.active = Some(generation);
        Ok(RdpmcSnapshot {
            origin: self.origin,
            generation,
            counts: [readings[0].count, readings[1].count],
            start,
            _thread_bound: PhantomData,
        })
    }
    fn after_window(
        &mut self,
        token: RdpmcSnapshot,
        _: WindowCompletion,
    ) -> Result<CounterDelta, Unavailable> {
        self.check_owner()?;
        if token.origin != self.origin || self.active != Some(token.generation) {
            return Err(Unavailable::new(
                UnavailableReason::ForeignOrStaleToken,
                "after_window",
            ));
        }
        self.active = None;
        let readings = self.read()?;
        let nanos = u64::try_from(token.start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        fully_scheduled(&readings)?;
        checked_delta(
            Provider::LinuxRdpmc,
            token.counts,
            [readings[0].count, readings[1].count],
            nanos,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// A page whose lock advances on a scripted number of reads.
    struct Scripted {
        words: [u8; 64],
        lock_reads: Cell<u32>,
        bump_lock_until: u32,
    }
    impl Scripted {
        fn new(index: u32, offset: i64, caps: u64, width: u16) -> Self {
            let mut words = [0u8; 64];
            words[INDEX..INDEX + 4].copy_from_slice(&index.to_le_bytes());
            words[OFFSET..OFFSET + 8].copy_from_slice(&offset.to_le_bytes());
            words[TIME_ENABLED..TIME_ENABLED + 8].copy_from_slice(&100u64.to_le_bytes());
            words[TIME_RUNNING..TIME_RUNNING + 8].copy_from_slice(&100u64.to_le_bytes());
            words[CAPABILITIES..CAPABILITIES + 8].copy_from_slice(&caps.to_le_bytes());
            words[PMC_WIDTH..PMC_WIDTH + 2].copy_from_slice(&width.to_le_bytes());
            Self {
                words,
                lock_reads: Cell::new(0),
                bump_lock_until: 0,
            }
        }
    }
    impl Page for Scripted {
        fn u32_at(&self, offset: usize) -> u32 {
            if offset == LOCK {
                let n = self.lock_reads.get() + 1;
                self.lock_reads.set(n);
                // Each read returns a new value while "the kernel is writing".
                return if n <= self.bump_lock_until { n } else { 0 };
            }
            u32::from_le_bytes(self.words[offset..offset + 4].try_into().unwrap())
        }
        fn u64_at(&self, offset: usize) -> u64 {
            u64::from_le_bytes(self.words[offset..offset + 8].try_into().unwrap())
        }
        fn u16_at(&self, offset: usize) -> u16 {
            u16::from_le_bytes(self.words[offset..offset + 2].try_into().unwrap())
        }
    }

    #[test]
    fn kernel_formula_sign_extends_from_pmc_width_and_adds_offset() {
        assert_eq!(sign_extend(0x7fff_ffff_ffff, 48), 0x7fff_ffff_ffff);
        assert_eq!(sign_extend(0xffff_ffff_ffff, 48), -1);
        assert_eq!(sign_extend(0x8000_0000_0000, 48), -(1 << 47));
        assert_eq!(sign_extend(u64::MAX, 64), -1);
        // Upper garbage above the width is discarded.
        assert_eq!(sign_extend(0xdead_0000_0000_0005, 48), 5);
        let page = Scripted::new(3, 1_000, CAP_USER_RDPMC, 48);
        let mut asked = None;
        let r = read_page(&page, |c| {
            asked = Some(c);
            0xffff_ffff_fff6 // -10 in 48 bits
        })
        .unwrap();
        assert_eq!(asked, Some(2));
        assert_eq!(r.count, 990);
        assert_eq!((r.time_enabled, r.time_running), (100, 100));
    }

    #[test]
    fn denied_unscheduled_and_malformed_pages_never_execute_rdpmc() {
        for (page, reason) in [
            (
                Scripted::new(1, 0, 0, 48),
                UnavailableReason::RdpmcNotPermitted,
            ),
            (
                Scripted::new(0, 0, CAP_USER_RDPMC, 48),
                UnavailableReason::NotScheduled,
            ),
            (
                Scripted::new(1, 0, CAP_USER_RDPMC, 0),
                UnavailableReason::InvalidMetadata,
            ),
            (
                Scripted::new(1, 0, CAP_USER_RDPMC, 65),
                UnavailableReason::InvalidMetadata,
            ),
        ] {
            let err = read_page(&page, |_| panic!("rdpmc must not run")).unwrap_err();
            assert_eq!(err.reason, reason);
        }
    }

    #[test]
    fn sequence_lock_retries_then_reports_contention() {
        let mut page = Scripted::new(1, 7, CAP_USER_RDPMC, 48);
        page.bump_lock_until = 5;
        let mut calls = 0;
        let r = read_page(&page, |_| {
            calls += 1;
            1
        })
        .unwrap();
        assert_eq!(r.count, 8);
        assert!(calls >= 2, "torn reads must be retried");
        let mut page = Scripted::new(1, 7, CAP_USER_RDPMC, 48);
        page.bump_lock_until = u32::MAX;
        assert_eq!(
            read_page(&page, |_| 1).unwrap_err().reason,
            UnavailableReason::Contended
        );
    }

    #[test]
    fn multiplexed_events_are_rejected() {
        let ok = Reading {
            count: 1,
            time_enabled: 5,
            time_running: 5,
        };
        assert!(fully_scheduled(&[ok, ok]).is_ok());
        let lost = Reading {
            time_running: 4,
            ..ok
        };
        assert_eq!(
            fully_scheduled(&[ok, lost]).unwrap_err().reason,
            UnavailableReason::NotFullyScheduled
        );
    }
}
