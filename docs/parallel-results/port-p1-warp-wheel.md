# Port wave P1 lane 3: vesper-warp-wheel in Vesper (pilot)

Claim #900. Baseline `main` at b69383e (descendant of d523661). Plan:
`docs/agent-prompts/port-p1/03-warp-wheel.md`; recipe:
`docs/maintenance/porting-a-crate.md`.

**Result.** The whole crate logic (timing wheel, PTO lease table, EDT launch
queue, virtual clock checks) runs in Vesper under `--features vesper-port`.
All 17 crate tests pass against both builds, `vesper-warp` passes against the
port, a divergent port is caught in every module, the steady state allocates
nothing on either heap, and the Vesper heap does not leak. The port is about
**8x slower** than the Rust build on the ABBA benchmark, so the Rust code is
**not deleted** in this PR (wave rule: a port slower than 2x keeps its Rust
code; see "Deletion" below).

## API inventory (re-measured)

Public items, all kept with the same paths and signatures:

| Module | Types | Functions and methods |
|---|---|---|
| `wheel` | `Wheel<T>`, `WheelConfig`, `WheelConfigError`, `InsertError<T>`, `InsertErrorKind`, `HandleError`, `AdvanceError`, `TimerHandle`, `Expired<T>`; consts `MAX_TIME_NS`, `MAX_CAPACITY` | `Wheel::{new, len, is_empty, capacity, tick_ns, now_ns, insert, deadline, cancel, advance, poll, next_wake_hint_ns, drain}`, `Drop`; `WheelConfig::with_capacity`; `InsertError::into_payload`; `leaked_timer_entries` |
| `leases` | `RetransmitLeases<L>`, `PacketNumberSpace`, `LeaseKey`, `ParkError<L>`, `ParkErrorKind`, `LeaseError`, `PtoExpired<L>`; const `MAX_PACKET_NUMBER` | `RetransmitLeases::{new, len, is_empty, contains, now_ns, next_wake_hint_ns, park, on_ack, rearm, poll_pto, discard_space, drain}`; `ParkError::into_lease`; `From<HandleError> for LeaseError` |
| `edt` | `EdtLaunchQueue<P>`, `SubmitError<P>`, `SubmitErrorKind`, `Launch<P>` | `EdtLaunchQueue::{new, len, is_empty, horizon_ns, now_ns, last_released_ns, next_wake_hint_ns, submit, poll_release, drain}`; `SubmitError::into_packet` |
| `clock` | `Clock` (trait), `VirtualClock`, `ClockError` | `VirtualClock::{starting_at, advance_to, advance_by}`, `Clock for VirtualClock` |

New, doc-hidden, port build only: `port_live_allocations()` (leak tests) and
`wheel::rust_reference` (the Rust wheel compiled beside the port for the ABBA
benchmark). New everywhere: `IMPLEMENTATION` (`"rust"` or `"vesper"`).

In-crate `#[cfg(test)]` unit tests: **none** (the crate's 17 tests are all
integration tests under `tests/`), so the unit-test rule has nothing to
translate.

## What was ported

The Vesper source is eight files in `vesper/src/`, joined in a fixed order
into one library (`build.rs` `PARTS`, BUILD.bazel `:wheel_source`):

| File | Rust original | Content |
|---|---|---|
| `types.ves` | all modules | the boundary types (same names, fields, variants) |
| `util.ves` | `wheel.rs` | total helpers: shifts, division, saturating ops, highest/lowest set bit |
| `wheel.ves`, `wheel_lists.ves` | `wheel.rs` | `WheelCore`: slab, generations, cascade, overflow, sorted ready list, bottom-up merge sort |
| `wheel_api.ves` | `wheel.rs` | the exported wheel functions |
| `leases.ves` | `leases.rs` | `LeaseCore`: open-addressing key index (SplitMix64, backward-shift delete) over a `WheelCore` |
| `edt.ves` | `edt.rs` | `EdtCore`: horizon, release order |
| `clock.ves` | `clock.rs` | `clock_advance_to`, `clock_advance_by` |

