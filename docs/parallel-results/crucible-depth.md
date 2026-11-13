# Wave six lane 20: Crucible virtual arenas, SimPulse timers and I/O, and DPOR (§29)

Issue #262. Follows the wave-five Crucible report
([crucible.md](crucible.md)), whose remaining gaps this lane closes in part.

- Base: `07322e3ba47890e939a48967a1ea8f62fdd7aac1` (origin/main, a
  descendant of the required baseline `cda092f`).
- Branch: `claude/project-thread-jtj4u5`.

## What was built

All code is in `crates/vesper-crucible` (still `#![forbid(unsafe_code)]`).
It now also depends, read-only, on `vesper-alloc`. No Lean files changed.

### 1. `VirtualArena`: 64 KiB reset-per-execution input arena

- `VirtualArena::new()` (64 KiB, `VIRTUAL_ARENA_BYTES`) and
  `with_bytes(n)` build one bounded `vesper_alloc::Region`: one chunk of
  exactly `n` bytes, which is also the reservation limit and is retained
  across resets. `ArenaSetupError` covers a zero size and a rejected region
  configuration.
- `run_replay(&tape, prop)` and `run_random(prefix, seed, max_draws, prop)`
  run `prop(&mut Source, &mut Region)` once. The arena keeps one `Source`
  and re-arms it before each run (the tape, realized-values and draw-kind
  buffers are cleared, not reallocated). The region is reset when each
  execution ends, so every handle from execution *n* is
  `HandleError::Stale` in execution *n + 1*, `used_bytes()` is 0 between
  executions and `epoch()` advances once per execution.
- `ArbitraryIn` (`Arbitrary[r]` in the design) generates values straight
  into the region: `RegionBytes` (`RegionSlice[r, u8]`, the design's
  `draw_bytes[r]`), `RegionVec<T>` for `T: Arbitrary`, and pairs. The free
  functions `draw_region_bytes` and `draw_region_vec` take explicit bounds.
  They use the same tape encoding as `Source::draw_bytes` / `draw_vec` (a
  continuation flag before each element), so shrinking still deletes whole
  elements and a tape decodes the same way on both paths. Bytes are staged
  in a 256-byte stack buffer and appended with `extend_bytes`.
- A value that does not fit is `Invalid::ArenaExhausted` (a new `Invalid`
  variant): the run is discarded, never truncated.
- An execution that leaves a linear region payload live (`alloc_linear`
  never consumed) fails with kind `"arena-linear-leak"`. The region refuses
  to reset over it, so the arena drops the region, which finalizes the
  payload, and builds a fresh one. `ArenaStats` counts executions, high
  water, exhausted runs and linear leaks.

### 2. SimPulse timers and I/O in schedule exploration

`SimFiber<W>` step machines run on the real `vesper_simpulse::Simulator`.
A step returns `SimStep::{Yield, Done, Sleep { ns }, Io(IoRequest)}`, with
`IoRequest::Scheduled { delay_ns, result }` (`Simulator::schedule_io`) or
`IoRequest::External { result }` (`park_io`; the explorer chooses when to
call `complete_io`). `StepCx` gives the step its wake reason (start, yield,
timer, or I/O completion with its result, as a `Wake` with per-fiber
sequence numbers) and SimPulse's virtual time, and
records the step's shared accesses.

In every state the machine offers the enabled `Proc`s in canonical order:
`Task(i)` (step ready fiber `i`), `Io { task, seq }` (deliver a pending
external completion) and, when no fiber is ready, `Timer { task, seq }`
for the pending timer or scheduled I/O with the earliest deadline. Firing
it lets SimPulse advance virtual time and fire everything due then, which
is SimPulse's own semantics. So a reply can be delivered before or after a
timeout fires, which the wave-five run/defer explorer could not express.

- `explore_random(src, world, fibers, cfg)` draws each decision from the
  choice tape (`draw_index` over the enabled list). Fuzzing and shrinking
  work unchanged, and shrinking prefers lower fibers first and deliveries
  before timers.
