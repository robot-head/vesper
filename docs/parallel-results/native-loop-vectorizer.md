# Native loop vectorizer for counted element loops

Claim #1474, gap 2b of SH6 lane 7's report (#1402, PR #1431). Gap 2a,
hoisting after inlining, landed in #1465. Owner: the SH3 lane 12 thread.

Cranelift does not vectorize loops, and LLVM vectorizes Rust's versions of
loops such as Bench's subset-sum step `dist[s + rank] += dist[s]`. Bounds
hoisting already gives each counted loop over a sequence an unchecked fast
copy. Its body is a straight line of element loads, integer arithmetic and
element stores. `src/native_vectorize.rs` gives that copy a two-lane
`i64x2` loop.

## What changed

- **`src/native_vectorize.rs`** (new): it runs on each function's CLIF on
  the optimized native path, right after `native_clif_opt`, for x86-64 and
  AArch64 targets. It never runs in the verified backend.
  `VESPER_VECTORIZE=off` turns it off, and `VESPER_VECTORIZE=debug` runs the
  Cranelift verifier after each rewrite and prints its errors.
- **The loops it takes**, all read from the CLIF:
  - The header has one parameter `j` and branches on `j <u B` or `j <=u B`,
    with `B` defined outside the loop.
  - The body has no parameters. It either jumps back with `j + 1`, goes on
    to a latch that does, or leaves at `j == B` (the inclusive form).
  - Every body instruction is one of these: an invariant, a pure integer
    operation of `j`, an `i64` load or store whose address moves 8 bytes per
    iteration (all the same way), or `iadd`, `isub`, `band`, `bor` or `bxor`
    of loaded values and invariants. Every load comes before every store.
- **What it emits:** the loop's entries go to a new block `pre(j0)` instead
  of the header.
  - `pre` computes the invariants once. It then checks that two iterations
    are left, and that each store of iteration `j0` is at least 8 bytes
    away from each load of iteration `j0 + 1`. Every access moves by the
    same 8 bytes, so that distance is the same for every pair, and one check
    covers the whole loop. If the check fails, `pre` goes to the scalar loop
    as it was.
  - `lanes(jv)` runs iterations `jv` and `jv + 1` as `i64x2` loads, lane
    arithmetic (with invariants splat) and `i64x2` stores. It repeats while
    two more iterations are left, then hands what remains to the scalar
    loop.
  - Lanes are in memory order. A descending loop keeps iteration `jv + 1`
    in lane 0, and its vectors start 8 bytes below iteration `jv`'s
    address.
- **Why two lanes are safe:** within a pair, both lanes' loads happen before
  both lanes' stores. That matches the scalar order whenever iteration `j`'s
  stores do not touch what iteration `j + 1` loads, and that is what `pre`
  checks.
- **`tests/native_vectorize.rs`** (new, 3 tests). Every program runs in the
  evaluator (optimized and not) and the JIT (optimized and not), and builds
  as x86-64 and AArch64 objects. Every backend must agree with the
  evaluator. The cases are:
  - The descending subset-sum step for ranks 0, 1, 2, 7, 99 and 150, with
    trip counts 1 to 201, and 20 ranks in a row. The CLIF must hold
    `i64x2`.
  - Ascending loops, exclusive and inclusive, with trip counts 0 to 5 and
    249, with invariant operands and every lane operation.
  - Recurrences, ascending and descending, where iteration `j` stores what
    iteration `j + 1` loads, so the check must send them to the scalar
    loop.
- **`tests/self_host_bounds_hoist_pass.rs`:** the flip gate's `build`
  workload now asserts that every timed build succeeds on both arms. This
  addresses the Codex review on #1518.

## Speed

These are standalone executables (`vesper build`) on the cloud host, with
the pass on and with `VESPER_VECTORIZE=off`. Each figure is the minimum of
14 runs in alternating order. The host was noisy, so medians moved by up to
30%.

| Workload | Off | On | Speedup |
| --- | --- | --- | --- |
| Descending subset-sum step, `for j in 0..=m` over 21,000 cells, 199 ranks, 160 reps | 0.140 s | 0.053 s | 2.6x |
| Ascending `d[j] = (d[j] + d[j + 50]) ^ k`, 20,000 cells, 200 passes, 160 reps | 0.258 s | 0.194 s | 1.33x |
| Both together | 0.377 s | 0.244 s | 1.55x |

My first version re-checked the overlap on every pair, and it was 2x slower
than scalar. Checking once in `pre`, and rotating the lane loop so that each
pair takes one compare and one branch, gave the figures above.

## What is left

- **Bench's own `add_rank` is not vectorized yet.** In
  `crates/vesper-bench/vesper/src/util.ves` it is a `loop` that counts
  `sum` down with `dec` and breaks at 0. Bounds hoisting does not take that
  loop as counted, so it has no unchecked copy to vectorize. Either change
  would let this pass take it:
  - bounds hoisting learns count-down loops, in `src/bounds_hoist.rs` and
    the Vesper port together; or
  - Bench's `add_rank` is written as `for j in 0..=max_sum` with
    `s = max_sum - j`, the form the tests use.
- Only `i64` elements and two lanes. `i32` and `u8` sequences, wider
  vectors, and multiplication (no `i64x2` multiply on x86-64 without
  AVX-512) are follow-ups.

## Validation

- `tests/native_vectorize.rs`: 3 of 3 pass.
- These test files pass:
  - `arrays`, `native_bounds_facts`, `native_clif_opt`, `native_seq_access`
    and `native_copy_within`;
  - `native_hoist_headers`, `native_hoist_calls`, `native_simd_bytes`,
    `native_seq_runtime` and `native_inline`;
  - `native_batch`, `native_short_scans`, `native_aggregates`,
    `oracle42_corpus` and `self_host_bounds_hoist_jit`.
- `cargo +1.100.0 clippy` and `cargo fmt` are clean.
- Stage 1 guard: no guarded path is touched, and the verified pipeline does
  not run the pass.
