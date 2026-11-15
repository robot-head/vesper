//! Real current-thread hardware-event observations, never a clock fallback.
//!
//! Three providers, each producing the same nonforgeable [`CounterDelta`]:
//!
//! * [`PerfGroup`]: Linux group-read syscalls on little-endian 64-bit
//!   x86_64/aarch64 (`linux.perf_event_open.group_read`).
//! * [`RdpmcCounters`]: Linux user-space counter reads through the perf mmap
//!   metadata page, with no syscall per window (`linux.perf_event_mmap.rdpmc`):
//!   `RDPMC` on x86_64, `PMCCNTR_EL0`/`PMEVCNTR<n>_EL0` under
//!   `perf_user_access` on AArch64.
//!
//! [`PerfEventSet`] adds L1D/LLC read misses, branch mispredictions,
//! reference cycles, SIMD operations and Top-Down slots where the kernel or
//! sysfs exposes them ([`events`]); [`AllocationObserver`] reads the
//! [`CountingAllocator`] around each window.
//! * [`KpcCounters`]: macOS AArch64 fixed thread counters through the private
//!   kperf framework, loaded with `dlopen`; requires root (`macos.kperf.kpc`).
//!
//! Every provider reports a typed [`Unavailable`] instead of a count when the
//! host cannot supply real counters, and [`host_pmu_status`] explains why on
//! Linux (no `cpu` PMU device, `perf_event_paranoid`). Counts exclude
//! kernel/hypervisor execution. They include observer boundary work; they are
//! not calibrated body-only costs. No affinity, sysctl or privilege settings
//! are changed.
//!
//! Group-read counts are cumulative kernel u64 accounting. RDPMC counts are the
//! kernel's `offset + sign_extend(pmc, pmc_width)` formula, read under the
//! page's sequence lock. A delta requires monotonic values and a fully
//! scheduled interval. No multiplex scaling is performed. Availability alone
//! proves no performance, compiler, allocation or SIMD property. Tokens and
//! counters cannot cross threads; OS process/thread checks additionally reject
//! use after fork.
//!
//! Thread ownership is enforced by the Rust API as well as OS identity checks:
//! ```compile_fail
//! fn require_send<T: Send>() {}
//! require_send::<vesper_bench_pmu::PerfGroup>();
//! ```
//! ```compile_fail
//! fn require_send<T: Send>() {}
//! require_send::<vesper_bench_pmu::Snapshot>();
//! ```
//! ```compile_fail
//! fn require_send<T: Send>() {}
//! require_send::<vesper_bench_pmu::CounterDelta>();
//! ```
//! ```compile_fail
//! fn require_send<T: Send>() {}
//! require_send::<vesper_bench_pmu::RdpmcCounters>();
//! ```
//! ```compile_fail
//! fn require_send<T: Send>() {}
//! require_send::<vesper_bench_pmu::KpcCounters>();
//! ```
#![cfg_attr(
    not(any(
        all(
            target_os = "linux",
            target_pointer_width = "64",
            target_endian = "little",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        all(target_os = "macos", target_arch = "aarch64")
    )),
    deny(unsafe_code)
)]

use std::{fmt, marker::PhantomData, rc::Rc};

pub mod alloc;
pub mod events;
pub use alloc::{
    AllocationCounts, AllocationDelta, AllocationObserver, CountingAllocator,
    counting_allocator_installed,
};
pub use events::HardwareEvent;

/// Structured failure; no fabricated zero count is substituted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnavailableReason {
    UnsupportedPlatform,
    PermissionDenied,
    OsError,
    InvalidRead,
    InvalidMetadata,
    WrongProcessOrThread,
    InvalidState,
    ForeignOrStaleToken,
    Exhausted,
    CounterRegression,
    NotFullyScheduled,
    NoEvents,
    /// The kernel has no hardware PMU for these events (ENOENT/ENODEV/EOPNOTSUPP).
    NoHardwarePmu,
    /// The perf mmap page does not grant user-space RDPMC (`cap_user_rdpmc`).
    RdpmcNotPermitted,
    /// The event is not on a hardware counter at read time (page index 0).
    NotScheduled,
    /// The page sequence lock kept changing; no consistent read.
    Contended,
    /// The kperf framework or one of its symbols could not be loaded.
    LibraryMissing,
    /// [`CountingAllocator`] is not this process's global allocator.
    AllocatorNotInstalled,
    /// The OS exposes no encoding for this event on this PMU (no generic
    /// perf event and no sysfs alias).
    EventNotExposed,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unavailable {
    pub reason: UnavailableReason,
    pub operation: &'static str,
    pub os_error: Option<i32>,
}
impl Unavailable {
    fn new(reason: UnavailableReason, operation: &'static str) -> Self {
        Self {
            reason,
            operation,
            os_error: None,
        }
    }
}
impl fmt::Display for Unavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "PMU unavailable: {:?} at {}",
            self.reason, self.operation
        )?;
        if let Some(errno) = self.os_error {
            write!(f, " (OS error {errno})")?;
        }
        Ok(())
    }
}
impl std::error::Error for Unavailable {}

