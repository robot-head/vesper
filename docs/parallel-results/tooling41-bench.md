# §41 callback benchmark engine (#68)

Read [issue #68](https://github.com/robot-head/vesper/issues/68),
`docs/rfcs/tooling41-contracts.md`, full specification §41 and
`docs/rfcs/continuation37-review.md`. Implementation is confined to new
`crates/vesper-bench/**`, `lean/MetronomeArithmetic.lean` and this report, on
`tooling41/bench` based on `a1a8d938dd8103803219a05c8364cb7a198be8d9`.
Root CLI/source/JIT integration, other production paths, source semantics,
checked arithmetic, interface v2, native ABI v2 and `k_i ⊑ m_i` are untouched.

## Executing callback boundary and integration API

The dependency-free safe Rust library exposes:

```rust
pub trait BenchCase {
    type Input;
    type Output;
    type Error;
    fn setup(&mut self, invocation: Invocation) -> Result<Self::Input, Self::Error>;
    fn run(&mut self, input: Self::Input) -> Result<Self::Output, Self::Error>;
    fn teardown(&mut self, output: Self::Output) -> Result<(), Self::Error>;
    fn discard(&mut self, input: Self::Input);
}
pub fn run_abba<A: BenchCase, B: BenchCase>(
    config: RunConfig, baseline: &mut A, candidate: &mut B,
) -> Result<RunReport, RunError<A::Error, B::Error>>;
```

`teardown` defaults to dropping its output and returning Ok; `discard` defaults
to dropping its unrun input. Each real epoch runs four windows in **A/B/B/A**
order. Input setup and both vector-capacity reservations precede the start
reading. The timed loop consumes owned inputs and retains outputs; end reading
precedes output teardown, unrun-input disposal and vector deallocation. Returned
setup/body/clock/teardown errors do not produce a passing partial report. On a
returned body error the clock stops before all available cleanup, and cleanup
continues after individual teardown errors; first callback error plus later
cleanup-error count are reported. A consumed failed input remains the callback's
responsibility. This Rust trait is not Vesper linear-exhaustion enforcement.

The sole production timing provider is **`std::time::Instant`**, reporting
**nanoseconds**. No clock resolution, accuracy or hardware-cycle interpretation
is inferred from those units. Timed totals include loop, input movement,
`std::hint::black_box`, result-vector writes and callback/adapter overhead.
There is no calibration subtraction, custom compiler barrier or zero-spill
promise. Untimed setup can still change caches and subsequent performance.

`RunConfig` fields are `epochs`, `warmup_epochs`, `operations_per_window`,
`max_total_calls`, `max_elapsed_nanos`, and `layout_seed`. Defaults are 12 measured
epochs, one warmup, 128 operations/window, 1,000,000 total calls, 10 seconds and
seed zero. Hard limits: measured epochs 1..=64, warmups 0..=64, operations/window
1..=65,536, total body calls <=10,000,000, elapsed budget 1ns..=600s. Checked total
call arithmetic includes warmups. Elapsed budget is checked between/after
windows and before returning; it cannot preempt arbitrary callbacks. Callbacks
must terminate; panic/abort/OOM handling and byte bounds for generic
input/output/callback-owned allocations are outside this slice.

`RunReport::{provider,unit,epochs,pairs,total_body_calls,elapsed_run_nanos}` are
read-only getters. An `EpochSample` exposes four `WindowSample`s (arm, elapsed
nanoseconds, operation count), its `PairedSample` and generated offset metadata.
Each pair sums the two baseline durations and two candidate durations with
checked u64 arithmetic; equal per-arm operations make their ratio a valid
within-epoch per-operation ratio. Warmups are excluded from statistical samples.
Zero measured windows/totals, invalid configs and arithmetic overflow are errors.

The runner applies **no regression threshold**. The coordinator can implement
its source case using `Input=()`, `Output=runtime::Value`, `Error=String`, compiling
`NativeBatch` before calling this API. Actual entry lookup/JIT conversion and
harness overhead remain timed and must be described as such. No dependency on
the root facade or NativeBatch is introduced; no competing JIT helper exists.

## Exact paired statistics and honest gate scope

`PairedSample::new(baseline_total, candidate_total, operations_per_arm)` requires
positive u64 values. `baseline_total`, `candidate_total`, `operations_per_arm`
and `regression_bp_bounds` expose its evidence. Basis-point bounds are directed
floor/ceiling i128 values, used only for presentation. Ratio sorting multiplies
u64 values in u128; exact threshold comparisons use checked u128 arithmetic.
Tiny nonzero regressions are not rounded away. Full u64 extrema and i64 threshold
extrema are supported; thresholds below -9999bp are invalid.

`analyze_pairs(samples, max_regression_bp)` implements a **two-sided exact
binomial/order-statistic >=95% interval for a population median of epoch ratios**,
not the scaffold's claimed Mann–Whitney/Hodges–Lehmann implementation. For n
independent identically distributed ratios, choose the largest rank k with
`200 * sum(j=0..k-1, choose(n,j)) <= 5 * 2^n`; return sorted order statistics
`[k, n-k+1]`. The interval reports exact binomial covered/total outcomes in u128.
Known cases include n=6 rank1 coverage62/64; n=10 rank2 coverage1002/1024;
n=20 rank6 coverage1005176/1048576; n=64 rank24 coverage
17842322501232739958/18446744073709551616. All combinatorial work is bounded by
64 samples and uses checked arithmetic.

The interpretation assumes independent identically distributed epoch ratios
and a fixed population median. Continuous distributions attain the binomial
bound; ties/discrete data make it conservative and are retained. Stateful or
cache-correlated repetitions, drift, order effects, optional stopping and
cherry-picking can invalidate inference. The runner does not certify those
assumptions. Coverage is per comparison, with no multiplicity or optional-stopping
correction. ABBA may mitigate some drift/order effects, but additive common
drift does **not** cancel ratio denominators. Only a difference under an actual
common additive term has the simple cancellation identity.

A finite interval requires at least six epochs. Valid smaller evidence returns
`Inconclusive`; empty evidence is an error. The upper endpoint <= threshold
means `Pass`; lower endpoint > threshold means `Regression`; crossing/overlap
means `Inconclusive`. Analysis also reports lower/upper central sample order
statistics, not a fabricated zero median. A measurement-only CLI may report
noisy/inconclusive data without treating it as a regression failure; applying a
threshold is an explicit separate action.

All PMU metrics remain structured `NoPmuProvider` evidence: cycles,
instructions, L1/LLC misses, branch misses, SIMD retired operations and TMA slots.
Allocation and static vectorization are likewise explicitly unavailable.
`evaluate_evidence_requirement` returns `Inconclusive`; it accepts a requirement,
not a caller-supplied proof boolean. There is no Linux perf/RDPMC, macOS KPC or
AArch64 PMCCNTR provider in this change. Wall time/static estimates cannot pass
hardware gates; no rights/counter-metadata read is claimed.

`modular_counter_delta` validates an explicit width 1..=64 and canonical raw
values, computes a residue in wider checked arithmetic, and requires a true
increment <2^width with no reset/reconfiguration for exact interpretation.
Multiple wraps are undetectable. It validates no PMU metadata/multiplexing/scaling.
`layout_offset_bytes` is only a deterministic modular SplitMix-style generator
of 64-byte offsets in [0,4096); it changes no code mapping, linker padding,
entry address or stack and is not actual Stabilizer randomization.

## Lean arithmetic models

Pinned Lean **4.24.0** kernel-checks four handwritten model lemmas. They do not
prove compiler extraction, Rust equivalence, PMU behavior, confidence assumptions
or statistical inference. No sorry, new axioms, native_decide or bv_decide:

| Theorem in `Vesper.MetronomeArithmetic` | Actual axioms |
| --- | --- |
| `width64_modular_delta_exact` | `propext`, `Quot.sound` |
| `paired_additive_drift_difference` | `propext`, `Quot.sound` |
| `common_drift_ratio_counterexample` | none |
| `scaled_u64_difference_fits_i128` | `propext`, `Classical.choice`, `Quot.sound` |

The modular theorem is a selected width-64 arithmetic model with explicit
start/increment bounds, not a universal width-48 hardware fact. The ratio
counterexample explicitly demonstrates common-drift non-cancellation.

## Validation and handoff

Passed on this Linux host using private `/tmp/vesper-bench-target`, one build
job, zero dev/test debug info and disabled incremental caches:

```sh
CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=/tmp/vesper-bench-target CARGO_INCREMENTAL=0 \
  CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
  cargo test --manifest-path crates/vesper-bench/Cargo.toml --offline
CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=/tmp/vesper-bench-target CARGO_INCREMENTAL=0 \
  CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
  cargo clippy --manifest-path crates/vesper-bench/Cargo.toml --all-targets --offline -- -D warnings
cargo fmt --manifest-path crates/vesper-bench/Cargo.toml -- --check
CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR=/tmp/vesper-bench-target CARGO_INCREMENTAL=0 \
  CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
  cargo run --manifest-path crates/vesper-bench/Cargo.toml --example paired_callbacks --offline
lean lean/MetronomeArithmetic.lean
```

**16 tests pass**: five internal callback/clock boundary and cleanup tests, four
actual public-run/config/error/order tests, seven independent statistics/counter/
missing-evidence cases. Actual monotonic runs verify callback counts and A/B/B/A
sequence. A private test-only clock observes setup/teardown/discard exclusion;
it is never a public measurement provider. Independent combinatorial vectors,
negative/nonpositive/empty/insufficient evidence, exact fractional/tiny/max-u64
ratios, all explicit counter widths/wrap/range boundaries, deterministic offsets,
and unavailable-gate behavior are exercised.

`paired_callbacks` runs actual kernels and validates counts/positive measurements,
then reports optional confidence data and unavailable hardware. An observed run
reported provider `std::time::Instant`, nanoseconds, nine measured epochs,
10,240 total body calls, order ranks2..8, coverage492/512. Wall-time performance
results vary; the example does not require any particular decision or sensitivity.
No sensitivity/latency benchmark, cross-platform execution, external collector,
thread isolation, zero allocation, causal profiling or hardware result is claimed.

The private target is approximately 6 MiB. No external dependencies, leaf lock
or generated artifacts are included. Coordinator registration: remove the
standalone `[workspace]`, add `crates/vesper-bench` to members/default-members and
root path dependencies as needed, refresh the root lock entry, wire owned
`src/bench_commands.rs` against this callback API and add the optional Lean file
to pinned validation. Stable example: `cargo run -p vesper-bench --example paired_callbacks`.

## §41 scaffold review resolution

The reference scaffold clamps invalid/nonpositive baselines, allows signed
subtraction overflow, saturates scaled differences and returns a passing zero
for missing observations. It also labels a sorted median as confidence inference
and accepts caller booleans as allocation/vectorization evidence. This library
replaces those claims with positive checked measurements, exact rational
comparisons, explicit finite-sample intervals, inconclusive missing evidence
and unsupported hardware/static gates. It does not implement the broader CAS
baseline loader, actual code-layout randomization, 12-cycle PMU access,
0.3%-under-two-seconds sensitivity, static/ASM gates or Pulse causal profiling.
