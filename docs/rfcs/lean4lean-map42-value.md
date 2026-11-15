# Actual map denotation (#419)

`Lean4LeanLevel42/MapValue.lean` defines `mapValue` as a maximum fold of the
actual `Lean.Level.Normalize.NormLevel` and defines `MapWF` by actual stored
rows. It imports the frozen `NameOrder` and pinned Std TreeMap lemmas.
Every comparator-dependent Std theorem receives explicit
`List.compareLex Name.cmp`; no global Name comparison supplies these proofs.

All seven contracted APIs are public, with the signatures frozen and sent
to the helpers and priming lanes before compilation. `mapValue_empty`,
`nodeValue_le_mapValue`, `mapValue_le`, and `mapValue_mono` follow from the
actual map fold/list and list-membership/lookup bridges. The three public
generic list lemmas `maxfold_le`, `le_maxfold`, and `mem_le_maxfold` prove
the required local fold bounds by list induction.

The public `map_get_empty`, `map_get_insert`, and `map_get_erase` bridges
state exact lookup equations, reflecting the actual comparator equality to
key equality using `path_cmp_eq_iff`. `mapValue_erase_le` supplies an
additional useful public bound. `mapValue_insert` and `mapValue_decompose`
then follow from pointwise lookup bounds in both directions, without a
tree-balancing proof or an assumed insertion/list permutation. Insertion
uses the erased-key context, so it accounts for overwriting an existing
row. These generic semantic facts require neither NodeWF nor a sorted-key
premise. `mapWF_empty` follows directly from empty lookup.

## Frozen dependencies and genuine execution

Before compiling, all ten source/object entries from the immutable
`/tmp/lean4lean-order42-name-v3/identities.json` were checked against their
recorded byte lengths and SHA256 hashes. Each of the five source files was
also compared byte for byte with the current frozen worktree source; the
two vendor files use `tools/lean4lean-foundations42/upstream`. Matching
source hashes are:

| Module | Source SHA256 |
| --- | --- |
| Lean4Lean.List | 4ddde2232aff649a50a8d1bec44e9495e2387dd3fcc29833d4f9c67c514431a4 |
| Lean4Lean.Level | 72fc6c19f871150b4b698f183bd1f4b0b0d91d6b1efa03fd43705ddb62b59af6 |
| Lean4LeanLevel42.Semantics | 2a4e8ccabdce2ebe1c36d9d1dbb682705e2c3270697ca9703ed7d44fac473159 |
| Lean4LeanLevel42.ListProofs | 0cdafbdac4884b86b1896170b005e1c6ef0f84bd8ebf71a99711f0f1ef3111ca |
| Lean4LeanLevel42.NameOrder | 932a487b98f1c895cdde9ccc5771cc9cda874eabca09ab40e236d987eac6ace9 |

The exact dependency sources and compiled objects were copied into new
versioned roots, never changed in their published parent roots. Final
immutable publication is `/tmp/lean4lean-map42-value-v4`. Its
`dependency-identities.json` records the ten inherited entries, and its
`identities.json` records all twelve final source/object entries. All
dependency identities were reverified after the successful compile; the
final MapValue source was also compared byte for byte with the owned file.

The genuine pinned Lean 4.24 executable was invoked with one thread and
the agreed limits. The final command exited zero with empty output:

```sh
LEAN_NUM_THREADS=1 LEAN_PATH=/tmp/lean4lean-map42-value-v4 \
  /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 \
  -R /tmp/lean4lean-map42-value-v4 \
  -o /tmp/lean4lean-map42-value-v4/Lean4LeanLevel42/MapValue.olean \
  /tmp/lean4lean-map42-value-v4/Lean4LeanLevel42/MapValue.lean
```

Three preceding genuine invocations used this exact command with every
`value-v4` path replaced respectively by `value-v1`, `value-v2`, and
`value-v3`; each exited one and retained its source in that versioned root.
Version 1 failed to infer the captured comparator's TransCmp/LawfulEqCmp
instances. Version 2 passed those explicit comparator arguments but its
lookup rewrites did not match through the actual map's reducible comparator
representation. Version 3 used a typed `calc` bridge, but a direct rewrite
of the conditional proposition had an ill-typed Decidable motive. The final
version uses the typed bridge and `simp only [path_cmp_eq_iff]`; no public
signature changed during these fixes.

Final MapValue source SHA256 is
`875688bc654b950ef09eafc321d311cab123ceb707ec01eb5e69b11ae647b071`
(5,638 bytes). Final olean SHA256 is
`83af99da1de2dfde8e5417282bf75e0fa076984a5611d43dde310c4b74c68a11`
(137,872 bytes).

A separate fixed handwritten `/tmp/lean4lean-map42-value-audit-v1/Audit.lean`
imports MapValue and runs `#print axioms` for all fourteen public theorem
roots, including all additional helpers. This command exited zero:

```sh
LEAN_NUM_THREADS=1 LEAN_PATH=/tmp/lean4lean-map42-value-v4 \
  /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 \
  /tmp/lean4lean-map42-value-audit-v1/Audit.lean
```

The three generic list fold lemmas depend only on `propext`; the other
eleven theorem roots depend only on `propext`, `Classical.choice`, and
`Quot.sound`. All Lean source in this lane was fixed handwritten source;
there was no generated Lean. No proof escape, replacement map, legacy
Verify.Axioms import, dependency edit, vendor edit, Cargo execution, push,
or PR occurred. Lean compiler lease 1 was explicitly released immediately
after the successful source audit and immutable publication.

This lane establishes generic map denotation and empty well-formedness.
Actual helper updates, current-path priming, the aggregate packet, complete
projection/alias association, and four acceptance matrices remain the
dependent lanes' and coordinator's integration work. It does not establish
normalization/minimization correctness or compiler/native refinement.
