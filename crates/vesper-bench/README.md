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

## Paired Hodges–Lehmann estimate

`hodges_lehmann_paired` accepts up to 32 caller-provided signed integer paired
differences. It returns the median Walsh average scaled by four, plus a
two-sided signed-rank interval with bounds scaled by two and exact nominal
coverage when a finite interval of at least 95% is available. Smaller samples
can return an estimate without an interval. Zeros, tied absolute differences,
and tied Walsh averages are rejected conservatively.

The interval's interpretation requires independent, identically distributed
continuous differences symmetric about their population location. The API
does not establish those assumptions or convert benchmark measurements into
differences. It is separate from the ratio-based regression gate below.

`gate_paired_difference` compares those unrounded interval bounds against a
maximum candidate-minus-baseline difference in the same units. It passes only
when the upper bound is at or below the threshold, reports a regression only
when the lower bound is above it, and otherwise returns an inconclusive result.
It retains the estimate and interval and uses the same assumptions and input
restrictions; it does not establish measurement provenance or operate the
existing ratio gate.

`mann_whitney_ranks(first, second)` separately computes exact Mann–Whitney U
rank counts for two bounded `u64` samples. Counts are doubled to represent tied
pairs without rounding. It returns no p-value, confidence interval or gate
decision. Independent-sample inference requires appropriate independence and
distribution assumptions; ABBA epoch measurements are paired and do not
automatically satisfy those assumptions.

`mann_whitney_exact(first, second)` computes the exact untied permutation
probability for two nonempty samples of at most 32 observations each. It rejects
repeated values anywhere in the combined samples. The returned fraction uses
twice the smaller inclusive tail, capped at one; it is not rounded. Its null
interpretation requires exchangeable observations, and it is not a paired ABBA
test or a confidence interval. It does not participate in the regression gate.

`mann_whitney_exact_with_ties(first, second)` also supports repeated values,
using pooled midranks and counting all physical label assignments. It calculates
both inclusive tails separately before doubling the smaller one, because ties
can produce an asymmetric permutation distribution. It uses the same sample
limits and exchangeability assumption and likewise does not operate the gate.

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

## Exact paired signed-rank probability

`signed_rank_exact` accepts up to 32 caller-provided paired integer differences.
It discards zeros using the Wilcoxon convention, assigns exact doubled midranks
to tied magnitudes, and reports a two-sided sign-randomization probability as
an exact fraction. It also reports discarded zeros and tied magnitude groups.
An empty input, more than 32 pairs, or an all-zero input is an explicit error.

Its null interpretation requires independent, equiprobable signs conditional
on the observed nonzero magnitudes and zero pattern. The API does not establish
these assumptions or measurement provenance; it does not compute a confidence
interval or operate the existing regression gate.

`signed_rank_exact_paired_samples` accepts the existing `PairedSample` totals
directly and ranks candidate-minus-baseline differences per operation as exact
rational values. Equivalent fractions tie without rounding. It uses the same
zero convention and statistical assumptions; callers must establish common
units and provenance.

`vesper bench FILE --signed-rank` optionally includes this exact paired
signed-rank report for the selected measured metric. It supports up to 32
measured epochs, or up to 32 aggregate randomized-layout samples; it excludes
deterministic mode. All-zero differences report unavailable. The report does
not add a gate and explicitly states the conditional-sign assumption.

## Tied paired signed-rank confidence sets

`signed_rank_confidence_set` inverts the exact two-sided 5% Wilcoxon
signed-rank test for up to 16 caller-provided integer paired differences. It
visits every distinct Walsh breakpoint and the intervals between them, discards
centered zeros, and uses doubled midranks for tied absolute differences. Its
ordered segments can be disconnected, open, closed, singleton, or unbounded;
finite endpoints are stored as twice the location without rounding. A candidate
with all centered differences zero is not rejected.

Coverage of at least 95% requires independent, equiprobable signs conditional
on the centered nonzero magnitudes and zero pattern at the true location. The
API does not establish those assumptions or measurement provenance, and it does
not alter the existing regression gate.

`gate_signed_rank_confidence` compares the complete confidence set against a
maximum candidate-minus-baseline difference in the same units. It passes only
if every retained location is at or below the threshold and reports a regression
only if every retained location is strictly above it. Empty or nonseparating
sets are inconclusive; open, closed, and unbounded endpoints retain their exact
meaning. The gate preserves the set and inherits its stated assumptions.

## Tied Walsh point estimates

`walsh_pseudomedian` returns the Hodges–Lehmann point estimate for up to 32
caller-provided integer paired differences, retaining zeros and tied Walsh
averages with their full multiplicity. The result is scaled by four so even
medians and half-unit Walsh averages are exact. It uses bounded stack storage
and returns an explicit error for empty or oversized input. It does not return a
confidence interval or regression decision and does not establish measurement
provenance.
