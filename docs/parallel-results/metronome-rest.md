# Metronome rest: CAS baselines, `--deterministic`, `--asm`, zero-alloc and vectorization evidence, `PmuSampleWire` (§41, #464)

Wave-eight lane 15. Base `1094e649c95e4d81eef4e705d13882ab1339542d` (origin/main,
a descendant of the required `ea1ed29`). Edits are confined to
`crates/vesper-bench/**`, `crates/vesper-bench-pmu/**`, `src/bench_commands.rs`
and this report. No root manifest, lockfile or Bazel change: every crate used
(`vesper-loom-cas`, `cranelift-codegen`, `cranelift-native`, `serde_json`,
`blake3`, `libc`) was already a dependency of the package that uses it, and
`python3 tools/bazel/sync_build_files.py` produced no diff.

## What was built

### 1. `vesper bench --compare git:REV` with Loom CAS baselines

`--compare git:REV [--store DIR]` takes the baseline to be the candidate file
as committed at `REV`:

1. `git rev-parse --verify --quiet --end-of-options REV^{commit}` resolves the
   revision to a full commit ID (40 or 64 hex digits, anything else is an
   error); `git rev-parse --show-toplevel` and the canonical candidate path give
   the repository-relative path. Revisions with whitespace, `:` or a leading
   `-` are rejected before git runs.
2. The Loom CAS (`--store DIR`, else `$VESPER_STORE`, else
   `~/.vesper/store`, §8.2) is asked for a derivation record keyed by builder
   `vesper-bench-baseline`, version `vesper.bench.baseline/1`, options
   `git.commit`/`git.path`, and a `locator` source blob naming both.
3. On a miss, the blob is read with `git cat-file -s` (refused above 256 KiB)
   and `git cat-file blob`, packed into a JSON baseline archive
   (`schema`, `commit`, `path`, `source_blake3`, `source`), stored as an
   artifact blob and recorded with `publish_derivation`. On a hit the archive
   is read back through the store's verifying `read_blob` and git is not asked
   for the blob again.
4. A decoded archive must name the same commit and path and its source must
   hash to `source_blake3`; anything else is a typed error.

The baseline is then compiled in the same process as the candidate, as before.
The report carries `baseline.cas` (`rev`, `commit`, `path`, `archive` object
ID, `cache: populated|hit`, `store`). Native code is not cached: what the CAS
holds is the baseline source archive, not a "precompiled" one, because the
benchmark JIT's code depends on in-process addresses.

### 2. `--deterministic` (simulated clock)

Counts the steps of Vesper's reference evaluator (`vesper::runtime::Runtime`,
unoptimized IR, the same program the benchmark JIT compiles) for one call of
each arm, repeated `--samples` times (2..64) in each arm. Every repetition must
give the identical count and value, and the evaluator's value must equal the
native JIT result, or the run is an error. The report gives
`steps_per_call` per arm, the exact ratio in basis points and, with
`--max-regression-bp N`, passes exactly when the ratio's ceiling is at most
`N`. It is a deterministic cost model of the evaluator, labelled as such
(`provider: vesper.runtime.evaluator_steps`, `unit: evaluator steps per call`),
not cycles or retired instructions. It excludes `--pmu*`, `--metric`,
`--pmu-events`, `--expect-zero-alloc` and `--randomize-layout`.

Observed: `fn main()->i64{wrapping_add(1,2)}` takes 5 steps per call;
`fn f(x:i64)->i64{wrapping_mul(x,x)} fn main()->i64{wrapping_add(f(3),f(4))}`
takes 17, a ratio of exactly 24000 bp, on every run.

### 3. `--asm` and `--expect-vectorized`

