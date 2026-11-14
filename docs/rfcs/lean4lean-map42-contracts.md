# Actual map construction and current-path coverage (#418)

Frozen parent is #417 at `ed1efa3bdad3915464ede023611a23591391f008`.
Claims #419–#422 precede source. No existing helper/vendor/provenance or parent
proof, projector/auditor/profile, runtime/compiler, Cargo or CI changes.
This is actual map construction, not full normalization/minimization.

Ownership: #419 owns NEW `MapValue.lean` and value report; #420 NEW
`MapHelpers.lean` and helpers report; #421 NEW `PathPriming.lean` and priming
report; #422 NEW `tests/lean4lean_map42.rs`, acceptance report and fourth static
Packet arm in `tests/common/lean4lean_proof_packet.rs`. The coordinator owns
this contract, NEW aggregate `lean/Lean4LeanMap42.lean`, integrated evidence
and all final execution. All proof files are under `lean/Lean4LeanLevel42/`;
all reports are NEW `docs/rfcs/lean4lean-map42-*.md`.

Use isolated worktrees, immutable versioned dependency/artifact roots and
verified exact source/compiled-parent correspondence. Exclusive Cargo lease;
at most two Lean compiler leases. #419 starts with lease1, #420 lease2 when
its dependencies are ready; #421 may draft but needs explicit coordinator
grant before compiling. No other lane runs Cargo. Fixed handwritten Lean
is allowed; generated Lean uses vesper-lean-syntax AST and one render, never
string-formatted Lean. All proof helpers are PUBLIC ASCII `theorem` commands.
No sorry/admit, project axiom, unsafe/partial or native_decide escape.

## Actual map denotation (#419)

Import frozen `NameOrder`, `Semantics`/`ListProofs` and pinned Std TreeMap
lemmas as needed. Namespace `Vesper.Lean4LeanLevel42`. Actual `NormLevel`
captures `List.compareLex Name.cmp` via the vendor's local Ord Name; unqualified
global Name `compare` is not interchangeable. Use the proved actual comparator
instances and existing Std lookup/fold/list lemmas, not a replacement map.

Definitions:

```lean
def mapValue (ρ : Valuation) (s : NormLevel) : Nat :=
  s.foldl (fun z p n => max z (nodeValue ρ p n)) 0
def MapWF (s : NormLevel) : Prop :=
  ∀ p n, s.get? p = some n → NodeWF p n
```

Required public APIs (additional genuine helper lemmas allowed):

- `mapValue_empty (ρ) : mapValue ρ ({} : NormLevel) = 0`.
- `nodeValue_le_mapValue`: actual lookup `s.get? p = some n` implies
  `nodeValue ρ p n ≤ mapValue ρ s`.
- `mapValue_le`: if every actually stored row's nodeValue is ≤ b, mapValue is
  ≤ b. This is a generic local fold bound, not an assumed map-update result.
- `mapValue_mono`: pointwise stored-row domination at the same keys implies
  mapValue domination; choose explicit existential matching lookup/row bound.
- `mapValue_insert`:
  `mapValue ρ (s.insert p n) =
   max (mapValue ρ (s.erase p)) (nodeValue ρ p n)`.
- `mapValue_decompose`:
  `mapValue ρ s = max (mapValue ρ (s.erase p))
    (((s.get? p).map (nodeValue ρ p)).getD 0)`.
- `mapWF_empty : MapWF ({} : NormLevel)`.

The erased-key context is essential: insert overwrites. Do not replace these
claims by union with an unchanged map containing the old row. No NodeWF or
sorted-key assumptions are needed for the generic denotation facts. Publish
exact signatures early for dependent lanes; no source API drift without
coordinator/dependent agreement.

## Actual map updates (#420)

Import `MapValue` and frozen `VarInsert` (plus needed list helpers).
Use ACTUAL `NormLevel.addNode`, `.addVar`, `.addConst`. Required APIs:

- `map_addNode_value`: for arbitrary ρ, v, k, p, s,
  `mapValue ρ (s.addNode v k p) =
   max (mapValue ρ s) (if enabled ρ p then ρ v + k else 0)`.
- `map_addVar_value`: the same equation for actual `s.addVar v k p`, with
  explicit actual key presence `s.get? p = some n`.
- `map_addConst_value`: the EXACT unconditional equation, retaining the
  actual skip condition `k = 0 || k = 1 && !p.isEmpty`; in its true branch
  the result is old mapValue, otherwise `max old (if enabled ρ p then k else 0)`.
