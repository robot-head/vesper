# Actual recursive normalization acceptance (#428)

The Normalize entry point and fifth static packet are authored. This lane ran
direct rustfmt and static source checks only. It did not execute Cargo, Lean,
any of the five genuine acceptance matrices, targeted strict Clippy or Bazel.
Those integration checks remain pending with the coordinator; this report does
not claim they passed.

`tests/lean4lean_normalize42.rs` calls the existing shared runner through the
closed `Packet::Normalize` choice. Its fixed specification selects
`Lean4LeanNormalize42`, projects `Lean4LeanNormalize42Kernel`, and uses label
`lean4lean_normalize42`, scope
`audited_pre_subsumption_normalization_proofs` and marker
`LEAN4LEAN_NORMALIZE42_EVIDENCE`. There is no runtime module selector,
supplied expected answer or alternative acceptance policy.

The static closure contains exactly fourteen modules in contracted dependency
order: `Lean4Lean.List`, `Lean4Lean.Level`, `Lean4LeanLevel42.Semantics`,
`Lean4LeanLevel42.ListProofs`, `Lean4LeanLevel42.OrderedInsert`,
`Lean4LeanLevel42.VarInsert`, `Lean4LeanLevel42.NameOrder`,
`Lean4LeanLevel42.MapValue`, `Lean4LeanLevel42.MapHelpers`,
`Lean4LeanLevel42.PathPriming`, `Lean4LeanLevel42.LevelSemantics`,
`Lean4LeanLevel42.NormalizeInvariants`, `Lean4LeanLevel42.NormalizeValue`
and the coordinator-owned `Lean4LeanNormalize42` aggregate. `SortedKeys` and
`Subsumption` remain outside this closure. The unchanged actual import-closure
check requires every selected source to enter the replayed artifact closure.

The 30 unique required theorem names guard all 22 contracted APIs: seventeen
structural evaluator, size and arithmetic facts, three recursive invariant
facts and two exact construction-value facts. The proof authors confirmed
these names and the eight actual witness names:

- `normalizeAux_imax_max_witness`
- `normalizeAux_imax_imax_witness`
- `normalizeAux_member_zero_witness`
- `normalizeAux_member_positive_witness`
- `normalizeAux_disabled_witness`
- `normalizeAux_retains_old_witness`
- `normalizeAux_primed_duplicate_witness`
- `normalizeAux_succ_mvar_counterexample`

Required names only guard presence. The unchanged source-command extraction
derives every public authored theorem in the actual selected helper closure,
including additional helpers absent from the guard catalog. The unchanged
root union adds every actual alternate-proof alias. The complete union is
audited in the existing 128-root batches without a golden root or alias count.

Static byte comparison against base
`08a260ca2476616307508634bcbf60a4bec11008` confirmed all four original
required arrays and all four original complete Packet specifications remain
identical. Array sizes for Foundations, Construction, Order and Map are
1,242, 1,655, 1,397 and 1,444 bytes; complete specification arms are 795,
776, 821 and 919 bytes. The complete runner from `const POLICY` through EOF
is byte-identical: 47,588 bytes, SHA-256
`2162a7e23f7de857d06542325e26be039d9e589ea117f6a93a7f62906b4da0b9`.
Static checks also confirmed the exact fourteen-module order, all five fixed
Normalize identifiers, the exact thirty unique namespace-qualified guards,
and the Normalize test entry point. Reversing only the added enum variant,
required array, specification arm and introductory four-to-five comment
restored the base runner byte-for-byte. Direct
`rustfmt --edition 2024 --check tests/common/lean4lean_proof_packet.rs tests/lean4lean_normalize42.rs`
and `git diff --check` passed.

The unchanged runner retains actual source/provenance binding,
original/projection hash associations, occurrence/partition validation,
complete authored-root/alias discovery, strict audit validation, bounded
regular-file capture, missing/corrupt original and projected artifact controls,
changed-source refusal and the forbidden alternate-proof control. Its fixed
standard-three axiom policy, source/artifact limits and finite 300-second
per-matrix budget are untouched. This lane changes only the new test, this
report and the narrow fifth static packet extension.

This packet concerns actual `Normalize.normalizeAux` construction before
subsumption. Its structural `NoMVars` domain excludes the metavariable
counterexample; it does not claim correctness for the totalized evaluator
outside that domain. It does not establish full `Normalize.normalize`,
minimization, compiler refinement, native equivalence or whole-checker
verification, and does not change the implementation's audit status. Genuine
post-extension Foundations, Construction, Order, Map and Normalize matrices,
targeted strict Clippy, Bazel and final source associations remain integration
work.
