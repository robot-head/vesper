# Rust exit SH6 lane 7: Ignis, Grammar and Bench (#1402), part 1

Claim #1402. Prompt: `docs/agent-prompts/self-host-sh6/07-ignis-grammar-bench.md`
(plan PR #1415). Base: `main` at `ab965f2`. Continues port parity lane 6
(#954, Ignis), port P1 lane 9 (#906, Grammar) and port P3a lane 5 (#1040,
Bench).

**Result.** No crate passes the flip bar yet, so nothing is flipped and no
Rust is deleted. Bench's estimators are now within the bar at n = 16 and
n = 32 and on its edge at n = 8; its statistics series (2.47x), Grammar's
Protobuf decode (about 2.8x) and Ignis's build and lower (2.1-2.8x) are
held above it by the compiler gaps listed below, each with a repro. This
part changes only `crates/vesper-bench/vesper/src/*.ves`.

## Measurements

Cloud x86-64 container (4 vCPU), release profile for both arms, ports
compiled by this tree's `vesper`. Ratio = Vesper time / Rust time, median
and the exact 95% interval from `vesper_bench::analyze_pairs` (12 epochs for
Bench, 10 for Ignis). Grammar's driver prints the median only.

### On `main` (`ab965f2`), before this PR

| Crate | Workload | Ratio | 95% interval |
|---|---|--:|---|
| Ignis | build, 64 / 256 passes | 2.15x / 2.06x | 1.80-2.42 / 1.64-2.36 |
| Ignis | plan, 64 / 256 passes | 1.24x / 0.58x | 1.03-1.31 / 0.53-0.63 |
| Ignis | lower, 64 / 256 passes | 2.82x / 2.35x | 2.15-3.17 / 1.80-2.47 |
| Grammar | Protobuf decode (909 bytes) | 2.5-2.8x over 3 runs | n/a |
| Grammar | DER decode (1,764 bytes) | 1.55-1.59x over 2 runs | n/a |
| Bench | estimators n = 8 / 16 / 32 | 1.97x / 1.73x / 1.72x | 1.96-1.99 / 1.64-1.80 / 1.56-1.83 |
| Bench | statistics, 100,000 samples | 2.98x | 2.83-3.07 |

The earlier reports had Ignis at 3.7-4.2x, Grammar at 6.3x / 4.7x and Bench
at 2.2-3.2x. The difference is the compiler work merged since (inliner,
bounds hoisting, aggregate registers, wide integers, Place IR).

### Bench after this PR

| Workload | Before | After | 95% interval after | Bar (1.5x) |
|---|--:|--:|---|---|
| estimators n = 8 (2,000 batches) | 1.97x | 1.53x | 1.47-1.55 | just above |
| estimators n = 16 (2,000 batches) | 1.73x | 1.16x | 1.08-1.20 | passes |
| estimators n = 32 (100 batches) | 1.72x | 0.91x | 0.85-0.95 | passes |
| statistics, 100,000 samples | 2.98x | 2.47x | 2.45-2.49 | fails |

All of Bench's tests pass on both builds (`port/check.sh`: the seed, the
port and the 14 divergent builds, each caught by its own test).

## What changed (Bench port)

1. **The signed-rank inversion stops its subset-sum at the tail limit**
   (`ranks.ves`, `accepts`). Only `dist[0..=tail_limit]` feeds the tail,
   and no cell depends on a cell above it, so the ranks are tallied first
   and the DP runs only below `min(positive, total - positive)`. This is
   an algorithmic change the Rust build does not have (the old report
   named it as a follow-up), so the n = 16 and n = 32 ratios are better
   than compiler parity, as Ignis's plan 256 is.
2. **Native 128-bit arithmetic** where the port emulated it with `W` word
   pairs: centered magnitudes (`CObs.mag: u128`), the ratio comparisons of
   the statistics sort, and the basis-point bounds (one `u128` division
   instead of the shift-and-subtract `w_udivrem`). `W` stays at the export
   boundary.
