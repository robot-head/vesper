# Actual level semantics and size (#425)

`lean/Lean4LeanLevel42/LevelSemantics.lean` defines `levelValue`, `NoMVars`,
and `levelSize` over actual `Lean.Level`, importing the frozen
`Lean4LeanLevel42.Semantics` module. `levelValue` totalizes metavariables at
zero. `NoMVars` excludes them structurally, including under successors and
both binary constructors. `levelSize` counts constructors and never reads
names or cached data. No equivalence with cached `Level.hasMVar` is claimed.

The seventeen public theorem signatures are exactly the contracted APIs:

- `levelValue_zero`, `levelValue_succ`, `levelValue_max`, `levelValue_imax`,
  `levelValue_param`, and `levelValue_mvar` give the evaluator equations.
- `levelSize_pos` proves positivity for every actual level.
- `levelSize_imax_max_left`, `levelSize_imax_max_right`,
  `levelSize_imax_imax_left`, and `levelSize_imax_imax_right` decrease the
  measure for the fresh expressions made by actual recursive normalization.
- `nat_imax_zero`, `nat_imax_succ`, `nat_imax_max`, and `nat_imax_imax`
  prove the natural-number interpretation of the actual imax rewrite cases.
- `max_add_offset` and `nat_imax_add_offset` handle arbitrary offsets.

All theorem binders are explicit: evaluator equations take `ρ` followed by
their constructor arguments; size lemmas take `l` or `u v w`; arithmetic
lemmas take `a`, `a b`, `a b c`, or `a b k` as appropriate. There are no
additional public helpers or aliases. The proofs use constructor equations,
natural-number max lemmas, case splits, and kernel-checked `omega` proofs.
They do not change or replace actual normalization.

## Verification and artifact identity

The parent root `/tmp/lean4lean-map42-priming-v2` was verified against all
twenty entries in its published `identities.json`, checking both byte length
and SHA-256. Every parent proof source was also compared byte-for-byte with
the worktree's `lean/Lean4LeanLevel42` source; the two actual upstream sources
were compared with `tools/lean4lean-foundations42/upstream`.

The successful compile command, executed from the semantics worktree, was:

```sh
LEAN_NUM_THREADS=1 LEAN_PATH=/tmp/lean4lean-map42-priming-v2 \
  /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 \
  -o /tmp/lean4lean-normalize42-semantics-v1/Lean4LeanLevel42/LevelSemantics.olean \
  lean/Lean4LeanLevel42/LevelSemantics.lean
```

The final compile exited zero without diagnostics. Three earlier proof
drafts failed: the first left an idempotent-max goal, the second left two
arithmetic goals involving opaque max expressions, and the third left an
arithmetic conditional goal and attempted a redundant split on a reflexive
goal. Explicit max unfolding and recursive case splitting resolved those
failures. An intermediate successful compile reported one unused simp
argument; removing it produced the final clean compile.

The genuine immutable publication is
`/tmp/lean4lean-normalize42-semantics-v1`. It contains twenty unchanged
parent source/artifact files plus the new source and compiled artifact.
`input-identities.json` records the twenty verified parent identities, and
`identities.json` records all twenty-two resulting identities. Its exact
hash scope is:

| File | Bytes | SHA-256 |
| --- | ---: | --- |
| `Lean4LeanLevel42/LevelSemantics.lean` | 3589 | `27e0aeed8b8f8e16cbd4d19f374687313a04a5f995a87d650775bb9b4e1edc9f` |
| `Lean4LeanLevel42/LevelSemantics.olean` | 404472 | `88e05290fb08068e2e5dcd023d6f36a8ffcdf0d291a1f719c9b9fa0e3ae29769` |
| `identities.json` | manifest | `a2d20cee90add64c10e1df295fe054bcf9f808cbb158d4c7bfc4f3d33a0b2965` |

Compiler lease 1 was released immediately after publication. This lane ran
no Cargo commands and no generated Lean or audit scripts. The coordinator's
integrated packet performs the source-command, axiom, projection, negative
control, and all-five-matrix checks; this lane makes no claim to those
results or to full `Normalize.normalize` correctness.
