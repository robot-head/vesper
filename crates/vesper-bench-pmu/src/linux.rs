use super::*;
use std::{
    io, mem,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    sync::atomic::{AtomicU64, Ordering},
};
use vesper_bench::{Invocation, WindowCompletion, WindowObserver};

const GROUP: libc::c_ulong = 1;
const ENABLE: libc::Ioctl = 0x2400;
const DISABLE: libc::Ioctl = 0x2401;
const ID: libc::Ioctl = 0x8008_2407;
const READ_FORMAT: u64 = 1 | 2 | 4 | 8; // enabled, running, ID, GROUP
static NEXT_ORIGIN: AtomicU64 = AtomicU64::new(1);

// Linux perf_event_attr version 0 (64 bytes), no pointers or padding with
// uninitialized contents. Supported LE64 targets use this documented ABI.
#[repr(C)]
#[derive(Default)]
pub(crate) struct Attr {
    pub(crate) event_type: u32,
    pub(crate) size: u32,
    pub(crate) config: u64,
    pub(crate) sample_period: u64,
    pub(crate) sample_type: u64,
    pub(crate) read_format: u64,
    pub(crate) flags: u64,
    pub(crate) wakeup_events: u32,
    pub(crate) bp_type: u32,
    pub(crate) bp_addr: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Counts {
    cycles: u64,
    instructions: u64,
    enabled: u64,
    running: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Disabled,
    Active(u64),
    Poisoned,
}

/// Owned current-thread cycles/instructions group. FDs close on every partial
/// construction failure and Drop. Not Send/Sync; never inherited by new execs.
pub struct PerfGroup {
    leader: OwnedFd,
    _member: OwnedFd,
    ids: [u64; 2],
    origin: u64,
    generation: u64,
    process: libc::pid_t,
    thread: libc::pid_t,
    state: State,
    _thread_bound: PhantomData<Rc<()>>,
}
/// Private authority for one enabled window; neither Clone nor publicly minted.
pub struct Snapshot {
    origin: u64,
    generation: u64,
    process: libc::pid_t,
    thread: libc::pid_t,
    counts: Counts,
    _thread_bound: PhantomData<Rc<()>>,
}
pub(crate) fn os_error(operation: &'static str) -> Unavailable {
    let errno = io::Error::last_os_error().raw_os_error();
    Unavailable {
        reason: if matches!(errno, Some(libc::EACCES | libc::EPERM)) {
            UnavailableReason::PermissionDenied
        } else {
            UnavailableReason::OsError
        },
        operation,
        os_error: errno,
    }
}
pub(crate) fn identity() -> (libc::pid_t, libc::pid_t) {
    // SAFETY: getpid/gettid have no pointer arguments and return the calling IDs.
    unsafe {
        (
            libc::getpid(),
            libc::syscall(libc::SYS_gettid) as libc::pid_t,
        )
    }
}
fn open_event(config: u64, leader: Option<RawFd>) -> Result<OwnedFd, Unavailable> {
    let attr = Attr {
        size: mem::size_of::<Attr>() as u32,
        config,
        read_format: READ_FORMAT,
        flags: (1 << 5) | (1 << 6) | if leader.is_none() { 1 | (1 << 2) } else { 0 },
        ..Attr::default()
    };
    // SAFETY: attr is the fully initialized v0 ABI, alive for syscall; pid0,
    // cpu-1 select current task on any CPU; supplied group FD is owned/alive.
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
        return Err(os_error("perf_event_open"));
    }
    // Kernel FDs are nonnegative C ints; successful return fits RawFd.
    let fd = fd as RawFd;
    // SAFETY: successful syscall returned a fresh, uniquely owned FD.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: fcntl F_GETFD only inspects this owned, live FD.
    let flags = unsafe { libc::fcntl(owned.as_raw_fd(), libc::F_GETFD) };
    if flags < 0 {
        return Err(os_error("fcntl(F_GETFD)"));
    }
    if flags & libc::FD_CLOEXEC == 0 {
        return Err(Unavailable::new(
            UnavailableReason::InvalidMetadata,
            "CLOEXEC",
        ));
    }
    Ok(owned)
}
pub(crate) fn event_id(fd: &OwnedFd) -> Result<u64, Unavailable> {
    let mut id = 0u64;
    // SAFETY: ID ioctl writes one u64 to a valid aligned output and FD is live.
    if unsafe { libc::ioctl(fd.as_raw_fd(), ID, &mut id) } < 0 {
        return Err(os_error("PERF_EVENT_IOC_ID"));
    }
    if id == 0 {
        return Err(Unavailable::new(
            UnavailableReason::InvalidMetadata,
            "zero event ID",
        ));
    }
    Ok(id)
}
fn control(fd: &OwnedFd, request: libc::Ioctl) -> Result<(), Unavailable> {
    // SAFETY: these documented ioctls use an integer group flag, not a pointer.
    if unsafe { libc::ioctl(fd.as_raw_fd(), request, GROUP) } < 0 {
        Err(os_error(if request == ENABLE {
            "group enable"
        } else {
            "group disable"
        }))
    } else {
        Ok(())
    }
}
fn decode(bytes: &[u8], ids: [u64; 2]) -> Result<Counts, Unavailable> {
    if bytes.len() != 56 {
        return Err(Unavailable::new(
            UnavailableReason::InvalidRead,
            "group read length",
        ));
    }
    let mut words = [0u64; 7];
    for (word, bytes) in words.iter_mut().zip(bytes.as_chunks::<8>().0) {
        *word = u64::from_le_bytes(*bytes);
    }
    let [nr, enabled, running, v0, id0, v1, id1] = words;
    if nr != 2 || ids[0] == 0 || ids[1] == 0 || ids[0] == ids[1] || running > enabled {
        return Err(Unavailable::new(
            UnavailableReason::InvalidMetadata,
            "group read metadata",
        ));
    }
    let (cycles, instructions) = if [id0, id1] == ids {
        (v0, v1)
    } else if [id1, id0] == ids {
        (v1, v0)
    } else {
        return Err(Unavailable::new(
            UnavailableReason::InvalidMetadata,
            "group event IDs",
        ));
    };
    Ok(Counts {
        cycles,
        instructions,
        enabled,
        running,
    })
}
fn read_counts(fd: &OwnedFd, ids: [u64; 2]) -> Result<Counts, Unavailable> {
    // One bounded read, with room for an extra word to reject mismatched ABI.
    let mut bytes = [0u8; 64];
    // SAFETY: writable initialized buffer has the provided length, FD is live.
    let read = unsafe { libc::read(fd.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len()) };
    if read < 0 {
        return Err(os_error("group read"));
    }
    decode(&bytes[..read as usize], ids)
}
fn delta(before: Counts, after: Counts) -> Result<CounterDelta, Unavailable> {
    let subtract = |a: u64, b: u64| {
        a.checked_sub(b)
            .ok_or_else(|| Unavailable::new(UnavailableReason::CounterRegression, "counter delta"))
    };
    let cycles = subtract(after.cycles, before.cycles)?;
    let instructions = subtract(after.instructions, before.instructions)?;
    let enabled = subtract(after.enabled, before.enabled)?;
    let running = subtract(after.running, before.running)?;
    if enabled == 0 || enabled != running {
        return Err(Unavailable::new(
            UnavailableReason::NotFullyScheduled,
            "window enabled/running",
        ));
    }
    if cycles == 0 || instructions == 0 {
        return Err(Unavailable::new(
            UnavailableReason::NoEvents,
            "window hardware events",
        ));
    }
    Ok(CounterDelta {
        provider: Provider::LinuxGroupRead,
        cycles,
        instructions,
        enabled,
        running,
        _thread_bound: PhantomData,
    })
}
pub(crate) fn next_origin() -> Result<u64, Unavailable> {
    let mut value = NEXT_ORIGIN.load(Ordering::Relaxed);
    loop {
        let next = value
            .checked_add(1)
            .ok_or_else(|| Unavailable::new(UnavailableReason::Exhausted, "group origin"))?;
        match NEXT_ORIGIN.compare_exchange_weak(value, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return Ok(value),
            Err(current) => value = current,
        }
    }
}
fn acquire_events(
    mut open: impl FnMut(u64, Option<RawFd>) -> Result<OwnedFd, Unavailable>,
) -> Result<(OwnedFd, OwnedFd), Unavailable> {
    let leader = open(0, None)?;
    let member = open(1, Some(leader.as_raw_fd()))?;
    Ok((leader, member))
}
impl PerfGroup {
    pub fn open() -> Result<Self, Unavailable> {
        let origin = next_origin()?;
        let (leader, member) = acquire_events(open_event)?;
        let ids = [event_id(&leader)?, event_id(&member)?];
        if ids[0] == ids[1] {
            return Err(Unavailable::new(
                UnavailableReason::InvalidMetadata,
                "duplicate group IDs",
            ));
        }
        let (process, thread) = identity();
        let group = Self {
            leader,
            _member: member,
            ids,
            origin,
            generation: 0,
            process,
            thread,
            state: State::Disabled,
            _thread_bound: PhantomData,
        };
        read_counts(&group.leader, group.ids)?;
        Ok(group)
    }
    fn check_owner(&self) -> Result<(), Unavailable> {
        if identity() != (self.process, self.thread) {
            Err(Unavailable::new(
                UnavailableReason::WrongProcessOrThread,
                "owning process/thread",
            ))
        } else {
            Ok(())
        }
    }
    fn poison(&mut self) {
        self.state = State::Poisoned;
        // Only invoked after successful owner validation. Best-effort disable;
        // the poisoned group is never reused regardless of this ioctl result.
        let _ = control(&self.leader, DISABLE);
    }
}
impl WindowObserver for PerfGroup {
    type Token = Snapshot;
    type Observation = CounterDelta;
    type Error = Unavailable;
    fn before_window(&mut self, _: Invocation, _: usize) -> Result<Snapshot, Unavailable> {
        self.check_owner()?;
        if self.state != State::Disabled {
            return Err(Unavailable::new(
                UnavailableReason::InvalidState,
                "before_window",
            ));
        }
        let generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| Unavailable::new(UnavailableReason::Exhausted, "window generation"))?;
        let counts = match read_counts(&self.leader, self.ids) {
            Ok(c) => c,
            Err(e) => {
                self.poison();
                return Err(e);
            }
        };
        if let Err(e) = control(&self.leader, ENABLE) {
            self.poison();
            return Err(e);
        }
        self.generation = generation;
        self.state = State::Active(generation);
        Ok(Snapshot {
            origin: self.origin,
            generation,
            process: self.process,
            thread: self.thread,
            counts,
            _thread_bound: PhantomData,
        })
    }
    fn after_window(
        &mut self,
        token: Snapshot,
        _: WindowCompletion,
    ) -> Result<CounterDelta, Unavailable> {
        self.check_owner()?;
        if token.origin != self.origin
            || (token.process, token.thread) != (self.process, self.thread)
            || self.state != State::Active(token.generation)
        {
            return Err(Unavailable::new(
                UnavailableReason::ForeignOrStaleToken,
                "after_window",
            ));
        }
        if let Err(e) = control(&self.leader, DISABLE) {
            self.poison();
            return Err(e);
        }
        let after = match read_counts(&self.leader, self.ids) {
            Ok(c) => c,
            Err(e) => {
                self.poison();
                return Err(e);
            }
        };
        self.state = State::Disabled;
        // Accounting/metadata failures fail this observation and poison future
        // reuse, avoiding evidence after any uncertain reset/accounting history.
        match delta(token.counts, after) {
            Ok(d) => Ok(d),
            Err(e) => {
                self.poison();
                Err(e)
            }
        }
    }
}
impl Drop for PerfGroup {
    fn drop(&mut self) {
        // In a fork child, close only this process's copies; do not disable the
        // parent's live group through its shared open file descriptions.
        if identity() == (self.process, self.thread) {
            let _ = control(&self.leader, DISABLE);
        }
        // OwnedFd fields close both FDs, including on errors.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn wire(words: [u64; 7]) -> Vec<u8> {
        words.into_iter().flat_map(u64::to_le_bytes).collect()
    }
    #[test]
    fn independent_kernel_group_read_fixtures_and_id_order() {
        // Linux documented ABI: nr, total_time_enabled, total_time_running,
        // (value,id) pairs. Literal bytes, independently ordered event IDs.
        let bytes = [
            2, 0, 0, 0, 0, 0, 0, 0, 100, 0, 0, 0, 0, 0, 0, 0, 90, 0, 0, 0, 0, 0, 0, 0, 30, 0, 0, 0,
            0, 0, 0, 0, 22, 0, 0, 0, 0, 0, 0, 0, 80, 0, 0, 0, 0, 0, 0, 0, 11, 0, 0, 0, 0, 0, 0, 0,
        ];
        assert_eq!(
            decode(&bytes, [11, 22]).unwrap(),
            Counts {
                cycles: 80,
                instructions: 30,
                enabled: 100,
                running: 90
            }
        );
        assert_eq!(
            decode(&wire([2, 100, 90, 80, 11, 30, 22]), [11, 22]).unwrap(),
            decode(&bytes, [11, 22]).unwrap()
        );
        for len in [0, 1, 48, 55, 57, 64] {
            assert!(decode(&vec![0; len], [11, 22]).is_err());
        }
        for words in [
            [1, 100, 90, 80, 11, 30, 22],
            [3, 100, 90, 80, 11, 30, 22],
            [2, 100, 101, 80, 11, 30, 22],
            [2, 100, 90, 80, 11, 30, 11],
            [2, 100, 90, 80, 11, 30, 23],
            [2, 100, 90, 80, 0, 30, 22],
        ] {
            assert!(decode(&wire(words), [11, 22]).is_err());
        }
        assert!(decode(&bytes, [11, 11]).is_err());
        assert!(decode(&bytes, [0, 22]).is_err());
    }
    #[test]
    fn checked_accounting_uses_full_u64_and_interval_scheduling() {
        let before = Counts {
            cycles: (1 << 48) + 4,
            instructions: 10,
            enabled: 100,
            running: 80,
        };
        let after = Counts {
            cycles: (1 << 48) + 104,
            instructions: 70,
            enabled: 200,
            running: 180,
        };
        let d = delta(before, after).unwrap();
        assert_eq!(
            (
                d.cycles(),
                d.instructions(),
                d.enabled_nanos(),
                d.running_nanos()
            ),
            (100, 60, 100, 100)
        );
        assert_eq!(d.provider(), "linux.perf_event_open.group_read");
        // Prior scheduling loss is acceptable; loss within the window is not.
        assert_eq!(
            delta(
                before,
                Counts {
                    running: 179,
                    ..after
                }
            )
            .unwrap_err()
            .reason,
            UnavailableReason::NotFullyScheduled
        );
        for bad in [
            Counts { cycles: 3, ..after },
            Counts {
                instructions: 9,
                ..after
            },
            Counts {
                enabled: 99,
                ..after
            },
            Counts {
                running: 79,
                ..after
            },
        ] {
            assert_eq!(
                delta(before, bad).unwrap_err().reason,
                UnavailableReason::CounterRegression
            );
        }
        assert_eq!(
            delta(before, before).unwrap_err().reason,
            UnavailableReason::NotFullyScheduled
        );
        assert_eq!(
            delta(
                before,
                Counts {
                    cycles: before.cycles,
                    ..after
                }
            )
            .unwrap_err()
            .reason,
            UnavailableReason::NoEvents
        );
        let d = delta(
            Counts {
                cycles: u64::MAX - 2,
                instructions: 1,
                enabled: 1,
                running: 1,
            },
            Counts {
                cycles: u64::MAX,
                instructions: 2,
                enabled: 2,
                running: 2,
            },
        )
        .unwrap();
        assert_eq!(d.cycles(), 2);
        assert!(
            delta(
                Counts {
                    cycles: u64::MAX,
                    ..before
                },
                Counts { cycles: 0, ..after }
            )
            .is_err()
        );
    }
    #[test]
    fn actual_owned_fd_partial_acquisition_is_closed_on_error() {
        use std::io::Read;
        use std::os::unix::net::UnixStream;
        let (stream, mut peer) = UnixStream::pair().unwrap();
        peer.set_nonblocking(true).unwrap();
        let mut first = Some(OwnedFd::from(stream));
        let error = acquire_events(|config, leader| {
            if config == 0 {
                assert!(leader.is_none());
                Ok(first.take().unwrap())
            } else {
                assert!(leader.is_some());
                Err(Unavailable::new(
                    UnavailableReason::OsError,
                    "injected member open",
                ))
            }
        })
        .unwrap_err();
        assert_eq!(error.operation, "injected member open");
        // EOF from the actual socket peer proves its returned OwnedFd closed.
        assert_eq!(peer.read(&mut [0u8; 1]).unwrap(), 0);
    }
    #[test]
    fn documented_attr_v0_layout_is_exact() {
        assert_eq!(mem::size_of::<Attr>(), 64);
        assert_eq!(mem::offset_of!(Attr, read_format), 32);
        assert_eq!(mem::offset_of!(Attr, flags), 40);
        assert_eq!(mem::offset_of!(Attr, bp_addr), 56);
        assert_eq!(READ_FORMAT, 15);
    }
}

