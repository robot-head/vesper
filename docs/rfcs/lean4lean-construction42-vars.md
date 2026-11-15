# Exact variable construction helpers (#410)

This child of #408 follows pushed contract `05aaeac`. It adds only
`lean/Lean4LeanLevel42/VarInsert.lean` and this report. The imported parent
semantics, list proofs, actual vendor/helper sources and all existing
kernel/profile, runtime, Cargo and CI sources remain unchanged.

The proofs concern the actual pinned `Lean.Level.Normalize.VarNode.addVar`
and the exact inner record update `{n with var := VarNode.addVar v k n.var}`.
They do not introduce a replacement algorithm or make a whole-map claim.

`addVar_value` proves
`varsValue ρ (VarNode.addVar v k xs) = max (ρ v + k) (varsValue ρ xs)`
for every input list. The proof follows the actual comparator branches:
insertion contributes the new value, merging uses actual `Name.cmp` equality
reflection and maximum offsets, and recursion preserves the old head.
No sorting, uniqueness, key well-formedness or semantic-conclusion premise
is required.

`addVar_names` characterizes output names exactly as the union of the inserted
name and input names. Thus `addVar_preserves_key` transfers the condition-key
membership invariant when the input names and inserted name belong to the key.
These statements allow repeated names and arbitrary list order.

`node_addVar_value` gives the value of the exact inner Node record update:
it is the maximum of the original node value and the newly inserted variable
value when the key is enabled, and zero when disabled. `node_addVar_WF` proves
that record update preserves `NodeWF` when the inserted name belongs to the key.
Neither statement asserts the behavior of `NormLevel.addVar`, `addNode` or
any whole-map update.

Four public kernel-checked numeric-name examples exercise the actual function:

- `numeric_name_insert_before` inserts name `0` before name `1`.
- `numeric_name_insert_after` recursively inserts name `1` after name `0`.
- `numeric_name_merge` merges equal numeric names, checking both a larger and
  a smaller newly supplied offset.
- `numeric_name_repeated_unsorted` inserts `0` into the unsorted list `[1,0]`,
  retaining the trailing duplicate. Repeating insertion merges the new front
  entry, while the old trailing entry remains. A concrete valuation checks
  the resulting maximum value eight.

All nine named proofs are PUBLIC ASCII `theorem` commands. There are no
`private`, `local`, `lemma`, `sorry`, `admit`, project axiom, `unsafe`, `partial`
or `native_decide` declarations. The concrete examples use kernel-checked
reflexivity of the actual helper results.

## Actual validation

All nine theorems freshly compiled with zero errors or warnings using the
actual pinned `/home/agent/.local/share/vesper-lean/bin/lean`, Lean 4.24.0,
commit `797c613eb9b6d4ec95db23e3e00af9ac6657f24b`, Release. The granted compiler
lease used `LEAN_NUM_THREADS=1`, `-j1 -M4096`, `-R lean` and the immutable
`/tmp/lean4lean-foundations42-semantics-v2` dependency root. Before compiling,
all eight published parent source/object BLAKE3 identities and both authored
dependency source files were verified against that root and the worktree.

The immutable complete consumer root
`/tmp/lean4lean-construction42-vars-v1` includes those unchanged dependencies
and an `identities.json` manifest with the new source/object identities:

| File | Bytes | BLAKE3 |
| --- | ---: | --- |
| `Lean4LeanLevel42/VarInsert.lean` | 4528 | `5f31e594e0fc28a7e95daaa39c59a6729cb53810f58d1b5b93b35885cc0512e5` |
| `Lean4LeanLevel42/VarInsert.olean` | 755736 | `9e8025149303133615855affa21452577e698c4ca81b0155ceaaf0777aecfd3d` |

`git diff --check` also passed. The compiler lease was released after the
successful compile. This lane ran no Cargo command. The separate complete
snapshot/public-theorem acceptance matrices remain the integration lane's work.

This packet establishes list/inner-record helper facts. It does not establish
whole-map construction, normalization or checker correctness, native/compiler
equivalence or whole-checker self-verification. No external profile/audit-status
claim changes follow from these results.
