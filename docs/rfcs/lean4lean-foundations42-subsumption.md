# Actual normalizer node subsumption (#404)

This packet proves facts about the exact pinned `Lean.Level.Normalize.Node.subsumeBy`
and `Node.subsume` implementations imported from the unchanged vendored Level/List
sources. It is a child of #402 and follows the pushed foundations contract.
Only `lean/Lean4LeanLevel42/Subsumption.lean` and this report belong to this lane.

`node_subsumeBy_union` handles either value of `same`. Its only structural
premises are `p2 ⊆ p1` and `NodeWF p2 n2`; the left node need not be well formed,
and no ordering, uniqueness or sorting premise is used. An enabled left key
makes the right key enabled. Right-node well-formedness then makes every right
variable positive, supplying the `offset + 1` bound needed when the actual helper
removes a constant using a variable. The variable part uses `subsumeVars_union`
for the actual recursive helper. The disabled-left case follows directly from
the semantics.

`node_subsume_union` proves the wrapper's max-union property from `NodeWF p2 n2`
alone: a successful actual `subset Name.cmp` test supplies inclusion, and a
failed test leaves the node unchanged. The file-local upstream `Ord Name`
instance is handled by the implementation's definitionally explicit `Name.cmp`;
no global replacement instance is introduced.

`node_subsumeBy_self` separately proves the stronger equality
`nodeValue ρ p (n.subsumeBy true n) = nodeValue ρ p n` under `NodeWF p n`.
The same-key branch retains variables, and its removed constant is bounded by
those variables themselves. `node_subsume_self` lifts that equality to the
actual public wrapper. These are not obtained by cancellation from max-union.

Five public computational theorems exercise actual output fields and values:

- `invalid_wf_counterexample`: a constant one and a zero-valued variable under
  an empty key have original union one; both the helper and public wrapper
  erase the constant and produce union zero. The right node explicitly fails
  `NodeWF`. Thus dropping that premise would be unsound.
- `enabled_variable_removal`: at enabled keys `[u,v]` and `[u]`, the variable
  `u+1` at valuation one actually removes both the left constant two and the
  matching variable, preserving union two.
- `larger_constant_removal`: a constant four at the empty key actually removes
  a constant three at `[u]`, preserving union four.
- `disabled_larger_key`: valuation zero disables `[u]` while leaving `[]`
  enabled; the actual constant removal still preserves union four.
- `valid_self_same_removal`: same-key self subsumption removes the constant one
  but retains the variable `u+0` at valuation one, preserving the node value.

All 13 named proofs are public ASCII `theorem` declarations. There are no
`private`, `local`, `lemma`, `sorry`, `admit`, project axioms, `unsafe`, `partial`
or `native_decide` declarations. Computational witnesses use kernel reduction
through definitional equality (`rfl`) and simplification with the actual helper
equations. The enabled-variable case explicitly unfolds the wrapper through
`subset Name.cmp`; it rewrites the actual well-founded `subsumeVars` equations
rather than relying on native evaluation.

## Validation status

All 13 public theorems freshly compiled with zero errors or warnings using the
actual pinned compiler `/home/agent/.local/share/vesper-lean/bin/lean`: Lean
4.24.0, commit `797c613eb9b6d4ec95db23e3e00af9ac6657f24b`, Release. The compile
used the granted exclusive lane lease, `LEAN_NUM_THREADS=1`, `-j1 -M4096`,
`-R lean`, and the immutable semantic dependency root
`/tmp/lean4lean-foundations42-semantics-v2`. Before compiling, all eight published
source/object BLAKE3 identities and both authored dependency sources were
checked against the v2 manifest and worktree copies.

The immutable complete consumer root is
`/tmp/lean4lean-foundations42-subsumption-v1`; its `identities.json` records the
unchanged v2 dependencies plus these two files:

| File | Bytes | BLAKE3 |
| --- | ---: | --- |
| `Lean4LeanLevel42/Subsumption.lean` | 9294 | `aa8a7ff62128bad3c2e7198bfa2f72497b5243b5137b2c5a50e0c4e126a53dc1` |
| `Lean4LeanLevel42/Subsumption.olean` | 1532768 | `a91b926684ab62b7d848d74f876df94645a096b5e50774d30b002f51821bb106` |

`git diff --check` also passed. The compiler lease was released after the
successful compile; no Cargo command was run by this lane. The separate
unchanged-snapshot/public-theorem acceptance audit remains pending and belongs
to the integration lane.

These results do not establish normalization, map minimization or checker
correctness, whole-checker self-verification, native equivalence or compiler
refinement. They change no existing kernel, model, metatheory, resource profile,
projector, auditor, runtime, Cargo/CI source or vendored checker source.
