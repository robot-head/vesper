# Strict sorted-key helper invariants (#415)

This lane follows the frozen order42 contract at `3f6a8c6` and adds only
`lean/Lean4LeanLevel42/SortedKeys.lean` and this report. It imports the sibling
proved Name comparator instances and frozen `OrderedInsert` and `VarInsert`.
All helper calls use the actual pinned definitions. No comparator, insertion
algorithm, existing proof, vendor source, runtime, compiler, profile or Cargo
source is replaced or changed.

`KeySorted p` means `p.Pairwise (fun a b => Name.cmp a b = .lt)`.
`VarsSorted xs` means strict pairwise comparison of `a.var` and `b.var` by
explicit `Name.cmp`. Offsets do not participate. In particular, the derived
`VarNode` ordering, which also compares offsets, cannot substitute for this
name uniqueness invariant.

The seven required public ASCII theorem roots establish:

- `keySorted_nodup`: strict keys contain no duplicate Names.
- `orderedInsert_sorted`: a successful actual insertion into a strict key
  returns a strict key.
- `orderedInsert_none_iff_mem`: on strict input, actual `none` is equivalent
  to prior membership, including recursive duplicate detection.
- `orderedInsert_sorted_nodup`: successful insertion into a strict key returns
  a list without duplicates, with no caller nonmembership premise.
- `addVar_sorted`: actual insertion and maximum-offset merging preserve
  strict variable-name order.
- `varsSorted_names_nodup`: the names projected from a strict variable list
  contain no duplicates.
- `addVar_sorted_names_nodup`: names in the actual output satisfy that same
  uniqueness guarantee.

The output-order proofs follow the actual less/equal/greater branches. They
use the sibling's proved `Std.TransCmp Name.cmp` and `Std.LawfulEqCmp Name.cmp`
instances. Their public statements have no comparator-law assumptions.
The variable proof uses the frozen exact output-name characterization
`addVar_names`, so its greater branch also covers the actual recursive result.

Four additional public, kernel-checked witnesses exercise genuine computations:

- `orderedInsert_numeric_sorted` inserts numeric name one between zero and
  two, and proves strict input and output order.
- `orderedInsert_numeric_duplicate` traverses past zero and detects the
  existing numeric name two.
- `addVar_numeric_sorted_merge` merges offset five into the existing
  zero-name offset two, retains the one-name offset seven, and proves output
  order and projected-name uniqueness.
- `orderedInsert_unsorted_key_witness` proves `[one, zero]` is unsorted and
  that actual zero insertion returns the unsorted `[zero, one, zero]`.

Sortedness is a real precondition. These facts do not assert that every key
inside the outer lawful TreeMap is internally sorted. Whole-map construction,
minimization, normalization, caller path priming, checker correctness,
implementation audit status, compiler refinement and native equivalence remain
outside this packet.

## Actual compilation and artifact associations

Before reuse, every published BLAKE3 source/object record in both immutable
parent roots `/tmp/lean4lean-construction42-ordered-v2` and
`/tmp/lean4lean-construction42-vars-v1` matched. Every dependency source also
byte-matched the frozen worktree or vendored upstream source. The sibling
`NameOrder.lean` and `.olean` were copied byte-for-byte from
`/tmp/lean4lean-order42-name-v3`, with its source also matched to the sibling
worktree. Independent overlays cannot supply two modules under the same Lean
directory, so a new coherent immutable consumer root was assembled.

The complete final source compiled successfully with exit zero, empty output
and no warnings in 0.822 seconds under pinned Lean 4.24.0, commit
`797c613eb9b6d4ec95db23e3e00af9ac6657f24b`, Release:

```text
cwd=/tmp/lean4lean-order42-sorted-v1
LEAN_PATH=/tmp/lean4lean-order42-sorted-v1
LEAN_NUM_THREADS=1
/home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 -o Lean4LeanLevel42/SortedKeys.olean Lean4LeanLevel42/SortedKeys.lean
```

All parent and sibling source/object copies still byte-matched after this
compile. The final source byte-matches the owned module. The consumer root's
`identities.json` records all sixteen source/object files with byte counts,
BLAKE3 and SHA256 associations. The compiler lease was released after this
successful compile. No Cargo command ran in this lane.

| File | Bytes | BLAKE3 |
| --- | ---: | --- |
| `SortedKeys.lean` | 6772 | `14434daec51ad1b88066bb9bcc6a3e92533d8b2d341cd59d6957d3b5350b5e2a` |
| `SortedKeys.olean` | 153104 | `fd3143e780507e97555676f446f624fcf65db4a9de290258870c25a8bc670774` |

The corresponding SHA256 values are
`be0e9a9c7fffd9de49a231c7771618a04ecc78652627b442534b8150877baf38`
and `9b820f531bc66e6b19a8cf76cc2d566cd22fd0b181ec962687ea24bbc2723cde`.

All eleven authored proofs are public ASCII `theorem` commands. The module
contains no placeholders, project axioms, unsafe or partial declarations, or
native evaluation proof shortcuts. `git diff --check` passed. The complete
packet projection, all-public-root and all-alias associations, strict standard
axiom audit, corruption controls, and genuine three-matrix acceptance remain
the integration lane's gate; this source report does not claim those ran here.
