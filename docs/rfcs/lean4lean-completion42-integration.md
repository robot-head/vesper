# Actual completion prerequisites and reconstruction congruence (#539–#543)

The final packet proves the actual Boolean feasibility search sound and
complete for an independently defined admissible-permutation certificate.
Completeness requires sorted stored keys, a sorted accumulator and sufficient
fuel; it permits duplicate remaining names and names already accumulated.
Actual subsumption preserves variable support and key origin, which together
preserve per-key completion. Lookup-equal maps have identical actual search,
tree reconstruction and reified Levels; the chain permutation theorem covers
every fuel value and fallback.

The four proof modules add 48 public theorem roots: twelve certificate-core,
thirteen feasible-search, twelve variable-support and eleven congruence roots.
The imports-only final module has a 23-module transitive closure. The complete
source-derived inventory is 203 roots, including 155 earlier roots and all
seven support helpers beyond the 41 presence guards.

## Final source and artifact identity

Final mathematical source revision:
`19d0d39380e9376af5fff5e5ece3903be629673a`. Admissible uses the existing
list fold, preserving its exact nil/cons equations while introducing no new
generated recursion helper. Core, Feasible, VariableSupport and CompletionFinal
were freshly recompiled; all 48 original-root axiom probes passed. The
original environment also explicitly confirmed that the additional Admissible
helper is absent. The sealed compilation manifest is
`/tmp/lean4lean-completion42-fold-v2/fold-identities.json`, SHA256
`c1661137e6c549444335b171a4e7e795beb13217a3ee9d4b93ff33acbeeae249`.

Actual projection records 607 original occurrences, 605 distinct constants
and 592 canonical constants. Its thirteen omissions exactly match existing
vendor helpers from Lean4Lean.List/Level; no project helper is omitted. Two
identical safe realizations of `NormLevel.feasible.go.eq_1` and `.eq_2` are
retained, with zero alternate-proof aliases. Exact inventory, constant-info
equality, kernel replay, verified reimport and empty extensions/IR all pass.
Logical traversal visits 1,751,028 expression nodes.

Both complete audit batches (128 and 75 roots) pass, with cache eligibility
false and actual axiom unions exactly Classical.choice, Quot.sound and
propext. The standalone projection is 7,628,088 bytes, BLAKE3
`0f471e17baf05fe8616a521af759b045323704397b6d86196b1ae2b6c3ac5725`.
All 46 selected original source/object records and every authored root are
bound to the actual compilation and audit results.

## Observed executions

The final exact-source Rust 1.100 harness replay passed in 238.664 seconds.
Its genuine archived Bazel-built auditor is recorded separately from the
directly compiled test binary; no Cargo-built test claim is made. Build and
execution receipts, complete evidence and full stdout are retained under
`/tmp/lean4lean-completion42-fold-v2-harness/`.

All nine earlier complete matrices passed at source revision `9f61536`.
Their 41 selected source files and shared runner are byte-identical at the
final revision, and none imports Completion. Complete authored/audited
catalogs and original/projected fingerprints match historical checkpoints.
[Acceptance](lean4lean-completion42-acceptance.md) records each timing and
source association. These runs are not relabeled as executions at 19d.

A separate actual Bazel 9.3.1/Rust 1.100 deployment passed at 19d:
`//:test/lean4lean_completion42` built in 74.514 seconds, then its single
executed test passed in 227.52 seconds, zero failures or ignored tests. The
test command took 262.100 seconds and was not cached. An earlier 998.650-second
9f build populated the isolated output/cache; no 9f Bazel test ran. Commands
retained system trust, used batch mode, a 2 GiB JVM, one build job and a
single test job. Across all validation, at most two Lean compilers ran;
`-j1 -M4096`, LEAN_NUM_THREADS=1 and the 300-second matrix budget remain.

Actual Bazel and final manual evidence match completely apart from 24
temporary paths. The deployed CLI SHA256 is
`6aef5208260f57c715fd879a69de027f43a7fa4670e69b742de969be53c63542`;
the test executable SHA256 is
`501b941fc1ab787d6df93017acd40e98f1956cc25dd7a6abb3e4dabc1c00af8e`.
The receipt binds binaries, full stdout/XML, six auditor invocations and
evidence: `/tmp/vesper-bazel-completion-v1-receipt.json`, SHA256
`f7cf8ee9ed47b2178581edff249734c07d6991abb79a8b55d942466a136d0a7b`.

Strict Clippy of all ten actual Rust test sources passed under Rust 1.100 in
1.622 seconds through the minimal real-dependency package. This is targeted
linting, not a whole-workspace check. Rustfmt 1.100, BUILD synchronization and
whitespace checks pass. All nine earlier specifications/catalogs and the
complete 47,588-byte validator suffix remain byte-identical. Source cap 24,
128-root batching, all refusal controls and standard-three policy are unchanged.

## History and remaining obligations

The earlier 9f Completion matrix passed with a recursive certificate, but its
203-root/594-canonical projection and additional generated helper are
superseded by the final fold replay. The initial fold compilation used a
reserved identifier; that failed attempt is retained separately. Individual
leaf reports preserve their original compilation identities; final changed
core/dependent objects are identified by the fold and runtime manifests above.

NormalizeAux completion preservation, lexChain admissibility/minimality,
tree denotation, normalized reification correctness and canonicality remain
separate obligations. No opaque native Level.beq reflection,
whole-checker/compiler/native equivalence or source-campaign completion is
claimed. Compiler-prefix standard libraries/shared libraries are outside
individual project-artifact attestation. Implementation-proof audit status
is unchanged. This stack targets own #538 without automatic merging onto its
unprotected branch; hosted required checks remain the merge gate.
