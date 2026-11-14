# Pulse work stealing, Windows AFD sockets and synchronous handles (wave eight, lane 21)

Claim: #470. Base: `1094e64` (descendant of the required baseline `ea1ed29`).
Spec: §12 (Pulse), in particular §12.4 (work-stealing `spawn` and core-pinned
`spawn_local`) and §12.6 (SimPulse), snapshot of 2026-11-13 04:46 UTC.
Exclusive paths: `crates/vesper-pulse/**`, this report. Root patch: none.
`Cargo.lock` is unchanged: the only manifest change is one more feature
(`Win32_Networking_WinSock`) on the `windows-sys 0.61` dependency the crate
already had.

## What was built

### 1. A multi-threaded pool with seeded work stealing (`pool`)

`vesper_pulse::pool` adds `Pool<C>`: `workers` scheduler loops, each with its
own completion driver. The public API is additive; `Scheduler`, `Nursery`,
the drivers and `select` are unchanged for existing callers
(`vesper-fiber-abi`, the root crate).

| Item | What it is |
| --- | --- |
| `PoolConfig { workers, seed, lifo_slot, driver_wakeup }` | `workers` in `1..=MAX_WORKERS` (1024), else `PoolError::InvalidWorkers`. `Default` is one worker per available core. |
| `Pool::spawn`, `Pool::spawn_pinned(worker, c)`, `Pool::spawner()`, `Pool::cancel()` | Portable fibers go to the shared injector; pinned fibers (§12.4 `spawn_local`) go to that worker's pinned queue and are never stolen. |
| `Spawner<C>` (`Clone`, `Send`) | `spawn` from inside a running fiber puts the child in the current worker's LIFO slot (the previous occupant moves to the run queue); `spawn_pinned`; `cancel`. Spawning into a finished pool is `PoolError::Closed` and returns the fiber. |
| `Pool::run(factory)` | One OS thread per worker (`std::thread::scope`, so neither the factory nor the fibers need `'static`). `factory(i)` builds worker *i*'s driver on its own thread, so the driver need not be `Send`; fibers must be `Send`. |
| `Pool::run_seeded(factory)` | The same workers interleaved on the calling thread. Which worker steps next and which victim a thief tries first come from the seed (SplitMix64 streams). Returns a `TraceEntry { worker, fiber, event, stolen_from }` per resumption. |
| `PoolOutput { completed, failures, workers: Vec<WorkerStats>, trace, abandoned }` | `WorkerStats { resumed, stolen, steal_attempts, submitted, parked }`. |
| `RunError::{Setup { worker, error, pool }, Driver { worker, error, output, stranded }}` | A driver that fails to start returns the untouched pool; a driver that fails while running returns everything that finished plus the runnable fibers it could not run (`Stranded`). |
| `Task<C>` | The opaque continuation type the drivers carry (`AnyDriver<Task<C>>`): a fiber or the pool's wake-up read. |

How a worker runs: harvest completions without blocking; take a fiber from
its LIFO slot (at most three in a row), its run queue or pinned queue
(alternating), the injector (served first every 61st pick so external spawns
are never starved), or steal half of a victim's run queue. A fiber's I/O goes
to the driver of the worker that ran it, and its completion is queued back on
that worker, where an idle sibling can steal it; requests never move between
drivers. A worker with nothing to run parks:

* with in-flight fiber I/O, in its driver (`poll(true)`). Each worker keeps a
  read on a private wake pipe pending in its own driver (an anonymous pipe on
  Unix, `iocp::wake_pipe` on Windows: an overlapped reader and a synchronous
  writer), so a sibling that publishes work writes one byte and the parked
  worker returns from `poll`. Wake-ups therefore use the completion protocol
  of every driver, with no driver-specific hook;
* otherwise on a condition variable.

The park/notify handshake is the Dekker pattern under `SeqCst`: a parker
publishes its state and then rechecks every queue it may serve; a notifier
publishes work and then reads the states. The condition-variable wait has a
50 ms safety timeout (1 ms when `driver_wakeup` is off and I/O is in flight).

Termination: the pool counts live fibers; the run ends when the count reaches
zero, and `Spawner::spawn` reserves with a compare-and-swap that refuses once a
started pool has drained, so no spawn can race past the end. Each worker then
drains its driver (cancelling the wake read).

