# Actual raw-map reconstruction denotation (#559)

The frozen source commit is `1db1a435618e6f860cf5e2e51602d25614505407`.
This lane owns only the new MapTreeValue.lean and this report, from contract
commit `75ced351d75cd8dde4d5495d90cdefcbb4e9ab85`. The contract was read before
authoring. The A source was consumed as a separate authorized dependency
commit; it is not included in this lane's source commit.

The final actual theorem is:

```lean
theorem norm_toTree_reify_value (ρ : Valuation) (s : NormLevel)
    (hKeys : ∀ p n, s.get? p = some n → KeySorted p)
    (hCompletes : MapCompletes s) :
    levelValue ρ s.toTree.reify = mapValue ρ s
```

Only strict sorting of actual stored keys and independent MapCompletes are
needed. There is no MapWF, variable sorting, nonempty-row, NoMVars, supplied
candidate, unexplained semantic-bound or desired-result premise. No normalized
image or normalized-level composition is asserted. The definitions remain the
unchanged compiler functions; the proof introduces no executable definition.

The nine public authored roots are `lexChain_key_injective`, `enabled_perm`,
`edgeCovered_enabled_le_mapValue`, `admissible_prefix_enabled_bound`,
`enabled_subset_gate_le`, `subsumeVars_zero_node_union`,
`map_tree_row_update_bounds`, `map_tree_fold_value_bounds`, and
`norm_toTree_reify_value`. Imports are exactly TreePathValue and
LexChainCompletion. Their existing 21-module union, A, B and the root-owned
imports-only final fit the unchanged 24-module cap. Unused parent modules in
the complete private artifact root are not hidden import dependencies.

## Proof structure and genuine premises

Unconditional actual chain permutation and strict Name.cmp ordering make
stored-key chains injective. Edge bounds come from actual EdgeCovered
witnesses at arbitrary offsets, not zero offsets or WF assumptions. Prefix
admissibility gives the accumulator front.reverse; its membership is then
transported to front. Gates handle zero-valued edge names explicitly.

The actual zero-offset subsumeVars pruning retains the max-union with the
incoming variable value, using the existing unconditional subsumeVars_union.
A's lower bounds recover each original row from its residual payload and
actual gated incoming edge. A's upper bound uses original mapValue, actual
row lookup and independently derived prefix-edge bounds.

The actual ORIGINAL s.toList fold maintains sibling sorting, zero payload at
pending distinct targets, an earned scalar lower bound, and the fixed original
map upper bound. Off-target payload framing preserves pending zero, including
new zero ancestors. Zero-target monotonicity retains previous row lower bounds.
No claim equates partial-tree value with processed-prefix map value: that
stronger claim is false when an edge is supported by an unprocessed row.

The final proof unfolds actual toTree and mapValue, rewrites their actual
folds by Std.TreeMap.foldl_eq_foldl_toList and uses actual stored-row membership
and distinct keys with comparator List.compareLex Name.cmp. Local let-bound
row expressions are the unfolded actual source step, not a replacement
updater or separately defined algorithm. No physical map equality or opaque
native Level.beq bridge is involved.

## Original dependency verification and compiler commands

Before initial compilation, all 50 records in the genuine provider
`/tmp/lean4lean-reconstruction42-integration-v1/reconstruction-integration-identities.json`
were verified by length and SHA256. Manifest SHA256:
`2ce9d9be5126bbdd3799d060dbada87946faf0109f165e754509c60e7fc039d1`.
Every recorded project/vendor source matched this checkout; Completion was
explicitly required to have the final folded-definition SHA256
`f89ca02776f406b633af9411aae7d0875e4067d927f0629c5d8f81025a678505`.
The initial complete private input manifest had 51 files and SHA256
`f33341bfc1027ad2605c6a8eabecde1995924497af59424cc42c402647408a7f`.

V1–v3 compiled only independent actual old-dependency lemmas, importing
LexChainCompletion. The lease was explicitly released while A was unfinished.
No draft, axiom, sorry or stub A module was compiled or imported. Full B
compilation began only after the root conditionally regranted a lease and
A's complete genuine artifact was frozen and verified.

A source commit `48c554145f0a5cb5abe1982f36087c0bfe11ec66` was cherry-picked
as a dependency. Complete provider `/tmp/lean4lean-map-tree42-path-value-v4`
has path-value-manifest.json SHA256
`a28993d82c9b5948c66f124e838024b040e808bb863308055485853329e3efa3`.
All 52 source/object records and eight evidence records were verified;
source bytes matched the checkout. A source SHA256 is
`85862113ff58b0ee4111e69dbe76b5edc1ffc89e99d3b04422f7550dbb339a2b`;
A original object SHA256 is
`bf5744df1f750b834e7b67b25af458c56cef8449de3c9da172c872f528f86209`.

