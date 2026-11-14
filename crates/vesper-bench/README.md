# Bounded paired callback measurements

`run_abba(config, &mut baseline, &mut candidate)` executes real `BenchCase`
callbacks using `std::time::Instant`. A measured epoch contains four windows in
fixed **A/B/B/A** order. Every window prepares its owned inputs and output-vector
capacity before the start reading, then consumes one input per body call. Output
teardown and disposal of unused inputs follow the end reading, including on
returned callback/clock errors. The first callback error is returned after all
available cleanup; later cleanup errors are counted. Generic callbacks remain
responsible for their consumed inputs, termination and panics.

Reported elapsed values are **nanoseconds**, not CPU cycles. Units do not imply
clock resolution or accuracy. Per-window totals include loop, input moves,
callback dispatch/adapter work, `std::hint::black_box` and output storage overhead.
No calibration is subtracted. Untimed setup can still affect caches and subsequent
execution; exclusion from a clock interval does not establish zero distortion.
The harness allocates host vectors and does not establish zero allocation,
spill-free barriers, thread pinning or scheduler isolation.

`RunConfig` defaults to 12 measured epochs, one warmup epoch and 128 operations
per window. Measured and warmup epochs have hard caps of 64 each; windows contain
1..=65,536 body calls. Total warmup/measured calls are checked against an explicit
budget, capped at 10,000,000. A total elapsed budget (1ns..=600s, default 10s) is
checked between/after windows and before returning. It cannot interrupt a
nonterminating callback or preempt long setup/body/cleanup. Generic input/output
sizes and callback-owned allocations are not bounded in bytes by this API.
Adapter vector reserve failures return an error; process OOM/panics are outside
the recoverable-error contract.

The runner is measurement-only and never applies a regression threshold.
`RunReport` exposes its provider/units, four windows and one `PairedSample` per
measured epoch, body-call count including warmups, and whole-run elapsed time.
`PairedSample` uses the sums of the two A durations and two B durations, with
equal operation counts per arm. Zero totals/operations fail. Warmups are not
included in statistical evidence.

## Optional population-median confidence gate

`analyze_pairs(samples, max_regression_bp)` targets the population median of the
paired ratio **B_total/A_total**. It implements an exact binomial/order-statistic
interval, not Mann–Whitney, Hodges–Lehmann, a mean-speedup estimate or a causal
experiment. For n epochs, choose the largest 1-based rank k satisfying

```text
200 * sum(j=0..k-1, choose(n,j)) <= 5 * 2^n
interval = [sorted_ratio[k], sorted_ratio[n-k+1]]
confidence >= 1 - 2 * sum(j=0..k-1, choose(n,j)) / 2^n >= 95%
```

Coverage assumes independent, identically distributed epoch ratios and a fixed
population median. For continuous distributions the binomial bound is exact;
ties/discrete observations are retained and make coverage conservative. Repeated
stateful/cache-correlated callbacks, drift, order effects, optional stopping and
cherry-picking can invalidate those assumptions. Coverage is per comparison;
there is no multiple-comparison or optional-stopping correction. ABBA can mitigate some order or
linear drift effects, but the runner does not prove independence or automatic
noise cancellation. Even exactly common additive drift changes ratio
**denominators**; only an unnormalized difference cancels that common term.

A finite interval needs at least six epochs. Valid smaller samples are
`Inconclusive`; empty evidence is an error. The upper interval endpoint at/below
the allowed threshold yields `Pass`; the lower endpoint strictly above it yields
`Regression`; overlap yields `Inconclusive`. The threshold is basis points of
relative elapsed regression, where 100bp = +1%. Values below -9999bp are rejected.
Exact u128 cross-products compare ratios and thresholds without truncation.
Displayed basis points use explicit floor/ceiling bounds in i128. All sample,
binomial and count arithmetic is checked or widened to a provably sufficient
integer width. Statistical work/data is bounded by 64 epochs.

## Unavailable evidence and helpers

There is no PMU backend in this slice. Hardware cycles, instructions, L1/LLC
misses, branch misses, SIMD retired operations and TMA slots remain structured
`NoPmuProvider` evidence. Allocation and compiler-vectorization requirements
likewise remain unavailable. `evaluate_evidence_requirement` always returns
`Inconclusive`; no caller boolean or wall-clock value can make these gates pass.
No Linux perf permission/counter metadata, RDPMC, macOS KPC or AArch64 PMCCNTR
access is implemented or claimed.

`modular_counter_delta(start,end,width)` is only checked arithmetic for an
explicit 1..=64-bit model. Raw values must fit the width. Its residue equals a
true continuously-running counter increment only if that increment is below
2^width and the counter was not reset/reconfigured. Endpoints cannot distinguish
zero from one or more full-modulus wraps. Counter metadata, multiplexing, scaling
and hardware permissions require a future real provider; 48 bits is not universal.

`layout_offset_bytes(seed,epoch)` is a deterministic modular offset generator of
64-byte values in [0,4096). It changes no code padding, executable mapping,
stack, cache, linker or entry point. It is not real Stabilizer randomization.

Run `cargo run --manifest-path crates/vesper-bench/Cargo.toml --example paired_callbacks`.
The example measures actual host kernels, validates body counts and positive
elapsed data, reports its optional median interval/gate and unavailable hardware
metrics. Its checks intentionally accept any statistically valid noisy decision.

## Layout randomization (`vesper_bench::layout`)

`layout_plan(seed, run)` draws a heap offset (16-byte multiple below 4 KiB), a
stack padding (64-byte frames below 4 KiB) and an environment size (below
4 KiB) per run. `run_randomized_layouts` runs one ABBA comparison per layout
under a live `HeapShim` block and `with_stack_padding` frames, reduces each run
to one aggregate `PairedSample` and calls `layout_survival`, which reports
`CandidateSlower`, `CandidateFaster` (the exact median interval excludes ratio
1), `DoesNotSurvive` or `InsufficientLayouts` (<6). `OffsetBuffer` places input
bytes at a chosen page offset. Environment size needs a fresh process; `vesper
bench --randomize-layout N` spawns one child per layout with
`VESPER_BENCH_LAYOUT_PAD` set. Machine code, function entries and globals are
not relocated (unlike Stabilizer), and layouts are treated as independent draws.

## Causal profiling (`vesper_bench::causal`)

A Coz-style virtual-speedup profiler for instrumented host threads.
`CausalProfiler::new(regions, progress_points)` hands each thread a
`CausalThread`; `region(id, f)` times a region and, when it is the selected
region, charges `elapsed * speedup%` of delay to every other thread, which pays
it at its next region entry, progress point or `delay_point()`. `profile` runs
baseline experiments and one experiment per (region, speedup), subtracts the
inserted delay from elapsed time and reports the predicted end-to-end speedup
in basis points per region. Unlike Coz, regions are explicit calls rather than
sampled lines, delay accrues when a region ends, and there is no credit for
delay accrued while a thread was blocked.
