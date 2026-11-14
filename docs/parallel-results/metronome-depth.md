# Metronome depth: hardware counters, layout randomization and causal profiling (§41, #255)

Wave-six lane 13. Base `07322e3ba47890e939a48967a1ea8f62fdd7aac1` (origin/main,
a descendant of the required `cda092f`). Edits are confined to
`crates/vesper-bench/**`, `crates/vesper-bench-pmu/**`, `src/bench_commands.rs`,
the `vesper bench` row and crate-tree line of `README.md`, and this report. No
root manifest, lockfile or Bazel BUILD change was needed: `libc` was already in
Cargo.lock and `tools/bazel/sync_build_files.py` produced no diff.

## What was built

### 1. Hardware counters (`vesper-bench-pmu`)

* **`RdpmcCounters`** (Linux x86_64, `linux.perf_event_mmap.rdpmc`). Opens a
  pinned, enabled, user-only cycles leader and instructions member for the
  current thread, maps each event's first page read-only and reads it with the
  kernel's documented protocol for `struct perf_event_mmap_page`: under the
  `lock` sequence counter, require `cap_user_rdpmc` and a nonzero `index`,
  execute `RDPMC(index - 1)` (inline `asm!`), sign-extend from `pmc_width` and
  add `offset`. No syscall runs between a window's two reads. A window also
  requires the page's cumulative `time_enabled == time_running` (never
  multiplexed). Unsafe code is confined to this gated module (mmap, volatile
  page reads, `rdpmc`, munmap), alongside the existing group-read module.
* **`KpcCounters`** (macOS AArch64, `macos.kperf.kpc`). `dlopen`s
  `/System/Library/PrivateFrameworks/kperf.framework/kperf`, resolves
  `kpc_get_counter_count`, `kpc_set_counting`, `kpc_set_thread_counting` and
  `kpc_get_thread_counters`, then refuses with `PermissionDenied` unless
  `geteuid() == 0` before changing any kpc state. With root it enables the
  fixed class and reads fixed counters 0 (cycles) and 1 (instructions) for the
  calling thread; Drop disables thread counting. Unsafe code is confined to
  this one gated module.
* `CounterDelta` now carries its `Provider`; new `UnavailableReason`s
  `NoHardwarePmu`, `RdpmcNotPermitted`, `NotScheduled`, `Contended`,
  `LibraryMissing`; `host_pmu_status()` reports `perf_event_paranoid`, whether
  a `cpu` PMU device exists and the `rdpmc` sysfs knob. The existing
  group-read provider is unchanged apart from carrying its provider tag.
* `vesper bench --pmu-provider group|rdpmc|kpc` selects the mechanism (default
  group everywhere, so plain `--pmu` keeps its meaning and the root-owned
  `tests/bench_cli.rs` expectations hold; RDPMC and kpc are opt-ins). An unavailable provider's JSON now includes
  the host status.

On this cloud host (no `cpu` PMU device, `perf_event_paranoid=2`) every
provider reports unavailable, as required:

```text
LIVE_RDPMC_UNAVAILABLE: PMU unavailable: NoHardwarePmu at perf_event_open (OS error 2);
  host HostPmuStatus { perf_event_paranoid: Some(2), cpu_pmu_device: Some(false), rdpmc_sysfs: None }
LIVE_PMU_UNAVAILABLE: PMU unavailable: OsError at perf_event_open (OS error 2)
LIVE_KPC_UNAVAILABLE: PMU unavailable: UnsupportedPlatform at kpc
```

The group-read provider still reports ENOENT as `OsError`, because the
root-owned `tests/bench_cli.rs` pins the list of allowed reasons; only the new
RDPMC provider maps it to `NoHardwarePmu`. Unifying them is a two-line
follow-up for whoever owns that test.

### 2. Stabilizer-style layout randomization (`vesper_bench::layout`)

* `layout_plan(seed, run)`: SplitMix64 draws of a heap offset (16-byte steps
  below 4 KiB), a stack padding (64-byte frames below 4 KiB) and an
  environment size (below 4 KiB), plus a per-run `RunConfig::layout_seed`.
* `with_stack_padding(bytes, f)`: runs `f` below real non-inlined frames each
  holding a live 64-byte local. A test checks the benchmark frame moves by at
  least the requested padding.
* `HeapShim`: a live block held for the run, shifting allocator state.
  `OffsetBuffer`: input bytes placed at an exact page offset (tested by
  address).
* `layout_survival(per_layout)`: the existing exact two-sided >=95%
  order-statistic median interval over one aggregate `PairedSample` per layout,
  tested against ratio 1 (`CandidateSlower`, `CandidateFaster`,
  `DoesNotSurvive`, `InsufficientLayouts`).
* `run_randomized_layouts`: in-process driver (heap + stack per layout).
* `vesper bench --randomize-layout N [--layout-seed S]` (N in 6..32): spawns one
  child `vesper bench ... --json --layout-run S:i` per layout with
  `VESPER_BENCH_LAYOUT_PAD` sized by the plan, so each run also gets its own
  environment size; the child applies heap shim and stack padding around its
  measurement and echoes its plan, which the parent checks. A child that cannot
  measure (for example PMU unavailable) makes the whole comparison
  inconclusive with the child report attached. `--max-regression-bp` gates the
  per-layout samples.

Observed on this host (debug build, `--samples 6 --iterations 16`, wall time):
an 8x-work candidate against its baseline gave `candidate_slower` over 6
layouts; the same file against itself gave `does_not_survive` over 8 layouts,
with per-layout median ratios ranging from -44 to +50 percent. That spread on
an identical kernel is the effect Stabilizer exists to expose.

### 3. Coz-style causal profiling (`vesper_bench::causal`)