- `map_addConst_value_of_covered`: the simplified max equation given
  `k = 1 → p ≠ [] → enabled ρ p = true → 1 ≤ mapValue ρ s`.
  #421 will discharge this specific skip-bound premise syntactically.
- `map_addNode_WF` / `map_addVar_WF`: MapWF preserved if `v ∈ p`.
- `map_addConst_WF`: MapWF preserved unconditionally.

Include actual public theorem counterexamples: a missing-key modify leaves
map value zero despite a positive variable contribution, and skipped-one
insertion into an empty/WF zero-node map loses the intended gated constant
without coverage. Do not pretend missing-key addVar inserts a row, or NodeWF
alone justifies skipping one. Conditional theorem premises must not assume
the entire desired semantic conclusion. Publish helper APIs needed by #421;
additional actual lookup/variable-witness preservation lemmas are useful.

## Syntactic current-path priming (#421)

Import `MapHelpers` and frozen `OrderedInsert`. Definition:

```lean
def PathPrimed (p : List Name) (s : NormLevel) : Prop :=
  (p ≠ [] → ∃ n, s.get? p = some n) ∧
  ∀ v ∈ p, ∃ q n w, s.get? q = some n ∧ q ⊆ p ∧
    w ∈ n.var ∧ w.var = v
```

This is a syntactic witness for EACH path variable, not merely one positive
edge, a semantic correctness flag, or a sortedness assertion. MapWF is
separate. Required APIs:

- `pathPrimed_empty`: PathPrimed [] s for arbitrary s.
- `pathPrimed_variable_bound`: PathPrimed p s, v ∈ p and enabled ρ p = true
  imply `ρ v ≤ mapValue ρ s`.
- `pathPrimed_one_bound`: PathPrimed p s, p ≠ [] and enabled ρ p = true imply
  `1 ≤ mapValue ρ s`.
- `pathPrimed_addConst`, `pathPrimed_addNode`, `pathPrimed_addVar`: the existing
  PathPrimed p s is preserved by each corresponding actual update at ANY q
  (with arbitrary v/k); updates never erase rows or existing variable names.
- `pathPrimed_extend`: PathPrimed p s and actual
  `orderedInsert Name.cmp v p = some p'` imply
  `PathPrimed p' (s.addNode v k p')`. No sorting premise is necessary.
- `map_addConst_value_of_primed`: discharge #420's skip-bound premise and
  prove the actual simplified gated-constant max equation from PathPrimed.

Include a real two-variable counterexample to insufficient one-edge coverage:
present key [u,v] with only v+0, enabled valuation with ρu > ρv. One present
edge can justify skipped constant one but not the `.param u`, k=0 branch.
Include a genuine newly extended-path priming witness. These facts support
future normalizeAux induction; do not claim that induction is complete.

## Fixed genuine acceptance (#422)

Exact minimal static closure in dependency order:
`Lean4Lean.List`, `Lean4Lean.Level`, `Lean4LeanLevel42.Semantics`,
`Lean4LeanLevel42.ListProofs`, `Lean4LeanLevel42.OrderedInsert`,
`Lean4LeanLevel42.VarInsert`, `Lean4LeanLevel42.NameOrder`,
`Lean4LeanLevel42.MapValue`, `Lean4LeanLevel42.MapHelpers`,
`Lean4LeanLevel42.PathPriming`, `Lean4LeanMap42`.
SortedKeys and Subsumption are intentionally outside this imported closure;
all their existing packet specifications stay fixed.

Closed `Packet::Map`, label `lean4lean_map42`, module `Lean4LeanMap42`,
projection `Lean4LeanMap42Kernel`, scope `audited_named_map_construction_proofs`,
marker `LEAN4LEAN_MAP42_EVIDENCE`. Add fixed guards for all contracted APIs
and chosen real witnesses; derive and audit the COMPLETE actual authored
closure plus ALL actual aliases, never only the guard catalog.

All three old required arrays/specifications and the complete runner from
`const POLICY` onward remain byte-identical. Preserve every original/
projection, source/provenance, occurrence/partition/alias, axiom, bounded
capture, missing/corrupt/changed-source/forbidden-alias gate and the finite
300-second per-matrix budget. No arbitrary-module, expected-answer or
weaker-policy mode. Coordinator runs ALL FOUR actual matrices after extension
plus targeted strict Clippy, formatting/Bazel/whitespace/source associations.
No guessed cap or scheduling increase, no profile audit-status change.
