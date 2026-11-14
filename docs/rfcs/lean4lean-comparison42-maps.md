# Actual map and level comparison soundness (#447)

This lane owns only `lean/Lean4LeanLevel42/MapComparison.lean` and this report.
It imports the new actual traversal and comparison-fold proofs together with
the frozen final normalization proof. No parent, helper, vendor, runtime,
auditor, profile, Cargo, or workflow source is changed.

`norm_le_sound` requires only right-map WF. The actual left-map `all` traversal
supplies successful comparison for every actual left lookup. The actual right
`foldlM` is linked definitionally to `compareStep`, then rewritten with Std's
`foldlM_eq_foldlM_toList`. Each right source row receives WF and a denotation
bound from its actual lookup in the unchanged right map. The genuine Option
fold theorem bounds the left node; taking the actual left-map maximum proves
the result. No left WF or sorting premise is used.

`geq_sound` applies actual map-comparison soundness to the two actual normalized
maps, then uses frozen `normalize_value` under structural `NoMVars` for both
levels and frozen `normalize_WF` for the upper level.

The four public witnesses cover a right-WF counterexample, genuine discharge
of one row by two distinct right rows, incompleteness for an explicit raw empty
row, and an actual successful `geq'` comparison of `max 2 u` above `imax 2 u`.
The split-row witness also exhibits the nonempty residual left by each right
row alone and the actual final early exit after both rows. The level witness
includes the semantic consequence at every valuation. The raw-empty-row
witness also states semantic inequality at every valuation, since both maps
denote zero; nevertheless the actual comparison returns false.

The public `comparison_insert_WF` helper proves the ordinary lookup-based WF
fact needed to construct witness maps. The additional helpers establish actual
singleton/pair `toList` equations, preservation of `toList` when an existing row
is reinserted unchanged, and the exact actual comparison-to-list-fold identity.
The existing-row theorem uses actual lookups and Std map equivalence; it claims
list equality rather than physical tree equality. The level witness rewrites
actual `normalizeAux` equations, both actual pure folds inside final
subsumption, and those actual map list equations before checking comparison.
Every authored declaration has a public ASCII name and enters the existing
root audit. There is no opaque `Level.beq` bridge, `isEquiv'` theorem, or
completeness claim.

The coordinator explicitly granted compiler lease 2 after the CompareFold lane
released it. Both genuine peer roots were verified in full:

| Dependency root | Records | Manifest SHA-256 |
| --- | ---: | --- |
| `/tmp/lean4lean-comparison42-all-v1` | 38 | `7b6c4b676daaa89d4c779bfbb663528d863b472970120131d050a4ee6685849a` |
| `/tmp/lean4lean-comparison42-fold-v1` | 38 | `8856a44e6e5519245e8f67826673c6128d29976b3cb33147616416ccc392e39b` |

All byte counts and SHA-256 hashes were checked, along with all 38 additional
published MapAll BLAKE3 hashes. The 36 shared parent artifacts in both peer
roots matched `/tmp/lean4lean-normalization42-subsumption-v2` byte for byte.
Both peer source files matched their isolated working-tree sources before
copying. The combined input snapshot contains exactly 40 dependency records;
its `input-identities.json` SHA-256 is
`4fbcb1bd6224f555cfd4a77688256e08a9bd8a639d8eb6b278f277dc0029c39e`.
All 40 dependency byte counts and SHA-256 hashes were reverified for every new
versioned snapshot and again after successful compilation. No dependency
source or object was altered or recompiled in this lane.

The successful immutable root is
`/tmp/lean4lean-comparison42-maps-v14`. This exact command ran there with the
pinned Lean 4.24 binary, one thread, and the granted lease, returning exit
status 0 with no diagnostics:

```sh
LEAN_PATH=/tmp/lean4lean-comparison42-maps-v14 LEAN_NUM_THREADS=1 /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 -o Lean4LeanLevel42/MapComparison.olean Lean4LeanLevel42/MapComparison.lean
```

Earlier attempts used the same command with `v1` through `v13` substituted in
`LEAN_PATH` and as the current directory. Each failed with exit status 1. The
top soundness proofs remained valid throughout; the failures concerned witness
evaluation or the auxiliary list-equivalence proof:

| Version | Diagnostics and correction |
| --- | --- |
| v1 | Reflexivity did not close actual comparison, same-variable filtering, or geq (`58:4`, `81:41`, `103:34`). |
| v2 | An all-to-list rewrite alone still left actual right folds and recursive filtering unevaluated (`65:4`, `83:40`, `111:4`). |
| v3 | Ordinary `decide` remained stuck on map/list operations; a second filtering equality also needed source equations (`66:4`, `84:35`, `114:4`). |
| v4 | Broad simplification left quantified list-membership/fold goals (`64:2`, `85:2`, `112:75`) and reported unused simp arguments. |
| v5 | Kernel `decide` also remained stuck on the actual comparison/geq terms (`64:4`, `83:4`, `109:4`). No decision tactic remains in the final source. |
| v6 | Explicit actual list equations solved every raw-map witness; direct geq evaluation still failed (`129:4`). |
| v7 | Reflexivity could not unfold the actual recursive normalizeAux equations (`133:6`, `138:6`). |
| v8 | The same two recursive-evaluation failures persisted with an anonymous-name trial; the final witness uses numeric name zero. |
| v9 | Field notation on an expanded TreeMap expression did not resolve `NormLevel.subsumption` (`134:14`, `144:14`); explicit qualification fixed it. |
| v10 | Explicit actual normalization/fold rewrites exposed duplicate existing-row insertions; their list equality remained to be proved (`132:84`, `142:94`). Unused comparator simp arguments were removed. |
| v11 | The list-equivalence helper needed the actual comparator supplied explicitly (`58:2`); existing-row list goals also remained (`144:84`, `155:94`). |
| v12 | Conditional simp rewriting did not discharge the existing-row lookup premises (`144:84`, `155:94`); explicit local equations replaced it. |
| v13 | Default simplification rewrote get? to getElem? before the proof-specific lookup rule (`151:51`, `161:53`, `168:53`). Explicit lookup rewrites closed the premises. |

Exact command strings and unabridged compiler output for all 14 runs are in
`/tmp/lean4lean-comparison42-maps-runs.json` (61,286 bytes, SHA-256
`8d0cbe3242d5c56ca20b32b3e51b83c7552d46ebf52612cf23bc383a15082612`).
No artifact from a failed attempt was consumed. The final source/object pair
is:

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| `MapComparison.lean` | 8812 | `028ad8ca0168c68b25b627dfaa62ae0030990ac2f1f50fe324ab95845c7f0064` |
| `MapComparison.olean` | 342944 | `2444568d77b777022c63a02a60dcafc5be06c0b13a6082ddb960a234ea2949b4` |

The read-only final root's complete 42-record `identities.json` has SHA-256
`100ebbb3d91de03d47ffc84e9aaeb6f7ab2aac4fc5dbd2c3de9454373838383a`.
Its source matches the owned working-tree source byte for byte. Compiler
lease 2 was explicitly released after compilation and post-build identity
verification. No Cargo, push, or PR command ran in this lane. Integrated
aggregate, acceptance matrices, and policy audits belong to the coordinator.
