# Normalized-map equality and shape acceptance (#503)

All eight genuine fixed matrices passed. The actual code revision was
`325b83972774c8c365173e6b8ac4ac8e3c72cbe0`; the seven prior matrices ran
from final Cargo-built executables at documentation-only descendant
`3bdeba499fefe4d66c7c5a1efd1bc53927eca74a`, with at most two concurrent
Lean processes. The coordinator ran Cargo/Lean. This acceptance lane
independently checked the actual evidence, source catalogs, executable/log
hashes and historical artifact identities without executing either.

| Genuine packet | Authored/audited roots | Audit batches | Observed seconds |
| --- | ---: | --- | ---: |
| Foundations | 49 | 49 | 129.630 |
| Construction | 37 | 37 | 130.534 |
| Order | 64 | 64 | 135.816 |
| Map | 99 | 99 | 144.935 |
| Normalize, before subsumption | 131 | 128 + 3 | 172.999 |
| Normalization, including subsumption | 173 | 128 + 45 | 188.026 |
| Comparison | 191 | 128 + 63 | 193.239 |
| Shape | 226 | 128 + 98 | 205.99 |

The seven prior timings are measured executable durations from their
complete receipt; Shape's timing is the test harness result. Every matrix
reported one passed test, zero failed and zero ignored tests, including
completion of the actual negative controls. All retained the unchanged
300-second internal budget. Strict targeted eight-test Clippy passed in
29.26 seconds; formatting, BUILD synchronization and whitespace checks passed.
No Bazel execution, hosted CI or broader compiler/runtime proof is claimed.

The fixed specification uses label `lean4lean_shape42`, module
`Lean4LeanLevel42.ShapeFinal`, projection `Lean4LeanShape42Kernel`, scope
`audited_normalized_map_equality_and_shape` and marker
`LEAN4LEAN_SHAPE42_EVIDENCE`. The coordinator owns the new aggregate, test
entry point, BUILD copy/scheduling entries and integration report. The closed
packet adds no runtime module/proof selector, supplied expected answer or
weaker acceptance policy.

Its exact minimal twenty-four-module source order is:

1. `Lean4Lean.List`
2. `Lean4Lean.Level`
3. `Lean4LeanLevel42.Semantics`
4. `Lean4LeanLevel42.ListProofs`
5. `Lean4LeanLevel42.Subsumption`
6. `Lean4LeanLevel42.OrderedInsert`
7. `Lean4LeanLevel42.VarInsert`
8. `Lean4LeanLevel42.NameOrder`
9. `Lean4LeanLevel42.SortedKeys`
10. `Lean4LeanLevel42.MapValue`
11. `Lean4LeanLevel42.MapHelpers`
12. `Lean4LeanLevel42.PathPriming`
13. `Lean4LeanLevel42.LevelSemantics`
14. `Lean4LeanLevel42.NormalizeInvariants`
15. `Lean4LeanLevel42.NormalizeValue`
16. `Lean4LeanLevel42.NodeResidual`
17. `Lean4LeanLevel42.MapMinimize`
18. `Lean4LeanLevel42.MapSubsumption`
19. `Lean4LeanLevel42.MapAll`
20. `Lean4LeanLevel42.Shape`
21. `Lean4LeanLevel42.MapEquality`
22. `Lean4LeanLevel42.ConstructionShape`
23. `Lean4LeanLevel42.SubsumptionShape`
24. `Lean4LeanLevel42.ShapeFinal`

This actual dependency closure fits the unchanged hard `SOURCE_COUNT = 24`
bound. `Shape` imports SortedKeys, NormalizeValue and MapSubsumption. The
final semantic bridge composes the existing actual normalizeAux and
subsumption denotation theorems directly. NormalizeFinal, the comparison
fold/geq modules and previous aggregates are outside this new closure; their
seven earlier packets remain unchanged and are verified separately. The
unchanged import-closure check requires every selected source to enter the
replayed artifact closure.

The coordinator's first genuine Shape attempt is retained at
`/tmp/lean4lean-shape42-root-shape-v1.log`. It correctly failed the required
root check for `Vesper.Lean4LeanLevel42.normalize_keysSorted` before any
packet compilation. The five final proofs were initially placed in a
top-level aggregate whose module name lies outside the frozen authored-root
collector's `Lean4LeanLevel42`/`Lean4LeanNames42` module prefixes. Prior
original-module compilation did not establish inclusion in that catalog.
The coordinator moved the proofs to the new `Lean4LeanLevel42.ShapeFinal`
module. This correction changes only Shape's fixed target module and final
source-list entry, preserving its twenty-four-module size, all guards and
other identifiers. The collector and validator are unchanged. The complete
post-correction results are recorded in the table above.


