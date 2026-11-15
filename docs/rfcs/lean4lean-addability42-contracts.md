# Actual addability foundations and raw reification boundary (#527–#532)

Base: own #526 `fef2b4f83595b121555cb8348e3629c3b31a3dad`.
Before source, all 84 open issue scopes and actual all-pages filenames/heads
of all 46 open PRs were reviewed. Snapshot:
`/tmp/lean4lean-addability42-ownership-20261114T1214.json`, SHA256
`1bf9f040b501723f1e85227bd5c3caeb3590665c42be060fc78b399f524cd38a`.
There is no external conflict. Design metadata remains
2026-11-13T04:46:25.948Z. External source/reference, metatheory, extractor,
runtime/native and Wave 8 files remain read-only.

## Proof lanes and frozen interfaces

All theorem names live in `Vesper.Lean4LeanLevel42`; use the actual unchanged
Lean4Lean definitions and prefix-first `Name.cmp`. Fixed handwritten Lean
modules are allowed. No copied checker/search implementation, supplied
success flag, sorry/admit/project axiom/unsafe/partial/native_decide.

- #528: NEW `SortedSubset.lean`, report `*-subset.md`. Import SortedKeys.
  Public APIs: `subset_sublist {p q} (h : Normalize.subset Name.cmp p q = true) : p.Sublist q`;
  `keySorted_erase (a p) (hp : KeySorted p) : KeySorted (p.erase a)`;
  `subset_complete (p q) (hp : KeySorted p) (hq : KeySorted q) (h : p ⊆ q)`;
  `subset_eq_true_iff (p q) (hp) (hq)`;
  `erase_subset_iff_subset_cons (p acc a) (hp : KeySorted p)`.
  The last theorem relates `p.erase a ⊆ acc` to `p ⊆ a :: acc`.
  Keep actual numerical counterexamples for missing left/right strict sorting
  and duplicate left entries. Existing subset_sound stays unchanged.
- #529: NEW `MapAny.lean`, report `*-any.md`. Import MapValue only.
  Public `norm_any_eq_toList_any (s f)`, `norm_any_eq_true_iff (s f)`,
  `norm_any_witness`, `norm_any_of_lookup`, and
  `norm_any_congr_lookup (s t f) (h : ∀ p, s.get? p = t.get? p)`.
  The central iff is actual `s.any f = true` iff
  `∃ p n, s.get? p = some n ∧ f p n = true`. Follow the actual short-circuit
  forIn/toList traversal. No sorting/WF/physical-map equality premise.
- #530: NEW `Addable.lean`, report `*-addable.md`. Import SortedSubset,
  MapAny and existing MapEquality if needed for actual map-BEq congruence.
  `addable_iff_witness (s a acc)` is unconditional and uses
  `∃ p n k, s.get? p = some n ∧ ⟨a,k⟩ ∈ n.var ∧
  Normalize.subset Name.cmp (p.erase a) acc = true`.
  `addable_iff_subset (s a acc)` replaces the final test by `p ⊆ a :: acc`
  under `∀ p n, s.get? p = some n → KeySorted p` and `KeySorted acc`.
  `addable_mono_sorted_acc` requires sorted old/new accumulators and inclusion,
  but no map-key sorting. `addable_mono_keys` requires sorted map keys and
  sorted new accumulator, but no old-accumulator sorting. Both retain actual
  old addability as premise. `addable_orderedInsert` preserves addability of
  b through actual `(orderedInsert Name.cmp a acc).getD acc` under sorted acc,
  without a map-key premise. `addable_lookup_congr` and `addable_beq_congr`
  reflect actual lookup and Boolean-map equality. Stored offset is arbitrary,
  never silently restricted to zero. Keep genuine sorting/offset witnesses.
- #531: NEW `ReifyCycle.lean`, report `*-cycle.md`. Import only the light
  dependencies actually needed (SortedKeys/MapValue/PathPriming and MapAny
  if useful); avoid heavy Shape/ShapeFinal imports. Formalize the actual
  two-name singleton raw map at `[a,b]` storing a+0 and b+0. Prove its MapWF,
  explicit lookup key/variable sorting and syntactic nonempty properties,
  and `PathPrimed [a,b]`. Check actual failed starts, feasible=false,
  lexChain fallback and actual toTree.reify/mapValue discrepancy at a concrete
  valuation. These are planning targets, not supplied expected answers:
  correct the statement if genuine Lean behavior differs. Do not claim the
  raw map is emitted by normalize. Publish exact theorem names before audit.
- #532: existing shared runner adds only fixed Packet::Addability, plus NEW
  `*-acceptance.md`. Preserve all eight old catalogs/specifications and the
  complete validator suffix byte-for-byte, including source cap 24, batch
  bound 128, 300-second whole-matrix budget, standard-three policy and every
  real refusal control. Root owns NEW `AddabilityFinal.lean` (imports only),
  NEW `tests/lean4lean_addability42.rs`, narrow BUILD additions and integration
  report. Preserve endpoint resource_oracle42 registration when merging main.

Fixed runner IDs: label `lean4lean_addability42`, source module
`Lean4LeanLevel42.AddabilityFinal`, projection `Lean4LeanAddability42Kernel`,
scope `audited_addability_foundations_and_raw_reification_boundary`, marker
`LEAN4LEAN_ADDABILITY42_EVIDENCE`. The actual transitive closure must fit
within the existing cap; no arbitrary modules or weaker-policy mode.

## Boundaries and execution

Sorted subset completeness and addability monotonicity prepare a later
greedy-completeness proof. The raw cycle demonstrates that sorted/WF/nonempty
rows and current PathPrimed coverage do not establish an admissible ordering.
This wave does not prove feasible completeness, fallback unreachability,
canonicality, reification correctness on normalized maps, opaque Level.beq
reflection, whole-checker/native equivalence or a successful source campaign.

Use isolated worktrees and complete immutable private dependency copies.
At most two Lean compiler leases and one Cargo/shared-target lease may run.
Lean is pinned 4.24.0, `-j1 -M4096`, LEAN_NUM_THREADS=1. Preserve all genuine
failures, original/projected source/object identities and tool/transcript
associations. Audit every actual public authored root and alternate alias;
run the new packet and all eight previous complete matrices. Local runtime
compiler/Bazel work is separately coordinated and must not add a third Lean
compiler. Own stack targets unprotected #526; no auto-merge there.
