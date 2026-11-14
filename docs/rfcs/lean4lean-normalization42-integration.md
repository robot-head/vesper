# Actual normalization-map integration (#435)

All six genuine acceptance matrices passed on integrated revision
`c62e1aed761fd473de1e0bf2607a0272ca295ace`. The coordinator executed these
matrices; this report was written by reading their actual evidence and test
log, without running Cargo or Lean in the acceptance lane. Each matrix had
one passing test and no failed, ignored or filtered tests:

| Packet | Actual authored roots | Audit batches | Test duration |
| --- | ---: | --- | ---: |
| Construction | 37 | 37 | 125.75 s |
| Foundations | 49 | 49 | 128.81 s |
| Map | 99 | 99 | 138.95 s |
| Normalization, including subsumption | 173 | 128 + 45 | 194.37 s |
| Normalize, before subsumption | 131 | 128 + 3 | 176.40 s |
| Order | 64 | 64 | 137.45 s |

These are observed test durations from
`/tmp/lean4lean-normalization42-root-acceptance.log`, within the unchanged
finite 300-second per-matrix budget. Structured evidence was read from
`/tmp/lean4lean-normalization42-{construction,foundations,map,normalization,normalize,order}42-evidence.json`.
The tests exercised the real compiler, logical projection, proof auditor and
negative controls, using the unchanged standard-three policy and existing
128-root batching.

The new Normalization packet replayed the exact nineteen-module closure. Its
kernel projection retained 623 canonical constants from 639 original
constant occurrences, with sixteen matched partial/unsafe compiler recursion
helpers accounted for by the existing projection rule. It reported zero
duplicate constants and zero alternate-proof aliases, exact inventory,
constant-info equality, empty extensions and IR, successful kernel replay
and verified reimport. Projection traversal visited 1,798,460 logical
expression nodes. `Lean4LeanNormalization42Kernel.olean` is 9,401,488 bytes,
BLAKE3 `eb9369b3f2fb93041169ced35712967dd48f34771c75d44b81ed82ea1c4eea4f`.

The 173 actual authored roots are the previous Normalize closure's 131,
twenty-nine newly authored public theorems and thirteen existing public
Subsumption theorems newly imported by this packet. The four new proof
modules contribute nine roots from `NodeResidual`, eight from `MapMinimize`,
nine from `MapSubsumption` and three from `NormalizeFinal`. This accounting
was checked against actual public theorem commands and the two evidence
catalogs. It is an observed inventory, not a hard-coded acceptance count.
The unchanged extractor audits every public authored root and all actual
aliases; this run discovered none of the latter.

Both Normalization audit batches were accepted with status `audited`,
`kernel_replayed = true`, `cache_eligible = false`, 623 checked constants
and no forbidden axioms. Their disjoint theorem sets exactly cover the 173
reported audited roots. The union of actual theorem axioms is exactly
`Classical.choice`, `Quot.sound` and `propext`. Independent structured checks
confirmed those same acceptance, replay, cache, root-coverage and actual
axiom-union properties for all five earlier packets.

Original source/object fingerprints were preserved through projection and
audit. The unchanged runner compares original and projected artifact hashes
before recording evidence, then again after the negative controls before
reporting complete acceptance. It also restores and rechecks the deliberately
changed source. The final successful test results therefore cover the
changed-source refusal, missing/corrupt original and projected artifact
controls and forbidden alternate-proof control. Cross-packet evidence checks
also found identical fingerprints for every shared original `.lean` and
`.olean` record, including all twenty-six records shared by the final and
pre-subsumption normalization packets. No original object was replaced by
its projection.

The resulting public theorem `normalize_value` states that, for every
valuation and level satisfying structural `NoMVars`, the actual unchanged
`Normalize.normalize` map has the exact `levelValue` denotation.
`normalize_WF` establishes `MapWF` for every level, without a `NoMVars`
premise. These results compose the existing actual `normalizeAux` theorems
with the new fixed-map minimization and actual subsumption proofs.

The inner minimization proof tracks both gated-variable and whole-node
semantic context over the fixed original `acc.toList`. The self row is
identified by actual lookup, and the original variables must remain covered
by the current residual variables or external map context. This variable
invariant is necessary when self comparison removes a constant using the
original variables; equality of whole-node values alone is insufficient.
The explicit self-context counterexample records that failure. The outer
proof folds the original map entries, preserves current lookup for unvisited
distinct keys, and relates `minimizeEntry` definitionally to the actual source
step. It does not replace the algorithm or re-enumerate the changing map.
The lookup and malformed-WF counterexamples, actual dominated-row deletion
and final imax witness are included in the audited roots.

The coordinator's independent review reported no actionable defect. These
results concern normalization-map denotation and well-formedness. They do
not establish canonicality, cached-domain properties, `.le`/reification,
whole-checker correctness, compiler refinement or native equivalence.
Evidence explicitly records `whole_checker_verified = false`; the
implementation's proof-audit status remains unchanged.

At this report's commit, final strict Clippy, repository formatting, Bazel,
whitespace and source-association validation remain coordinator work; this
report makes no pass claim for those checks. The separately claimed CI work
#443 addresses macOS-only duplicate-replay scheduling while preserving the
Rust/Lean checks, acceptance policy and budgets. Its hosted retry remains
pending. The coordinator's final validation report follows separately.
