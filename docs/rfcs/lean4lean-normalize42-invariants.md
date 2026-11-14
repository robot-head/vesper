# Actual recursive construction invariants (#426)

This packet adds `lean/Lean4LeanLevel42/NormalizeInvariants.lean` over the
unchanged actual `Lean.Level.Normalize.normalizeAux`, before subsumption.
It imports the structural `LevelSemantics` measure and the frozen actual
map/path update proofs. No existing source is changed.

The public APIs are:

```lean
theorem normalizeAux_preserves_pathPrimed
    (l : Level) (p q : List Name) (k : Nat) (acc : NormLevel)
    (h : PathPrimed q acc) :
    PathPrimed q (Normalize.normalizeAux l p k acc)

theorem normalizeAux_preserves_WF
    (l : Level) (p : List Name) (k : Nat) (acc : NormLevel)
    (h : MapWF acc) : MapWF (Normalize.normalizeAux l p k acc)

theorem normalizeAux_empty_WF (l : Level) :
    MapWF (Normalize.normalizeAux l [] 0 {})
```

Priming is preserved at arbitrary `q`, independently of normalization's path
`p`. Both preservation theorems hold for all actual levels, including
metavariables, without domain, current-path priming or sorting assumptions.

The additional public helper `normalizeAux_preserves_property` proves the
recursive argument once for any map predicate preserved by actual
`addConst`, `addNode` and `addVar`. Its variable-update preservation premises
require actual key membership. Successful ordered insertion supplies
`orderedInsert_inserted_mem`; the none branch supplies
`orderedInsert_none_mem`. The required theorems instantiate this helper with
existing genuine actual-update preservation lemmas. The helper does not
assume a result of normalization.

The recursion uses `levelSize l` and unfolds the actual well-founded
normalizer through its equation theorem with `rw [Normalize.normalizeAux]`.
The fresh calls for imax over max and nested imax are explicitly present;
their strict measure decreases reduce to natural-number arithmetic.

Validation and immutable source/artifact identities follow below.

The pinned binary reports Lean 4.24.0, commit
`797c613eb9b6d4ec95db23e3e00af9ac6657f24b` (Release). With the exclusive
assigned compiler lease, the actual command was run in
`/tmp/lean4lean-normalize42-invariants-v1`:

```sh
LEAN_NUM_THREADS=1 LEAN_PATH=/tmp/lean4lean-normalize42-invariants-v1 /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 -o Lean4LeanLevel42/NormalizeInvariants.olean Lean4LeanLevel42/NormalizeInvariants.lean
```

Three real compiler attempts were made. The first exited 1: in the standalone
parameter none branch, `split` selected a retained reducible match before the
intended offset conditional. The second exited 0 with an unused simp-argument
warning after explicitly reducing that match. The final source uses `change`
to expose the offset conditional and the third attempt exited 0 with no
diagnostics. Equation rewriting and every measure-decrease obligation passed.
No Cargo command was run. No generated Lean was used; the source is fixed
handwritten Lean. The compiler lease was explicitly released after freezing
the published source and artifact.

All 20 source/artifact identities from immutable
`/tmp/lean4lean-map42-priming-v2` were verified, with all ten source files
byte-equal to their actual worktree counterparts. The published semantics
closure `/tmp/lean4lean-normalize42-semantics-v1` was then verified against
all 22 records and its manifest SHA-256
`a2d20cee90add64c10e1df295fe054bcf9f808cbb158d4c7bfc4f3d33a0b2965`.
The shared parent bytes agreed exactly. The final invariant source was
byte-compared with the worktree; all 22 imported source/artifact records were
verified again before publication.

The immutable invariant root contains the exact 24-file source/artifact
closure, with `input-identities.json`, `parent-identities.json` and final
`identities.json`. Final manifest SHA-256:
`ec2f9a69e43af504e2ae14fa11d9f2dfcb791bdb99d3af704b3fab4067a73563`.

| Owned artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| `Lean4LeanLevel42/NormalizeInvariants.lean` | 4260 | `a4318b8b2d9327d1eec069717379e5b625d45a29faba7093dc2e798e901575df` |
| `Lean4LeanLevel42/NormalizeInvariants.olean` | 206840 | `ac8a209d781520461ee05bd880abc88a223348c7a5c27b1781b7adda8ca893bc` |

These are pre-subsumption construction invariants. They make no claim about
full normalization, the later subsumption pass, or implementation-proof
audit status. Integration acceptance and the five full matrices belong to
the coordinator.
