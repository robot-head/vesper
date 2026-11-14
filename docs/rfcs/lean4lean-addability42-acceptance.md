# Addability foundations and raw reification acceptance (#532)

The ninth fixed `Packet::Addability` specification and required catalog are
authored. This lane ran direct rustfmt and static byte/source checks only.
It ran no Cargo or Lean command, genuine matrix, Clippy or Bazel execution.
The new packet and all eight prior complete matrices remain coordinator
execution work; this report does not claim they passed.

The fixed IDs are label `lean4lean_addability42`, source module
`Lean4LeanLevel42.AddabilityFinal`, projection
`Lean4LeanAddability42Kernel`, scope
`audited_addability_foundations_and_raw_reification_boundary` and marker
`LEAN4LEAN_ADDABILITY42_EVIDENCE`. The coordinator owns the imports-only final
module, new test entry point, narrow BUILD additions and integration report.
This closed packet adds no runtime module/proof selector, supplied expected
answer or weaker-policy flag.

The nineteen selected modules are ordered as follows:

1. `Lean4Lean.List`
2. `Lean4Lean.Level`
3. `Lean4LeanLevel42.Semantics`
4. `Lean4LeanLevel42.ListProofs`
5. `Lean4LeanLevel42.OrderedInsert`
6. `Lean4LeanLevel42.VarInsert`
7. `Lean4LeanLevel42.NameOrder`
8. `Lean4LeanLevel42.SortedKeys`
9. `Lean4LeanLevel42.MapValue`
10. `Lean4LeanLevel42.MapHelpers`
11. `Lean4LeanLevel42.PathPriming`
12. `Lean4LeanLevel42.LevelSemantics`
13. `Lean4LeanLevel42.MapAll`
14. `Lean4LeanLevel42.MapEquality`
15. `Lean4LeanLevel42.SortedSubset`
16. `Lean4LeanLevel42.MapAny`
17. `Lean4LeanLevel42.Addable`
18. `Lean4LeanLevel42.ReifyCycle`
19. `Lean4LeanLevel42.AddabilityFinal`

Traversing the actual source imports from the committed final module,
which imports Addable and ReifyCycle, recovered exactly these nineteen
modules. The four proof-module import graphs share eighteen modules; the
imports-only final module completes the closure. Every dependency precedes
its user in the fixed source order. LevelSemantics is needed by the cycle's
actual level denotation statement. The closure uses
light existing dependencies and excludes Shape, ShapeFinal, recursive
normalization and comparison-fold/geq proof modules. It fits the unchanged
hard source-count bound of 24. The unchanged import-closure check requires
every selected source to enter the replayed original artifact closure.

The 38 unique required theorem guards use the authors' public ASCII names
in namespace `Vesper.Lean4LeanLevel42`:

- Sorted subset APIs: `subset_sublist`, `keySorted_erase`,
  `subset_complete`, `subset_eq_true_iff`, `erase_subset_iff_subset_cons`.
- Subset boundary witnesses: `subset_needs_left_sorted`,
  `subset_needs_right_sorted`, `subset_duplicate_left_counterexample`,
  `erase_subset_needs_nodup_counterexample`.
- Actual map-any traversal: `norm_any_eq_toList_any`,
  `norm_any_eq_true_iff`, `norm_any_witness`, `norm_any_of_lookup`,
  `norm_any_congr_lookup`.
- Actual addability: `addable_iff_witness`, `addable_iff_subset`,
  `addable_mono_sorted_acc`, `addable_mono_keys`, `addable_orderedInsert`,
  `addable_lookup_congr`, `addable_beq_congr`.
- Addability witnesses: `addable_needs_sorted_target`,
  `addable_needs_sorted_keys`, `addable_nonzero_offset_witness`.
- Concrete raw-cycle properties: `reifyCycle_lookup`, `reifyCycle_WF`,
  `reifyCycle_keysSorted`, `reifyCycle_varsSorted`, `reifyCycle_noEmpty`,
  `reifyCycle_pathPrimed`, `reifyCycle_addable_a`, `reifyCycle_addable_b`,
  `reifyCycle_feasible`, `reifyCycle_lexChain`, `reifyCycle_toTree_reify`,
  `reifyCycle_mapValue`, `reifyCycle_reifiedValue`,
  `reifyCycle_counterexample`.

Guards only require presence. The unchanged source-command extractor derives
every actual public authored theorem in the selected helper closure,
including the generic map-any helper and any additional helpers absent from
the guard list. The unchanged root union adds every actual alternate-proof
alias. Existing 128-root batches audit the complete union, without a golden
root or alias count. Actual original compilation, projection and audit must
establish the complete inventory and source/object associations; guard counts
and static source inspection do not establish a successful replay.

Static byte comparison against base
`164562d9b9371d2e810af11ed05cfea90e37802f` confirmed that all eight older
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
| Shape | 1,390 | 1,653 |

The complete runner from `const POLICY` through EOF remains byte-identical:
47,588 bytes, SHA-256
`2162a7e23f7de857d06542325e26be039d9e589ea117f6a93a7f62906b4da0b9`.
Removing only the new enum variant, required catalog, specification arm and
introductory eight-to-nine comment restores the base runner byte-for-byte.
Static assertions also passed for the nineteen unique selected module names,
the five fixed IDs, all 38 unique namespace-qualified guards, their catalog
association and the unchanged source-count bound. All 38 guards also
matched actual public theorem commands in the four new proof modules. These
checks and the inspected import graph/source hashes are retained in
`/tmp/lean4lean-addability42-acceptance-static-check.json`. Direct
`rustfmt --edition 2024 --check tests/common/lean4lean_proof_packet.rs` and
`git diff --check` passed. This lane changes only the narrow runner extension
and this new report; the existing endpoint registration and every BUILD
attribute remain coordinator-owned.

The unchanged validator retains all actual source/provenance binding,
original/projected source and artifact associations, occurrence/partition
validation, complete authored-root/alias discovery, strict audit checks,
bounded regular-file capture, missing/corrupt original and projected
artifact controls, changed-source refusal and the forbidden alternate-proof
control. The source cap of 24, 128-root batching, standard-three axiom policy,
all other caps and finite 300-second whole-matrix budget are unchanged.

The intended mathematical claims concern the actual prefix-first sorted
subset test, actual short-circuit `NormLevel.any`, and actual addability.
Map-any and lookup congruence require no sorting, map WF or physical tree
equality premise. The unconditional addability witness retains an arbitrary
stored offset; it is not restricted to zero. The subset characterization
requires sorted stored keys and accumulator. The two monotonicity forms keep
their distinct sorting premises, while actual ordered insertion preserves
addability under sorted accumulation without a map-key premise.

The concrete raw cycle addresses a separate boundary. WF, sorted keys,
sorted variable lists, syntactic nonempty rows and current PathPrimed
coverage do not establish an admissible ordering or reification denotation.
Its actual failed starts, feasibility test, fallback and denotation
counterexample are authored mathematical statements, not supplied expected
answers or a second checker. No claim says that normalize emits this raw map.

Sorted subset completeness and addability foundations prepare later greedy
completeness work. This packet does not prove feasible completeness,
fallback unreachability, canonicality, reification correctness on normalized
maps, reflection of opaque native Level.beq, whole-checker correctness,
compiler/native equivalence or a successful source campaign. Existing
implementation-proof audit status is unchanged. Genuine Addability and all
eight earlier complete matrices, strict targeted Clippy, repository
formatting, BUILD synchronization, Bazel and final source/object evidence
remain coordinator integration work.