The 26 unique required theorem guards use the proof authors' frozen public
names in namespace `Vesper.Lean4LeanLevel42`:

- Equality reflection and denotation: `varNode_beq_iff_eq`,
  `varNode_lawfulBEq`, `node_beq_iff_eq`, `node_lawfulBEq`,
  `nodeOption_beq_iff_eq`, `norm_beq_iff_lookup_eq`,
  `norm_lookup_eq_mapValue`, `norm_beq_mapValue`.
- Empty-map and update shape: `map_keysSorted_empty`,
  `map_varsSorted_empty`, `map_addConst_keysSorted`,
  `map_addNode_keysSorted`, `map_addVar_keysSorted`,
  `map_addConst_varsSorted`, `map_addNode_varsSorted`,
  `map_addVar_varsSorted`.
- Recursive construction: `normalizeAux_keysSorted`,
  `normalizeAux_varsSorted`.
- Actual outer pruning fold: `subsumption_keysSorted`,
  `subsumption_varsSorted`, `subsumption_noEmpty`.
- Final module: `normalize_keysSorted`, `normalize_varsSorted`,
  `normalize_noEmpty`, `normalize_shape`, `normalized_beq_value`.

These guards only require presence. The unchanged source-command extractor
derives every actual public authored theorem in the selected helper closure,
including additional proof helpers and witnesses outside the guard list.
The unchanged root union includes all actual alternate-proof aliases. The
existing 128-root batches audit that complete union without a golden root or
alias count. Genuine projection and audit evidence must establish the actual
inventory and original/projected identities; static guard counts do not
establish them.

Static byte comparison against base
`d3cdefb98cfc96fde3bfca6160c8aaeb4a29c981` confirmed that all seven older
required catalogs and complete Packet specification arms remain identical.
Measured including their terminating newline, the byte counts are:

| Prior packet | Required catalog | Complete specification arm |
| --- | ---: | ---: |
| Foundations | 1,243 | 796 |
| Construction | 1,656 | 777 |
| Order | 1,398 | 822 |
| Map | 1,445 | 920 |
| Normalize | 1,668 | 1,139 |
| Normalization | 1,293 | 1,426 |
| Comparison | 685 | 1,556 |

The full runner suffix from `const POLICY` through EOF is byte-identical:
47,588 bytes, SHA-256
`2162a7e23f7de857d06542325e26be039d9e589ea117f6a93a7f62906b4da0b9`.
Removing only the new enum variant, catalog, specification arm and
introductory seven-to-eight comment restores the base runner byte-for-byte.
Static assertions also passed for the exact twenty-four-module order, all
five fixed identifiers, all 26 unique qualified guards, the required-catalog
association and the unchanged source-count bound. Direct
`rustfmt --edition 2024 --check tests/common/lean4lean_proof_packet.rs` and
`git diff --check` passed. This lane changes only the narrow runner extension
and this new report.

The unchanged validator retains actual source/provenance correspondence,
original and projected artifact associations, occurrence/partition checks,
complete root/alias discovery, strict audit checks, bounded regular-file
capture, missing/corrupt original and projected artifact controls,
changed-source refusal and the forbidden alternate-proof control. Its
standard-three axiom policy, every cap, 128-root batching and finite
300-second per-matrix budget remain unchanged.

The proved claims concern actual map Boolean equality and actual
normalization shape. Map equality reflection and its denotation consequence
require no map WF or sorting premise. Construction carries the current
path's sortedness where needed, while pruning preserves the respective
initial key/variable invariants. The no-empty result applies to arbitrary
raw maps. The final shape theorems apply to every Level; the normalized-map
BEq denotation bridge has the existing structural `NoMVars` premises on both
levels. MapWF remains a separate property.

There is no reflection assumption for the opaque native `Lean.Level.beq`
fast branch and no unconditional `isEquiv'` soundness claim. Comparison
completeness, canonicality, reification, whole-checker correctness and
compiler/native equivalence remain outside this packet, and existing
implementation-proof audit status is unchanged. All eight post-extension
matrices and the observed repository checks are recorded here; the
coordinator owns the final integration report and PR status.


Independent inspection of
`/tmp/lean4lean-shape42-shape42-evidence.json` and
`/tmp/lean4lean-shape42-root-shape-v2.log` confirmed the complete corrected
Shape replay, audit and negative-control run. There are 226 actual public
authored roots, exactly equal to the audited root set. An independent source
inventory of every selected helper module recovered the same 226 names,
including all forty-one newly authored roots: eight from `MapEquality`,
fourteen from `ConstructionShape`, fourteen from `SubsumptionShape` and five
from `ShapeFinal`. The 185 existing roots comprise 170 shared with the prior
Normalization packet, eleven SortedKeys theorems and four MapAll theorems.
All actual public helpers and the two construction witnesses are included;
the guard list is not the audited inventory.

