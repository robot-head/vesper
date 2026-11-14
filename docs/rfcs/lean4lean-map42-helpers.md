# Actual map helper proofs (#420)

`Lean4LeanLevel42.MapHelpers` proves denotation and condition-invariant
preservation for the pinned `NormLevel.addNode`, `NormLevel.addVar`, and
`NormLevel.addConst`. It imports the new `MapValue` and the frozen `VarInsert`;
no vendor, parent proof, runtime, compiler, Cargo, or CI source was changed.
This lane owns only the new proof module and this report.

The 22 public ASCII theorem roots include all seven contracted value/WF APIs,
the conditional covered-constant theorem, actual-comparator lookup bridges,
a lookup-extensional denotation helper and a local update helper. The update
helper uses `mapValue_insert` and the old map's erased-key decomposition:
it does not model overwriting as a union with an unchanged old map.
`map_addVar_value` explicitly requires `s.get? p = some n`.
`map_addConst_value` retains the exact unconditional skip condition
`k = 0 || k = 1 && !p.isEmpty`; `map_addConst_value_of_covered` simplifies it
only under the contracted one-bound premise. The file temporarily removes
the standard lookup-notation simp rule while proving these wrappers and
restores it before the namespace closes.

For the priming lane, each operation has `map_add*_preserves_lookup` and
`map_add*_preserves_var`, at an arbitrary update path. The second API
preserves an existing variable *name* with a new row/member witness, allowing
offsets and the physical list member to change. `map_addNode_var` supplies
an actual row/member witness for the newly inserted variable even when the
key already exists. Their exact signatures were agreed with #421 before
its implementation.

The actual counterexamples use numeric name zero and the valuation constantly
equal to two:

- `missing_key_addVar_counterexample`: modifying the empty map retains value
  zero although the enabled variable contribution is two.
- `skipped_one_empty_counterexample`: the empty map is WF but skipping one
  on a nonempty enabled path leaves value zero, versus intended constant one.
- `skipped_one_zero_node_counterexample`: inserting a zero node first gives
  actual key presence and WF; skipping one still leaves value zero.

All proofs are fixed handwritten Lean. No `sorry`, `admit`, project axiom,
`unsafe`, `partial`, or `native_decide` is used. The scope is actual map
construction; it does not claim full normalizeAux induction or minimization.

## Dependency correspondence and execution

Dependencies were copied from the sibling's genuinely built immutable
`/tmp/lean4lean-map42-value-v4` and frozen
`/tmp/lean4lean-construction42-vars-v1`. Every overlapping source and compiled
object was compared byte-for-byte. Frozen sources were also compared with
this worktree's corresponding proof/vendor sources. The copied inputs have
byte counts and SHA-256 identities in each snapshot's
`input-identities.json`; these were rechecked after the final compilation.
No dependency in either published sibling root was mutated.

Compiler lease 2 was used only after the MapValue dependency was genuinely
built. The pinned executable was
`/home/agent/.local/share/vesper-lean/bin/lean` (Lean 4.24), always with
`LEAN_NUM_THREADS=1`, `-j1`, and `-M4096`. No Cargo command was run in this
lane. Each source revision has its own versioned root; no previous snapshot
was edited.

The first attempted command used the worktree as cwd and an absolute input
under `/tmp/lean4lean-map42-helpers-v1`. Lean rejected it because the source
was outside its root directory. Retrying from that snapshot's directory
performed the genuine v1 compilation and exposed simplifier normalization
of `get?` to bracket notation before the bridge rules could match. Revision
v2 attempted a `local attribute` command, which Lean rejected as invalid
syntax. Revision v3 uses an attribute removal/restoration around the module
proofs and compiles successfully, with exit status zero and empty output:

```sh
cd /tmp/lean4lean-map42-helpers-v3
LEAN_PATH=/tmp/lean4lean-map42-helpers-v3 LEAN_NUM_THREADS=1 \
  /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 \
  -o Lean4LeanLevel42/MapHelpers.olean Lean4LeanLevel42/MapHelpers.lean
```

The final source is byte-identical to the owned worktree source. Its SHA-256
is `4ba34b4255000586cce552d4fe38cb3b68bddfc83484a83aa895e3ce9af1ade8`
(11,663 bytes); the resulting `.olean` SHA-256 is
`ed1dc39a6d1015f4500712cca8fac41a4903ba0fef4d11a2d867b85d9fffeb41`
(640,528 bytes). The final root also contains `identities.json` for the
complete source/object set. `git diff --check` passed.

Compiler lease 2 was explicitly released after successful compilation and
publication. The coordinator owns all four final acceptance matrices,
projection/axiom audits, and integrated validation; none is claimed as run
by this lane.
