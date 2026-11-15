# Actual Name comparator laws (#414)

The new `Lean4LeanLevel42/NameOrder.lean` imports frozen ListProofs at
`3f6a8c67739108429a1aca4d9f75b67524d9f27e` and proves the contract's laws for
explicit `Lean.Name.cmp`. It uses that prefix-first comparator throughout;
the global Name Ord uses `quickCmp` and supplies none of these laws.

The sixteen public ASCII theorem roots establish reflexivity, swap symmetry,
equality reflection and its converse, strict transitivity and irreflexivity,
and `Ordering.isLE` transitivity. The recursive strict proof uses a public
constructor-step lemma, `name_cmp_suffix_lt_trans`, with the existing scalar
comparison laws for Nat and String. The proved `nameCmpTrans` and
`nameCmpLawfulEq` theorem values are registered as instances using
`attribute [instance]`. `path_cmp_trans` and `path_cmp_lawfulEq` lift them
through the pinned core instances for explicit `List.compareLex Name.cmp`;
`path_cmp_eq_iff` exposes list equality reflection without another list-order
implementation.

Four kernel computations are public theorem roots:

- `name_cmp_numeric_witness`: numeric Name 2 precedes numeric Name 10.
- `name_cmp_string_witness`: string Name alpha precedes string Name beta.
- `name_cmp_prefix_witness`: numeric prefix 2 with suffix z precedes numeric
  prefix 10 with suffix a, exercising recursive prefix priority.
- `path_cmp_prefix_witness`: the singleton numeric path precedes its extension.

These are comparator and representation facts. They do not establish sorted
arbitrary input keys, map-wide construction or normalization correctness,
caller path priming, compiler refinement, or native equivalence.

## Dependency verification and kernel execution

Before compilation, each copied dependency source was compared byte for byte
with the frozen worktree source (the first two use the frozen vendor source
under `tools/lean4lean-foundations42/upstream`). The matching source SHA256
values are:

| Module | Source SHA256 |
| --- | --- |
| Lean4Lean.List | 4ddde2232aff649a50a8d1bec44e9495e2387dd3fcc29833d4f9c67c514431a4 |
| Lean4Lean.Level | 72fc6c19f871150b4b698f183bd1f4b0b0d91d6b1efa03fd43705ddb62b59af6 |
| Lean4LeanLevel42.Semantics | 2a4e8ccabdce2ebe1c36d9d1dbb682705e2c3270697ca9703ed7d44fac473159 |
| Lean4LeanLevel42.ListProofs | 0cdafbdac4884b86b1896170b005e1c6ef0f84bd8ebf71a99711f0f1ef3111ca |

Their exact source and olean bytes were copied from the verified immutable
`/tmp/lean4lean-construction42-ordered-v2` dependency root. The final root,
`/tmp/lean4lean-order42-name-v3`, records SHA256 and byte counts for all ten
source/object files in `identities.json`, and the eight dependency records
also appear in `dependency-identities.json`. Dependencies were not edited.

The initially supplied executable name `bin/lean4.24` did not exist and exited
127 before invoking any compiler. The actual pinned executable `bin/lean`
reports Lean 4.24.0, commit `797c613eb9b6d4ec95db23e3e00af9ac6657f24b`.
The first genuine attempt in version 1 failed on an insufficiently typed
scalar swap lemma. Version 2 failed because using the generic swap theorem
as a simp rewrite looped. Version 3 uses the scalar swap theorem explicitly
after structural simplification and exits zero with empty stdout/stderr:

```sh
LEAN_NUM_THREADS=1 LEAN_PATH=/tmp/lean4lean-order42-name-v3 \
  /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 \
  -R /tmp/lean4lean-order42-name-v3 \
  -o /tmp/lean4lean-order42-name-v3/Lean4LeanLevel42/NameOrder.olean \
  /tmp/lean4lean-order42-name-v3/Lean4LeanLevel42/NameOrder.lean
```

Final new source SHA256:
`932a487b98f1c895cdde9ccc5771cc9cda874eabca09ab40e236d987eac6ace9`
(4,382 bytes). Final olean SHA256:
`2490c4e3edefa313511c50c898d0bedb20f757d239dcd918c234772c509121c6`
(161,448 bytes).

A separate generated `/tmp/lean4lean-order42-name-audit-v1/Audit.lean`
imports this module and runs `#print axioms` for all sixteen actual authored
roots. This command exits zero:

```sh
LEAN_NUM_THREADS=1 LEAN_PATH=/tmp/lean4lean-order42-name-v3 \
  /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 \
  /tmp/lean4lean-order42-name-audit-v1/Audit.lean
```

The twelve general laws/bridges depend only on `propext`, `Classical.choice`,
and `Quot.sound`; all four computation witnesses have no axiom dependencies.
Whitespace verification passes. No Cargo invocation, dependency mutation,
profile edit, vendor edit, or parent-proof edit occurred. The Lean compiler
lease is released after this source-level audit. Complete projection,
all-alias association and all three strict acceptance matrices are the
coordinator's integration gate; this lane does not claim those have run.
