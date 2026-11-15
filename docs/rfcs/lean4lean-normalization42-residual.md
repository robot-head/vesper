# Actual node residuals (#436)

Implemented `lean/Lean4LeanLevel42/NodeResidual.lean` against the frozen
normalization42 contract. It imports the existing `Subsumption` and `NameOrder`
modules and uses the actual pinned `Normalize.subsumeVars`, `Node.subsumeBy`,
and `Node.subsume` definitions. No helper, vendor, provenance, runtime, compiler,
parent proof, or acceptance-policy source was changed.

The nine public ASCII theorem roots are `subsumeVars_sublist`,
`node_subsumeBy_var_sublist`, `node_subsume_var_sublist`, `node_subsume_WF`,
`node_isEmpty_value_zero`, `node_subsume_vars_union`,
`node_subsumeBy_same_context`, `node_subsume_same_context`, and
`self_context_needs_vars_counterexample`. The public definition `nodeVarsValue`
gates variable denotation with the node's actual enabled condition set.

The sublist results require neither sorting nor well-formedness. Consequently,
the actual wrapper preserves left-node well-formedness without requiring a
well-formed right node. The gated-variable union theorem also has no
well-formedness premise: it reuses `node_subsumeBy_vars_union` and the wrapper's
actual subset test. Comparator reasoning uses `Name.cmp`, not the global
`Ord Name` instance's `quickCmp`.

The self-context theorem permits an arbitrary residual node and external
context `C`. Only the original node must be well formed, and its gated variables
must be covered by the residual's gated variables or `C`. A same-context step
retains residual variables; if it removes the constant,
`subsumeBy_removed_const_le_vars` bounds that constant by the original
variables, and the coverage premise absorbs it. No self-value or map-value
preservation is assumed.

The public counterexample takes `p = [u]`, valuation constantly one, original
`{ const := 1, var := [u + 0] }`, residual `{ const := 1 }`, and `C = 0`.
Both nodes are well formed, the residual variables form a sublist of the
original variables, and their whole-node values agree. The actual self step
still changes residual value one to zero; the required variable coverage is
false. This is a counterexample to replacing that coverage with whole-value
equality, a sublist, and well-formedness.

## Genuine Lean validation

The source is fixed handwritten Lean. There are no generated Lean audit files,
`sorry`, `admit`, project axioms, `unsafe`, `partial`, or `native_decide` uses.
No Cargo command was run in this lane. Compiler lease 1 was released explicitly
to the coordinator immediately after freezing the successful artifact.

Pinned compiler:

```text
/home/agent/.local/share/vesper-lean/bin/lean --version
Lean (version 4.24.0, x86_64-unknown-linux-gnu, commit 797c613eb9b6d4ec95db23e3e00af9ac6657f24b, Release)
```

Successful command, run from the residual worktree (exit 0, no diagnostics):

```sh
LEAN_PATH=/tmp/lean4lean-normalization42-residual-v1 LEAN_NUM_THREADS=1 \
  /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 \
  -o /tmp/lean4lean-normalization42-residual-draft/Lean4LeanLevel42/NodeResidual.olean \
  lean/Lean4LeanLevel42/NodeResidual.lean
```

Two earlier compiler invocations failed and were corrected. The first used
separate dependency roots; Lean selected the first module-directory root and
could not find `NameOrder.olean` there. The second, using the combined verified
root, found an unopened `Lean.Level` namespace and an incorrectly applied
Boolean simplification name. Adding `Lean.Level` to `open` and obtaining the
empty-node field conjunction by `simp [Node.isEmpty]` resolved those errors.
The successful invocation checked every final theorem, including the actual
counterexample.

The following dependency manifests were checked against every recorded file's
byte count and digest before use: foundations42-subsumption-v1 (10 BLAKE3
records), order42-name-v3 (10 SHA256 records), and normalize42-value-v1
(26 SHA256 records). A combined immutable dependency closure was copied into
`/tmp/lean4lean-normalization42-residual-v1`; overlapping files were checked for
byte equality. Published dependency roots were not changed.

That versioned root now contains the final source and object plus the verified
dependency closure. Its `identities.json` records all 30 source/object files;
`dependency-identities.json` preserves their input records. After publication,
all 30 byte-count/SHA256 records were verified again, and the five sources in
the actual residual import closure were compared with their repository files.

| Final artifact | Bytes | SHA256 |
| --- | ---: | --- |
| `Lean4LeanLevel42/NodeResidual.lean` | 5137 | `e14fa851a1c2ff578b55e8e1e576d28e4c545813a7d42b45a7b62abdee9a5269` |
| `Lean4LeanLevel42/NodeResidual.olean` | 593912 | `8733df2b079021f3dc4f4d6c938327477560801e5000d78acde1704623aa5002` |

`git diff --check` passed. The coordinator owns the genuine packet audit and
integrated acceptance matrices; this lane claims actual residual structure and
contextual denotation only, not canonicality, reification, whole-checker, or
compiler correctness.
