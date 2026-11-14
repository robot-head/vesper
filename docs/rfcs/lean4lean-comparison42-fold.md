# Actual comparison fold (#446)

This lane implements the frozen #444–#448 contract from `ec327ef6f52bc317f9ea0045f724067d3ca13f6c`.
It owns only `lean/Lean4LeanLevel42/CompareFold.lean` and this report. The module
imports only frozen `Lean4LeanLevel42.NodeResidual`; no parent, vendor, helper,
runtime, auditor, profile, Cargo or CI source changed.

`compareStep` abbreviates exactly the actual `NormLevel.le` fold step. Its
explicit `Name.cmp` is definitionally the vendor's captured local comparator.
The step tests the actual subset helper, applies actual `subsumeBy false`,
and returns `none` precisely at the actual empty-residual test. It does not
substitute minimization for comparison.

The three public theorems have the exact contracted names and binders:

- `compareStep_none_bound`: a real early exit bounds the source node's value
  by its dominator's value, requiring only the dominator's `NodeWF`.
- `compareStep_some_union`: a surviving actual residual preserves max-union
  with the dominator, again requiring only dominator WF.
- `compareFold_none_bound`: induction over the real list `Option.foldlM`
  bounds the original node by an arbitrary common row bound when the actual
  result is `none`. Every row has explicit WF and value-bound hypotheses.

The empty-list case cannot produce `none`. In a cons case, `none` is discharged
by the actual step's bound; `some r` continues through the actual tail fold,
whose induction bound and step union recover the original node's bound. There
is no source-node WF, sortedness, map, priming or assumed-result premise. All
proof helpers are public ASCII `theorem` commands. No axiom, proof escape or
opaque equality bridge was introduced. This lane proves no comparison
completeness, canonicality, reification or whole-checker/native equivalence.

## Genuine compilation and artifact association

The coordinator granted Lean compiler lease 2 before source compilation. The
input snapshot was `/tmp/lean4lean-normalization42-subsumption-v2`, whose
complete 36-record `identities.json` has SHA-256
`1a4dd362d02dffa414901e9f55ce109c2d2f83cb2474d93c476c6efa9713be9c`.
All 36 recorded byte counts and SHA-256 hashes were verified before copying;
all 18 source files also byte-matched the owned worktree's frozen proof or
vendor source. The exact manifest and all recorded files were copied into the
new root `/tmp/lean4lean-comparison42-fold-v1`. No existing artifact root was
modified and no parent source was recompiled.

The sole Lean invocation ran from that new root:

```sh
LEAN_PATH=/tmp/lean4lean-comparison42-fold-v1 LEAN_NUM_THREADS=1 /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 -o Lean4LeanLevel42/CompareFold.olean Lean4LeanLevel42/CompareFold.lean
```

This genuine pinned Lean 4.24 invocation exited 0 with empty stdout/stderr on
the first attempt. There were no failed Lean runs. No Cargo invocation ran.

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| `CompareFold.lean` | 3121 | `0c72e6850a9b38baf6a5c6567342c24efc668e9996d38cffcf9b2ec5cdd71094` |
| `CompareFold.olean` | 162936 | `d8fc03402f642b2c380bb9f26ad790e7a7d4f4f8b8b499cb22ee4de3f751d7a6` |

After compilation, all 36 input identities were verified again and the new
snapshot source was checked byte-for-byte against the owned source. The full
immutable 38-record source/object manifest is
`/tmp/lean4lean-comparison42-fold-v1/identities.json`, SHA-256
`8856a44e6e5519245e8f67826673c6128d29976b3cb33147616416ccc392e39b`.
The root was sealed read-only, including its copied input manifest. Compiler
lease 2 was explicitly released after this verification; the MapComparison
author received the unchanged APIs and exact artifact associations.

The coordinator's aggregate compilation and complete genuine acceptance
matrices remain separate integration gates. This source-only compile is not
reported as completion of those gates. No push or PR was performed.