The algorithms are line-for-line translations: same entry layout (as
structure-of-arrays `ChunkSeq[u64]`s), same list numbering, same placement
rule, same `next_work_tick` and cascade order, same merge sort, same probe
sequence. Mutating exports return `Result`/`Option` values instead of raising
`Fail`, because an abortable `Fail` may not cross an active `inout` parameter
(E0402); constructors, which take no `inout`, raise `Fail[WheelConfigError]`
and become `Result<Handle, WheelConfigError>` in the binding.

**Why not a Loom package.** A package's root module keeps its item names;
every other module's items get qualified names
(`vesper__warp_2wheel__types__WheelConfig`), which then appear in the
binding, and a module cannot import the root module (`use self::lib` and
`use @vesper/<self>` are both refused). The boundary types and the handle
types (with all the code that touches their fields) would have to be in one
file. **Why not one file.** `vesper fmt` refuses sources over 8192 tokens
(`crates/vesper-fmt/src/recovery.rs`), and the port is larger; an `impl`
block with more than about 16 methods also hits the formatter's depth limit
of 24, so `WheelCore` has three `impl` blocks. Every file is canonically
formatted.

## What stays in Rust, and why

| File | What | Reason |
|---|---|---|
| `src/port/slab.rs` | the token slab | pattern (b): generic payloads `T`, `L`, `P` stay in Rust; Vesper stores entry indices |
| `src/port/wheel.rs` | `Wheel<T>` wrapper | payload slab; wheel identity (`HandleError::Foreign` for another wheel's handle; a process-global counter); `drain` sink adaptor (pattern c); `Drop` leak report |
| `src/port/leases.rs` | `RetransmitLeases<L>` wrapper | lease slab (with the key); `discard_space`/`drain` sink adaptors; `Drop` leak report |
| `src/port/edt.rs` | `EdtLaunchQueue<P>` wrapper | packet slab; `drain` sink adaptor; `Drop` leak report |
| `src/port/clock.rs` | `VirtualClock` moves | the wrapper keeps the clock value and passes it in (pattern d) |
| `src/clock.rs` | `Clock` trait, `VirtualClock::starting_at` | a trait needs `dyn`/generic trait objects (F7b); `starting_at` is a `const fn` |
| `src/{wheel,leases,edt,clock}.rs` | error types, their `Display`/`Error` impls, `Debug` impls | Rust trait impls on the crate's own types (shared by both builds) |
| `src/wheel.rs` | `leaked_timer_entries`, wheel ids | process-global atomics (shared-memory concurrency, F12) |
| `port/binding.rs` | generated binding | the only `unsafe` in the port build (FFI calls; the crate is `deny(unsafe_code)` with one `allow` on this module, and `forbid(unsafe_code)` in the Rust build) |

## Unit tests and divergence checks

- `tests/{alloc,layers,model,wheel}.rs` (17 tests) run unchanged against both
  builds.
- New: `tests/port_memory.rs` (port only): 2000 build/use/drain/drop cycles
  of all three types leave `port_live_allocations()` unchanged. **It found a
  real leak**: `LeaseCore::drain` dropped a local `ChunkSeq`, and native code
  does not free dropped sequences yet; the port now drains without the
  temporary.
- New: `tests/alloc_audit.rs`, see below.
- Divergence: one deliberate bug per ported module (`DIVERGENT_EDITS` in
  build.rs, `edits` in BUILD.bazel):

  | Module | Edit | Caught by (and an unchanged test that still passes) |
  |---|---|---|
  | wheel | an entry fires 1 ns early | `nothing_fires_early_within_a_tick`, `wheel_matches_btreemap_reference` (`equal_deadlines_fire_fifo_across_cascades` passes) |
  | leases | packet number 2^62 accepted | `park_refusals_return_the_lease` |
  | edt | an EDT exactly at the horizon refused | `edt_horizon_and_order_rejections_return_packet` (`discard_space_and_index_stay_consistent_under_churn` passes) |
  | clock | moving back by 1 ns accepted | `clock_never_moves_backwards` (`invalid_configs_are_rejected` passes) |

  Bazel: `//crates/vesper-warp-wheel:port_divergence_detected`. macOS and
  Windows: `crates/vesper-warp-wheel/port/test-port.sh`, called from
  `scripts/test-port-harness.sh`.

## Zero allocation

- **Rust heap:** `tests/alloc.rs`'s counting-allocator claims hold for the
  port unchanged (both tests pass under `vesper-port`): the slabs are
  preallocated in `new`, and the steady state stores and takes payloads in
  place.
- **Vesper heap:** native Vesper code allocates through the C library, which
  a Rust `GlobalAlloc` never sees, and the runtime keeps only a live count,
  not a total. `@fip` could prove it statically, but it rejects loops and
  `return`. So `tests/alloc_audit.rs` proves it over the source: from every
  steady-state export (`insert`, `cancel`, `poll`, `advance`, `deadline`,
  `park`, `on_ack`, `rearm`, `poll_pto`, `contains`, `submit`,
  `poll_release`, the clock moves, ...), a conservative call graph reaches
  no allocating operation (`ChunkSeq::new`/`with_capacity`, `push`,
  `extend`, `Box::new`, string builders). The same audit flags every
  constructor and `drain`, so it can fail.
- **Difference:** `drain` and `discard_space` allocate one `Vec` each in the
  port (Vesper returns the removed entries as a sequence); the Rust build
  does not allocate there. Both are teardown paths outside the steady state.

## Benchmark (ABBA, `vesper-bench`)

`tests/abba.rs` (`--ignored`), both builds in one process. One operation:
insert 4096 entries at mixed near/mid/far/overflow deadlines into a
4096-entry wheel (1 µs ticks, 64 slots, 4 levels), advance in 64 steps of
1 ms polling every due entry, then drain the rest. 30 epochs + 2 warm-up, 8
operations per window (16 per arm per epoch).

Setup: cloud container, 4 vCPU Intel Xeon @ 2.10 GHz, Linux x86_64, Rust
1.97 `--release`, Vesper `optimize = true` (the harness default).

| | Rust | Vesper |
|---|---|---|
| median per operation | 328 µs | 2,601 µs |
| Vesper/Rust ratio | | median 8.00 (min 4.05, max 9.18); 95% median interval [7.83, 8.14] |
| 2x gate (`analyze_pairs`, 10000 bp) | | **Regression** |

Profile (callgrind, one operation each): the hottest Vesper functions are
the element read helper (`try_get` on a slice view, 16% of instructions,
not inlined: Cranelift does no inlining across Vesper functions) and the
large functions (`open_tick`, `advance`), which zero 1.4 KB stack frames on
every call. The slowdown is code generation, not the algorithm; it is the
compiler's to close (inlining of small functions and not zeroing whole
frames), not this lane's.

## Platforms run

- Linux x86_64 (cloud): both builds, 17/17 tests each, `tests/port_memory.rs`,
  `tests/alloc_audit.rs`, `cargo test -p vesper-warp --features
  vesper-warp-wheel/vesper-port`, the divergent build,
  `crates/vesper-warp-wheel/port/test-port.sh`,
  `cargo +1.100.0 clippy --all-targets -D warnings` (default, `vesper-port`,
  `vesper-port-divergent`, and vesper-warp on the port), `vesper fmt --check`
  on every port file.
- macOS AArch64, Windows x86_64 MSVC, Linux Bazel: in CI on the PR.

## Deletion

**Not done.** The port is 8x slower than the Rust build, above the wave's 2x
limit. The crate keeps both builds, Rust as the default; the owner was asked
whether to delete anyway. Deleting later is mechanical: drop
`src/reference/`, `wheel::rust_reference` and `tests/abba.rs`'s Rust arm,
make `build.rs` unconditional, remove the feature.

## Root patch

`Cargo.lock` only (the crate's new optional and dev dependencies), in its own
commit. Shared lists: one call in `scripts/test-port-harness.sh`, one skip
line in `fixtures()` in `tests/fixtures/self_host/driver.rs` (the port's
files are one library and do not check one by one).
