# Pulse pool depth (wave nine, lane 22), part 1

> Retained work context for open issues, unmerged branches, or their dependencies.
> The [manuscript](../manuscript/README.md) is the sole specification; use the
> [section map](../section-map.md) for earlier numbering. Confirm live ownership before acting.

Claim: #599. Base: `db62471` (main). Spec: the Pulse chapter of
`docs/manuscript/content.typ` (worker shards, work-stealing `spawn` and
core-pinned `spawn_local`). Exclusive paths: `crates/vesper-pulse/**`, this
report. This is part 1 of the lane; part 2 is listed under "Remaining".

## What was built

### Lock-free run queues

Each worker's run queue is now a Chase-Lev deque (`crossbeam-deque` 0.8.8,
already in `Cargo.lock` through rayon) and the shared injector a lock-free
`Injector`. They replace `Mutex<VecDeque>`.

* Only the owning worker pushes or pops at its deque's owner end. Thieves
  use the deque's `Stealer`: `steal_batch_and_pop` takes about half the
  victim's queue (at most 32 fibers, crossbeam's batch limit), runs the first
  and keeps the rest. Previously a thief took exactly half.
* A fiber spawned from inside a running fiber still goes to the LIFO slot.
  The fiber it displaces (or every child when the LIFO slot is off) goes to
  a per-worker `spill` list, and the worker moves it onto its own deque when
  the resumption returns, because only the owner may push there. Siblings
  can steal it from that point on.
* The LIFO slot, the pinned queue and the spill list keep their mutexes, but
  each has an atomic length that the hot path reads first, so a worker no
  longer takes a lock per resumption for an empty side queue. The
  park/notify recheck (`has_own`) still locks, so the Dekker handshake is
  unchanged.
* Cancellation still wraps every queued event in `Event::Cancelled`. This
  happens when the fiber is taken off a queue, not while it sits there (the
  deques cannot be edited in place); the observable rule is the same.
* `run_seeded` stays deterministic: identical seeds give identical traces,
  outputs and statistics (the existing replay test passes unchanged).

### A long-lived pool

`Pool::start(factory) -> Result<RunningPool<C>, RunError<C>>` runs the
workers on background threads (`C: Send + 'static`, and a `Send + Sync +
'static` factory). The pool does not end when it runs out of fibers. Idle
workers park on the condition variable, or in their driver when they own
I/O, and any thread can spawn through `RunningPool::spawner()`.

* `start` returns once every driver is built. If one cannot be built, it
  returns `RunError::Setup` with the pool and its queued fibers, and that
  pool can be started again.
* `RunningPool::{spawn, spawn_pinned, spawner, cancel, workers, live}`.
* `RunningPool::shutdown()` stops new work once the live fibers are done.
  Fibers may still spawn children while it drains. It then joins the
  workers and returns the `PoolOutput`, and re-raises a worker's panic.
  Dropping a `RunningPool` shuts it down the same way and discards the
  output.
* `PoolConfig::keep_results(false)` keeps no per-fiber results, so a
  long-lived pool whose fibers report results themselves uses bounded
  memory.

### CPU pinning

`PoolConfig::pin_cores(true)` pins worker *i* to the *i*-th core the OS
reports, round robin (`core_affinity` 0.8.3). `WorkerStats::pinned_core`
records the core, or `None` when the OS refused. Linux and Windows honour
hard affinity, and the test checks that each worker gets its own core
there. macOS has no hard affinity (`THREAD_AFFINITY_POLICY` is a hint and
is not supported on Apple Silicon), so the test only prints what it got.
`run_seeded` never pins because it runs on the caller's thread.

## Tests

* New `tests/long_lived_pool.rs`, run on every host driver (io_uring and
  epoll here, kqueue on macOS, IOCP on Windows):
  * `an_idle_pool_stays_alive_and_takes_spawns_from_many_threads`: an
    idle wait, then 6 outside threads released together spawn 900 fibers
    doing file writes and reads, then another idle wait and a second burst,
    then shutdown. After shutdown, spawning returns `PoolError::Closed`
    with the fiber.
  * `fibers_spawned_inside_a_running_pool_finish_before_shutdown_returns`:
    40 parents fan out 9 children each, and shutdown is called at once.
  * `contended_queues_resume_every_fiber_exactly_as_often_as_it_yields`:
    8 workers and 4 feeder threads, with 64 roots fanning out 31 children
    each (2,048 fibers, 43,008 resumptions). Every fiber runs exactly
    `yields + 1` times.
  * `pinned_workers_report_their_core_where_the_os_allows_it`,
    `a_failed_start_returns_the_pool_with_its_fibers`,
    `dropping_a_running_pool_shuts_it_down`.
* The existing pool unit tests and `tests/pool.rs` pass unchanged.
* Locally, all of `cargo test -p vesper-pulse` and the two pool
  integration tests passed 8 and 15 times in a row.

## Throughput against the mutex version

`tests/pool_throughput.rs` (ignored; `cargo test --release -p vesper-pulse
--test pool_throughput -- --ignored --nocapture`) uses only `Pool::run`, so
the same file runs on `main`. Each fiber only yields, so it measures the
queues, stealing and parking, not I/O, and fibers share no cache lines
while they run. Setup: the cloud container with 4 vCPUs, the io_uring
driver, Rust 1.97 release builds, best of 5. The numbers are the second
of two alternating runs per build, in millions of resumptions per second.

| Workers | Fibers (roots x fan-out, yields) | main (mutex) | this PR | Speedup |
| --- | --- | --- | --- | --- |
| 1 | 64 x 64, 400 | 9.61 | 15.19 | 1.58x |
| 2 | 64 x 64, 400 | 18.83 | 27.19 | 1.44x |
| 4 | 64 x 64, 400 | 27.65 | 54.53 | 1.97x |
| 8 | 64 x 64, 400 | 30.30 | 54.63 | 1.80x |
| 4 | 4,096 x 1, 400 | 28.16 | 54.55 | 1.94x |
| 4 | 64 x 1,024, 4 | 9.95 | 15.48 | 1.56x |

The first run of each build was within about 12% of these figures. Four workers
on 4 vCPUs now scale about 3.6x over one worker, against 2.9x before.

## Decisions

* The fiber that a LIFO-slot spawn displaces goes to the spill list rather
  than straight onto the deque. A thread-local handle to the owner's deque
  would need unsafe lifetime erasure. The spill list costs one uncontended
  lock per such spawn, and a sibling cannot steal that fiber until the
  parent's resumption returns.
* The injector is drained one fiber at a time. A batch steal from the
  injector changes which fiber IDs children get under `run_seeded`, which
  the replay test pins down, and it made no measurable difference here.
* Rust ownership here is runtime evidence only. It is not an
  implementation of the Vesper source typing rules for portable fibers.

## Remaining (part 2 and later)

* A pollable descriptor for each driver (the io_uring eventfd, the epoll
  fd, the kqueue fd), so a GLib main context can wait on it (lane 23).
* Splitting large socket sends, and restoring blocking mode on sockets the
  IOCP driver gives back.
* A cap on IOCP offload threads (today one per concurrent synchronous
  request).
* Per-worker registered buffers (io_uring fixed buffers) and fiber stack
  slabs.
* Scoped workers for compiled Vesper (`@vesper-std/sync` `scope`/`spawn`,
  #1037) can now be built on `Pool::start` and `Spawner`. That work belongs
  to the atomics lane, not this one.
