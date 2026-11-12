# Source tooling integration (#69)

This batch integrates the separately claimed #63–#70 lanes against published
main `ba0fadedf91be3bfd059362dd4fe14cc2f2dc51a`. The issue board and actual Warp
PR paths were checked before assigning work. Ownership, dependency boundaries
and shared schema constraints are recorded in
[`tooling41-contracts.md`](../rfcs/tooling41-contracts.md).
External Warp issues #36–#45 and their source remain outside this batch.

## User-facing commands

The root executable now routes `fmt`, `lint`, `doc`, `test --doc` and `bench` to
their checked adapters. New root-free workspace packages are `vesper-grammar`,
`vesper-fmt`, `vesper-lint`, `vesper-doc` and `vesper-bench`. The workspace has
42 packages and introduces no new registry dependencies or dependency-version
changes. Existing source AST, checker, Place IR, arithmetic, interface and
native ABI schemas stay fixed. The existing Lean extractor is unchanged.

The matching lane reports document exact APIs and supported source profiles:

- [Canonical binary/Arrow validation](tooling41-grammar.md)
- [Token/comment-conserving formatter](tooling41-fmt.md)
- [Checked diagnostics and transactional fixes](tooling41-lint.md)
- [Checked documentation and structural signature search](tooling41-doc.md)
- [Shared native batch and actual documentation tests](tooling41-doctest.md)
- [Paired callback measurements and statistics](tooling41-bench.md)
- [Lexer/parser limits and compatibility](tooling41-parser-limits.md)

The root CLI owns final registration and removes temporary standalone
formatter test wiring; root integration tests invoke the actual executable.
Doctest help and JSON argument errors follow the requested output format.

## Checked source benchmarks

```text
vesper bench FILE [--baseline FILE] [--entry NAME] [--baseline-entry NAME]
  [--samples N] [--warmup N] [--iterations N]
  [--max-regression-bp N] [--json]
```

Both sources are read as bounded regular UTF-8 files, parsed, fully checked
and compiled into separate owned `NativeBatch` modules before timing. With no
baseline argument, the baseline is a separately compiled copy of the candidate.
Each report records the BLAKE3 of the actual compiled bytes; different reads of
the same supplied path must agree. No Git/CAS baseline resolver is implemented.

Before the unchanged checker runs, iterative AST inspection admits only
ordinary nongeneric pure primitive-scalar functions, scalar local expressions,
conditionals and direct resolved source/wrapping-intrinsic calls. Unknown calls,
nonempty tuples, constructors, closures, handlers, models, complex patterns and
composite annotations are refused. This prevents compact inferred composite
growth from reaching inference. NativeBatch then rejects recursion, cyclic
CFGs, unsupported ABIs and expanded call-DAG work beyond its limits.

The adapter limits each file to 256 KiB, 16,384 lexer tokens, 32 delimiter
levels, 128 function tokens, 64 conditionals and 256 tokens per source segment.
The iterative scalar AST limit is 4096 nodes / depth 32. Configuration permits
1–64 measured epochs, 0–64 warmup epochs, 1–65,536 calls per window, and at most
one million total calls across both arms including warmup. NativeBatch imposes
additional structural bounds. These are logical work limits, not a sandbox,
hard wall-clock deadline or allocator/RSS guarantee.

Actual A/B/B/A windows use `std::time::Instant` nanoseconds. Setup and both
compilations precede timed windows; the measurements include Rust/JIT dispatch,
loop and output-storage overhead. Each report includes raw paired totals,
actual module/compilation/run counts and the exact binomial order-statistic
population-median interval. Confidence assumes independent identically
distributed epoch ratios. ABBA does not establish cancellation of drift.
This is not a Mann–Whitney/Hodges–Lehmann implementation.

Without an explicit threshold, successful measurement exits zero even when
there are too few epochs for a finite interval. An explicit threshold fails
on either regression or inconclusive evidence. JSON output is exactly one
document on either outcome. Potentially large ratio bounds and confidence
coverage counts are decimal strings. Hardware PMU, allocation and vectorization
evidence remain explicitly unavailable. Native optimization is disabled in
this batch; no performance improvement is asserted.

## Integrated validation

The acceptance workflow exercises the actual executable and leaf examples on
Linux, Windows and macOS, using disposable sources for formatting/autofixes.
Its benchmark assertions concern execution/report invariants, never a noisy
wall-clock speedup. A separate job checks the new arithmetic model with the
repository-pinned Lean toolchain. Normal protected-branch CI remains required.

The parser default was raised from the initial lane's 64 to its already tested
hard ceiling of 128 to retain ordinary nested generic-call compatibility.
Explicit lower budgets still reject the same sources. The documentation AST
depth regression accepts either its own budget diagnostic or the earlier exact
parser resource diagnostic E0003; arbitrary errors do not satisfy the test.

Integrated local validation:

- `cargo test --workspace --locked --offline --no-fail-fast`: 1,658 passing
  tests across 203 suites, zero failures or ignored tests. The installed pinned
  Lean is on PATH, and the actual proof-audit/kernel tests executed.
- A final lint staging-ownership correction disarms cleanup after successful
  rename. New deterministic regressions preserve a subsequent owner's file at
  the vacated staging name and retain cleanup after failed rename. After this
  correction, all 18 root binary unit tests and 19 lint CLI tests passed.
- `cargo clippy --workspace --locked --offline --all-targets -- -D warnings`:
  passed. `cargo fmt --all -- --check` and `git diff --check` passed.
- `lean lean/MetronomeArithmetic.lean`: passed with actual axiom reports. The
  ratio-drift counterexample is axiom-free; other declarations report their
  Lean logical axioms explicitly, including Classical.choice for the scaled
  integer bound. These scoped results are not a compiler-correctness proof.
- Windows GNU all-targets cross-check passed for all five new leaf packages and
  `vesper-syntax`. The full workspace cross-check stopped in the existing
  Wasmtime fiber C build because this cloud image lacks
  `x86_64-w64-mingw32-gcc`; it did not establish a full root Windows build.
  Native Windows/macOS execution is delegated to the normal CI jobs and the new
  tooling workflow. This image has no provisioned sudo/package installer, so
  system privilege configuration was left unchanged.
- The recorded specification inventory and its 14 Python regressions passed.
- `scripts/test-source-tooling.sh`: passed the full Cargo build/run sequence.
  The actual CLI formatted/rechecked/executed a source returning 42, applied a
  SemVer-safe lint fix to a disposable demo returning 42, matched only `close`
  in the typed documentation query, rendered deterministic escaped local HTML,
  compiled two doctest entries/shared helper once, matched exact E0302 and
  rejected the wrong requested code, and executed six ABBA epochs / 192 calls.
  All three Grammar/formatter/paired-callback examples ran successfully.
  No benchmark speedup or wall-clock confidence decision was required to pass.

Commands used one Cargo build job, disabled incremental compilation and zero
dev/test debug information. Local Windows cross-checks are compilation evidence,
not native execution. Final platform results are available on the integration PR.

## Remaining design scope

This batch implements executable, bounded profiles for the claimed tooling
lanes; it does not complete all sections of the design. General source Grammar
macros/codecs, full Arrow IPC, unrestricted formatter macro payloads, lint
signature rewrites, proof-backed documentation badges, general SimPulse I/O
doctests, PMU providers, source-level allocation/SIMD evidence, code-layout
randomization and causal profiling remain separate work. Unsupported source
profiles fail explicitly rather than reporting a successful skipped result.