/// Which mechanism produced a [`CounterDelta`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    LinuxGroupRead,
    LinuxRdpmc,
    MacosKpc,
}
impl Provider {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LinuxGroupRead => "linux.perf_event_open.group_read",
            Self::LinuxRdpmc => "linux.perf_event_mmap.rdpmc",
            Self::MacosKpc => "macos.kperf.kpc",
        }
    }
}

/// Provider-produced, nonforgeable raw hardware-event delta. No public constructor.
#[derive(Debug)]
pub struct CounterDelta {
    provider: Provider,
    cycles: u64,
    instructions: u64,
    enabled: u64,
    running: u64,
    _thread_bound: PhantomData<Rc<()>>,
}
impl CounterDelta {
    pub fn cycles(&self) -> u64 {
        self.cycles
    }
    pub fn instructions(&self) -> u64 {
        self.instructions
    }
    /// Group read: kernel enabled time. RDPMC and kpc: the observer's
    /// `Instant` interval between its two reads (equal to running time once
    /// the provider has verified the event stayed scheduled).
    pub fn enabled_nanos(&self) -> u64 {
        self.enabled
    }
    pub fn running_nanos(&self) -> u64 {
        self.running
    }
    pub fn provider(&self) -> &'static str {
        self.provider.as_str()
    }
    pub fn provider_kind(&self) -> Provider {
        self.provider
    }
    pub fn unit(&self) -> &'static str {
        "hardware events"
    }
}

#[cfg(all(
    target_os = "linux",
    target_pointer_width = "64",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod linux;
#[cfg(all(
    target_os = "linux",
    target_pointer_width = "64",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub use linux::{EventSetToken, PerfEventSet, PerfGroup, Snapshot};

/// Per-window extended event counts, one entry per requested event, each a
/// real delta or the reason it is unavailable. Zero is a valid count here
/// (for example no LLC misses); unavailability is never a zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventDelta {
    pub values: Vec<(HardwareEvent, Result<u64, Unavailable>)>,
}
impl EventDelta {
    pub fn get(&self, event: HardwareEvent) -> Option<&Result<u64, Unavailable>> {
        self.values
            .iter()
            .find(|(e, _)| *e == event)
            .map(|(_, r)| r)
    }
}

#[cfg(not(all(
    target_os = "linux",
    target_pointer_width = "64",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
mod unsupported {
    use super::*;
    use vesper_bench::{Invocation, WindowCompletion, WindowObserver};
    pub struct PerfGroup {
        _thread_bound: PhantomData<Rc<()>>,
    }
    pub struct Snapshot {
        _thread_bound: PhantomData<Rc<()>>,
    }
    impl PerfGroup {
        pub fn open() -> Result<Self, Unavailable> {
            Err(Unavailable::new(
                UnavailableReason::UnsupportedPlatform,
                "perf_event_open",
            ))
        }
    }
    /// Extended events need Linux perf_event_open.
    pub struct PerfEventSet {
        _thread_bound: PhantomData<Rc<()>>,
    }
    pub struct EventSetToken {
        _thread_bound: PhantomData<Rc<()>>,
    }
    impl PerfEventSet {
        pub fn open(_: &[HardwareEvent]) -> Result<Self, Unavailable> {
            Err(Unavailable::new(
                UnavailableReason::UnsupportedPlatform,
                "perf_event_open (extended events)",
            ))
        }
        pub fn opened(&self) -> Vec<(HardwareEvent, String)> {
            Vec::new()
        }
        pub fn unavailable(&self) -> &[(HardwareEvent, Unavailable)] {
            &[]
        }
    }
    impl WindowObserver for PerfEventSet {
        type Token = EventSetToken;
        type Observation = EventDelta;
        type Error = Unavailable;
        fn before_window(&mut self, _: Invocation, _: usize) -> Result<EventSetToken, Unavailable> {
            Err(Unavailable::new(
                UnavailableReason::UnsupportedPlatform,
                "before_window",
            ))
        }
        fn after_window(
            &mut self,
            _: EventSetToken,
            _: WindowCompletion,
        ) -> Result<EventDelta, Unavailable> {
            Err(Unavailable::new(
                UnavailableReason::UnsupportedPlatform,
                "after_window",
            ))
        }
    }
    impl WindowObserver for PerfGroup {
        type Token = Snapshot;
        type Observation = CounterDelta;
        type Error = Unavailable;
        fn before_window(&mut self, _: Invocation, _: usize) -> Result<Snapshot, Unavailable> {
            Err(Unavailable::new(
                UnavailableReason::UnsupportedPlatform,
                "before_window",
            ))
        }
        fn after_window(
            &mut self,
            _: Snapshot,
            _: WindowCompletion,
        ) -> Result<CounterDelta, Unavailable> {
            Err(Unavailable::new(
                UnavailableReason::UnsupportedPlatform,
                "after_window",
            ))
        }
    }
}
#[cfg(not(all(
    target_os = "linux",
    target_pointer_width = "64",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
