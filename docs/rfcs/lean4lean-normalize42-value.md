# Actual recursive construction value (#427)

Scope is the actual `Lean.Level.Normalize.normalizeAux` construction before
subsumption. The new source is `lean/Lean4LeanLevel42/NormalizeValue.lean`.
Frozen parent helpers, actual vendor normalizer, and provenance remain unchanged.

`normalizeAux_value` states the exact maximum of the arbitrary old map value
and the enabled source level value plus the arbitrary offset. Its only premises
are structural `NoMVars l` and `PathPrimed p acc`. It assumes neither map
well-formedness nor path sorting. `normalizeAux_empty_value` specializes this
to the empty map, empty path, and zero offset.

The proof recurses on structural `levelSize`, including the fresh imax levels
produced by the max and nested-imax rewrites. Sequential recursive calls use
arbitrary-path priming preservation. The parameter helper proves the actual
some/none branches: some first adds the old-path constant then adds the new
variable at the extended path; none with zero offset uses existing variable
coverage, while positive offset uses actual current-key presence for modify.
The skipped-one constant uses priming-derived coverage through the frozen
parent theorem. No cached level flags enter either domain or measure.

Eight public witnesses refer to actual construction:

- `normalizeAux_imax_max_witness`: fresh imax-over-max rewrite with offset 3.
- `normalizeAux_imax_imax_witness`: fresh nested-imax rewrite with offset 4.
- `normalizeAux_member_zero_witness`: repeated member at zero offset.
- `normalizeAux_member_positive_witness`: repeated member with positive offset.
- `normalizeAux_disabled_witness`: zero-valued activation disables imax input.
- `normalizeAux_retains_old_witness`: arbitrary existing-map contribution remains.
- `normalizeAux_primed_duplicate_witness`: a genuinely primed, unsorted key
  already containing duplicate names, with insertion creating another duplicate.
- `normalizeAux_succ_mvar_counterexample`: actual construction of successor
  metavariable has value zero, but the totalized evaluator has value one.

The counterexample explains the structural domain restriction. There is no
claim about full `Normalize.normalize`, subsumption, cached-domain equivalence,
or implementation/compiler/native equivalence.

Validation used the explicitly granted compiler lease 1, then released it after
the successful compile. The exact successful command, run from
`/tmp/lean4lean-normalize42-value-v1`, was:

```sh
LEAN_PATH=/tmp/lean4lean-normalize42-value-v1 LEAN_NUM_THREADS=1 /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 -o Lean4LeanLevel42/NormalizeValue.olean Lean4LeanLevel42/NormalizeValue.lean
```

Exit status was zero with no diagnostics. All eleven authored public theorem
commands compiled, including the two main APIs, parameter helper, and eight
witnesses. Four earlier source compilations failed: first on a simp loop from
opposite max-offset rewrite orientations, unfinished termination simplification,
and an insufficiently explicit duplicate-key lookup witness; second on an
indentation error while editing the proof; third on missing retained level-case
equalities in recursive termination goals; fourth on structural-domain transport
after retaining those equalities. The final source fixes these problems using
`simp only`, explicit lookup witnesses, named case equalities, and explicit
structural-domain simplification. Two earlier invocation/setup failures occurred
before source compilation: a copied read-only metadata file could not be
rewritten, and Lean rejected an input outside the invocation root directory.
Only copied directory permissions and fresh metadata/source files were adjusted;
all dependency source and compiled bytes remained unchanged.

The immutable published root is `/tmp/lean4lean-normalize42-value-v1`. Its
24 dependency source/artifact identities were checked against the published
invariants manifest before use and again after compilation. The ten frozen
parent source files were also compared byte-for-byte against this worktree.
The two new dependency sources were consumed directly from their genuine
compiled immutable closure. The published 26-file closure has:

- `value-input-identities.json` SHA-256:
  `ec2f9a69e43af504e2ae14fa11d9f2dfcb791bdb99d3af704b3fab4067a73563`.
- `NormalizeValue.lean`, 10857 bytes, SHA-256:
  `94826b3eb799c1c67eadd22efd75864492d7cf558fbc418b226bf5ade6dafbef`.
- `NormalizeValue.olean`, 1704360 bytes, SHA-256:
  `0e11fed6c872dd963f95729b925d2dea30df0b8ecc04b4df2c08a58986f02406`.
- Final `identities.json` SHA-256:
  `2f3d597fd9dfd69c674bb83ba637560d4fb9617c3bd1b596196f37bf52ad5cd9`.

The published source is byte-identical to the owned worktree source. All source
is fixed handwritten Lean; no generated Lean or audit script was introduced.
`git diff --check` passed. No Cargo command was run by this task. Integration
and all-five acceptance matrices belong to the coordinator; this report makes
no additional audit-status or implementation-equivalence claim.
