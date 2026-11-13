# Executable Lean arithmetic oracle integration (#269)

The release harness completed 100000 distinct valid programs and all 15
isolated static negatives, with agreement on every one of the 800000 actual
evaluator/JIT outcomes. The process exited successfully with empty stderr.
The unmodified stdout report is preserved in
[oracle42-full100000-report.json](oracle42-full100000-report.json).

This is executed evidence for the bounded integer profile below. It does not
establish full §42 completion, whole-language checker/compiler refinement,
dynamic resource traces, Lean4Lean validation or bootstrap/DDC correctness.

## Source, configuration and provenance

The coordinator built and executed the real release binary from
`8e2f653e91729e1e893d65432ef15554f628d639`, incorporating main `11e0d9d`.
The recorded run configuration was Full100000, seed0, jobs4, with default
limits and shrinking disabled. The runtime tools were Rust 1.99.1
(`58cf8f9c3`, 2026-10-13) and Lean 4.24.0
(`797c613eb9b6d4ec95db23e3e00af9ac6657f24b`, Release), on Linux x86_64.

The coordinator's standard release build completed in 7m40s. A fresh build
after the main merge completed in 0.49s; that merge added an unrelated Chorus
workspace crate and did not change the root package's dependencies. These are
observations of this execution, not build-performance guarantees.

The writer of this report authored independent acceptance checks and reviewed
the actual stdout JSON, location metadata, stored report/configuration and
validation logs. The writer independently asserted the completion totals in
the saved JSON and copied its exact 2385 bytes. All Cargo builds, Lean
compilation, backend execution and the full run were performed by the
coordinator, not the report writer. The coordinator's wrapper separately
checked exit status and exact completion counts.

The JSON contains hashes of the actual worker executable, bundled model,
static driver, semantic corpus and confirmed Lean wire frames. Source-revision
association comes from the coordinator's run metadata; hashes are inspection
data, not a proof of Rust-to-model binding or a provenance certificate.

## Completed run

| Observation | Actual result |
|---|---:|
| Requested/generated/unique/committed valid programs | 100000 each |
| Valid programs per kind, u8/u32/u64/i64 | 25000 each |
| Generated/matched isolated negatives | 15 each |
| Evaluator calls, each of four AST/IR settings | 100000 |
| Native JIT calls, each of four AST/IR settings | 100000 |
| Total backend outcomes | 800000 |
| Parser/checker/AST-optimizer calls | 100015 / 200013 / 100000 |
| Lean requests/responses | 100015 each |
| Persistent Lean oracle spawns | 1 |
| Lean values / arithmetic overflows | 52105 / 47895 |
| Rust worker spawns / recycles | 3128 / 3124 |
| Maximum handled cases per worker / observed in-flight cases | 32 / 4 |
| Timeouts, crashes, protocol/outcome/diagnostic errors | 0 |
| Interrupted workers, cancelled cases, shrink attempts | 0 |
| First failed case | none |
| Reported elapsed time | 490106 ms |
| Coordinator wrapper wall time | 490.124 s |

Every per-mode evaluator/JIT slot completed its own calls. These are the
Cartesian product of original/optimized AST, IR optimization off/on, and
evaluator/native JIT. Eight outcomes are not counted as eight programs.
The 15 negatives are counted separately, with diagnostic-class counts
`[3,2,5,2,1,1,1]` in the frozen order. All shrink-call counters are zero.
Phase timings in the JSON are observed wall durations that can overlap;
they are not additive CPU-time or resource-consumption measurements.

## Implemented boundary

The standalone developer binary parses and checks real rendered Vesper source,
normalizes the actual parsed module back to the intended DTO, and executes
the actual evaluator and JIT. The reference answers come from one persistent,
framed Lean process running the exact bundled static ArithmeticOracle driver
over RewriteWitness semantics. Requests contain data rather than expected
answers or generated Lean source.

The finite profile contains u8, u32, u64 and i64; checked and wrapping
add/subtract/multiply; up to three typed inputs, three sequential immutable
copy locals, 63 total occurrence nodes and expression depth8. Normal corpus
programs retain two typed input arguments. Local initializers execute strictly,
including unused locals. Static checking precedes execution of the complete
program. There are no arbitrary calls, casts, branches, recursion, heap effects
or linear-resource operations in this profile.

The deterministic rank/input-pair construction gives distinct programs for
each kind and rank, independently of comments or case IDs. Generator tests
check that construction; runtime fingerprints detect duplicates conservatively.
Seed0's completed 100000-program run is the evidence here, not a claim about
every possible seed or every program in the profile.

Framing caps, deadlines, exact reads, bounded queues, worker recycling and
kill/reap/join cleanup are implemented and exercised. They bound the stated
logical data and process operations, not compiler internal allocations,
whole-process RSS or arbitrary callback termination. The parser/checker
diagnostic comparison covers the 15 isolated negative fixtures; it is not
general diagnostic-set or source-span equivalence.

## Validation evidence

The full-run stdout and location metadata were
`/tmp/oracle42-root-full100000.json` and
`/tmp/oracle42-root-full100000-location.json`; stderr was
`/tmp/oracle42-root-full100000.stderr` (zero bytes). The retained run directory
was `/tmp/vesper-oracle42-full100000-jpvth8dl/` with its unique per-run child.
Those temporary locations identify this session's observations; the copied
JSON is the repository's durable full-run evidence.

| Coordinator check | Actual result |
|---|---|
| Full root-package regression, `/tmp/oracle42-root-regression.log` | 946 passed, 0 failed, 0 ignored across 78 result records, including doctests |
| Focused corpus/runner/acceptance targets | 51 passing tests; these are included in the 946 total, not additional tests |
| Independent acceptance, `/tmp/oracle42-root-acceptance-final3.log` | Complete serial matrix passed in 50.86s, including CI128 with jobs1/jobs2, actual Lean semantic/framing/fuel checks, artifacts and all ten portable fault modes |
| Final strict Clippy, `/tmp/oracle42-root-clippy-final2.log` | Passed, 1.40s |
| Formatting/diff and static-model CI | Coordinator reported passed |
| Release build and full release run | Completed; full run exited0 with empty stderr |

See [the acceptance report](oracle42-acceptance-report.md) for its exact
earlier revisions, corrections and separation of authored assertions from
coordinator execution. No macOS, Windows, Bazel or BuildBuddy execution is
established by the local logs summarized here.

## Remaining §42 work

This chunk supplies an executed Stage-0b integer differential oracle. The
broader design still calls for full-language reference typing/evaluation,
bit-identical resource mint/consume traces, and external Lean4Lean checking.
The executable Lean checker here is not proved to refine the Rust checker,
and the AST-to-model/source binding is checked by code rather than proved in
Lean.

The bounded arithmetic witness/replay foundation does not cover all PlaceIr,
SIMD, constructor-reuse or handler-fusion rewrites. Backend translation
validation and a direct verified emitter need their actual refinement and
forward-simulation proofs. Self-hosted compiler generations, hermetic
self-verification, independently derived Lean/CompCert DDC binaries,
stage2/stage3/stage3-prime fixed points and builder/attestation quorum evidence
are not established by this run. Neither hashes nor finite agreement imply
those remaining guarantees.