`--asm` hands every Cranelift function of the measured closure (from
`vesper::native::object_with_observer`, unoptimized, i.e. the batch JIT's IR)
to `cranelift_codegen::Context::compile` with the benchmark JIT's ISA flags
(host target, `is_pic=false`, `use_colocated_libcalls=false`, NaN
canonicalization for DetFloat programs) and `set_disasm(true)`. The report
lists, per function and per arm, the CLIF text, Cranelift's post-regalloc
machine listing, the code size and its BLAKE3, the CLIF instruction count and
the number of vector-typed CLIF instructions. Text output prints the listings.
These bytes are a recompilation of the same IR with the same flags; they are
not read out of the JIT's executable memory.

Vectorization evidence: a function is vectorized when any CLIF instruction
has a vector controlling type, result or argument (Cranelift emits SIMD
machine instructions only for those). Machine-listing lines naming vector
registers are counted too (`machine_vector_register_lines`), but only as a
listing aid: on x86-64 scalar floats also use `xmm` registers.
`evidence.vectorization` is `scalar` or `vectorized` whenever `--asm` or
`--expect-vectorized` ran; `--expect-vectorized` fails the command unless the
candidate is vectorized. Every kernel `vesper bench` accepts today is scalar
(the supported subset has no vector types), so today this gate always fails,
correctly.

### 4. `--expect-zero-alloc`

`vesper_bench_pmu::CountingAllocator` forwards to `System` and counts
allocations, reallocations, deallocations and bytes process-wide and per
thread (a const, destructor-free thread local that never allocates). The
`vesper` binary installs it as its global allocator (one line in
`src/bench_commands.rs`); it is `System` otherwise, so other commands behave
as before. `AllocationObserver` reads the measuring thread's counters around
every ABBA window, innermost of all observers; `AllocationObserver::open`
first proves the allocator is installed with a probe allocation, otherwise the
evidence is `AllocatorNotInstalled`, never zero.

`--expect-zero-alloc` reports `allocation` (measured windows, allocating
windows, totals) and `evidence.zero_allocation: zero|allocated`, and fails
unless every measured (post-warmup) window allocated nothing. Observed on this
host: 24 of 24 measured windows, 0 allocations, for the kernels above. The
windows include the harness's own dispatch, clock reads and result storage;
work handed to another thread would not be attributed.

### 5. `PmuSampleWire` and extended events

`vesper_bench::wire::PmuSampleWire` is the §41.2 record: seven `u64le` then two
`u32le`, exactly 64 bytes with no padding (a test walks the field offsets),
with `encode`/`decode`. Decoding rejects other lengths, TMA basis points above
10000 and layout offsets that are not multiples of 64 below 4096. Because the
record has no room for availability, `PmuSample` keeps every field as measured
or unavailable: `to_wire` refuses a sample with any unavailable field (listing
them), and `to_wire_masked` encodes them as zero together with an
`AvailabilityMask`. `backend_bound_bp` computes the TMA share exactly.

`vesper_bench_pmu::events` selects L1D read misses and LLC read misses
(`PERF_TYPE_HW_CACHE`), branch mispredictions and reference cycles
(`PERF_TYPE_HARDWARE`), and SIMD operations and Top-Down slots from sysfs
aliases of a core PMU (`slots`/`topdown-be-bound` on Intel,
`stall_slot`/`stall_slot_backend`/`ase_spec`/`sve_inst_spec` on Arm),
encoding sysfs event strings with the device's `format/` bit layout (`config`
and `config1`; `config2` is rejected because this crate uses the v0
`perf_event_attr`). An event with no encoding is `EventNotExposed`.
`PerfEventSet` (Linux) opens each event as its own unpinned group, except the
TMA pair, which shares a group with slots leading as Intel requires; it never
fails because one event is missing, and reports per window either a fully
scheduled delta (zero is a valid count) or the reason. `vesper bench
--pmu-events LIST` adds `pmu_events` to the report: per-event totals or
reasons, the host PMU status, and one masked `PmuSampleWire` record (hex) per
window. `arena_bump_bytes` and `icache_rand_offset_bytes` are never measured
by this command (no region arena, no code-entry jitter) and stay outside the
mask.

On this host every event is unavailable, as required:

```text
LIVE_EVENT_UNAVAILABLE: branch-mispredicts PMU unavailable: NoHardwarePmu at perf_event_open (extended event) (OS error 2)
LIVE_EVENT_UNAVAILABLE: l1d-read-misses ... NoHardwarePmu (OS error 2)
LIVE_EVENT_UNAVAILABLE: llc-read-misses ... NoHardwarePmu (OS error 2)
LIVE_EVENT_UNAVAILABLE: ref-cycles ... NoHardwarePmu (OS error 2)
LIVE_EVENT_UNAVAILABLE: simd-ops PMU unavailable: EventNotExposed at simd-ops
LIVE_EVENT_UNAVAILABLE: tma-backend-bound PMU unavailable: EventNotExposed at tma-backend-bound
LIVE_EVENT_UNAVAILABLE: tma-slots PMU unavailable: EventNotExposed at tma-slots
```

### 6. AArch64 `PMCCNTR_EL0` / `perf_user_access`

`RdpmcCounters` now also builds on Linux AArch64. It opens the events with the
arm_pmuv3 `config1` bits `long` and `rdpmc` and follows the same perf mmap
page protocol; when the page grants `cap_user_rdpmc` (which the kernel does
only with `perf_user_access=1`), index 32 reads `PMCCNTR_EL0` and index `n`
reads `PMEVCNTR<n-1>_EL0`, each between `ISB`s. Indices above 32 are rejected
before any read. `HostPmuStatus` gains `perf_user_access`. This compiles and
assembles for `aarch64-unknown-linux-gnu` and passes Clippy there; it has not
run on AArch64 Linux hardware (no such host or CI runner), so it is unexercised.

## Checks

`CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0`,
target directory inside the checkout:

```sh
cargo fmt --all -- --check                                                   # clean
cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings       # clean
for t in aarch64-unknown-linux-gnu aarch64-apple-darwin x86_64-pc-windows-msvc; do
  cargo +1.100.0 clippy -p vesper-bench -p vesper-bench-pmu --locked --all-targets \
    --target $t -- -D warnings                                               # clean
done
cargo +1.100.0 build -p vesper-bench-pmu --lib --target aarch64-unknown-linux-gnu  # assembles
cargo test -p vesper-bench -p vesper-bench-pmu --locked                      # pass
cargo test --locked --bin vesper bench_commands                              # 14 pass
cargo test --locked -p vesper --test bench_cli                               # 6 pass
cargo test --workspace --locked --no-fail-fast                               # all pass except 6 Lean targets
```

