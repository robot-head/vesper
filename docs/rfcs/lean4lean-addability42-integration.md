# Actual addability and raw reconstruction boundary (#527–#532)

The four new proof modules establish forty public theorem roots about the
actual compiler definitions: nine sorted-subset roots, six map-any roots,
eleven addability roots and fourteen raw-cycle roots. The final module only
imports Addable and ReifyCycle. The fixed packet includes nineteen original
modules and audits all 179 discovered authored roots, including the two
helpers beyond the 38 presence guards.

Operational witnesses retain arbitrary stored offsets. Declarative subset
completeness and the two addability monotonicity forms keep their distinct
sorting hypotheses. Map-any and lookup congruence need no sorting or map-WF
premise. The raw cycle is closed and concrete: WF, sorted/nonempty rows and
PathPrimed hold, while actual reconstruction denotes one where the raw map
denotes zero. This does not assert that normalization emits the cycle.

## Execution and exact sources

All mathematical and runner execution below used source revision
`0714ae39c83ee107e9fc559d79514e5c7e330fbb`. Subsequent changes add reports
and BUILD scheduler tags; the executed Lean, Rust and audit sources are
unchanged. [Acceptance](lean4lean-addability42-acceptance.md) records all nine
complete matrices, historical artifact comparisons and the distinct build
origins of the exact-source Rust 1.100 harnesses and archived genuine
Bazel-built auditor. Those harnesses are not described as Cargo- or
Bazel-built tests.

A separate actual deployment check used repository-pinned Bazel 9.3.1 and
Rust 1.100.0, with fresh output base
`/tmp/vesper-bazel-addability-v1`. The actual build of
`//:test/lean4lean_addability42` passed in 469.044 seconds, executing 3,357
actions. Its single actual Bazel test invocation passed in 236.749 seconds;
the Rust acceptance test took 187.93 seconds, with zero failures or ignored
tests. This run exercised the packaged test and newly built repository CLI,
including original compilation, projection, complete audit and every real
refusal control. `--test_arg=--nocapture` retained the actual evidence.

Independent comparison of the complete actual Bazel evidence with the earlier
matrix found equality in every field except the projection and nineteen
original-module temporary paths. All source/object fingerprints, complete
catalogs, provenance, policy, audits and snapshot-tool identities match.
The projection is 5,984,688 bytes, BLAKE3
`e33177656ba1fd8f1975a1c004ac5242c475dae293393a17a52dfe1ad2ebc9be`.
The comparison receipt is retained as
`/tmp/vesper-bazel-addability-v1-root-comparison-receipt.json`.

Commands used `--batch --nohome_rc`, existing system trust configuration,
`--config=ci --jobs=2 --loading_phase_threads=2`; the test also used
`--local_test_jobs=1`. No remote execution/cache claim follows from this local
deployment. The fixed runner explicitly sets `LEAN_NUM_THREADS=1` and passes
`-j1 -M4096`. No more than two Lean compiler processes ran concurrently in
the separate nine-matrix validation. All test budgets and policies remain
unchanged.

Full deployment logs are retained as
`/tmp/vesper-bazel-addability-v1-build.log`,
`/tmp/vesper-bazel-addability-v1-test.log`,
`/tmp/vesper-bazel-addability-v1-target-test.log` and
`/tmp/vesper-bazel-addability-v1-target-test.xml`.

## Other checks and limits

Strict Clippy of all nine exact test sources passed under Rust 1.100.0 via
the minimal real-dependency package in 8.295 seconds. Rustfmt 1.100, BUILD
synchronization and whitespace checks passed. The complete old validator
suffix and all eight older packet catalogs/specifications remain
byte-identical. The new source closure fits the existing cap of 24, uses
128-root batches and retains the 300-second whole-matrix budget.

[The scheduler reservation report](lean4lean-proof-reservations42.md)
records the separate pinned Bazel query of all nine wrappers. The 5120 MB
reservation is a scheduling estimate, not measured RSS or a diagnosed fix
for inaccessible hosted BuildBuddy failures. The actual deployment test
above preceded that BUILD-only reservation change.

This packet proves helper properties and the raw reconstruction boundary.
Admissible-order preservation, greedy completeness, normalized reification
correctness, canonicality, opaque native Level.beq reflection and
whole-checker/compiler/native equivalence remain separate obligations.
No source/native campaign completion or changed implementation-proof audit
status is claimed. Hosted required checks remain the merge gate. The PR
targets own #526; automatic merging onto that unprotected branch is disabled.