The exact new inventory consists of the 26 guarded declarations listed above
plus these fifteen additional public declarations in the same namespace:

- Construction: `normalizeAux_empty_varsSorted`,
  `normalizeAux_empty_keysSorted`, `normalizeAux_needs_sorted_path`,
  `map_addVar_varsSorted_merge_witness`.
- Pruning and outer fold: `subsumeVars_shape_sublist`, `subsumeVars_sorted`,
  `node_subsumeBy_varsSorted`, `node_subsume_varsSorted`,
  `minimize_varsSorted`, `minimizeEntry_keysSorted`,
  `minimizeEntry_varsSorted`, `subsumption_fold_keysSorted`,
  `subsumption_fold_varsSorted`, `minimizeEntry_current_noEmpty`,
  `subsumption_fold_noEmpty`.


The actual twenty-four-module transitive import closure exactly matches the
fixed selected sources and contains 48 original source/object identities.
There are 713 original constant occurrences, partitioned into 697 retained
canonical constants and sixteen exact matched compiler recursion helpers.
The omission set, including module, name, parent and reason, is identical to
the prior Normalization checkpoint. It includes existing generated helpers
from List, Level and LevelSemantics; no new proof omission was added. The
evidence reports zero duplicates and zero alternate-proof aliases, exact
inventory, constant-info equality, empty extensions and IR, successful kernel
replay and verified reimport. Logical traversal visited 1,834,420 expression
nodes. The projection `Lean4LeanShape42Kernel.olean` is 9,772,016 bytes,
BLAKE3 `dcdcb30859e5bb7d0919d0a1164af835408c6c3209588b7a25fd7f3f8754730f`.

The audit batches contain 128 and 98 disjoint roots, exactly covering all
226 authored roots. Both are accepted with status `audited`, successful
kernel replay, `cache_eligible = false`, 697 checked constants and no
forbidden axioms. Each batch's actual theorem axiom union is exactly
`Classical.choice`, `Quot.sound` and `propext`. Both audit artifact records
match the projection's exact filename, byte count and BLAKE3. All authored
root names occur in the retained canonical occurrence partition. The actual
original module is `Lean4LeanLevel42.ShapeFinal` throughout these associations.
Evidence records `whole_checker_verified = false`.

Every shared original `.lean`/`.olean` fingerprint matches the previous
Normalization and Comparison checkpoints: thirty-four shared records with
Normalization and thirty-six with Comparison. Independent source hashes
match the proof-lane identities, including ShapeFinal SHA-256
`d143cf873e59e6bd1604de7923bc4a501c8b5677c0ed0f8549c8d6f47d873a98`.
No source-catalog, scope or original/projected association defect was found.
The inspection result and all forty-one new root names are retained in
`/tmp/lean4lean-shape42-independent-check.json`.

The complete seven-prior-matrix execution receipt is
`/tmp/lean4lean-shape42-prior-matrices-receipt.json`; its progress log ends
`ALL SEVEN PRIOR MATRICES PASSED`. Independent checks verified every recorded
executable and full log SHA-256. For each prior packet, the complete authored
and audited root catalogs, all original source/object fingerprints, the
projected artifact fingerprint, provenance and snapshot-tool identity match
the corresponding previous Comparison-wave checkpoint exactly. Every audit
is accepted, kernel-replayed, cache-ineligible and free of forbidden axioms,
with complete root coverage and actual per-packet axiom union equal to the
standard three. Each batch uses only that allowed set; the final three-root
Normalize batch uses `propext` alone. All original records shared with Shape
also match. These final comparisons are retained in the independent-check
JSON alongside the forty-one new root names.

The coordinator's execution receipt is
`/tmp/lean4lean-shape42-root-execution-receipt.json`. This lane independently
verified its five log hashes and seven current source hashes. The strict
Clippy pass is recorded in `/tmp/lean4lean-shape42-root-clippy-v1.log`;
`/tmp/lean4lean-shape42-root-fmt-v1.log` and
`/tmp/lean4lean-shape42-root-buildsync-v1.log` are empty successful logs.
The original v1 required-root failure remains intact and is not represented
as a successful replay. All genuine evidence comes from original compiled
objects and their exact existing logical projection, with no source,
validator, policy, cap or budget exception.
