# Normalized-map equality and shape acceptance (#503)

The eighth fixed `Packet::Shape` specification and required catalog are
authored. This lane ran direct rustfmt and static byte/source checks only.
It ran no Cargo or Lean command, genuine matrix, Clippy or Bazel execution.
All eight genuine matrices and final integrated validation remain coordinator
work; this report does not claim they passed.

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
other identifiers. The collector and validator are unchanged. All eight
complete post-correction genuine matrices remain pending.


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

The intended claims concern actual map Boolean equality and actual
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
implementation-proof audit status is unchanged. Genuine post-extension
Foundations, Construction, Order, Map, Normalize, Normalization, Comparison
and Shape matrices, strict targeted Clippy, repository formatting, BUILD
synchronization, Bazel and final source associations remain coordinator
integration work.