pub use unsupported::{EventSetToken, PerfEventSet, PerfGroup, Snapshot};

// Used by the RDPMC and kpc providers; neither exists on other targets.
#[cfg_attr(
    not(any(
        all(
            target_os = "linux",
            target_pointer_width = "64",
            target_endian = "little",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        all(target_os = "macos", target_arch = "aarch64")
    )),
    allow(dead_code)
)]
fn checked_delta(
    provider: Provider,
    before: [u64; 2],
    after: [u64; 2],
    nanos: u64,
) -> Result<CounterDelta, Unavailable> {
    let sub = |a: u64, b: u64| {
        a.checked_sub(b)
            .ok_or_else(|| Unavailable::new(UnavailableReason::CounterRegression, "counter delta"))
    };
    let cycles = sub(after[0], before[0])?;
    let instructions = sub(after[1], before[1])?;
    if nanos == 0 {
        return Err(Unavailable::new(
            UnavailableReason::NotFullyScheduled,
            "window interval",
        ));
    }
    if cycles == 0 || instructions == 0 {
        return Err(Unavailable::new(
            UnavailableReason::NoEvents,
            "window hardware events",
        ));
    }
    Ok(CounterDelta {
        provider,
        cycles,
        instructions,
        enabled: nanos,
        running: nanos,
        _thread_bound: PhantomData,
    })
}

/// Why a host can or cannot provide hardware counters, as far as can be read
/// without opening events. Linux reads `/proc/sys/kernel/perf_event_paranoid`
/// and checks for a `cpu` (or `armv8_pmuv3*`) device under
/// `/sys/bus/event_source/devices`; other hosts report `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostPmuStatus {
    pub perf_event_paranoid: Option<i32>,
    pub cpu_pmu_device: Option<bool>,
    /// `/sys/bus/event_source/devices/cpu/rdpmc`, when present (1 or 2 allow RDPMC).
    pub rdpmc_sysfs: Option<i32>,
    /// AArch64 `/proc/sys/kernel/perf_user_access` (1 allows EL0 counter reads).
    pub perf_user_access: Option<i32>,
}
pub fn host_pmu_status() -> HostPmuStatus {
    #[cfg(target_os = "linux")]
    {
        let read = |path: &str| {
            std::fs::read_to_string(path)
                .ok()
                .and_then(|s| s.trim().parse::<i32>().ok())
        };
        let devices = std::fs::read_dir("/sys/bus/event_source/devices").ok();
        let cpu = devices.map(|entries| {
            entries.flatten().any(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                name == "cpu" || name == "cpu_core" || name.starts_with("armv8_pmuv3")
            })
        });
        HostPmuStatus {
            perf_event_paranoid: read("/proc/sys/kernel/perf_event_paranoid"),
            cpu_pmu_device: cpu,
            rdpmc_sysfs: read("/sys/bus/event_source/devices/cpu/rdpmc"),
            perf_user_access: read("/proc/sys/kernel/perf_user_access"),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        HostPmuStatus {
            perf_event_paranoid: None,
            cpu_pmu_device: None,
            rdpmc_sysfs: None,
            perf_user_access: None,
        }
    }
}

#[cfg(all(
    target_os = "linux",
    target_pointer_width = "64",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod rdpmc;