- `replay_schedule(world, fibers, schedule, Some(journal), cfg)` re-runs a
  schedule under `Simulator::replay`, which validates every transition and
  requires the journal to be fully consumed.
- SimPulse has no "dispatch task *i*" call. The machine steers it by handing
  tokens it did not want back with `yield_task`, which the journal records.
  It keeps each fiber's wake reason from the first dispatch after the
  wake-up, because those hand-backs overwrite SimPulse's reason with
  `Yield`.
- Typed errors (`ExploreError`): an access outside the declared footprint,
  zero-delay sleeps or scheduled I/O (SimPulse would fire them inside the
  next dispatch, so they are not separately schedulable), SimPulse errors,
  a disabled process in a schedule, nondeterministic re-execution, the depth
  limit and the execution limit. Inside a property these become
  `PropError`s (kind `"explore"`, or `Invalid::Overrun` for depth).

### 3. Exhaustive exploration and DPOR

`explore_exhaustive(cfg, setup, observe)` and `explore_dpor(cfg, setup,
observe)` are stateless explorers. Every execution re-runs `setup` and
replays the recorded prefix, so worlds and fibers need not be `Clone`. Each
returns an `Exploration`: every distinct `Outcome` (the observer's value, or
the `Failure` that stopped the run) with the first schedule that produced
it, plus the counts of executions, sleep-blocked prefixes and transitions.

**Algorithm.** Persistent-set DPOR (Flanagan and Godefroid, POPL 2005),
with clock-vector happens-before and sleep sets:

- Each fiber declares, through `SimFiber::footprint`, the objects its next
  step may read and write. The declaration can depend only on the fiber's
  own state. The machine checks every recorded access against it.
- Dependence: conflicting footprints; a fiber's step and the delivery of its
  own I/O; anything with a timer firing. A timer is disabled by any ready
  fiber and by an earlier deadline, and it changes the time steps observe.
- Each delivery and each timer is its own process. It is created by the step
  that submitted or armed it and happens after that step. Races are reversed
  only between transitions that can be enabled together: a fiber step and a
  timer never are, and neither are a fiber's step and the delivery of its
  own I/O.
- At each new state, for every process `p` with a next transition, the
  explorer finds the latest racing event `i` and computes FG05's set `E` at
  the state before `i`: `p` if it is enabled there, plus every process
  enabled there that runs a later event which happens before `p`. It adds
  the first member of `E` that is not in that state's sleep set. If every
  member is asleep, or `E` is empty, it adds every enabled process.

**Why not source-DPOR or optimal DPOR.** The prompt asked for one of them.
Both assume that transitions do not disable one another. In SimPulse, a
timer is disabled by every I/O delivery that readies a fiber, and SimPulse
fires timers only when no fiber is ready. Persistent-set DPOR handles that
disabling by falling back to every enabled process.

**What the differential tests caught.** Two earlier versions failed the
DPOR-vs-exhaustive tests, and both bugs are fixed:

1. A single clock process whose next transition changed when other fibers
   armed timers. This violated FG05's assumption that a process's next
   transition depends only on its own state. It is now one process per timer.
2. Adding only `p` when `p` was enabled but asleep at the backtrack state.
   That pruned the reversal; the fix is FG05's `E` set with awake members
   preferred.

**Guarantee and its limits.** If footprints are accurate and fibers and
`setup` are deterministic, every reachable final state and every reachable
failure should be found. This is checked differentially, not proved (see
below). There is no Lean model of the algorithm.

## Acceptance evidence

Unless a run says otherwise, numbers are from
`cargo test -p vesper-crucible --locked -- --nocapture` on the cloud
container (x86_64 Linux, 4 cores). The runs are deterministic.

**Arena reset (`tests/arena.rs`).**

- 50 random executions each generate a `RegionVec<u64>` and `RegionBytes`.
  Each starts with 0 bytes used, finds the previous execution's handle
  stale, ends with 0 bytes used and advances the epoch by exactly 1.
- Replaying the realized tape regenerates byte-identical values, and the
  same tape decodes the same way through the heap `Source::draw_bytes`.
