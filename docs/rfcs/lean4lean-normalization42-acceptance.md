# Actual normalization-map acceptance (#439)

The Normalization entry point, sixth static packet and one BUILD runfiles label
are authored. This lane ran direct rustfmt and static source checks only. It
ran no Cargo or Lean command, genuine acceptance matrix, strict Clippy or
Bazel check. All six genuine matrices and integration checks remain pending
with the coordinator; this report does not claim they passed.

`tests/lean4lean_normalization42.rs` calls the unchanged shared runner through
the closed `Packet::Normalization` choice. Its fixed specification selects
`Lean4LeanNormalization42`, projects `Lean4LeanNormalization42Kernel`, and
uses label `lean4lean_normalization42`, scope
`audited_normalization_map_denotation` and marker
`LEAN4LEAN_NORMALIZATION42_EVIDENCE`. It adds no runtime module selector,
supplied expected answer or alternative acceptance policy.

The static closure contains exactly nineteen modules in contracted dependency
order: `Lean4Lean.List`, `Lean4Lean.Level`, `Lean4LeanLevel42.Semantics`,
`Lean4LeanLevel42.ListProofs`, `Lean4LeanLevel42.Subsumption`,
`Lean4LeanLevel42.OrderedInsert`, `Lean4LeanLevel42.VarInsert`,
`Lean4LeanLevel42.NameOrder`, `Lean4LeanLevel42.MapValue`,
`Lean4LeanLevel42.MapHelpers`, `Lean4LeanLevel42.PathPriming`,
`Lean4LeanLevel42.LevelSemantics`, `Lean4LeanLevel42.NormalizeInvariants`,
`Lean4LeanLevel42.NormalizeValue`, `Lean4LeanLevel42.NodeResidual`,
`Lean4LeanLevel42.MapMinimize`, `Lean4LeanLevel42.MapSubsumption`,
`Lean4LeanLevel42.NormalizeFinal`, and the coordinator-owned
`Lean4LeanNormalization42` aggregate. Previous aggregates and `SortedKeys`
are outside this imported closure. The unchanged actual import-closure check
requires every selected source to enter the replayed artifact closure.

The 23 unique required theorem names guard all eighteen contracted APIs and
five witnesses. The proof authors confirmed these public ASCII names, each
in namespace `Vesper.Lean4LeanLevel42`:

- Residual structure and self context: `subsumeVars_sublist`,
  `node_subsumeBy_var_sublist`, `node_subsume_var_sublist`, `node_subsume_WF`,
  `node_isEmpty_value_zero`, `node_subsume_vars_union`,
  `node_subsume_same_context`, and `self_context_needs_vars_counterexample`.
- Fixed-map minimization: `minimize_var_sublist`, `minimize_WF`,
  `minimize_vars_context`, `minimize_value_context`, and
  `minimize_missing_lookup_counterexample`.
- Actual replacement and outer fold: `minimizeEntry_value`,
  `minimizeEntry_WF`, `minimizeEntry_get_other`, `subsumption_value`,
  `subsumption_WF`, `subsumption_needs_WF_counterexample`, and
  `subsumption_erases_dominated_row`.
- Final actual normalization: `normalize_value`, `normalize_WF`, and
  `normalize_value_witness`.

Required names only guard presence. The unchanged source-command extraction
derives every actual public authored theorem in the selected helper closure,
including additional helpers absent from the guard catalog. The unchanged
root union adds all actual alternate-proof aliases. The complete union is
audited in the existing 128-root batches without a golden root or alias count.

Static byte comparison against base
`68b159795811f2b03fe26cc0de1f7a7516d6a843` confirmed that all five original
required arrays and complete Packet specification arms remain identical.
Measured including their terminating newline, array sizes for Foundations,
Construction, Order, Map and Normalize are 1,243, 1,656, 1,398, 1,445 and
1,668 bytes; complete specification arm sizes are 796, 777, 822, 920 and
1,139 bytes. The complete runner from `const POLICY` through EOF is
byte-identical: 47,588 bytes, SHA-256
`2162a7e23f7de857d06542325e26be039d9e589ea117f6a93a7f62906b4da0b9`.
Reversing only the new enum variant, required array, specification arm and
introductory five-to-six comment restores the base runner byte-for-byte.

Static assertions also passed for the exact nineteen-module order, all five
fixed identifiers, the exact 23 unique namespace-qualified guards and the
Normalization test entry point. `BUILD.bazel` differs from the base solely by
the single `test/lean4lean_normalization42` addition to `copy_test_cwd`; no
other label or attribute changes. This uses the existing strict regular-input
copy mode. Direct
`rustfmt --edition 2024 --check tests/common/lean4lean_proof_packet.rs tests/lean4lean_normalization42.rs`
and `git diff --check` passed.

The unchanged runner retains actual source/provenance binding,
original/projection hash associations, occurrence/partition validation,
complete authored-root/alias discovery, strict audit validation, bounded
regular-file capture, missing/corrupt original and projected artifact
controls, changed-source refusal and the forbidden alternate-proof control.
Its fixed standard-three axiom policy, source/artifact limits and finite
300-second per-matrix budget are untouched. This lane changes only the new
test, this report, the narrow sixth static packet extension and the single
BUILD label.

This packet concerns denotation and well-formedness of the actual unchanged
`Normalize.normalize`, through fixed-map minimization and actual subsumption.
`normalize_value` has the contracted structural `NoMVars` domain. It does
not establish cached-domain or canonicality properties, `.le`/reification,
whole-checker verification, compiler refinement or native equivalence; the
implementation's proof-audit status is unchanged. Genuine post-extension
Foundations, Construction, Order, Map, Normalize and Normalization matrices,
targeted strict Clippy, repository fmt, Bazel, whitespace and final source
associations remain coordinator integration work. The inherited Kinetic
1.99 Clippy compatibility dependency remains separately tracked by #434.