#[cfg(all(
    target_os = "linux",
    target_pointer_width = "64",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub use rdpmc::{RdpmcCounters, RdpmcSnapshot};

#[cfg(not(all(
    target_os = "linux",
    target_pointer_width = "64",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
mod rdpmc_unsupported {
    use super::*;
    use vesper_bench::{Invocation, WindowCompletion, WindowObserver};
    /// User-space counter reads are implemented for Linux x86_64 (RDPMC)
    /// and Linux AArch64 (`perf_user_access`) only.
    pub struct RdpmcCounters {
        _thread_bound: PhantomData<Rc<()>>,
    }
    pub struct RdpmcSnapshot {
        _thread_bound: PhantomData<Rc<()>>,
    }
    impl RdpmcCounters {
        pub fn open() -> Result<Self, Unavailable> {
            Err(Unavailable::new(
                UnavailableReason::UnsupportedPlatform,
                "rdpmc",
            ))
        }
    }
    impl WindowObserver for RdpmcCounters {
        type Token = RdpmcSnapshot;
        type Observation = CounterDelta;
        type Error = Unavailable;
        fn before_window(&mut self, _: Invocation, _: usize) -> Result<RdpmcSnapshot, Unavailable> {
            Err(Unavailable::new(
                UnavailableReason::UnsupportedPlatform,
                "rdpmc",
            ))
        }
        fn after_window(
            &mut self,
            _: RdpmcSnapshot,
            _: WindowCompletion,
        ) -> Result<CounterDelta, Unavailable> {
            Err(Unavailable::new(
                UnavailableReason::UnsupportedPlatform,
                "rdpmc",
            ))
        }
    }
}
#[cfg(not(all(
    target_os = "linux",
    target_pointer_width = "64",
    target_endian = "little",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
pub use rdpmc_unsupported::{RdpmcCounters, RdpmcSnapshot};

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod kpc;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub use kpc::{KpcCounters, KpcSnapshot};

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
mod kpc_unsupported {
    use super::*;
    use vesper_bench::{Invocation, WindowCompletion, WindowObserver};
    /// kpc is implemented for macOS AArch64 only.
    pub struct KpcCounters {
        _thread_bound: PhantomData<Rc<()>>,
    }
    pub struct KpcSnapshot {
        _thread_bound: PhantomData<Rc<()>>,
    }
    impl KpcCounters {
        pub fn open() -> Result<Self, Unavailable> {
            Err(Unavailable::new(
                UnavailableReason::UnsupportedPlatform,
                "kpc",
            ))
        }
    }
    impl WindowObserver for KpcCounters {
        type Token = KpcSnapshot;
        type Observation = CounterDelta;
        type Error = Unavailable;
        fn before_window(&mut self, _: Invocation, _: usize) -> Result<KpcSnapshot, Unavailable> {
            Err(Unavailable::new(
                UnavailableReason::UnsupportedPlatform,
                "kpc",
            ))
        }
        fn after_window(
            &mut self,
            _: KpcSnapshot,
            _: WindowCompletion,
        ) -> Result<CounterDelta, Unavailable> {
            Err(Unavailable::new(
                UnavailableReason::UnsupportedPlatform,
                "kpc",
            ))
        }
    }
}
#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
pub use kpc_unsupported::{KpcCounters, KpcSnapshot};

#[cfg(test)]
mod delta_tests {
    use super::*;
    #[test]
    fn shared_delta_rejects_regression_empty_and_zero_interval() {
        let d = checked_delta(Provider::LinuxRdpmc, [10, 20], [110, 70], 5).unwrap();
        assert_eq!((d.cycles(), d.instructions()), (100, 50));
        assert_eq!((d.enabled_nanos(), d.running_nanos()), (5, 5));
        assert_eq!(d.provider(), "linux.perf_event_mmap.rdpmc");
        let reason = |r: Result<CounterDelta, Unavailable>| r.unwrap_err().reason;
        assert_eq!(
            reason(checked_delta(Provider::MacosKpc, [10, 20], [9, 70], 5)),
            UnavailableReason::CounterRegression
        );
        assert_eq!(
            reason(checked_delta(Provider::MacosKpc, [10, 20], [10, 70], 5)),
            UnavailableReason::NoEvents
        );
        assert_eq!(
            reason(checked_delta(Provider::MacosKpc, [10, 20], [11, 70], 0)),
            UnavailableReason::NotFullyScheduled
        );
    }
}
