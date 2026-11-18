# Port P1 lane 5: vesper-simpulse in Vesper

Claim #902. Baseline `main` at 69c79b9 (a descendant of d523661). The
[manuscript](../manuscript/README.md) is the sole specification; this lane
needed no manuscript change.

**Result.** The whole SimPulse scheduling kernel runs in Vesper
(`crates/vesper-simpulse/vesper/src/lib.ves`). Both builds pass the crate's
18 kernel tests and a new stress test with byte-identical journals. The port
runs at 0.3x to 1.1x of the Rust build's time (table below). Following the
compiler's seed-copy rule, **the Rust code stays** and the four dependents
the compiler links keep the Rust build. At parity, the owner chose to move
the dependents outside the compiler to the port (follow-up #996):
`vesper-chorus` and `vesper-flux` now link it.

## API inventory (re-measured)

`src/lib.rs` at the baseline, 712 lines, one module, no `#[cfg(test)]` unit
tests.

| Item | Kind | Where it lives now |
|---|---|---|
| `TaskId`, `CompletionId` (`get`) | `copy` newtypes, serde | shared, `src/lib.rs` |
| `IoResult`, `WakeReason`, `TraceEvent` | enums, serde | shared |
| `Journal` (`to_json`, `from_json`, version check) | struct, serde | shared (serde stays Rust) |
| `Error` (13 variants, `Display`, `std::error::Error`) | enum | shared |
| `RunToken<T>` (`task`, `payload`, `payload_mut`, `reason`, `cancel_requested`) | owned token | shared |
| `TransitionError<T>`, `SpawnError<T>` | error carriers | shared |
| `TaskState` | enum | shared |
| `Simulator<T>`: `new`, `replay`, `now_ns`, `task_state`, `cancel_requested`, `pending_count`, `is_complete`, `journal`, `finish_replay`, `spawn`, `yield_task`, `finish`, `sleep`, `park_io`, `schedule_io`, `request_cancel`, `complete_io`, `next_task` | generic simulator | Rust build `src/reference.rs`; port build `src/ported.rs` over `Kernel` |
| SplitMix64 `random`, rejection `sample_index` | private | `reference.rs`; Vesper `random` and the sampling loop in `kernel_next_task` |
| simulator instance counter | private static | shared `next_instance()` |
| new: `IMPLEMENTATION` (`"vesper"` or `"rust"`), hidden `port_live_allocations` (port only) | const, fn | `src/lib.rs` |

## What was ported

Everything that decides something: the run queue and its rounds, the seeded
choice, the virtual clock and its jumps, timers, injected and external I/O
completions, cancellation flags, ID exhaustion and overflow checks, the
token-ownership check, and record/replay (the batch check before any state
changes, the replay cursor, unconsumed events). The Rust wrapper only
translates codes and words.

Data structures (Vesper `Kernel` handle, one per simulator):

- Task and completion records are dense sequences indexed by id, because
  ids start at 1 and are never reused.
- Timers and injected completions are a binary min-heap keyed by
  `(deadline, completion)`, the key order of the Rust build's `BTreeMap`.
  A manually completed entry goes stale and is dropped at the top.
- The dispatch uses two Fenwick trees over task ids: runnable tasks, and
  runnable tasks not dispatched this round. Picking the `i`-th candidate in id
  order takes O(log tasks). The Rust build scans every task on every dispatch.
  A new round revisits only the tasks dispatched in the last one.

**Deviation from the prompt:** the prompt says maps use `@vesper-std`
`CacheBTree`. The port uses none. Each Rust map either became a dense
sequence or the heap. `CacheBTree` removal does not merge or free leaves, and
`first()` walks the empty ones. Simpulse's keys grow over time (deadlines and
completion ids), so taking the earliest timer would cost time in proportion
to the whole history. Order is still deterministic: every key is unique and
totally ordered. Fixing node reclamation belongs to `@vesper-std` (#897
follow-up).

**ABI.** The library uses export ABI v2 with no `Fail`. Every operation
returns a `copy` result struct with a code. The wrapper reads each call's
committed events in place, through the handle view (`Kernel::as_ref().out()`,
a `SeqRef<Ev>`), and appends them to its serde `Journal`. Events cross the
boundary as a flat seven-word `copy struct Ev` with `impl Eq`, so the replay
check is one comparison per event.

## What stays in Rust, and why