- In a 4 KiB arena, a tape forcing 5,000 bytes is `ArenaExhausted` and the
  next small run passes.
- A leaked linear payload fails with `arena-linear-leak`, is counted, and
  the next execution starts clean.

**Zero allocation (`tests/arena_alloc.rs`).** A counting global allocator,
with a self-check. After warm-up over the same inputs, 4,100 executions
allocated 0 times. That is 4 rounds of 512 random runs, 512 strict replays
and one 48 KiB input. The arena's high water was 49,152 of 65,536 bytes.

**Planted timer/I-O race (`tests/explore.rs`).** A requester sends a request
and parks on its external reply. A watchdog sleeps 100 ns and, if the
request is still pending, applies a fallback. The buggy requester applies
the reply even when the fallback was already applied.

- Random-schedule fuzzing found it on execution 2. It shrank to the 6-step
  schedule `step(0) step(1) tick(1#0) step(1) deliver(0#0) step(0)`
  (timeout before reply; final time 100 ns).
- That schedule replays under SimPulse journal validation, and a journal
  missing its last 2 events is rejected.
- DPOR finds the same race in 3 executions; exhaustive needs 10.
- The fixed requester survives 2,000 fuzz executions, and DPOR finds no
  failure.
- Also tested: timers and scheduled I/O fire in virtual-time order, and
  deliveries of two external completions can overtake each other.

**DPOR vs exhaustive (`tests/dpor.rs`).** Same set of distinct outcomes on
every program:

| Program | Exhaustive executions | DPOR executions | Distinct outcomes |
| --- | --- | --- | --- |
| 3 independent fibers × 3 steps | 1,680 | 1 | 1 |
| Lost update (2 read/update fibers + 1 independent) | 90 | 4 | 4 |
| Requester/watchdog, buggy and fixed | 10 | 3 | as exhaustive |
| Timers + scheduled I/O + external I/O + 3 shared cells | 3,150 | 10 | 10 |
| 150 random programs, up to 3 ops per fiber (CI sweep) | 13,445 in total | 319 in total | equal on every program; DPOR strictly fewer on 148 |

Random programs have 2 or 3 fibers. Their steps are reads, writes,
read-modify-writes, local steps, checks that fail on an odd cell value,
sleeps, scheduled I/O and external I/O over 3 cells, with deadlines that often collide. Each step folds its wake reason,
virtual time and read values into a per-fiber accumulator, so outcomes tell
schedules apart.

Stress runs, in release with `VESPER_CRUCIBLE_DPOR_SEEDS`, up to 4 ops per
fiber:

- After the review fixes (see below), with failing `Check` steps in the
  mix: 11,770 of 12,000 programs fit the exhaustive budget (20,000
  executions). They took 6,273,874 exhaustive executions against 37,815
  for DPOR, with equal outcome sets on all of them; DPOR was strictly fewer
  on 11,711. The run took 135 s.
- Before those fixes, the same sweep without failing steps (11,732
  programs, 6,887,074 vs 37,134 executions) also agreed.

Every DPOR outcome's schedule also replays to that outcome.

## Checks

On the cloud container, Rust 1.100, isolated target directory:

- `cargo fmt --all -- --check`: clean.
- `cargo test -p vesper-crucible --locked`: 46 tests pass (13 api, 5 arena,
  1 arena_alloc, 7 dpor, 6 explore, 6 fuzzing, 2 lean_model, 6 shrinking).
- `cargo clippy --workspace --locked --all-targets -- -D warnings`:
  clean.
- `cargo test --workspace --locked --no-fail-fast` (on the first PR
  head): 3,343 passed, 0 failed. `vesper-loom-build`'s build script finds
  `Cargo.lock` by walking up from the target directory, so with a target
  directory outside the checkout it needs `VLB_CARGO_LOCK=$PWD/Cargo.lock`;
  without it its 14 cache tests fail with `UnknownFingerprint`, unrelated to
  this lane.
