# Port P1 lane 18: vesper-crucible (parts 1, 2a and 2b)

Claim: [#915](https://github.com/robot-head/vesper/issues/915). Base: Simpulse
port PR #964 at `382f11a` (which merges `main` at `153f264`), because the owner
asked for this lane to start stacked on work still waiting to merge.
Crucible's Rust code links the Simpulse and Strata seed copies, so it depends
on #964 only for the `vesper-simpulse-seed` package name.

This is part 1 of the lane. It ports the choice tape, the generator, the draw
decision of `Source` and the shrinker, splits the crate into a port-backed
package and a Rust seed copy, and proves fixed-seed parity. The rest of the
crate (fuzzer, schedule explorers, DPOR, arena) stays Rust for now and is
listed below as part 2.

**The Rust code is not deleted.** The compiler links the crate (through
`src/crucible_commands.rs` and `src/native_coverage.rs`), so the seed-copy
rule applies. Under the owner's parity rule, the seed is also what every other
dependent uses until the port matches Rust in speed.

## Layout

- `crates/vesper-crucible` (package `vesper-crucible`) is the port-backed
  build. `build.rs` links the Loom package `vesper/` (`@vesper/crucible`,
  869 lines, export ABI v3) and sets `cfg(vesper_port)`.
- `crates/vesper-crucible/seed` (package `vesper-crucible-seed`, library
  `vesper_crucible`) builds the same `src/` without the port. The root
  `[workspace.dependencies]` entry points at it, so the compiler and all of
  its dependents get the Rust build.
- `port/binding.rs` is the checked-in generated binding.
  `port/check.sh` is the Cargo harness check (the Linux Platforms job since #1011).
- `tests/parity.rs` is new and runs against both packages.
  `tests/port_memory.rs` is new and runs against the port package only.
- `examples/abba.rs` and `bench-abba.sh` are the ABBA benchmark.

## API inventory (re-measured)

There are no `#[cfg(test)]` unit tests in the crate. The integration tests are
59 tests in 9 files (api 13, arena 5, arena_alloc 1, dpor 7, dpor_rest 11,
explore 6, fuzzing 6, lean_model 4, shrinking 6). The public API is the
re-export list in `src/lib.rs`:

| Module | Public items | This PR |
|---|---|---|
| `tape.rs` | `ChoiceTape` (`new`, `len`, `is_empty`, `key`, `to_text`, `from_text`), `ShrinkKey` (`of`, `smaller_than`), `TapeError`, `MAX_TAPE_LEN`, `TAPE_HEADER` | **ported**: key, order, text writer and parser |
| `rng.rs` | `SplitMix64` (`next_u64`, `below_inclusive`, `index`, `chance`) | **ported** |
| `source.rs` | `Source` and its draws, `Probe`, `Arbitrary`, `Failure`, `Invalid`, `PropError`, `DrawKind`, `CmpKind`, `CmpOperands`, `CmpRecord`, `check`, limits | **draw decision ported** (`Source::raw`: replay, clamping, overrun, fresh values); buffers, `Probe` and `Arbitrary` stay Rust |
| `shrink.rs` | `shrink`, `replay`, `ShrinkConfig`, `ShrinkStats`, `Shrunk`, `DoesNotFail` | **ported** as a step machine; the closure loop stays Rust |
| `exec.rs` | `Verdict`, `PANIC_KIND` | stays Rust (runs the closure, catches panics) |
| `fuzz.rs` | `fuzz`, `FuzzConfig`, `FuzzReport`, `Counterexample`, `Strategy`, `NONDETERMINISTIC_KIND` | **ported in part 2a** as a step machine (corpus, mutators, novelty, comparison solver); the closure call, replay checks and shrinking hand-off stay Rust |
| `refute.rs` | `refute`, `Refutation`, `REFUTED_KIND` | stays Rust (generic `T: Debug` closures over `fuzz`) |
| `sim.rs` | `run_fibers`, `replay_fibers`, `Fiber`, `FiberStep`, `SimConfig`, `SimRun` | stays Rust (see below) |
| `explore.rs` | `explore_random`, `replay_schedule`, `format_schedule`, `SimFiber`, `StepCx`, `ExploreConfig`, footprints, I/O requests | stays Rust (see below); dependence and co-enabling moved into the port's search |
| `dpor.rs` | `explore_exhaustive`, `explore_dpor`, `explore_cached`, `Exploration`, `Outcome`, `Search` | **search core ported in part 2b** (backtrack and sleep sets, clock vectors, the preemption bound, the state cache); re-execution, the observer and key interning stay Rust |
| `arena.rs` | `VirtualArena`, `ArbitraryIn`, `draw_region_bytes`, `draw_region_vec`, limits | stays Rust for now (see below) |
| re-exports | `Region`, `RegionBytes`, `RegionVec` (Strata), `IoResult`, `Journal`, `WakeReason` (SimPulse) | Rust seed types |

The public API did not change. Every public signature is the same in both
builds. The port build only adds `IMPLEMENTATION` (`"vesper"` or `"rust"`)
and a hidden `port_live_allocations()`.

## What was ported, and how

- **Tape.** `tape_key` sums the choices into two words, because the Rust
  `u128` sum needs up to 84 bits. `key_smaller` is the `(len, sum)` order.
  `tape_to_text` writes ASCII bytes, and Rust turns them into the `String`.
  `tape_parse` returns a handle with a code, an index, the trimmed header's
  byte range and the values. It never fails on untrusted text. It trims
  Unicode `White_Space` exactly as Rust's `str::trim` does, by matching the
  UTF-8 encodings of all 25 characters. The `UnknownHeader` text (the first
  64 characters) is cut in Rust from that byte range.
- **SplitMix64.** `rng_next`, `rng_below_inclusive` and `rng_chance` take
  the state in place (an ABI v3 mirror). `SplitMix64(pub u64)` keeps its
  public field. `index` stays a Rust call to `below_inclusive`.
- **Draws.** `source_draw` makes the decision for each draw: an overrun at
  the budget, the replayed value clamped to the bound, an overrun under
  strict replay, or a fresh uniform or Bernoulli value. It reads the prefix
  as a borrowed slice. `Source` keeps its buffers (`prefix`, `realized`,
  `kinds`) in Rust, so `realized_values() -> &[u64]` still borrows.
- **Shrinker.** `Shrinker` is the four passes of `Reducer` (span deletion,
  span zeroing, small-then-binary-search lowering, lower-and-delete) and the
  round loop, rewritten as a resumable machine with a program counter. Each
  `attempt` either fails without a run (the candidate is not smaller, or the
  budget is spent), or stops the machine with a candidate. The Rust loop in
  `reduce_port` runs the property closure on that candidate and reports
  "same failure kind", "invalid" or "other" with the realized tape. The
  machine then decides whether to accept it. Budget checks run in the same
  order as in the Rust code, so `budget_exhausted` and the run count match
  too. Rust keeps only the closure call and the `Failure` value of the
  accepted run.

## What stays in Rust, and why

| File | Reason | Unblocked by |
|---|---|---|
| `src/exec.rs`, the loop in `shrink.rs` | Properties are Rust closures (`FnMut(&mut Source)`), and `catch_unwind` is how panics become failures. The lane prompt keeps the closure loop in Rust. | not planned (by design) |
| `src/source.rs` (`Source` buffers, `Probe`, `Arbitrary`) | `Source` stays a Rust handle (lane prompt). `realized_values()` returns a borrowed `&[u64]`, which the binding cannot lend out of a Vesper sequence (`SeqRef` has no slice view). `Probe` is per-comparison bookkeeping that the property calls in Rust. | part 2 can move `Probe`'s maps behind a handle |
| `src/arena.rs` | `ArbitraryIn` stays a Rust trait (lane prompt). `VirtualArena` wraps the Strata `Region` type, which crates outside the closure get as the Rust seed (the Strata port is non-default). | the Strata port becoming default |
| `src/refute.rs`, the closure loop in `fuzz.rs` | `refute` is a Rust adapter over generic `T: Debug` closures, and the fuzz loop runs Rust property closures (see `exec.rs`). | not planned (by design) |
| `src/sim.rs`, `src/explore.rs` (the `Machine`), the loop in `dpor.rs` | Fibers are caller types implementing `SimFiber<W>` over a caller-chosen world `W`, stepped on the SimPulse seed, and cache keys are caller `Ord` values. The port's search makes every scheduling choice; Rust runs the steps. There is no `dyn Trait` (F7b), and the SimPulse port is not what crates outside its closure link. | `dyn Trait` and the SimPulse port as the default |
| `seed/` | The frozen seed copy that the compiler links (seed-copy rule) | self-hosting Stage 2 |

The port has no unsafe Vesper code. The only unsafe Rust is in the generated
`port/binding.rs` (FFI calls). `src/lib.rs` keeps `forbid(unsafe_code)` in the
Rust build and `deny(unsafe_code)` in the port build, with an allow on the
binding module only (Grammar and Beacon do the same).

## Tests and parity

- **All 59 tests pass against both packages** (`cargo test -p vesper-crucible
  -p vesper-crucible-seed`, Linux x86_64, 2026-11-18), `tests/lean_model.rs`
  included. Its `lean/` lookup now walks up from the manifest, so the seed
  package (one level deeper) finds the same files.
- **Fixed-seed parity**, `tests/parity.rs` (5 tests, both packages). The
  expected values were recorded from the Rust build: SplitMix64 outputs,
  bounded and full-range draws, and `index`; a random `Source` (64 bounded
  draws, bytes, a schedule draw, a vector of `i64` ranges) and its realized
  tape; replay clamping and overrun; the text format, including Unicode
  whitespace and a 64-character `UnknownHeader`; keys above 2^64; the default
  `fuzz` campaign on the README's magic guard (65 executions, counterexample
  `[1, d, 1, f0, 1, ed, 1, fe, 0]`, 304 shrink runs, 2 accepted); and the
  **exact sequence of tapes the shrinker runs** (328 property calls, fixed by
  their fingerprint, the final tape and all four counters), plus a budget that
  runs out partway through a pass. The port produces the same tapes, the same
  counterexample and the same shrink sequence.
- **Leaks.** `tests/port_memory.rs` checks that warmed-up tapes, parses (good
  and bad) and shrinks give back every Vesper allocation.
- **Divergence checks.** `build.rs` lists one edit per ported area. Cargo
  applies them one at a time (`VESPER_CRUCIBLE_DIVERGENT=<index>`), and
  `port/check.sh` asserts the parity test that fails while another still
  passes. All four were caught locally:
  - rng: a different SplitMix64 increment fails
    `splitmix_outputs_match_the_rust_build`.
  - draws: unclamped replay fails
    `random_and_replayed_sources_match_the_rust_build`.
  - tape: hex digits a-f off by one letter fails
    `tape_text_and_keys_match_the_rust_build`.
  - shrink: accepting invalid runs fails
    `shrink_sequence_matches_the_rust_build`.

  Bazel builds each divergent object to check that it still compiles.
- **CI.** Linux runs through Bazel: `vesper_port_object` plus the crate's
  tests against the port, and the seed's `rust_test`s for every test file
  except `lean_model`, which runs against the seed in Cargo. The Linux
  Platforms job runs `scripts/test-port-harness.sh`, which sources
  `port/check.sh` (since #1011, macOS and Windows stop after the export ABI
  tier).
- `cargo +1.100.0 clippy --all-targets -- -D warnings` is clean for both
  packages and for the divergent feature. `cargo fmt --check` and
  `vesper fmt` are clean.

## Benchmarks

Measured with `crates/vesper-crucible/bench-abba.sh 11 20`: release builds,
11 ABBA epochs, 20 timed runs per measurement after 5 warm-up runs. The
machine was a 4-core x86_64 cloud container, Linux 6.18, on 2026-11-18, and
the compiler was the debug `vesper` built from this branch. The figures are
the medians of per-epoch ratios (port/Rust).

| Workload | Rust ns/run | Port ns/run | Port/Rust (median) | Range |
|---|---:|---:|---:|---|
| `draws` (4,096 bounded draws) | 37,694 | 89,167 | 2.39x | 1.99x-3.44x |
| `tape-text` (4,096-choice tape to text and back) | 344,984 | 914,149 | 2.68x | 1.74x-3.40x |
| `shrink` (64-element counterexample) | 1,125,666 | 3,947,946 | 3.55x | 2.56x-3.66x |
| `fuzz` (README magic guard, then shrink) | 215,923 | 695,044 | 3.10x | 2.45x-3.53x |

The port is 2.4x to 3.6x slower than Rust. The draw and generator cost is
mostly the call boundary: one FFI call per draw, which Rust inlines. The
shrinker copies the best tape into the candidate element by element, with a
checked `push` for each value, and returns every candidate as a fresh
sequence that the binding copies into a `Vec`.

The Rust build stays the default.

## Compiler gaps found

- Assigning a new sequence to a field of an `inout` handle
  (`s.best = copy_of(..)`) leaks the old sequence. The shrinker refills the
  buffer in place instead. This is the same struct-field sequence leak the
  Quarry parity lane reported.
- A long `if / else if` chain (19 arms) makes `vesper fmt` fail with
  `DepthLimit`. The machine is split into one function per pass.
- `state` is a reserved word, and cannot be used as a field name.
- `x >> n` with a variable `n` raises `Fail[RuntimeError]` (a shift of 64 or
  more), so the hex writer uses constant shifts.
- Each draw, generator step and key comparison is an FFI call. Rust inlines
  these, and the port cannot inline across the boundary.

## Part 2a: the fuzzer

The fuzz campaign is now a second Vesper step machine, `Fuzzer` (lib.ves
grows from 869 to 1,645 lines). `fuzz()` keeps its signature. Under
`cfg(vesper_port)` it calls `campaign_port`, which loops: `fuzzer_next` picks
the next input (a seed, a fresh random tape, a mutation of a corpus entry,
or a solver candidate), Rust runs the property on it, and `fuzzer_observe`
gets the verdict and the run's features. When the machine answers "keep",
Rust sends the realized tape, its draw kinds and its comparisons with
`fuzzer_keep`. The counterexample, replay and shrink steps after a failure
are the Rust code both builds share.

- **Features** are flattened to words that keep the Rust `BTreeSet` order:
  edges `id << 9 | bucket`, comparisons `1 << 62 | site << 9 | taken << 8 |
  bucket`, and closeness `2 << 62 | site` with the distance as a second
  word. The machine keeps the seen set as a sorted sequence.
- **The corpus** lives in flat sequences with offsets (values, kind tags,
  kind bounds, comparison records, solved flags), because a sequence of
  handles that own sequences cannot be returned or nested yet.
- **The comparison solver** builds the same integer and byte-replacement
  candidates, in the same order, from encoded comparison records
  (`0, kind, lhs, rhs` or `1, len_l, len_r, bytes..`).
- **Parity.** `fuzz_campaigns_match_the_rust_build` runs five campaigns and
  compares a report string recorded from the Rust build: executions, invalid
  runs, corpus size and an FNV fingerprint of every corpus tape, feature
  count, solver runs and hits, and the counterexample. The campaigns cover a
  layered guard that needs four solver steps, a budget that runs out, the
  magic guard, and a hashed target with 573 invalid runs and 63 corpus
  entries. The port matches all five exactly.
- **Divergence.** A fifth edit makes the zero mutation write 1, and
  `port/check.sh` asserts that `fuzz_campaigns_match_the_rust_build` fails
  while the tape test passes. All five edits were caught locally.
- `tests/port_memory.rs` runs a fuzz campaign in its leak loop, and every
  allocation comes back.
- All 59 tests and the 6 parity tests pass on both packages. Clippy 1.100
  (both packages and the divergent feature) and `cargo fmt --check` are
  clean.

### Benchmarks after part 2a

Same script, machine and settings as above, on `main` at `d5750af` (which
has the newer compiler work), 2026-11-18:

| Workload | Rust ns/run | Port ns/run | Port/Rust (median) | Range |
|---|---:|---:|---:|---|
| `draws` | 40,083 | 59,798 | 1.54x | 1.35x-2.16x |
| `tape-text` | 342,539 | 685,841 | 2.03x | 1.38x-2.15x |
| `shrink` | 1,106,754 | 2,751,637 | 2.47x | 1.33x-2.87x |
| `fuzz` (now the ported fuzzer) | 214,231 | 541,115 | 2.47x | 1.35x-3.39x |

The port is 1.5x to 2.5x slower than Rust, so the Rust build stays the
default. Even with the whole campaign loop ported, `fuzz` is faster than
in part 1. Most of its cost is still one call per draw and the copies of
each candidate and kept tape across the binding.

## Part 2b: the schedule search

The search loop of `dpor.rs` is now a third Vesper step machine, `Search`
(lib.ves grows to 2,894 lines). It covers exhaustive search and
persistent-set DPOR with sleep sets, the preemption bound (bounded POR), and
the state cache with its sleep-set subsumption and subtree summaries. The
Rust loop under `cfg(vesper_port)` does four things. It re-executes the
prefix that `search_prefix` hands it, and reports each new state's enabled
processes and `(process, transition)` pairs (`search_root`,
`search_child`). It interns the state key and passes its id to
`search_key`, which answers "cache hit" and adds the backtracking points.
Last, it executes the process that `search_choose` picks. `search_backtrack`
moves to the next unexplored alternative. The public API is unchanged.

- **Processes** are words that keep the Rust `Proc` order: `Task(i) = i`,
  `Io = 1 << 62 | task << 40 | seq << 8 | choice`, and
  `Timer = 2 << 62 | task << 32 | seq`. A fiber's next transition is the id
  of its footprint. Rust interns the footprints, and the port gets their
  sorted reads, writes, spawn flag and cancellation targets once. The port
  decides dependence and co-enabling itself.
- **Nodes** are stack arenas indexed by depth: enabled processes with
  backtrack, done and slept flags, a sorted order for backtrack iteration,
  next transitions, the sleep set on arrival, and process clocks.
  Clocks are immutable blocks that child states share. Popping a node
  truncates every arena to its marks. Subtree summaries (caching only) are
  a stack too: a popped node's block is merged into its parent's block,
  which sits directly below it.
- **Parity.** `explorations_match_the_rust_build` runs each explorer on 48
  random programs: exhaustive, DPOR, cached exhaustive and cached DPOR, each
  unbounded and with a preemption bound of one, on plain programs and on
  programs with spawns, cancellation and chosen I/O results. It also runs
  the RPC timeout race. Each run records executions, sleep-blocked runs,
  transitions, cache hits, bound-pruned choices, and a fingerprint of every
  outcome with its schedule, or the error (some runs hit the 3,000-execution
  limit). The results were recorded from the Rust build. The port matches
  all 50 lines exactly.
- **Divergence.** A sixth edit makes unbounded DPOR add every awake
  candidate instead of the first one, which is still sound but explores
  more. `explorations_match_the_rust_build` fails while the tape test
  passes. All six edits were caught locally.
- All 18 DPOR tests (`dpor`, `dpor_rest`) and the 6 exploration tests pass
  on the port. `tests/port_memory.rs` adds bounded and cached searches to
  its leak loop, and the port leaks nothing.

### Benchmarks after part 2b

The script, machine and settings are the same as above, and the run is from
2026-11-18. There are two new workloads. `dpor` is DPOR over 16 random
three-fiber programs from the exploration tests. `cached` is cached DPOR
with a preemption bound of one, over 16 random two-fiber programs with
spawns, cancellation and chosen I/O results. The machine was noisy in this
run: the first four workloads came out 5-15% above the part 2a run,
although their code did not change.

| Workload | Rust ns/run | Port ns/run | Port/Rust (median) | Range |
|---|---:|---:|---:|---|
| `draws` | 43,387 | 73,597 | 1.70x | 1.39x-2.47x |
| `tape-text` | 419,894 | 711,738 | 1.89x | 1.38x-2.13x |
| `shrink` | 1,223,027 | 3,454,667 | 2.53x | 1.84x-3.47x |
| `fuzz` | 225,882 | 599,575 | 2.81x | 1.61x-3.34x |
| `dpor` | 136,920 | 122,269 | 0.86x | 0.56x-1.07x |
| `cached` | 162,277 | 147,926 | 0.87x | 0.53x-1.35x |

The ported search is at parity: about 14% faster than Rust in the median.
The Rust search clones `BTreeMap` clocks and `BTreeSet` sets for every node.
The port keeps flat arenas, and children share immutable clock blocks.
Executing each step still costs the same Rust SimPulse machine in both
builds. The crate as a whole is not at parity (the generator, shrinker and
fuzzer are 1.7x to 2.8x slower), so the Rust build stays the default.

## Remaining in this lane

`arena.rs` waits until `VirtualArena` can sit on the Strata port. `sim.rs`
and the `explore.rs` machine stay Rust adapters, for the reasons in the
table above.
