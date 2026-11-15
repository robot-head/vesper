# Actual subsumption and final normalization (#438)

This lane adds only `lean/Lean4LeanLevel42/MapSubsumption.lean`,
`lean/Lean4LeanLevel42/NormalizeFinal.lean`, and this report. The unchanged
normalizer source and all parent proof modules remain outside its ownership.

`minimizeEntry` abbreviates exactly the actual outer-fold source expression:
minimize the original row against the current accumulator, then erase an empty
result or insert the residual at the same key. Its value and WF theorems require
both `MapWF acc` and the actual lookup `acc.get? p = some original`.
`minimizeEntry_get_other` has no semantic premise.

`subsumption_fold_preserves` generalizes over a fixed list of original rows.
Its hypotheses give pairwise distinct source keys, a well-formed current map,
and the current lookup of every remaining original row. The induction uses the
outside-key lookup theorem to preserve those remaining lookups after visiting
one key. The specialization uses the actual `acc.toList`, Std's distinct-key
theorem, and Std's lookup/membership equivalence. Definitional reduction of
`minimizeEntry` identifies this specialization with the actual unchanged
`NormLevel.subsumption` fold.

The public witnesses execute actual operations. The bad-WF singleton stores
constant one and variable `u+0` under the empty key; at valuation zero, actual
subsumption changes its denotation from one to zero. The deletion witness
stores constant two at `[]` and constant one at `[u]`; actual subsumption erases
the distinct dominated `[u]` key while retaining the empty-key dominator.

`normalize_value` applies the new subsumption denotation theorem to frozen
`normalizeAux_empty_value` and `normalizeAux_empty_WF`, under structural
`NoMVars`. `normalize_WF` applies to every level. `normalize_value_witness`
normalizes an actual imax-over-max expression through final subsumption.

These results make no sortedness, cached-flag, canonicality, comparison,
reification, compiler, or native-code claim. They do not take a union with the
old self row. Every authored proof helper is a public ASCII theorem; the exact
source-expression abbreviation is the sole new definition.

The coordinator granted compiler lease 1 after the residual lane released it.
The immutable dependency snapshot was
`/tmp/lean4lean-normalization42-minimize-v1`: all 32 source/object byte counts
and SHA-256 hashes were verified before copying and again before compiling.
Its manifest SHA-256 is
`a90b33b336fd44629932689f399a392606cddab7256f634564dc436522b3b1d3`.
The 30 overlapping residual artifacts matched the residual snapshot byte for
byte; those in turn matched all 26 frozen normalize-value parent artifacts and
the frozen Subsumption source/object pair. No dependency source was edited or
recompiled in this lane.

The first immutable attempt, `/tmp/lean4lean-normalization42-subsumption-v1`,
failed at `MapSubsumption.lean:20:2`, `:33:2`, and `:52:2` because `split` did
not reduce the source expression's `let`; the proof now explicitly performs
`dsimp only`. At `:81:9`, Std comparator inference selected `compare` and could
not synthesize `Std.TransCmp compare`; the following `intro` consequently
reported no goals. The proof now supplies the actual
`List.compareLex Name.cmp` comparator explicitly. The algorithm and theorem
statements did not change. No object from this failed attempt was used.

Both commands below succeeded with exit status 0 and no diagnostics in the
fresh immutable `/tmp/lean4lean-normalization42-subsumption-v2` directory:

```sh
LEAN_PATH=/tmp/lean4lean-normalization42-subsumption-v2 LEAN_NUM_THREADS=1 /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 -o Lean4LeanLevel42/MapSubsumption.olean Lean4LeanLevel42/MapSubsumption.lean
LEAN_PATH=/tmp/lean4lean-normalization42-subsumption-v2 LEAN_NUM_THREADS=1 /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 -o Lean4LeanLevel42/NormalizeFinal.olean Lean4LeanLevel42/NormalizeFinal.lean
```

The failed v1 invocation used the same MapSubsumption command with the v1
directory in `LEAN_PATH` and as the current directory. All invocations used the
pinned Lean 4.24 binary, one thread, and the granted lease. The lease was
explicitly released immediately after the successful pair and post-build
identity verification. No Cargo command ran in this lane.

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| `MapSubsumption.lean` | 5606 | `3341dfa5a0f2873c41ac30e6abca407a0a254a4ffd4ce1c0f8cde2ff83d6b558` |
| `MapSubsumption.olean` | 143272 | `957b5abaa1280825f488653c1284077433db3bfff450a24dc5cb5e43d4f4e6ea` |
| `NormalizeFinal.lean` | 1152 | `c40b041cfcb518b5c3c16249004b339b390067c74b653a7bf61e3e050545705c` |
| `NormalizeFinal.olean` | 33736 | `2d4ac834ee8d8e9428e57acead7764ef91f08509c8e51a02f8eadda0f29679fa` |

The final snapshot's complete 36-record `identities.json` has SHA-256
`1a4dd362d02dffa414901e9f55ce109c2d2f83cb2474d93c476c6efa9713be9c`.
All 32 dependency identities were reverified after compilation, and both final
snapshot source files matched the working-tree files byte for byte. The final
source and object pair is ready for the coordinator's aggregate and genuine
acceptance checks; those integrated checks are outside this lane.