- `python3 tools/bazel/sync_build_files.py`: no BUILD changes. Bazel picks
  up the new dependency from `cargo metadata`.

## Root patch

One commit: the regenerated `Cargo.lock` entry for the new
`vesper-crucible → vesper-alloc` path dependency. No new workspace
dependencies, and no `src/**`, CI or global doc edits.

## Decisions

- **The arena owns the source.** `VirtualArena` holds the `Source`, so
  steady-state runs reuse its buffers. The heap `fuzz`/`shrink` engine is
  unchanged. Running it on arenas (one arena per campaign) is a small
  follow-up, left out to keep the wave-five fuzzer's measured behaviour
  intact.
- **Footprints are declared, not inferred.** Without compiler
  instrumentation, the only sound source of dependence is the program. The
  machine records the actual accesses, rejects any outside the declaration,
  and DPOR uses the declarations. A step that does not record an access it
  makes is invisible to the check; that is the fiber author's obligation.
- **Equal timer deadlines.** The machine offers only the lowest
  `(task, seq)` timer among equal earliest deadlines, because SimPulse fires
  them together anyway. This avoids redundant branching in both explorers.
- The wave-five `run_fibers` / `Fiber` API (run/defer over SimPulse's own
  dispatch order) is kept as it was.

## Review fixes (Codex review on 2d0944f)

- **A failing step ended the execution before its races were analysed**, so
  DPOR could miss schedules that avoid the failure (a reader failing on an
  unset flag never saw the writer go first). A failed fiber is now retired
  in SimPulse and the other fibers run on. An `Outcome` failure is the
  sorted list of every failure in the execution, which is the same for
  every interleaving of a trace; "first failure" was not. Regression test:
  `a_failing_step_still_reverses_its_races`, and the random sweep now
  includes failing steps. `explore_random` still stops at the first failure,
  which is what fuzzing wants.
- **A panicking property skipped the arena reset**, so a caller that caught
  the panic and reused the arena inherited the previous execution's
  allocations. The arena now marks an execution in progress and rebuilds or
  resets the region before the next one. Regression test:
  `an_execution_that_unwinds_does_not_leak_into_the_next`.
- **SimPulse completion IDs made disjoint steps order-dependent.** They come
  from one counter shared by all fibers, and steps could read them from the
  wake reason. Steps now see `Wake`, with per-fiber timer and I/O sequence
  numbers, instead of SimPulse's `WakeReason`. Regression test:
  `wake_identities_do_not_depend_on_the_interleaving`.

## Corrections to the design text

- §29.1 item 1 says each fuzz iteration runs in a resettable 64 KiB
  VirtualArena at over 500,000 executions per second per core. The arena and
  its zero-allocation steady state are implemented. No throughput claim is
  made, because Vesper code does not run here yet.
- §12.6 / §29.2 call SimPulse a DPOR schedule explorer. SimPulse itself
  still is not (its crate docs say so). DPOR lives in Crucible, over Rust
  step machines with declared footprints. It is persistent-set DPOR, not
  source or optimal DPOR, for the disabling reason above.

## Remaining gaps

- No proof. DPOR's soundness here is argued from FG05 and tested
  differentially against exhaustive search; there is no Lean model of the
  algorithm, and none of this is a theorem about the Rust code.
- Vesper source fibers do not run on the step machine. Lowering `@fuzz_prop`
  and `vesper test --sim` onto it, and the source-level `linear effect
  FuzzSource` / `Arbitrary[r]`, are compiler work. Rust ownership in these
  tests is not evidence that Vesper's typing rules are implemented.
- Not done in this lane: the `vesper fuzz` / `vesper verify --refute` CLIs
  (integration B), Cranelift coverage (the native coverage lane) and MPK
  differential shadowing.
- Fibers cannot spawn fibers or request cancellation during exploration.
  External I/O results are fixed by the fiber; the explorer chooses only
  when a completion arrives, not its value.
- Systematic exploration is bounded by `max_depth` and `max_executions` and
  reports exceeding them as typed errors. There is no preemption bounding
  or state caching.