3. **The ratio sort merges sample positions**, not `Sample` records, with
   the keys in two word sequences (`stats.ves`, `sorted_by_ratio`). A
   microbenchmark of 64 samples went from 67k to 32k instructions per sort
   (Rust's `sort_by`: 9.7k).

Tried and dropped: a stack `[u64; 1057]` table for the DP (the guarded
array accesses still emit their own checks, and zeroing 1,057 cells made
n = 8 slower), the counted `for j in 0..=max` form of `add_rank` (hoisted
in isolation, slower inside `accepts`), a `u128` ratio key per sample
(39k instructions per sort), and a LEB128 fast path in Grammar (no change
outside the noise).

## Flip-gate verdicts

| Crate | Differential | Speed | Verdict |
|---|---|---|---|
| vesper-bench | 0 differences (every test, both builds) | statistics 2.47x [2.45, 2.49]; estimators n = 8 1.53x [1.47, 1.55] | not flipped |
| vesper-grammar | 0 differences (unchanged since #980) | Protobuf 2.5-2.8x, DER 1.55-1.59x | not flipped |
| vesper-ignis | 0 differences (unchanged since #981) | build 2.1x, lower 2.4-2.8x | not flipped |

Rust lines deleted: 0. The census row is not updated in this PR because
`scripts/rust-census.py` arrives with the plan PR (#1415), which has not
merged; part 2 updates it.

## Compiler gaps (with repros)

1. **A proven array index still gets a runtime check.** The checker proves
   `dist[t & 2047]` in bounds for `[u64; 2048]`, but native code emits
   `and $0x7ff; cmp $0x800; jae ud2` for every access
   (`src/native_arrays.rs` `array_store` always branches, and so does the
   tuple read). Repro:

   ```text
   fn run(rank: u64, top: u64) -> u64 {
       let mut dist: [u64; 2048] = [0; 2048];
       dist[0] = 1;
       let mut sum = wrapping_add(top, 1);
       loop {
           if sum == 0 { break; }
           sum = wrapping_sub(sum, 1);
           let t = wrapping_add(sum, rank);
           dist[t & 2047] = wrapping_add(dist[t & 2047], dist[sum & 2047]);
       }
       dist[top & 2047]
   }
   ```

   Where the checker recorded no possible failure, the check could be
   dropped for masks and constants (the cases Cranelift cannot see).
2. **`try_get`/`try_set` loops are not vectorized, and the bounds hoist
   does not fire inside a larger function.** Bench's hot loop
   `dist[s + rank] += dist[s]` for `s` from `max` down to 0 is 20-25
   instructions per element in the port (two checked `try_get`s, a
   `try_set` that selects a dummy slot with `cmov`, and the sequence header
   reloaded from the stack each iteration). LLVM vectorizes Rust's version
   (writes are above every later read). The counted form
   `for j in 0..=max { let s = max - j; ... }` is hoisted to 18 unchecked
   instructions in a standalone function but not after `add_rank` is
   inlined into `accepts`.
3. **Merge steps cost about 60 instructions** in a stable merge of word
   positions (Rust's small-sort about 25 per comparison). The `bool` of each
   comparison is materialized (`setb; movzbq; test; jne`), loop bounds are
   reloaded from the stack, and each `try_get` keeps its check. Repro: the
   64-element merge of positions with `u128` cross products in
   `stats.ves` `sorted_by_ratio`, 32k instructions per sort against 9.7k
   for `sort_by`. This keeps the statistics series at 2.47x.
4. **A `Slice` view parameter's header is reloaded on every element read.**
   In Grammar's `leb_decode` (`framing.ves`) each byte read loads the
   view pointer, then base, offset and length (five loads), and every
   `fail(...)` site turns into a materialized flag test. The varint loop is
   about 45 instructions per byte; it is a third of the Protobuf decode.
5. **`/` and `%` of the same `u128` operands are two divisions**, and the
   tuple result goes through stack slots (Bench `udiv_u128`). Signed
   `i128` division needs both `Fail[RuntimeError]` and
   `Fail[ArithOverflow]` in the row, which is right but undocumented next
   to the unsigned form.
6. **Straight-line validation code** (Ignis `validate_use`,
   `graph_add_pass`, `vesper_ignis_lower_plan`) is still the cost of Ignis
   build and lower. The frames are much smaller than in #981 (656 bytes for
   `validate_use`, was 4,432), but each early-return enum is still built in
   memory and copied out.

## What is left

- Bench: the statistics series needs gaps 1-3, which would likely also
  bring `accepts` at n = 8 under 1.5x (not measured).
- Grammar: Protobuf decode needs gap 4; DER is near the bar (1.55x).
- Ignis: build and lower need gap 6 and the Rust glue's per-use
  conversions (`UseDesc` puts, pass-name formatting).
- The flips themselves. Bench and Grammar are linked by `vesper-tools`
  and `vesper-hybrid` (through their seeds) and every other port's ABBA
  harness dev-depends on `vesper-bench-seed`, so their flips use the seed
  pattern (#1102); Ignis is linked only by `vesper-ignis-vulkan` and can be
  deleted outright once it passes.