#[cfg(test)]
mod authority_tests {
    use super::*;
    fn group() -> PerfGroup {
        use std::os::unix::net::UnixStream;
        let (leader, member) = UnixStream::pair().unwrap();
        let (process, thread) = identity();
        PerfGroup {
            leader: OwnedFd::from(leader),
            _member: OwnedFd::from(member),
            ids: [11, 22],
            origin: next_origin().unwrap(),
            generation: 0,
            process,
            thread,
            state: State::Disabled,
            _thread_bound: PhantomData,
        }
    }
    fn token(g: &PerfGroup, generation: u64) -> Snapshot {
        Snapshot {
            origin: g.origin,
            generation,
            process: g.process,
            thread: g.thread,
            counts: Counts {
                cycles: 0,
                instructions: 0,
                enabled: 0,
                running: 0,
            },
            _thread_bound: PhantomData,
        }
    }
    fn invocation() -> Invocation {
        Invocation {
            arm: vesper_bench::Arm::Baseline,
            epoch: 0,
            window: 0,
            operation: 0,
            warmup: false,
            layout_offset_bytes: 0,
        }
    }
    fn completion() -> WindowCompletion {
        WindowCompletion {
            invocation: invocation(),
            planned_operations: 1,
            attempted_operations: 1,
            successful_operations: 1,
            body: vesper_bench::BodyStatus::Completed,
            clock: vesper_bench::ClockStatus::Measured(1),
        }
    }
    #[test]
    fn foreign_and_stale_authorities_do_not_touch_current_active_window() {
        let mut a = group();
        let b = group();
        a.state = State::Active(3);
        a.generation = 3;
        assert_eq!(
            a.after_window(token(&b, 3), completion())
                .unwrap_err()
                .reason,
            UnavailableReason::ForeignOrStaleToken
        );
        assert_eq!(a.state, State::Active(3));
        assert_eq!(
            a.after_window(token(&a, 2), completion())
                .unwrap_err()
                .reason,
            UnavailableReason::ForeignOrStaleToken
        );
        assert_eq!(a.state, State::Active(3));
        assert!(matches!(
            a.before_window(invocation(), 1),
            Err(Unavailable {
                reason: UnavailableReason::InvalidState,
                ..
            })
        ));
        assert_eq!(a.state, State::Active(3));
    }
    #[test]
    fn process_thread_mismatch_and_generation_exhaustion_fail_before_io() {
        let mut a = group();
        a.process += 1;
        assert!(matches!(
            a.before_window(invocation(), 1),
            Err(Unavailable {
                reason: UnavailableReason::WrongProcessOrThread,
                ..
            })
        ));
        a.process = identity().0;
        a.thread += 1;
        assert!(matches!(
            a.before_window(invocation(), 1),
            Err(Unavailable {
                reason: UnavailableReason::WrongProcessOrThread,
                ..
            })
        ));
        a.thread = identity().1;
        a.generation = u64::MAX;
        assert!(matches!(
            a.before_window(invocation(), 1),
            Err(Unavailable {
                reason: UnavailableReason::Exhausted,
                ..
            })
        ));
        assert_eq!(a.state, State::Disabled);
    }
    #[test]
    fn disable_error_poisoned_group_cannot_be_reused() {
        // Socket FDs deliberately do not implement the perf ioctl. This checks
        // transition failure only, and can never manufacture provider evidence.
        let mut a = group();
        a.state = State::Active(1);
        a.generation = 1;
        let t = token(&a, 1);
        assert_eq!(
            a.after_window(t, completion()).unwrap_err().reason,
            UnavailableReason::OsError
        );
        assert_eq!(a.state, State::Poisoned);
        assert!(matches!(
            a.before_window(invocation(), 1),
            Err(Unavailable {
                reason: UnavailableReason::InvalidState,
                ..
            })
        ));
    }
}
