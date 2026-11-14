# Pulse portable completion drivers (wave seven, lane 2)

Claim: #325. Base: `283c811` (descendant of the required baseline `c025b9a`).
Spec: §12 (Pulse), snapshot of 2026-11-13 04:46 UTC.
Exclusive paths: `crates/vesper-pulse/**`, this report. Root patch: `Cargo.lock`
only (two dependency lines for `vesper-pulse`), in its own commit.

## What was built

`vesper-pulse` no longer ties its scheduler to io_uring. The scheduler,
nursery and every driver are portable; the host chooses a driver at run time.

| Module | Platform | What it is |
| --- | --- | --- |
| `driver` | all | Backend-neutral protocol: `Request`, `Operation`, `RequestId`, `Completion`, `PollError`, `DriverKind`, and the `CompletionDriver` trait (`submit`, `cancel`, `cancel_all`, `poll`, `pending`, default `drain`). `is_cancelled`/`cancelled_error` name the per-OS cancellation code. `abandoned_requests()` counts continuations lost by dropping an undrained driver. |
| `uring` | Linux | The existing io_uring driver, moved unchanged in behaviour (its ledger tests and kernel round trip move with it) and implementing `CompletionDriver`. |
| `readiness` (private) | Linux, macOS | Completion semantics on top of a readiness notifier, shared by epoll and kqueue. |
| `epoll` | Linux | `EpollDriver`: one-shot `EPOLLIN`/`EPOLLOUT` registration; regular files and devices that epoll refuses with `EPERM` are serviced inline with `pread`/`pwrite`. |
| `kqueue` | macOS | `KqueueDriver`: `EV_ADD | EV_ONESHOT` on `EVFILT_READ`/`EVFILT_WRITE`; regular files (and devices that answer `EINVAL`/`ENODEV`/`ENOTSUP`) inline. |
| `iocp` | Windows | `IocpDriver`: a completion port; overlapped handles are associated with `FILE_SKIP_COMPLETION_PORT_ON_SUCCESS` and complete asynchronously; synchronous handles (the `std::fs::File` default, detected with `NtQueryInformationFile(FileModeInformation)`) run positionally in `submit` and their result is posted to the port. `overlapped_pipe()` makes a connected overlapped named pipe. |
| `select` | all | `open(entries, Preference)`, `open_from_env` (`VESPER_PULSE_DRIVER=auto|io_uring|epoll|kqueue|iocp`), `AnyDriver`, `DriverReport { selected, preference, skipped }` and `SelectError { UnknownDriver, NotCompiled, Unavailable, NoDriver, InvalidEntries }`. |
| `scheduler` | all (was Linux only) | Unchanged logic. `Driver` is implemented for every `CompletionDriver` and for `AnyDriver`; `accept_completions` takes the neutral `Completion`. |
| `io_driver` | all | Compatibility path: re-exports the neutral types everywhere, plus `IoDriver`/`ShutdownError` on Linux, so `vesper_pulse::io_driver::{IoDriver, Request, Operation}` keeps working for `vesper-fiber-abi` and the root crate without edits. |

Selection order: Linux tries io_uring, then epoll (every skipped driver is in
the report with its OS error); macOS uses kqueue; Windows uses IOCP. A forced
driver never falls back silently. SimPulse does not use this module and is
unchanged.

Unsafe code: the crate is `#![deny(unsafe_code)]`; only `uring`, `readiness`,
`epoll`, `kqueue` and `iocp` allow it, as the assignment permits.

### Protocol guarantees shared by all drivers

* A request is owned by the driver from `submit` to exactly one `Completion`;
  IDs are never reused. A rejected submission (`WouldBlock` when the driver
  already owns `entries` requests, `ESPIPE` for a non-zero offset on a stream
  under epoll/kqueue, an association failure on IOCP) returns the request
  untouched.
* Completions are delivered only by `poll`, never by `submit`, including
  inline (always-ready) and synchronous-handle operations.
* `poll` keeps every completion harvested in a failed batch (`PollError`);
  unknown or duplicate events and packets are errors, never completions.
* `cancel` never releases a request: it completes once, with `ECANCELED`
  (`ERROR_OPERATION_ABORTED` on Windows) or with its real result if it had
  already finished. `drain` cancels everything and polls until nothing is
  owned. Reads that finish truncate the buffer to the bytes read; a cancelled
  read returns its whole buffer.
* End of file and a closed pipe read as `Ok(0)` on every driver
  (`ERROR_HANDLE_EOF`/`ERROR_BROKEN_PIPE` are mapped on Windows).
* The IOCP driver's `Drop`, like io_uring's, cancels and waits for every packet
  before freeing buffers the kernel may still write, and leaks rather than
  frees them if the port fails. Readiness drivers never lend buffers to the
  kernel, so their `Drop` is memory safe; it counts the discarded
  continuations in `abandoned_requests()` instead of dropping them silently.

Rust ownership is not evidence that the Vesper source typing rules for linear
completion obligations are implemented; this lane only gives the runtime
protocol those rules will target.

## Tests

Unit tests (`cargo test -p vesper-pulse --lib`): the existing scheduler and
nursery tests (now portable; the mock driver opens `NUL` on Windows), the
io_uring ledger tests, scripted-poller tests of the readiness core (unknown
and duplicate events, spurious wake-ups re-arm, a failed wait keeps inline
completions, capacity and stream-offset rejections), driver-name parsing and
the selection report.