`CausalProfiler::new(regions, progress_points)`, one `CausalThread` per
thread. `region(id, f)` times a region; when it is the selected region the
thread adds `elapsed * s / 100` to a global delay count and its own local
count. Other threads pay what they owe at their next region entry, progress
point or `delay_point()`. `profile(&profiler, &config, program)` runs baseline
experiments and `repetitions` runs per (region, speedup), computes effective
time as elapsed minus inserted delay, and reports the exact predicted
end-to-end speedup `1 - period_s/period_0` in basis points per region.
Each experiment run is paired with an adjacent baseline run and the median
per-pair prediction is reported, so load drift between experiments cancels;
pauses busy-wait instead of sleeping, because macOS sleeps of a few
milliseconds wake late and bunch delays into long stalls (the second macOS CI
run, with the test already alone, predicted 2% for a true 50%); any overshoot
is still credited against later delay, as in Coz. Inserted delay exceeding elapsed time is a typed error, not a
prediction. The test is tagged `exclusive` in Bazel: the first macOS CI run,
sharing three cores with other test actions, produced negative predictions for
every region before these changes.

Known-bottleneck test (`crates/vesper-bench/tests/causal.rs`): two threads per
round, "slow" spins 8 ms and "fast" 3 ms, then meet at a barrier; the round is a
progress point. Theory: slow +50% gives 5000 bp, slow +100% gives
`1 - 3/8 = 6250` bp, fast gives 0. Three local runs:

| Region | +50% | +100% |
| --- | --- | --- |
| slow | 4964, 4977, 4981 bp | 6516, 6507, 6500 bp |
| fast | 16, -4, 19 bp | 1, -3, 17 bp |

The test accepts 3000..7000 / 4000..8000 bp for slow and -1500..1500 for fast,
and requires slow +50% to beat fast +100% by 2000 bp.

### 4. README

The `vesper bench` row now describes only what the command does on main:
paired A/B/B/A runs with the median-ratio gate, group/RDPMC/kpc counters
reported unavailable otherwise, and layout randomization, stating that causal
profiling is a library API. The "replaces" column drops `iai-callgrind` and
`cargo-show-asm` (no simulator or `--asm`) and adds `coz`. A unit test in
`src/bench_commands.rs` reads README.md, checks every `--flag` in the row
appears in `vesper bench --help`, and rejects the removed claims (`12-cycle`,
`CAS`, `--causal`, `--asm`).

## Checks

Commands run with `CARGO_TARGET_DIR=/home/claude/target CARGO_BUILD_JOBS=4
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0`:

```sh
cargo fmt --all -- --check                                              # clean
cargo clippy --workspace --locked --all-targets -- -D warnings          # clean
cargo clippy -p vesper-bench -p vesper-bench-pmu --locked --all-targets \
  --target aarch64-apple-darwin -- -D warnings                         # clean (kpc module)
cargo clippy -p vesper-bench -p vesper-bench-pmu --locked --all-targets \
  --target x86_64-pc-windows-msvc -- -D warnings                       # clean
cargo test -p vesper-bench -p vesper-bench-pmu --locked                 # pass
cargo test --locked --bin vesper bench_commands                         # 7 pass
VLB_CARGO_LOCK=$PWD/Cargo.lock cargo test --workspace --locked --no-fail-fast  # pass, exit 0
```

New tests: RDPMC formula, sign extension, denied/unscheduled/malformed pages
never executing `RDPMC`, sequence-lock retry and contention, multiplex
rejection, shared delta checks; live RDPMC and kpc tests that accept only real
counters or a reason consistent with the host; layout plan bounds and
variation, real stack and heap placement, survival verdicts, an in-process
randomized run detecting an 8x difference; causal accounting and the
known-bottleneck experiment; CLI flag parsing and forwarding, the layout driver
with scripted children (survives, does not survive, mismatched plan,
unavailable child) and the README check.

`VLB_CARGO_LOCK` is needed only because the target directory sits outside the
checkout; without it `vesper-loom-build` cannot find the lockfile and its tests
fail with `UnknownFingerprint`, unrelated to this lane.

## Decisions

* Perf events through `libc` + inline `asm!` rather than a new crate: the
  existing provider already uses `libc`, the RDPMC protocol is small and
  documented, and no lockfile change was needed.
* kpc fixed counters only, which need no kpep event database; configurable
  counters are not used.
* The environment size can only differ between processes, so layout
  randomization in the CLI is per child process; the library offers the
  in-process heap/stack variant.
* Causal profiling stays a library API: `vesper bench` measures single-threaded
  scalar kernels, where a causal experiment has nothing to say.

## Remaining gaps

* No real counters were exercised: this host has no hardware PMU, and macOS CI
  is not root, so kpc only proves the framework and symbols resolve. RDPMC on
  Linux AArch64 (`perf_user_access`) and AArch64 `PMCCNTR_EL0` are not done.
* No measurement of RDPMC read cost; the spec's "12-cycle" figure is not
  claimed.
* Layout randomization does not relocate code, function entries or globals,
  and does not re-randomize during a run as Stabilizer does.
* Causal profiling: explicit regions instead of sampled lines, delay accrues at
  region end, no blocked-thread delay credit, host threads rather than Pulse
  fibers, no p99 latency curves, no Vesper source surface
  (`BenchHarness::causal_progress_point`).
* Still not built from §41: CAS baseline loading (`--compare git:main`),
  `--deterministic`, `--asm`, `@bench` static gates, zero-alloc and
  vectorization evidence, L1/LLC/branch/TMA events, `PmuSampleWire`.
* Rust ownership in these APIs is not evidence that Vesper's source typing
  rules for linear benchmark inputs are implemented.
