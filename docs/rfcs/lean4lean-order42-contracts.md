# Actual comparison and sorted-key proofs (#413)

Base is frozen #412 at `93cf3e681a62fb88650c6d4d5888585856c4e490`.
Claims #414, #415 and #416 were published before source. Existing helper,
vendor/provenance, parent proof, projector/auditor/profile, runtime/compiler,
Cargo and CI sources remain unchanged. The shared fixed runner receives only
a third static packet; both existing packets and all validation stay intact.

## Ownership and resource leases

- #414: NEW `lean/Lean4LeanLevel42/NameOrder.lean` and name report.
- #415: NEW `lean/Lean4LeanLevel42/SortedKeys.lean` and sorted report.
- #416: NEW `tests/lean4lean_order42.rs`, acceptance report and static extension
  of `tests/common/lean4lean_proof_packet.rs`.
- Coordinator: this contract, NEW `lean/Lean4LeanOrder42.lean`, integrated
  evidence, exact audit execution and PR stack management.

Use isolated worktrees. One exclusive Cargo lease, at most two Lean compiler
leases, and immutable versioned dependency/artifact roots. Public ASCII
`theorem` commands for proof roots; no sorry/admit, project axioms,
unsafe/partial or native_decide. Helpers and semantics are the actual frozen
definitions, not replacements. A Lean import or compilation alone is not
the final strict acceptance matrix.

## Actual Name comparator (#414)

Namespace `Vesper.Lean4LeanLevel42`, import frozen `ListProofs`.
Use explicit `Lean.Name.cmp`; the global Name Ord uses `quickCmp` and is not
interchangeable. Required public APIs:

- `name_cmp_refl (a : Name) : Name.cmp a a = .eq`.
- `name_cmp_swap (a b : Name) : Name.cmp a b = (Name.cmp b a).swap`.
- `name_cmp_isLE_trans {a b c : Name}`: isLE transitivity.
- `name_cmp_eq_iff (a b : Name) : Name.cmp a b = .eq ↔ a = b`.
- `name_cmp_lt_trans`: two `.lt` comparisons imply the third.
- `name_cmp_lt_irrefl (a : Name) : Name.cmp a a ≠ .lt`.
- Named instances `nameCmpTrans : Std.TransCmp Name.cmp` and
  `nameCmpLawfulEq : Std.LawfulEqCmp Name.cmp` using proved facts.
- `path_cmp_trans : Std.TransCmp (List.compareLex Name.cmp)` and
  `path_cmp_lawfulEq : Std.LawfulEqCmp (List.compareLex Name.cmp)` via existing
  pinned core lifting, not a duplicated list-order implementation.
- `path_cmp_eq_iff` for explicit `List.compareLex Name.cmp`.

Include actual numeric, string and recursive-prefix comparison witnesses as
public theorem roots. No assumptions about ordering hidden in an axiom or
copied old `Verify.Axioms` library.

## Sorted helper invariants (#415)

Import `NameOrder`, frozen `OrderedInsert` and `VarInsert`. Definitions:

```lean
def KeySorted (p : List Name) : Prop :=
  p.Pairwise (fun a b => Name.cmp a b = .lt)
def VarsSorted (xs : List VarNode) : Prop :=
  xs.Pairwise (fun a b => Name.cmp a.var b.var = .lt)
```

Required public names:

- `keySorted_nodup`: strict keys have no duplicate Names.
- `orderedInsert_sorted`: sorted p and actual result some p' imply sorted p'.
- `orderedInsert_none_iff_mem`: sorted p implies actual none iff v ∈ p.
- `orderedInsert_sorted_nodup`: actual sorted output has no duplicates,
  with no redundant caller nonmembership premise.
- `addVar_sorted`: actual `VarNode.addVar v k` preserves `VarsSorted`.
- `varsSorted_names_nodup`: sorted variable names have no duplicates.
- `addVar_sorted_names_nodup`: corresponding actual output guarantee.

Generic lawful-comparator lemmas are allowed to enable independent work; the
required actual Name statements use the sibling proved instances. VarNode's
derived Ord also compares offsets and is not the name-uniqueness invariant.
Include genuine numeric-key insertion/duplicate detection, numeric VarNode
offset merge, and an explicit unsorted key witness. Do not claim every map
key is internally sorted merely because the outer TreeMap is lawful.

## Fixed genuine acceptance (#416)

Static module closure in dependency order:

1. `Lean4Lean.List`
2. `Lean4Lean.Level`
3. `Lean4LeanLevel42.Semantics`
4. `Lean4LeanLevel42.ListProofs`
5. `Lean4LeanLevel42.OrderedInsert`
6. `Lean4LeanLevel42.VarInsert`
7. `Lean4LeanLevel42.NameOrder`
8. `Lean4LeanLevel42.SortedKeys`
9. `Lean4LeanOrder42`

Use closed `Packet::Order`, module `Lean4LeanOrder42`, projection
`Lean4LeanOrder42Kernel`, label `lean4lean_order42`, scope
`audited_named_order_proofs`, marker `LEAN4LEAN_ORDER42_EVIDENCE`.
Guard all required APIs listed above in the fixed required catalog; discover
and audit the COMPLETE actual authored theorem closure plus ALL actual
aliases, never only the catalog. Preserve every provenance, occurrence,
partition, original/projection hash association, strict standard3 axiom,
real corruption/changed-source/forbidden-alias control and bounded capture.
Keep the finite 300-second per-matrix budget. Rerun all three genuine matrices
after the narrow runner change and run targeted strict Clippy/fmt/Bazel checks.

This packet proves order and list representation invariants only. Map-wide
construction/minimization/normalization, caller path priming, implementation
audit status, compiler refinement and native equivalence remain unproved.
