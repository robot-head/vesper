# Current-path priming (#421)

The authored module is `lean/Lean4LeanLevel42/PathPriming.lean`, importing
`MapHelpers` and the frozen `OrderedInsert`. It follows the definition in
`lean4lean-map42-contracts.md` exactly: every current-path variable has a
represented-name witness in an actual stored row at a subset key; a nonempty
current path additionally has an actual row at the current key. `MapWF`
remains a separate property.

The eight contracted APIs prove empty-path priming, bounds for every current
variable and for constant one under enabledness, preservation by each actual
map update at arbitrary keys, extension by actual ordered insertion followed
by actual `addNode`, and the simplified actual `addConst` value equation.
Preservation consumes `MapHelpers`' lookup and represented-name preservation
lemmas. Extension transports old subset-key witnesses and gets the inserted
name from the actual new row. It needs no sortedness premise.

The public `pathPrimed_one_edge_counterexample` uses numeric names `u = 0`
and `v = 1`, the present key `[u,v]`, and a single stored edge `v+0`. Its
enabled valuation has `ρ u = 3`, `ρ v = 1`, and actual map value one. The
actual skipped constant-one update preserves the map, but so does the
actual `.param u`, zero-offset `normalizeAux` branch, despite `ρ u` exceeding
the map value. The map is not `PathPrimed`. This is a concrete failure of
one-edge coverage, not a claim about a hypothetical replacement map.

The public `pathPrimed_extend_example` starts at the empty actual map,
inserts `u+2` at `[u]`, then uses the actual ordered insertion of `v` and
actual `addNode v 4 [u,v]`. The resulting current path is primed; its old
`u` witness lives at the strict subset key `[u]`.

No full `normalizeAux` induction, full normalization or minimization
correctness is claimed. The definition has no semantic correctness flag,
and the proofs do not assume the desired update equation as a premise.

The coordinator explicitly granted compiler lease 1 after the value lane
released it. The genuine final compile in immutable snapshot
`/tmp/lean4lean-map42-priming-v2` exited 0 with empty output:

```sh
LEAN_NUM_THREADS=1 LEAN_PATH=/tmp/lean4lean-map42-priming-v2 /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 -o Lean4LeanLevel42/PathPriming.olean Lean4LeanLevel42/PathPriming.lean
```

The compiler reported Lean 4.24.0, commit
`797c613eb9b6d4ec95db23e3e00af9ac6657f24b`. The first immutable snapshot,
`/tmp/lean4lean-map42-priming-v1`, failed one obligation: `rfl` did not unfold
the well-founded actual `normalizeAux` definition in the counterexample.
The final proof explicitly uses its defining equation before `rfl`. All
other obligations passed in that first compile. A preliminary command could
not start because its intended working directory had not yet been created;
no compiler ran in that failed launch.

Dependencies came from genuinely compiled `map42-helpers-v3` (including
`MapValue` and frozen `VarInsert`) and `construction42-ordered-v2` (frozen
`OrderedInsert`). Every input source/object byte length and SHA-256 was
checked against its producer's manifest. Common dependencies in those two
roots were byte-identical. All actual authored/frozen source files were also
compared with their corresponding worktree sources. The final snapshot's
19 input identities were verified unchanged after compilation, and its
`identities.json` records all 20 final source/object identities. No input
artifact was mutated or substituted.

The SHA-256 of final `input-identities.json` is
`babe7f01d858dc745380e3cdffc0601d04fbd0ebd401c828ac91ef84ffcef7e6`;
the SHA-256 of final `identities.json` is
`3d77ef75709e8cabb98aa573b66a1e4edec52cd0eb6fb991ac67fdd6587a0369`.
The complete final module identities are:

| Actual module | Source SHA-256 | Object SHA-256 |
| --- | --- | --- |
| `Lean4Lean/List` | `4ddde2232aff649a50a8d1bec44e9495e2387dd3fcc29833d4f9c67c514431a4` | `dde701ce601d2399c91a48dfb13c1912ba584a41339816236478b3ef8f5a4b35` |
| `Lean4Lean/Level` | `72fc6c19f871150b4b698f183bd1f4b0b0d91d6b1efa03fd43705ddb62b59af6` | `398a9c8af2a5746fd9a6b4c2356edca90a7ddf36da5446e78773e214af00b6d3` |
| `Lean4LeanLevel42/Semantics` | `2a4e8ccabdce2ebe1c36d9d1dbb682705e2c3270697ca9703ed7d44fac473159` | `fcae60546bdac127e59630c691c889321e9729e116c58cf57fac99d21884ec67` |
| `Lean4LeanLevel42/ListProofs` | `0cdafbdac4884b86b1896170b005e1c6ef0f84bd8ebf71a99711f0f1ef3111ca` | `41528d3a2b75f33b76060ad4d41fc4939222f9c03b3d14aee7dfc3fd0c932927` |
| `Lean4LeanLevel42/OrderedInsert` | `05611454d9d4abec32610b5f89a904775f13f365f6a02b03e8dad19639e12e74` | `73dc47f5ef2c9d22d47b0f173b2795f95100cf4c9bd389283a9c6d59c98e8308` |
| `Lean4LeanLevel42/VarInsert` | `5e4fe1627e7ef66fd3afdb248172a349734217e335b1acc6182ed26c78458729` | `cafa7c00ea2b0c82c7d5d54f8801f3d59c3dc3750bb0c6be7db8277635ec5a36` |
| `Lean4LeanLevel42/NameOrder` | `932a487b98f1c895cdde9ccc5771cc9cda874eabca09ab40e236d987eac6ace9` | `2490c4e3edefa313511c50c898d0bedb20f757d239dcd918c234772c509121c6` |
| `Lean4LeanLevel42/MapValue` | `875688bc654b950ef09eafc321d311cab123ceb707ec01eb5e69b11ae647b071` | `83af99da1de2dfde8e5417282bf75e0fa076984a5611d43dde310c4b74c68a11` |
| `Lean4LeanLevel42/MapHelpers` | `4ba34b4255000586cce552d4fe38cb3b68bddfc83484a83aa895e3ce9af1ade8` | `ed1dc39a6d1015f4500712cca8fac41a4903ba0fef4d11a2d867b85d9fffeb41` |
| `Lean4LeanLevel42/PathPriming` | `29e97d7b6a0f83e34bac0dd64a4f840753156cbb0997c4ba5cbb7ae715902e4c` | `43225dc8778cf4d56d8a2cbff928cd07f9f0cd3f96449b6a9e2071084ac1184d` |

`git diff --check` passed. This lane ran no Cargo, generated no Lean source,
and did not run the integrated acceptance matrices; the coordinator owns
those final checks. Compiler lease 1 was explicitly released after the
successful genuine compile and postcompile identity verification.