Full B attempts copied that WHOLE complete root; only fresh private copies
were unsealed. No dependency object was recompiled or changed. Their
61-record map-value-input-identities.json SHA256 is
`5079af52a6834d8971329178b58b7c978cf3b8400446c1611b92cc18a9137623`.
Every input, the original provider manifests and source associations were
verified again after the successful compile/probe. The final own source
matches the source commit byte for byte. One complete LEAN_PATH prevented
namespace-prefix fallback or shadowing.

All commands used pinned Lean 4.24.0, commit
`797c613eb9b6d4ec95db23e3e00af9ac6657f24b`, executable
`/home/agent/.local/share/vesper-lean/bin/lean`, SHA256
`8fadc3ed92cc9decb94c96ee9c0876e91e03a3a5e69138f32114f02789e2766c`.
No Cargo or Bazel command ran in this lane. The exact compile command ran
from each complete private vN directory, N=1 through N=7:

```sh
LEAN_PATH=/tmp/lean4lean-map-tree42-map-value-vN LEAN_NUM_THREADS=1 \
  /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 \
  -o Lean4LeanLevel42/MapTreeValue.olean Lean4LeanLevel42/MapTreeValue.lean
```

V1 exited 1 because record projections needed reduction before omega.
V2 (four helpers) and v3 (six helpers) exited 0 with empty diagnostics.
V4, the first full nine-root attempt, exited 1: multiline record-field if-let
layout needed disambiguation, and actual default reification required A's
already proved tree_default_value rather than rfl. V5 exited 1 with three
local path-case elaboration mismatches. V6 exited 1 with the final nil-path
gate-rewrite mismatch. V7 exited 0 with empty diagnostics. These were proof
elaboration changes; no final premise or conclusion was weakened. No failed
object was consumed. All seven original inputs/results and genuine objects
remain in immutable versioned roots.

The final probe command, from v7, exited 0:

```sh
LEAN_PATH=/tmp/lean4lean-map-tree42-map-value-v7 LEAN_NUM_THREADS=1 \
  /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 MapTreeValueAudit.lean
```

Its fixed source prints axioms for all nine authored roots and uses actual
module ownership to inventory and collect axioms for EVERY original module
declaration. All 13 actual declarations are safe, nonpartial and nonaxiom,
with axiom sets contained in propext, Classical.choice and Quot.sound.
enabled_perm and enabled_subset_gate_le need only propext/Quot.sound; the
other seven authored roots use the standard three. The extra actual roots
are map_tree_row_update_bounds.match_1, the two
edgeCovered_enabled_le_mapValue._proof_1_* declarations, and
subsumeVars_zero_node_union._proof_1_1. Every one was probed. No new recursive
definition, unsafe_rec helper, omission or weaker policy was introduced.
The compiler lease was explicitly released immediately after this probe;
no further compiler or broad probe ran.

## Immutable final receipt and boundaries

Final root: `/tmp/lean4lean-map-tree42-map-value-v7`.
Its map-value-identities.json records all 71 pre-manifest files, including
original dependency manifests, actual source/object companions, commands,
toolchain identity and probe input/output. Manifest SHA256:
`99faf4228dd6cf4e4ea86d178b1d063cbf44f59da418ff2e14bae94de3583072`.

| Final artifact | Bytes | SHA256 |
| --- | ---: | --- |
| MapTreeValue.lean | 12485 | `42b34ed8b95cfd649acaf86041d89d8d56532f4a7a91f8cbe072fae28bbda3f9` |
| MapTreeValue.olean | 586552 | `14b650df25de4c73c734acfabb27d8e6ab3f5bf8b4977c7ee35d86cf750048f5` |
| MapTreeValueAudit.lean | 1307 | `4bb6fbb8a2eee0e6619a81d13c75560debd470cc3b394c16c106f37d38988065` |
| map-audit.log | 2862 | `c81e845ff1583d2c1923e3120af74d42249fed7924a592901670ad9e32a0c8cd` |
| map-attempt-identities.json | 4928 | `acc24e858219ae014c5b85782ca3515596a8007eabef2561812fedbd0a00c086` |

The attempt manifest binds every retained source/object/log/result across
v1–v7. In particular failed v4/v5/v6 log SHA256 values are
`fef4405b8e883210a5c41aaaab5c1e191ef9b59c680650c6e18f224698aecc79`,
`a857b1ba1a6ec03ce151aeba88beacda331f485a20f2ab78cca7edaa49419c71`, and
`c887e6f5e2423be05c128bdb61cee09b7695451b0adf644908749db77be57cdb`.
The final compile log is empty, SHA256
`e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`.
All private roots are read-only. `git diff --check` passed.

Source was committed immediately after the successful complete probe so
root could begin genuine packet replay; this report follows separately.
The coordinator's full projected replay, corruption/alias controls, old
matrices and actual Bazel packaging remain separate integration evidence;
none is claimed by this local report. There is no normalized reification,
minimality/canonicality, opaque Level equality, whole-checker/native
equivalence or source-campaign claim. No push or PR action ran.
