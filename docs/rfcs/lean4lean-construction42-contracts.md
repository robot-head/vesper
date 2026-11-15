# Exact construction helpers (#408)

This dependent packet keeps #407 frozen. Source starts only after this contract
is pushed and children #409–#411 are published. Existing vendor/provenance,
helper definitions, semantic API, kernel/profile, runtime, Cargo and CI files
remain unchanged. New named proofs are PUBLIC ASCII `theorem` commands;
no sorry/admit, project axiom, unsafe/partial or native_decide escape.

Ownership: #409 owns only `lean/Lean4LeanLevel42/OrderedInsert.lean` and its new
ordered report; #410 only `lean/Lean4LeanLevel42/VarInsert.lean` and vars report;
#411 owns `tests/common/lean4lean_proof_packet.rs`,
`tests/lean4lean_construction42.rs`, the narrow extraction of
`tests/lean4lean_foundations42.rs`, and acceptance report. Root owns this
contract, new aggregate and integrated evidence. Use isolated worktrees,
immutable compiled dependency roots, one exclusive Cargo lease and at most
two active Lean compiler leases.

## Ordered insertion

Namespace `Vesper.Lean4LeanLevel42`, importing the frozen ListProofs/Semantics.
Use the ACTUAL `Lean.Level.Normalize.orderedInsert`; do not replace it.

Required public names/propositions:

- `orderedInsert_some_perm`: for any type/comparator, actual result `some p'`
  implies `p'.Perm (a :: p)`.
- `orderedInsert_some_length`: `p'.length = p.length + 1`.
- `orderedInsert_some_mem`: for Names, `x ∈ p' ↔ x = v ∨ x ∈ p`.
- `orderedInsert_none_mem`: actual `orderedInsert Name.cmp v p = none`
  implies `v ∈ p`, using actual comparator equality reflection.
- `orderedInsert_some_of_not_mem`: a nonmember yields some actual result.
- `orderedInsert_enabled`: result activation is exactly
  `enabled ρ p' = (decide (0 < ρ v) && enabled ρ p)`.
- `orderedInsert_none_positive`: none plus enabled input implies `0 < ρ v`.
- Membership/WF transfer and conditional Nodup from input Nodup plus nonmember.

No sorting premise is needed for these soundness facts. Do not claim an
existing member always produces none on unsorted input: actual numeric Names
0/1 with path `[1,0]` insert another 0 before 1. Include a kernel-checked
duplicate counterexample and valid activation/none cases.

## Variable insertion

Use actual `Lean.Level.Normalize.VarNode.addVar`, same namespace/parent API.
Required public names:

- `addVar_value`: `varsValue ρ (VarNode.addVar v k xs) =
  max (ρ v + k) (varsValue ρ xs)` for arbitrary lists.
- `addVar_names`: `(∃ w ∈ VarNode.addVar v k xs, w.var = x) ↔
  x = v ∨ ∃ w ∈ xs, w.var = x`.
- `addVar_preserves_key`: all input variable names in p, and v in p, imply
  all output variable names in p.
- `node_addVar_value`: for the exact inner record update
  `{n with var := VarNode.addVar v k n.var}`, nodeValue is
  `max (nodeValue ρ p n) (if enabled ρ p then ρ v + k else 0)`.
- `node_addVar_WF`: actual record update preserves NodeWF when v is in p.

No sorting/uniqueness or semantic-conclusion premise is needed. Include actual
numeric-name insertion/merge/repetition cases. These are list/inner-record
facts, not whole-map addVar/addNode/normalizer correctness.

## Shared genuine acceptance

Extract the already-passing fixed-packet runner. Packet specs are static module
bytes, fixed names and metadata; never runtime arbitrary proof modules,
expected acceptance or weakened policy flags. Preserve the parent's complete
8-module/49-root matrix and every existing provenance, original/projection,
partition/occurrence/ALL-alias, standard-axiom, corruption/forbidden-alias and
bounded-capture check. Run both actual matrices after extraction.

The new minimal closure is actual List/Level, existing Semantics/ListProofs,
OrderedInsert/VarInsert and `Lean4LeanConstruction42`. Audit all actual authored
theorems in this closure plus ALL actual aliases, replaying every canonical
declaration. Start with the same finite 300-second per-matrix budget; no guessed
cap or scheduling increase. No external profile/audit-status change or whole
checker/native/compiler equivalence claim follows from this packet.
