# Actual fixed-map minimization (#437)

The new `lean/Lean4LeanLevel42/MapMinimize.lean` proves facts about the unchanged
`Lean.Level.Normalize.NormLevel.minimize` implementation. Its fold source is
exactly the fixed input map's actual `toList`; every enumerated row is connected
to that map through `Std.TreeMap.mem_toList_iff_getElem?_eq_some`.

`minimize_var_sublist` needs no lookup or well-formedness assumption. Each actual
`Node.subsume` removes variables, and transitivity propagates that fact through
the fold. `minimize_WF` uses only the input node's `NodeWF` and this sublist.

`minimize_vars_context` and `minimize_value_context` assume only `MapWF acc` and
the actual lookup `acc.get? p = some original`. The semantic proof carries the
variable and whole-node equalities together. Its fixed context is
`mapValue ρ (acc.erase p)`. An external source row is bounded by that context
using its actual lookup after erasure. The existing whole-node union theorem
and the new gated-variable union theorem preserve their respective invariants.
At the self row, lookup uniqueness identifies the row with `original`; current
variables retain coverage of the original variables in the fixed context,
which supplies the premise for safe constant removal. The initialization uses
`mapValue_decompose` and the original lookup.

The proof does not assume sorting, a residual bound, a precomputed correctness
flag, or union with the old self entry. Its public missing-lookup witness uses
the empty map, empty key and constant-one original: actual minimization still
has value one while the map has value zero.

Only the new Lean module and this report belong to this lane. No actual helper,
vendor source, runtime, compiler, Cargo or workflow file is changed.

Validation used the genuine pinned Lean 4.24.0 binary at
`/home/agent/.local/share/vesper-lean/bin/lean` (commit
`797c613eb9b6d4ec95db23e3e00af9ac6657f24b`). All 30 source/object identities in
`/tmp/lean4lean-normalization42-residual-v1/identities.json` were checked for
exact byte length and SHA-256 before compilation. The actual command was:

```sh
LEAN_NUM_THREADS=1 LEAN_PATH=/tmp/lean4lean-normalization42-residual-v1 \
  /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 \
  -o /tmp/lean4lean-normalization42-minimize-draft/MapMinimize.olean \
  lean/Lean4LeanLevel42/MapMinimize.lean
```

The first run exited 1 because simplification left the constant-test branch in
`minimize_self_var`; explicitly splitting that branch proved its unchanged
variable projection. The second run exited 0, with no diagnostics (1.48s).
No Cargo command ran in this lane. Lean lease 2 was explicitly released after
publishing the genuine dependency artifact.

The immutable publication is
`/tmp/lean4lean-normalization42-minimize-v1`, containing 32 exact Lean
source/object files. Its complete `identities.json` was independently checked
after copying; its SHA-256 is
`a90b33b336fd44629932689f399a392606cddab7256f634564dc436522b3b1d3`.
The new source is 6413 bytes, SHA-256
`b45dde47cca924f30dfb49c78d56fc3af8691ee12fc9050eac01742b081235f3`;
the genuine object is 407072 bytes, SHA-256
`a56942ddea33dff961a8eeccb13649277bbcfd827e081164b1e2c90e5923da07`.
The compilation and snapshot use genuine dependency objects, with no stubs,
new axioms, admission, native decision procedure, or replacement algorithm.
The coordinator owns the integrated acceptance and axiom audit.
