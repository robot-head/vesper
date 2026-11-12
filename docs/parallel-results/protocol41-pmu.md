# Protocol 41 PMU observations (#75)

Implemented a real Linux current-thread cycles/instructions group and safe
ABBA observer hooks. The clock-only `run_abba` API remains available and does
not allocate observation records. Benchmark arithmetic/statistics, root CLI,
root manifests/lock, workflows and other lanes were not changed.

## Public integration API

`run_abba_observed(config, &mut baseline, &mut candidate, &mut observer)` returns
`ObservedRun<O::Observation>` or `ObservedRunError<A::Error,B::Error,O::Error>`.
`WindowObserver` owns a `Token` from `before_window(Invocation,usize)` through
`after_window(Token,WindowCompletion)`. `ObservedRun::report()` borrows the clock
report, `observations()` borrows ordered `ObservedWindow<T>` records, and
`into_parts()` returns both. Each record has public `invocation`, `completion`,
`observation`; warmups are explicitly identified rather than inferred from index.

`WindowCompletion` exposes invocation, planned/attempted/successful operation
counts, `BodyStatus::{Completed,Failed}` and
`ClockStatus::{Measured(u64),Failed(Error)}`. Attempted includes a failing body
call. Completion describes body/clock state before future teardown.
`ObservedRunError::Run` retains the original error, optional secondary observer
failure and optional completion; `Observer` retains failure/cleanup count and
optional completion. Setup/before errors have no clock completion. Body/teardown
errors take priority over clock, then observer errors. Clock and observer
failures remain visible when combined with body failure. Inline error metadata
avoids an additional fallible allocation on the error path.

The independent `vesper-bench-pmu` crate depends on `vesper-bench`, plus Linux
`libc` only. `PerfGroup::open()->Result<PerfGroup,Unavailable>` and its
`WindowObserver<Token=Snapshot,Observation=CounterDelta,Error=Unavailable>`
implementation are the provider boundary. `CounterDelta` getters return cycles,
instructions, enabled_nanos, running_nanos, provider and unit. Provider is
`linux.perf_event_open.group_read`; unit is `hardware events`. `Unavailable`
contains public reason, operation and optional errno. Snapshots/deltas have no
public constructors; group/snapshot/delta are non-Send/non-Sync. No benchmark
core lib registration is needed because `execution::*` is already re-exported.
The new platform crate uses a standalone `[workspace]` for lane tests; remove
that stanza when registering it in the root workspace. No leaf lock is committed.

## Measurement and ownership

The initial explicit ABI profile is little-endian 64-bit Linux x86_64/aarch64.
Other platforms return structured UnsupportedPlatform. Events are hardware
cycles (pinned disabled leader) and instructions (group child), current task
pid0/cpu-1, inherit0, CLOEXEC, excluding kernel/hypervisor execution. Both IDs are
read from actual FDs. Every group read checks exact 56-byte ABI, nr2, each unique
expected ID in either order and enabled/running consistency. All four delta
subtractions are checked. Positive delta-enabled must equal delta-running; no
multiplex scaling is attempted. Earlier scheduling loss outside the window is
allowed. Positive cycles/instructions are required. Kernel cumulative u64
accounting is used without guessing a physical width: no full-u64 wrap between
snapshots and no outside reset/reconfiguration are explicit assumptions.

All FDs become owned immediately and close on partial construction errors.
Private nonwrapping origin/generation authorities prevent cross-group/stale
window reuse. OS getpid/gettid checks at each boundary additionally reject use
after fork, including inherited Rust thread IDs. Foreign/stale tokens do not
disable a different active window. Uncertain control/read/accounting transitions
poison the group. Drop attempts group disable only in the owning process/thread,
then closes both FDs; a fork child does not disable its parent's group. Dropping
a token alone does not prove progress; group Drop is needed to close its FDs.

Hooks surround actual body windows after all setup/capacity preparation and
before discard/teardown, outside Instant timing. Every successful before has
exactly one after on normal Result paths, including body/clock error. Observer
errors never skip available input/output cleanup. Framework record storage is
reserved before setup and bounded to 512 windows by existing RunConfig limits.
The ordinary path uses a zero-sized no-op observer and no observation allocation.
User callbacks/hooks still control their own runtime and internal allocations;
panics/aborts are not caught. Hardware counts include enable-return/dispatch,
clock readings, completion bookkeeping and after dispatch through disable.
These are syscall-read boundaries, not RDPMC/12-cycle or body-only measurements.
No counter confidence gate, calibration, zero-allocation, SIMD or performance
claim is inferred from availability. No privilege/affinity/sysctl changes occur.

## Validation and limits

All checks used one build job, zero dev/test debug info, no incremental compile
and private `/tmp/vesper-pmu-target`:

- `cargo test -p vesper-bench`: 24 tests pass (16 existing, 8 new observation and
  injected-clock/error-precedence cases).
- Platform crate tests: 7 unit fixtures/authority/FD-transition tests, 1
  conditional live test and 3 compile-fail non-Send doctests pass. Literal Linux
  group-read fixtures exercise member ordering, lengths, IDs and metadata;
  arithmetic fixtures exercise full-u64 values/regression/scheduling boundaries.
  Actual socket-owned FDs verify partial acquisition cleanup via peer EOF and
  failed ioctl poisoning; these fixtures never mint public counter evidence.
- Both crates strict Clippy `--all-targets -- -D warnings` and formatting pass.
- Platform crate all-target checks pass for native Linux x86_64, Linux aarch64,
  Windows x86_64 GNU and macOS aarch64. Non-native checks are compile evidence,
  not actual hardware/OS runtime tests.
- Actual `cargo run --manifest-path crates/vesper-bench-pmu/Cargo.toml --example
  probe` exits2 with `PermissionDenied at perf_event_open (OS error 1)` in this
  environment. The live test explicitly logs `LIVE_PMU_UNAVAILABLE`; its pass
  does not assert positive hardware availability. Actual positive counter
  coverage remains dependent on a permitted Linux host. The example runs a
  bounded real workload and checks positive/full-scheduling results when allowed.

No new Lean model/theorems, RDPMC, distributed measurement, privilege helper,
compiler guarantees or hardware performance badges are included.
