# Actual map construction acceptance (#422)

The Map entry point and fourth static packet are authored. This lane ran direct
rustfmt and static source checks only. It did not execute Cargo, Lean, any of
the four genuine acceptance matrices, targeted strict Clippy or Bazel. Those
integration checks remain pending with the coordinator; this report does not
claim they passed.

`tests/lean4lean_map42.rs` calls the existing shared runner through the closed
`Packet::Map` choice. The fixed specification selects `Lean4LeanMap42`, projects
`Lean4LeanMap42Kernel`, and uses label `lean4lean_map42`, scope
`audited_named_map_construction_proofs` and marker `LEAN4LEAN_MAP42_EVIDENCE`.
There is no runtime module selector, supplied expected answer or alternative
acceptance policy.

The static closure contains exactly eleven modules in contracted dependency
order: `Lean4Lean.List`, `Lean4Lean.Level`, `Lean4LeanLevel42.Semantics`,
`Lean4LeanLevel42.ListProofs`, `Lean4LeanLevel42.OrderedInsert`,
`Lean4LeanLevel42.VarInsert`, `Lean4LeanLevel42.NameOrder`,
`Lean4LeanLevel42.MapValue`, `Lean4LeanLevel42.MapHelpers`,
`Lean4LeanLevel42.PathPriming` and the import-only `Lean4LeanMap42` aggregate.
`SortedKeys` and `Subsumption` are outside this closure. The unchanged actual
import-closure check requires every selected source to enter the replayed
artifact closure.

The 27 unique required theorem names guard all 22 contracted APIs: seven map
value/WF facts, seven actual map-update value/WF facts, and eight syntactic
path-priming facts. The other five guards are the proof authors' actual
witnesses: `missing_key_addVar_counterexample`,
`skipped_one_empty_counterexample`, `skipped_one_zero_node_counterexample`,
`pathPrimed_one_edge_counterexample` and `pathPrimed_extend_example`.
The one-edge witness uses two variables to distinguish a sufficient constant-one
bound from full per-variable coverage. Required names only guard presence.
The unchanged source-command extraction derives every public authored theorem
in the actual selected helper closure, and the unchanged root union adds every
actual alternate-proof alias. The complete union is audited without a golden
root or alias count.

Static comparison against base `69addebfaaee26abc83c9b77fbbb32fc5eaf043a`
confirmed all three original required arrays and all three original complete
Packet specifications remain byte-identical. The array sizes are 1,242,
1,655 and 1,397 bytes for Foundations, Construction and Order; their complete
specification arms are respectively 795, 776 and 821 bytes. The complete runner
from `const POLICY` through EOF is byte-identical: 47,588 bytes, SHA-256
`2162a7e23f7de857d06542325e26be039d9e589ea117f6a93a7f62906b4da0b9`.
Static checks also confirmed the exact eleven-module order, all fixed Map
identifiers, uniqueness and namespace of all 27 guards, and the Map test entry
point. Direct `rustfmt --edition 2024 --check` on the shared runner and new test,
and `git diff --check`, passed.

The unchanged runner therefore retains source/provenance binding,
original/projection hash associations, occurrence/partition validation,
complete authored-root/alias discovery, strict audit validation, bounded
regular-file capture, missing/corrupt original and projected artifact controls,
changed-source refusal and the forbidden alternate-proof control. Its fixed
standard-three axiom policy, 128-root batches, source/artifact limits and finite
300-second per-matrix budget are untouched. No validator, control, budget,
policy, runtime/compiler, profile, auditor, vendor, aggregate, contract, CI or
Cargo file is changed by this lane.

These are actual map construction and current-path coverage proofs. They do
not complete normalization induction, minimization, compiler refinement,
native equivalence or whole-checker verification, and do not change the
implementation's audit status. Genuine post-extension Foundations,
Construction, Order and Map matrices, targeted strict Clippy, Bazel and final
source associations remain integration work.