Integration tests (`crates/vesper-pulse/tests/drivers.rs`) run the real
scheduler/nursery on every driver the host starts and print which one ran:

* `scheduler_round_trips_files_on_every_host_driver`: three tasks write at an
  offset, read back, yield and complete.
* `a_stream_read_waits_for_its_writer`: a read parks in the driver on an empty
  pipe until a sibling task writes.
* `a_closed_writer_reads_as_end_of_file`.
* `nursery_cancellation_drains_every_in_flight_request`: four parked reads and
  one queued child; each resumes exactly once with the cancellation error.
* `drain_returns_every_owner_exactly_once`.
* `per_request_and_driver_faults_are_typed_errors`: a write to a read-only
  handle fails that request only (`EBADF`, `ERROR_ACCESS_DENIED`) and returns
  its buffer; a full driver hands the request back with `WouldBlock`; zero
  entries and unknown names are typed `SelectError`s.
* `runtime_selection_reports_the_driver_that_ran` (honours
  `VESPER_PULSE_DRIVER`), `epoll_runs_when_io_uring_is_not_chosen` (Linux:
  forces epoll, and reports whether io_uring is permitted),
  `iocp_overlapped_files_complete_through_the_port` (Windows,
  `FILE_FLAG_OVERLAPPED` file).

### Where each driver ran

| Where | Driver(s) | Result |
| --- | --- | --- |
| Cloud container, Linux x86_64, Rust 1.97 | io_uring (permitted on this host) and epoll; `VESPER_PULSE_DRIVER=epoll` forced run | lib 25/25, drivers 8/8, three repeated runs |
| Wine (wine-stable from Ubuntu), `x86_64-pc-windows-gnu` build | IOCP | lib 16/16, drivers 8/8 |
| `aarch64-apple-darwin` | kqueue | `cargo +1.100.0 clippy --all-targets -D warnings` only; not executed locally |
| CI: Linux BuildBuddy (`bazel test //...`), macOS (AArch64), Windows (x86_64 MSVC) | io_uring if permitted plus epoll; kqueue; IOCP | see the PR's checks |

Wine is a reimplementation of the Win32 API, so the Wine run validates the
protocol, not Windows; the Windows CI job is the real IOCP evidence. Likewise
the kqueue driver is only proven by the macOS CI job.

### Commands

```
cargo fmt --all -- --check
cargo test -p vesper-pulse                                  # lib 25, drivers 8
VESPER_PULSE_DRIVER=epoll cargo test -p vesper-pulse --test drivers
cargo test --locked -p vesper-fiber-abi                     # dependents unchanged
cargo +1.100.0 clippy -p vesper-pulse --all-targets [--target aarch64-apple-darwin | --target x86_64-pc-windows-gnu] -- -D warnings
cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings
cargo test --workspace --locked
python3 tools/bazel/sync_build_files.py                     # no BUILD change needed
```

## Decisions

* `libc` (already a workspace dependency) and `windows-sys 0.61` (already in
  `Cargo.lock` through `vesper-fiber-abi`) instead of `mio`: the drivers need
  one-shot registration, positional I/O for always-ready files and completion
  ports with skip-on-success, which `mio` does not expose.
* Readiness drivers service regular files inline. epoll cannot watch them and
  kqueue reports them always readable, so this is the honest equivalent; the
  cost is that a slow disk read blocks the scheduler thread for its duration
  (io_uring does not).
* Streams under epoll/kqueue use `O_NONBLOCK` only for the call and restore the
  file's flags, so a caller's file description keeps its mode.
* IOCP keeps synchronous handles working (positional, then posted), because
  `std::fs::File` opens them that way; true asynchrony needs
  `FILE_FLAG_OVERLAPPED`.
* The scheduler's `Driver` trait and the `io_driver` path are kept so that
  `vesper-fiber-abi` (lane 3's path) compiles without edits.

## Remaining gaps

* `vesper-fiber-abi`'s `pulse` adapter and its `tests/lifecycle.rs`,
  `tests/fp_env.rs` and `tests/scheduler.rs` are still gated to Linux by their
  own `cfg`s, and the tests open `/dev/null` and `OwnedFd`. They live in
  lane 3's paths, so this lane could not move them. With this PR they only need
  their gates widened (and a portable null file) to run on macOS and Windows
  against `select::open`; that is a small follow-up for the fiber-abi owner.
* The cloud host here permits io_uring, so the automatic io_uring → epoll
  fallback was exercised by forcing epoll and by the selection report, not by
  an actual `EPERM` from `io_uring_setup`. On a denying host `open` records the
  io_uring error in `DriverReport::skipped` and runs epoll.
* Synchronous Windows handles block in `submit`; an empty synchronous pipe
  blocks the scheduler. Sockets are not yet special-cased on Windows (they
  work as overlapped handles but `AFD` polling is not used).
* IOCP association is per handle for life. Reusing a closed handle's value for
  a new overlapped file on the same driver is unsupported (documented).
* No performance figures were measured.
* Only one scheduler thread per driver; no work stealing across cores.
