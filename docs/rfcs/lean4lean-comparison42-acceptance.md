# Actual level-comparison acceptance (#448)

The Comparison entry point, seventh static packet and its BUILD label/select
are authored. This lane ran direct rustfmt, static byte/source assertions,
BUILD synchronization and whitespace checks. It ran no Cargo or Lean command,
genuine acceptance matrix, strict Clippy or Bazel execution. All seven genuine
matrices and final integration checks remain coordinator work; this report
does not claim they passed.

`tests/lean4lean_comparison42.rs` calls the shared runner through the closed
`Packet::Comparison` choice. Its fixed specification selects
`Lean4LeanComparison42`, projects `Lean4LeanComparison42Kernel`, and uses
label `lean4lean_comparison42`, scope `audited_level_comparison_soundness`
and marker `LEAN4LEAN_COMPARISON42_EVIDENCE`. It adds no runtime proof/module
selector, supplied expected answer or alternative acceptance policy.

The static closure contains exactly twenty-two modules in the contracted
source order: `Lean4Lean.List`, `Lean4Lean.Level`,
`Lean4LeanLevel42.Semantics`, `Lean4LeanLevel42.ListProofs`,
`Lean4LeanLevel42.Subsumption`, `Lean4LeanLevel42.OrderedInsert`,
`Lean4LeanLevel42.VarInsert`, `Lean4LeanLevel42.NameOrder`,
`Lean4LeanLevel42.MapValue`, `Lean4LeanLevel42.MapHelpers`,
`Lean4LeanLevel42.PathPriming`, `Lean4LeanLevel42.LevelSemantics`,
`Lean4LeanLevel42.NormalizeInvariants`, `Lean4LeanLevel42.NormalizeValue`,
`Lean4LeanLevel42.NodeResidual`, `Lean4LeanLevel42.MapMinimize`,
`Lean4LeanLevel42.MapSubsumption`, `Lean4LeanLevel42.NormalizeFinal`,
`Lean4LeanLevel42.MapAll`, `Lean4LeanLevel42.CompareFold`,
`Lean4LeanLevel42.MapComparison`, and the coordinator-owned
`Lean4LeanComparison42` aggregate. Previous aggregates and `SortedKeys`
are outside this closure. The unchanged actual import-closure check requires
every selected source to enter the replayed artifact closure.

The twelve unique namespace-qualified guards select the exact contracted
APIs and witnesses in `Vesper.Lean4LeanLevel42`:

- Actual map traversal: `norm_all_eq_toList_all`, `norm_all_eq_true_iff`,
  `norm_all_lookup`.
- Actual Option comparison fold: `compareStep_none_bound`,
  `compareStep_some_union`, `compareFold_none_bound`.
- Soundness: `norm_le_sound`, `geq_sound`.
- Witnesses: `norm_le_needs_right_WF_counterexample`,
  `norm_le_split_dominators`, `norm_le_raw_empty_row_incomplete`,
  `geq_split_dominators`.

Required names only guard presence. The unchanged source-command extractor
derives every actual public authored theorem from the selected helper
closure, including helpers absent from the guard catalog. The unchanged root
union adds every actual alternate-proof alias. The existing 128-root batches
audit that complete union without a golden theorem count or alias count.

Static byte comparison against base
`ec327ef6f52bc317f9ea0045f724067d3ca13f6c` confirmed all six older required
arrays and complete Packet specification arms remain identical. Including
their terminating newline, the respective byte counts are:

| Prior packet | Required array | Complete specification arm |
| --- | ---: | ---: |
| Foundations | 1,243 | 796 |
| Construction | 1,656 | 777 |
| Order | 1,398 | 822 |
| Map | 1,445 | 920 |
| Normalize | 1,668 | 1,139 |
| Normalization | 1,293 | 1,426 |

The complete runner from `const POLICY` through EOF remains byte-identical:
47,588 bytes, SHA-256
`2162a7e23f7de857d06542325e26be039d9e589ea117f6a93a7f62906b4da0b9`.
Reversing only the new enum variant, required array, specification arm and
introductory six-to-seven comment restores the base runner byte-for-byte.
Static assertions also passed for the exact twenty-two-module order, all
five identifiers, the exact twelve unique namespace-qualified guards and the
Comparison test entry point. The full-normalization packet is byte-frozen.

`BUILD.bazel` differs solely by the new `test/lean4lean_comparison42` entry
in `copy_test_cwd` and its matching #443 compatibility select. Removing those
two additions restores the base BUILD byte-for-byte. The select excludes
only macOS scheduling of this duplicate logical replay; its default branch
is empty and all existing labels, attributes, timeouts and selectors remain
unchanged. The test uses the existing strict regular-input copy mode. No
Rust/Lean check, semantic assertion, policy or internal budget is altered.

The following lane checks passed:

- `rustfmt --edition 2024 --check tests/common/lean4lean_proof_packet.rs tests/lean4lean_comparison42.rs`
- `python3 tools/bazel/sync_build_files.py --check`
- `git diff --check`
- Static byte restoration, prior-array/specification equality, exact packet
  identifiers, module order, required guards and test-entry assertions.

The unchanged validator retains all actual source/provenance binding,
original/projection artifact associations, occurrence/partition validation,
complete root/alias discovery, strict audit checks, regular-file capture,
missing/corrupt original and projected artifact controls, changed-source
refusal and the forbidden alternate-proof control. The standard-three axiom
policy, every source/artifact cap, existing 128-root batching and finite
300-second per-matrix budget are unchanged.

The intended result is soundness of the actual `NormLevel.le`, requiring WF
only for the right map, and actual `Lean.Level.geq'`, with structural
`NoMVars` assumptions on both levels. This packet makes no comparison
completeness, canonicality, reification, whole-checker correctness or
compiler/native equivalence claim. It introduces no bridge for the opaque
extern `Level.beq` and makes no unconditional `isEquiv'` soundness claim.
The implementation's proof-audit status remains unchanged. Genuine
post-extension Foundations, Construction, Order, Map, Normalize,
Normalization and Comparison matrices, strict targeted Clippy, repository
formatting, Bazel and final source associations remain coordinator work.