| File | Why | Unblocked by |
|---|---|---|
| `src/reference.rs` (the Rust `Simulator`) | the frozen seed copy that the compiler links (`vesper-doc`, `vesper-chrono` and `vesper-crucible` are inside `cargo tree -p vesper`) | self-hosting Stage 2 |
| `src/lib.rs` shared types, `Journal::to_json`/`from_json` | serde serialization of traces (the prompt keeps it in Rust) | a Vesper serde (#916 and later) |
| `src/ported.rs` payload slab `Vec<Option<T>>` | generic Rust payloads `T` (token-slab pattern) | generic export (not planned) |
| instance identity (`AtomicU64`) | shared-memory atomics (F12) | F12 |
| `Error::ReplayMismatch.expected` | the wrapper copies the expected event from its own `Journal` | none needed |

The Rust code was **not deleted**. The port reached parity on three of the
four benchmarks, but the compiler still needs the seed copy until Stage 2,
and the owner's rule keeps Rust as the default.

## Build layout and dependents

- `crates/vesper-simpulse` (package `vesper-simpulse`) is the port-backed build:
  `build.rs` always links the Loom package `@vesper/simpulse` and sets
  `cfg(vesper_port)`.
- `crates/vesper-simpulse/seed` (package `vesper-simpulse-seed`, library
  `vesper_simpulse`) builds the same `src/` without the port. Its
  `BUILD.bazel` is hand-written and reads its serde deps from the Cargo
  metadata (`PACKAGES`).
- `vesper-sim-session`, `vesper-doc`, `vesper-chrono` and `vesper-crucible`
  (inside the compiler closure) depend on the seed by rename.
  `vesper-chorus`, `vesper-flux` and `vesper-nexus` (outside it) first stayed
  on the seed too, under the owner's parity rule. Since the follow-up (#996),
  `vesper-chorus` and `vesper-flux` link the port, which the owner approved
  once the benchmarks reached parity. Their tests pass against it.
  `vesper-nexus` stays on the seed: the root package lists it as a
  dev-dependency, and Bazel's `//:vesper` targets take dev-dependencies, so
  the port there made a dependency cycle through `//:bin/vesper`.
- Crucible (lane 18) can import the port as `@vesper/simpulse` (one module,
  as the Rust crate has one).

## Tests

- `tests/kernel.rs` (18 tests, unchanged) passes against both packages:
  `cargo test -p vesper-simpulse -p vesper-simpulse-seed`. The README
  doctests (including the `compile_fail` one) pass for both.
- New `tests/stress.rs` (both packages): three seeded runs of a workload (`workload/mod.rs`, shared with the benchmark) that
  uses every transition, including stale, duplicate and foreign-task calls
  (up to 5,000 dispatches and 16,309 journal events). It asserts that the
  digest of every outcome's `Debug` text (errors included), the digest of the
  journal JSON bytes, and the event count equal constants recorded from the
  Rust build. Each journal must also replay through `from_json` and
  `finish_replay`.
- New `tests/port_memory.rs` (port package only): the package links the port
  (`IMPLEMENTATION == "vesper"`), and recording plus replaying simulators
  leaves the Vesper library's live allocation count unchanged.
- Unit tests: the crate had none.

## Divergence check

`vesper-port-divergent` (Cargo, `EDITS` in `build.rs`) and
`:simpulse_vesper_divergent` (Bazel, same edit): a dispatched task is never
marked as seen, so rounds never reset. `scripts/test-port-harness.sh`
(through `port/check.sh`) asserts that
`each_fixed_runnable_task_runs_once_per_round` fails and
`virtual_clock_only_jumps_when_nothing_can_run` passes. Checked locally: 17
passed, and exactly that test failed.

## Benchmark

`crates/vesper-simpulse/bench-abba.sh 11 40` (`examples/abba.rs`, built once
per package, run as separate processes in Rust, port, port, Rust order per
epoch, paired analysis with `vesper-bench`). Each run records a full journal
on a fresh simulator. Cloud container, 4 vCPU Intel Xeon @ 2.10GHz, release
builds, compiler from this branch (`--release` `vesper`).

| Workload | Rust ns/run | Port ns/run | Port/Rust (median) | Range over 11 epochs |
|---|---:|---:|---:|---|
| `mixed-8`: 8 tasks, 2,000 dispatches over every transition | 1,437,893 | 1,462,496 | 1.00x | 0.90x..1.30x |
| `mixed-256`: 256 tasks, 4,000 dispatches | 12,032,515 | 3,509,282 | 0.29x | 0.27x..0.30x |
| `yield-64`: 64 tasks that only yield, 4,000 dispatches | 5,981,908 | 1,810,538 | 0.30x | 0.27x..0.35x |
| `timers-32`: 32 sleeping tasks, 4,000 dispatches | 3,063,509 | 3,334,246 | 1.06x | 0.91x..1.16x |

Before the Fenwick trees, the port scanned every task the way the Rust build
does, and measured 1.57x, 2.30x, 0.58x and 1.67x on the same four workloads.

Each per-run time includes the Rust-side journal recording, the same work
in both builds. The port is faster where the Rust build's O(tasks) scan per
dispatch dominates (`mixed-256`, `yield-64`). It stays close where heap and
record traffic dominate (`timers-32`).

## Platforms

Linux x86_64 locally (Cargo). The four required CI checks run both packages:
Bazel on Linux (`//crates/vesper-simpulse/...`, the seed targets, the
divergent object), and `scripts/test-port-harness.sh` on macOS AArch64 and
Windows x86_64 MSVC.

## Remaining gaps

- `@vesper-std` `CacheBTree` should reclaim empty leaves so that ports with
  growing keys can use it.
- Per-completion records are never reclaimed (about 34 bytes per completion
  ever made). The Rust build's journal already grows by one event per
  completion.