Protocol guarantees:

* Every continuation is resumed exactly once per event. Cancellation wraps
  every queued event, and every event delivered after it, in
  `Event::Cancelled`, cancels all in-flight I/O in every driver, and cancels
  any request submitted after it at once, so a cancelled pool always ends.
* A driver whose `poll` fails is recorded (the first error is returned) and the
  pool cancels itself; harvested completions are never lost. After
  `MAX_POLL_FAILURES` (3) consecutive failures the worker gives up: its run
  queue moves to the injector, the fibers its driver still owns are counted in
  `PoolOutput::abandoned` (and by the driver's own `Drop`), and its pinned
  fibers come back in `stranded`.
* A pool dropped with queued fibers counts them in
  `driver::abandoned_requests()`, like a dropped driver.
* A panicking fiber stops every worker, drops the drivers through their own
  `Drop` protocol, and the panic is re-raised from `run`.

Determinism: `run_seeded` with a deterministic driver produces an identical
trace, output and per-worker statistics for identical seeds (tested), and a
different schedule for a different seed. With an OS driver, completion timing
is the only nondeterminism left; the results still match. `run` is a real
multi-threaded run: the seed fixes each worker's victim order, not the global
interleaving. SimPulse itself (`vesper-crucible`, the root `--sim` paths)
does not use `vesper-pulse` and is unchanged; its replay tests pass (below).

Rust ownership is runtime evidence only; this is not an implementation of the
Vesper source typing rules for portable versus local fibers (§12.4's
portability modes) or of linear obligations.

### 2. AFD socket readiness on Windows (`afd`, `iocp`)

Sockets are no longer treated as plain overlapped handles. `IocpDriver`
recognises a Winsock socket (`GetFileType == FILE_TYPE_PIPE`, not a named
pipe, `getsockopt(SO_TYPE)` succeeds, `SIO_BASE_HANDLE` gives the base
provider socket), switches it to non-blocking mode (left set, as the
readiness drivers leave `O_NONBLOCK`), and tries the `recv`/`send` at once. If
it would block, the driver issues `IOCTL_AFD_POLL` for the base socket on its
own AFD device handle (`\Device\Afd\VesperPulse`, opened on first use and
associated with the port). When the poll's packet arrives the driver retries
the transfer, completes it, or re-arms the poll on `WSAEWOULDBLOCK`. This is
the wepoll/mio/libuv mechanism. Consequences:

* sockets are never bound to a completion port, so the same socket can be
  used from several drivers (tested) and closed at any time;
* the kernel never holds a socket buffer: a cancelled socket read returns its
  whole buffer; cancellation is `CancelIoEx` on the poll, and a poll that
  fired but finds the socket not ready completes cancelled instead of
  re-arming;
* end of stream reads `Ok(0)`; `AFD_POLL_LOCAL_CLOSE` is `ERROR_INVALID_HANDLE`;
  NTSTATUS failures map through `RtlNtStatusToDosError` (`STATUS_CANCELLED`
  to the cancellation error).

`IocpDriver::socket_polls()` counts armed polls, so tests prove the AFD path
ran. The unsafe code is confined to the new `afd` module and the existing
`iocp` module, as the assignment allows.

### 3. Synchronous handles off the scheduler thread, safe handle reuse (`iocp`)

* Synchronous regular files (the `std::fs::File` default) are still
  transferred positionally in `submit` (they cannot wait for a peer), as
  before.
* Every other synchronous handle (anonymous pipes, character devices) goes to
  an offload thread: one thread per in-flight operation, started on demand and
  reused once idle (`offload_threads()`). The thread performs the positional
  `ReadFile`/`WriteFile`, queues the result, then posts the request's own
  `OVERLAPPED` to the port; `poll` delivers it. An empty synchronous pipe no
  longer blocks the scheduler (tested, with a watchdog). Cancellation uses
  `CancelSynchronousIo` on that thread, checked under a per-thread lock so it
  only targets the request's own call; an operation that has not entered the
  kernel yet is either skipped (cancelled before start) or re-cancelled every
  5 ms by `poll(true)` until it completes with `ERROR_OPERATION_ABORTED`.
  Threads share the port through an `Arc`, so the port outlives them.
* Packets are matched to requests through a table of the driver's own
  `OVERLAPPED` addresses; the driver no longer dereferences an unknown
  pointer. A stray packet is a typed `PollError`.
* Reused handle values: a handle that another port owns and that this driver
  never bound is rejected (`InvalidInput`, request returned). A value this
  driver bound before, which no request of its own currently holds open, may
  have been closed and reused by a file that another port owns: the driver
  rebinds it to its own port with `NtSetInformationFile`
  (`FileReplaceCompletionInformation`) before issuing I/O, so a packet for
  this driver's `OVERLAPPED` cannot reach another port. Where the OS lacks that
  class (probed once by `iocp::rebinds_reused_handles()`; Wine 9 lacks it),
  the previous assumption is kept and that one case stays unsupported.

## Tests

Unit tests (`cargo test -p vesper-pulse --lib`, 35 on Linux): the existing scheduler,
nursery, frames, ledger, readiness and selection tests, plus seven pool tests
over a deterministic in-memory driver:

* `seeded_runs_replay_identically_and_steal`: 88 fibers (64 spawned, 24 spawned
  by fibers) on 4 workers; identical trace, output and statistics for the same
  seed, at least one steal, every worker ran fibers; a different seed gives a
  different schedule and the same results.
* `pinned_fibers_stay_home_and_children_use_the_lifo_slot`.
* `cancellation_reaches_queued_and_in_flight_fibers_once` (seeded and
  threaded; cancel before the run and from a fiber while reads are parked).
* `a_failing_driver_strands_nothing_silently`: every fiber is completed,
  failed, abandoned or returned as stranded.
* `threaded_mock_pool_runs_every_fiber`, `a_dropped_unrun_pool_counts_its_fibers`,
  `configuration_and_setup_errors_are_typed` (zero or too many workers, an
  unknown pinned worker, a driver that fails to start returns the pool, which
  then runs).

Integration tests on every driver the host starts (`tests/pool.rs`):

* `many_fibers_across_threads_on_every_host_driver`, the stress test: 4 worker
  threads, 1,250 fibers: 1,200 file jobs (write 8 bytes at their own offset of
  one shared file, read them back, yield up to 3 times; half spawned from
  outside, half by one fiber so the others must steal), 24 pipe pairs with the
  reader pinned to one worker and the writer to another (the reader's worker
  is woken out of its driver), and a tail fiber. Checks every slot's bytes,
  every message, that more than one worker ran fibers and that at least 2,400
  requests were submitted.
* `seeded_runs_finish_the_same_workload_on_every_host_driver`.
* `cancelling_the_pool_completes_parked_reads_on_every_worker`.

Windows only (`tests/windows.rs`, each with a 60 s watchdog):
`a_socket_read_waits_on_afd_until_its_peer_writes` (asserts the AFD poll ran,
then reuses the sockets on a second driver), `socket_end_of_stream_and_cancellation`,
`an_empty_synchronous_pipe_does_not_block_the_scheduler`,
`an_offloaded_read_on_an_empty_pipe_can_be_cancelled`,
`reused_handle_values_never_route_packets_to_another_port`,
`sockets_across_pool_workers` (12 TCP pairs over a 3-thread pool).

The existing `tests/drivers.rs` (8) passes unchanged on every driver.

### Where it ran

| Where | Driver(s) | Result |
| --- | --- | --- |
| Cloud container, Linux x86_64, Rust 1.97 | io_uring (permitted here) and epoll; also `VESPER_PULSE_DRIVER=epoll` | lib 35/35, drivers 8/8, pool 3/3; the pool integration binary 40 times in a row without a failure or hang |
| Wine 9.0 (Ubuntu), `x86_64-pc-windows-gnu` build, Rust 1.100 | IOCP, AFD, offload threads | lib 22/22 (Windows count), drivers 8/8, pool 3/3, windows 6/6, five repeated rounds |
| `aarch64-apple-darwin` | kqueue | `cargo +1.100.0 clippy --all-targets -D warnings` only; not executed locally |
| CI: Linux BuildBuddy, macOS (AArch64), Windows (x86_64 MSVC) | as above; kqueue; real IOCP/AFD | see the PR's checks |

Wine reimplements Win32: it validates the protocol, not Windows. Under Wine the
AFD poll and `CancelSynchronousIo` both worked; `FileReplaceCompletionInformation`
is not implemented, so the rebind branch of the reuse test only runs on the
Windows CI job (the test prints which branch ran).

Timings observed in the stress test (debug build, 4 workers, 1,250 fibers; the
container's core count is not controlled, so these are observations, not
benchmarks): epoll 3–5 ms, io_uring 72–96 ms (io-wq services the regular-file
operations), IOCP under Wine about 290 ms. No other performance was measured.

### Commands

```
cargo fmt --all -- --check
cargo test -p vesper-pulse                                   # lib 35 (Linux), drivers 8, pool 3
VESPER_PULSE_DRIVER=epoll cargo test -p vesper-pulse --test pool --test drivers
cargo test --locked -p vesper-fiber-abi -p vesper-crucible   # dependants and SimPulse
cargo +1.100.0 clippy -p vesper-pulse --all-targets --target {x86_64-unknown-linux-gnu,x86_64-pc-windows-gnu,aarch64-apple-darwin} -- -D warnings
cargo +1.100.0 build -p vesper-pulse --tests --target x86_64-pc-windows-gnu && WINEPREFIX=/tmp/wineprefix wine <each test exe>
cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings
cargo test --workspace --locked
python3 tools/bazel/sync_build_files.py                      # no BUILD change needed
```

## Decisions

* One driver per worker, and requests stay in the driver that accepted them.
  Moving an in-flight request between rings or ports is impossible on io_uring
  and IOCP; stealing works on runnable continuations instead, which is where
  the imbalance is.
* Mutex-protected deques rather than a lock-free Chase-Lev deque: the crate
  forbids unsafe code outside the driver modules, and no new dependency was
  allowed. Thieves take half of a victim's queue from its head, which keeps
  lock traffic per stolen fiber low.
* Wake-ups through a pipe read kept pending in each driver, rather than a new
  `CompletionDriver` method: it works unchanged on all four drivers and any
  future one. `driver_wakeup(false)` exists for test doubles that cannot read
  a pipe.
* `run_seeded` interleaves the real workers on one thread instead of being a
  separate simulator, so the code under test is the code that runs threaded.
* Synchronous regular files stay inline on Windows (they finish without a
  peer, like the readiness drivers' inline files); only handles that can wait
  indefinitely go to offload threads, which keeps a burst of file I/O from
  spawning hundreds of threads.
* `Win32_Networking_WinSock` was added to the existing `windows-sys` features
  for `recv`/`send`/`getsockopt`/`ioctlsocket`/`WSAIoctl`; no crate was added
  and `Cargo.lock` did not change.

## Remaining gaps

* Work stealing uses mutexes; there is no lock-free deque, no per-core thread
  pinning (CPU affinity) and no per-worker registered buffers or stack slabs
  (§12.4–§12.5 mention both). `spawn_pinned` pins a fiber to a worker, not a
  worker to a core.
* The pool is a host-library runtime. The compiler and `vesper-fiber-abi`
  still drive the single-threaded `Scheduler`; lowering `s.spawn` /
  `s.spawn_local` to the pool, and the source-level portability checks that
  decide which fibers may migrate, are not done.
* `run` returns once every fiber has finished: a pool cannot be kept alive
  idle for spawns from outside, and outside spawns into a finished pool are
  refused (`PoolError::Closed`).
* Socket sends are not split: a large `send` may complete partially, with the
  byte count, as stream writes do on every driver. Sockets submitted to the
  IOCP driver are left in non-blocking mode. Sockets must be wrapped in a
  `File` (as `Request` requires) and should be closed with `closesocket`.
* Offload threads are not capped beyond the driver's `entries` bound; a
  workload with thousands of parked synchronous pipe reads spawns that many
  threads. Overlapped handles avoid this.
* The handle-rebind path depends on `FileReplaceCompletionInformation`
  (Windows 8.1+); on systems without it (Wine) reusing a closed handle value
  for a file another port owns remains unsupported, as documented in the code.
* SimPulse (`vesper-crucible`) still runs its own single-threaded
  interpreter; it does not yet model several workers stealing from each other.