The six failing workspace targets (`lean4lean42_acceptance`,
`oracle42_acceptance`, `oracle42_runner`, `proof_audit_private`,
`replay42_kernel`, `resource_model42`) need a `lean` executable, which this
checkout does not have (`"lean" "--print-prefix": No such file or directory`);
they are §42 paths this lane does not touch and run in CI with Lean.

New tests:

* `crates/vesper-bench/tests/wire.rs`: exact padding-free layout,
  little-endian round trip, malformed lengths/basis points/offsets, masked
  encoding never turning an unavailable field into a measured zero.
* `crates/vesper-bench-pmu/tests/alloc.rs` (installs the allocator): exact
  thread counts for one `Box`, another thread's allocations not attributed, a
  real ABBA run with zero allocations in every arithmetic window and at least
  16 allocations per window in an allocating arm. Unit test: without the
  allocator installed the observer reports `AllocatorNotInstalled`.
* `events` unit tests: generic encodings, Intel and Arm sysfs fixtures, bit
  scattering, malformed format/event strings and missing PMU types.
* `linux` extended-event unit tests: group read decoding by ID, scheduling and
  regression checks, unresolvable events reported rather than fatal; a live
  test that every requested event is a count or a reason.
* `bench_commands` CLI tests (in process through the real option parser and
  `execute`): flag parsing and every rejected combination; deterministic
  equality, exact regression/pass gates and repeatability; `--asm` listings
  for both functions of a two-function kernel and the failing
  `--expect-vectorized` gate; the zero-allocation gate on a real run plus
  synthetic allocating windows; `--pmu-events` reports with decoded
  `PmuSampleWire` records; CAS populate/hit (no git blob read on a hit),
  archive tampering and path/commit mismatch, unknown revisions, short
  commit IDs, paths outside the work tree and oversized blobs with scripted
  git; and the full `--compare git:HEAD` flow against a real temporary git
  repository (populated, then hit, deterministic and timed). If no `git`
  executable exists the last test prints `GIT_UNAVAILABLE` and the scripted
  tests still cover the CAS logic.

## Decisions

* The counting allocator needs `unsafe impl GlobalAlloc`. It lives in
  `vesper-bench-pmu`, the one crate where this lane may write unsafe code,
  in a new `alloc` module that only forwards to `System`. On targets where the
  crate previously had `forbid(unsafe_code)` (Windows, Linux 32-bit, macOS
  x86_64) the lint is now `deny`, with `#[allow(unsafe_code)]` on that module
  only, so zero-allocation evidence works on every CI platform.
* Per-thread counts: process-wide counts would let the test harness's or any
  other thread's allocations leak into a window (and make the in-process CLI
  tests flaky).
* `--deterministic` uses the evaluator rather than counting native
  instructions: counting executed native instructions needs either hardware
  counters (absent here) or instrumented code run through `unsafe` calls in
  the root crate, which this lane may not add. The evaluator is exact,
  portable and already the reference semantics; the native result is checked
  against it.
* `--compare` stores source, not machine code (see above), and resolves
  revisions with the `git` executable, so it works for any revision syntax git
  accepts; `git` is the only new runtime requirement and only for `--compare`.
* `--asm`, `--expect-*` and `--pmu-events` are per-run evidence and are
  rejected with `--randomize-layout`.

## Remaining gaps

* No hardware counter was exercised: this host has no PMU, so the extended
  events, RDPMC and the new AArch64 path all report unavailable here. TMA uses
  only the slots/backend-bound pair (no frontend/bad-speculation/retiring),
  and SIMD operations exist only where the PMU publishes an alias (not Intel).
* `PmuSampleWire` is produced only for windows with counters; nothing fills
  `arena_bump_bytes` or `icache_rand_offset_bytes`.
* `--asm` shows CLIF and machine code but no side-by-side diff between arms and
  no per-instruction PMU stall annotation or cache-miss heatmap (§41.1 item 4).
* `--expect-vectorized` and `--expect-zero-alloc` are CLI gates; the `@bench`
  attribute (`zero_alloc`, `expect_vectorized`, `max_instructions_per_byte`)
  and its static `Alloc`-absent check belong to the §16.4 macro work held by
  Codex, and `max_instructions_per_byte` needs real instruction counts.
* The CAS holds baseline sources; it does not hold compiled baselines, and
  there is no `--compare` for anything but `git:`.
* The README `vesper bench` row (root-owned) does not mention the new flags
  yet; its test now only rejects claims that remain false (`12-cycle`,
  `--causal`).
* Rust ownership in these APIs is not evidence that Vesper's source typing
  rules for linear benchmark inputs (`BenchHarness::iter_linear_batched`) are
  implemented; they are not.
