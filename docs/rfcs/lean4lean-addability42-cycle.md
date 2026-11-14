# Raw reification cycle boundary (#531)

`Lean4LeanLevel42.ReifyCycle` proves the concrete two-name raw-map witness from
`lean4lean-addability42-contracts.md`. It uses the actual unchanged addable,
feasible, lexChain, toTree and Tree.reify functions, not replacement algorithms.
No claim is made that normalization emits this map or that full normalize' is
incorrect. Greedy completeness and normalized-map reification remain separate.

Let a be numeric Name 0 and b numeric Name 1. The actual singleton map stores
key [a,b] with constant zero and variables [a+0,b+0]. It has MapWF, strictly
sorted keys and variable names, no empty row, and existing PathPrimed coverage
for [a,b]. Nevertheless neither name can start: both actual addable tests at
[] are false, actual feasible [] [a,b] is false, and actual lexChain falls back
to [a,b]. Coverage is cyclic rather than an admissible ordering.

The exact actual reconstruction is `imax (imax b a) b`. At the concrete
valuation a=0, all other names=1, mapValue is zero and reconstructed levelValue
is one. The proof explicitly unfolds actual subsumeVars and Tree.reify
equations, accounts for the reverse nesting in Tree.modify and the real
zero-offset variable pruning, then proves the value discrepancy. Propositional
Level equality is used; no opaque native Level.beq reflection is assumed.

## Frozen source and public roots

Imports are SortedKeys, MapValue, PathPriming and LevelSemantics. Shape and
ShapeFinal are not imported. Shape properties use explicit lookup predicates,
and there are no premises asserting the desired reconstruction behavior.
Six concrete definitions fix A/B/path/node/map/valuation. All 14 public theorem
names remained unchanged throughout authoring:

- reifyCycle_lookup
- reifyCycle_WF
- reifyCycle_keysSorted
- reifyCycle_varsSorted
- reifyCycle_noEmpty
- reifyCycle_pathPrimed
- reifyCycle_addable_a
- reifyCycle_addable_b
- reifyCycle_feasible
- reifyCycle_lexChain
- reifyCycle_toTree_reify
- reifyCycle_mapValue
- reifyCycle_reifiedValue
- reifyCycle_counterexample

Final source: 4,966 bytes, SHA256
`d014ff7e12b0d93bc3d9c10f1fa9fb57f181391def01e350fe99138859cae838`, BLAKE3
`da5c54e0db0df0f70ed9bcd64e78c3b491794379e443442a69e00b4bb78e3abd`.
Final original object: 204,184 bytes, SHA256
`d73396c7fdd48fc5dd76596dcbdf4c48c45ad05224f198c6bbca51325aac3838`, BLAKE3
`9b3901d836352a393078a34eb32e648195bb4da2c2cefbb43b53fc028608a373`.

## Genuine compilation and original-object audit

Final immutable private root:
`/tmp/lean4lean-addability42-cycle-v5`.

The complete shared dependency root was copied, and only private copies were
unsealed. All 58 input records match the frozen dependency manifest SHA256
`021fd786e9b6f71a5dc9e62bb9f9c26928965386021d1f5c12bf660b7aa6111b`.
All corresponding project/upstream source bytes match this worktree. Inputs
were verified before the first compile and after the final compile/audit;
the shared root and existing source/object files remain unchanged.

Pinned Lean 4.24.0 commit
`797c613eb9b6d4ec95db23e3e00af9ac6657f24b` was used, with one thread and
`-M4096`. From the final private root, the actual successful commands were:

```text
/home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 -o Lean4LeanLevel42/ReifyCycle.olean Lean4LeanLevel42/ReifyCycle.lean
/home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 CycleAudit.lean
```

Environment: `PATH=/usr/bin:/bin`, `LANG=C.UTF-8`, `LEAN_NUM_THREADS=1`,
`LEAN_PATH=/tmp/lean4lean-addability42-cycle-v5:/home/agent/.local/share/vesper-lean/lib/lean`.
Each invocation had a 180-second outer deadline. Compilation exited zero in
1.395 seconds with empty output. The genuine original-object `#print axioms`
probe for every named root exited zero in 2.841 seconds, producing 1,483 bytes.
Every root uses exactly the permitted standard three: propext, Classical.choice
and Quot.sound. No forbidden proof escape is present in the authored source.
This is not an external replay or a proof about native implementation helpers.

`cycle-identities.json` has 60 records: the unchanged 58 plus the new source
and original object. Its SHA256 is
`6d0846b39da39b85a171b549506a22b37ba8817d8475553fd57b090da8ef696f`;
BLAKE3 `30127015dc9542de3812c02c526e96ebae7d33d3420dcee7f5ecedf47e1118e3`.
`cycle-manifest.json` records those identities, all root axiom sets and evidence
file identities, including commands, outputs and failure history. It has
22,556 bytes, SHA256
`f89033fefa97d749769e7ce04535750942b8701944696bbccd33080acf43f9da`, BLAKE3
`6283e707cb385ff8adb88672f64a97ffb875584c66f0a585b57826063653287f`.
The compiler executable identity/version are in `cycle-toolchain.json`.

## Preserved failed attempts and boundaries

Fresh private versions v1-v4 preserve every failed compile and exact input.
V1 needed explicit unfolding to synthesize strict-sort decisions and could
not reduce well-founded reconstruction by rfl alone. V2 exposed the remaining
closed Nat comparator goals and insufficient simplifier unfolding. V3's
explicit tree change was blocked by the actual well-founded subsumeVars term.
V4 proved that pruning equality and exposed the final addOffset-zero reduction.
V5 discharged that final definitional reduction. The mathematical statements,
map, valuation and theorem inventory were not weakened or changed.

A v3 diagnostic probe initially failed because field notation on a raw
TreeMap.insert expression did not retain the NormLevel alias; its corrected
named-NormLevel probe succeeded and showed the blocked well-founded terms.
Both probe inputs/results and all earlier compile inputs/logs remain in the
private roots and final history directory. Failed objects were not imported.

`git diff --check` passed. No Cargo, existing proof/model source edit, push,
issue/PR write or policy change was performed by this author. The compiler
lease is released. Root-owned fresh packet projection/replay, full audits and
all nine acceptance matrices remain pending; none is claimed by this report.
