# Rust exit SH6 lane 1: the flip gate, scoreboards and census

> Rust-exit wave SH6, lane 1 (#1396). Part 1 written 2026-11-21 against
> `main` at `ab965f2a`. The [manuscript](../manuscript/README.md) is the
> sole specification; this lane changes no language or library behavior.

## Part 1: the flip gate

`scripts/ci/flip_gate.py <crate-or-module>` checks the owner's bar
(2026-11-21): every test passes, the differential finds 0 differences, and
every ABBA workload's 95% interval has its upper end at or below 1.5x of
Rust's time. It prints a one-line verdict and a Markdown row for the
scoreboard, and exits 0 (PASS), 1 (FAIL), 2 (INCONCLUSIVE) or 3
(configuration error).

- **Configuration** is a TOML file per crate (`crates/<crate>/flip-gate.toml`,
  in the lane's own path) or per compiler module
  (`scripts/ci/flip-gate/<module>.toml`): build commands, test commands,
  the differential, ABBA workloads and the Rust paths the flip deletes. The
  format is in the script's header.
- **ABBA** comes either from the gate itself (it alternates a Rust command
  and a Vesper command R V V R for 30 epochs, after a warm-up of each),
  which fits every existing `abba once <workload> <arm> <batches>` example
  and the Rust and hybrid compilers, or from a harness that prints
  `flip-gate pairs <workload> <rust_ns>:<vesper_ns> ...`.
- **The interval** is vesper-bench's exact binomial median interval
  (`analyze_pairs` in `crates/vesper-bench/src/statistics.rs`), recomputed on
  exact ratios (`fractions.Fraction`), so the gate and the Rust and Vesper
  benchmark crates agree on ranks (rank 10 of 30 epochs).
- **The test** is `scripts/ci/tests/test_platform_flip_gate.py` (15 tests),
  which the Platforms workflow's plan job already runs with every
  `test_platform*.py`. The prompt named `tests/flip_gate*.rs`; a Python test
  next to the other CI-script tests adds no Rust, which is this program's
  point, and needs no Bazel target.

### Worked example: vesper-span

`scripts/ci/flip-gate/vesper-span.toml`. vesper-span is port-backed already
(SH0 lane 6, #1128); the Rust left is `src/lib.rs`, which the compiler links
as the seed package. Linux x86-64, 4 cores, release examples, compiler from
this commit (`VESPER_PORT_COMPILER`), 4 min 50 s for the whole run:

~~~text
flip-gate vesper-span: FAIL (bar: 0 differences, every test, 95% upper <= 1.50x) | tests 1/1 pass | differential 0 differences | ABBA vesper/rust lookup 1.35x [1.33x, 1.44x] n=30; index 0.32x [0.30x, 0.33x] n=30; edits 3.08x [2.92x, 3.15x] n=30
~~~

| Flip | Lane | Differential | Speed (ABBA) | Rust deleted | PR |
|---|---|---|---|--:|---|
| `vesper-span` (FAIL) | SH0-6 | 0 differences; tests 1/1 pass | lookup 1.35x [1.33x, 1.44x]; index 0.32x [0.30x, 0.33x]; edits 3.08x [2.92x, 3.15x] (n=30) | (755 when it flips) | |

The `edits` workload (64 `SourceEdits` applied to a 64 KiB source) is the
only one over the bar, as the SH0 report measured in instructions (3.8x Ir):
`push_str` has no memcpy and the binding re-validates returned strings
(gaps in [self-host-toolchain-pilots.md](self-host-toolchain-pilots.md)).
Wall clock includes the input setup in both arms, which pulls ratios toward
1.0; workloads sized at 0.5-1.3 s per Rust run keep that small.

## Compiler gaps

None new in part 1 (no Vesper code). vesper-span's `edits` gap is the one
above, already reported by SH0 lane 6.

## What is left

- Part 2, once plan PR #1415 merges: the "Flipping a module" recipe in
  `docs/maintenance/rust-exit-plan.md` and its scoreboard; the single-build
  re-measure of every merged SH4 and SH5 swap in the hybrid (full suite and
  corpus differential, ABBA through the gate with
  `scripts/ci/flip-gate/compiler.toml`, callgrind per family) and the SH4
  and SH5 scoreboards; closes #1343.
- Part 3: the census STATUS tables kept current from each lane's report,
  and the re-score of the library ports whose numbers predate the P3b-gaps
  and port-speed waves (#1028, #1097).
