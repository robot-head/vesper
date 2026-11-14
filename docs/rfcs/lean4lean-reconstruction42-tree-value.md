# Actual tree-reification denotation (#550)

This lane owns only `TreeReifyValue.lean` and this report, from
`ed18ce23b06d3ffe0fe8a1a3eb1f605e043945ac`. The reconstruction contract was
read before authoring. Imports are exactly LevelSemantics and MapValue.

The main `tree_reify_value ρ t` equation is

```text
levelValue ρ t.reify =
  max t.const (max (varsValue ρ t.var)
    (t.child.foldr (fun e z => max (reifyChildValue ρ e) z) 0))
```

The two new nonrecursive definitions are `reifyOptionValue`, actual
`Option.elim 0 (levelValue ρ)`, and `reifyChildValue ρ (a,u)`, exactly
`Nat.imax (levelValue ρ u.reify) (ρ a)`. There is no recursive TreeValue
specification or replacement reconstruction algorithm. The main proof uses
one actual Tree.reify equation and the actual child/variable folds; no tree
induction or assumed child-correctness premise is needed.

`reify_mkChild_value` proves a uniform imax contribution even when the
reified child is syntactically zero and actual mkChild emits a parameter.
That branch is justified denotationally by `Nat.imax 0 (ρ a) = ρ a`, including
`ρ a = 0`. No opaque Level.beq reflection is used.

All eighteen public theorem names are frozen:

- `levelValue_ofNat`, `levelValue_addOffsetAux`, `levelValue_addOffset`;
- `reify_mkMax_value`, `reify_mkChild_value`;
- `reify_children_fold_value`, `reify_vars_fold_value`;
- `tree_reify_value`, `foldr_max_mem_bound`, `foldr_max_le`;
- `tree_reify_const_bound`, `tree_reify_vars_bound`, `tree_reify_payload_bound`;
- `tree_reify_var_bound`, `tree_reify_child_bound`, `tree_reify_edge_bound`;
- `tree_reify_leaf_value`, `tree_reify_le`.

The bounds are unconditional local facts. A stored variable contributes its
valuation plus arbitrary offset. A child contributes its actual imax value;
its edge parameter alone is also bounded. The upper-bound theorem takes
bounds on the actual constant, varsValue and every actual child contribution.
It does not assume its conclusion, MapWF, sorting, or a tree-shape predicate.
The leaf equation is exactly `max c (varsValue ρ vs)`.

## Genuine dependencies and execution

The original complete parent `/tmp/lean4lean-completion42-fold-v2` has a
50-record `fold-identities.json`, SHA256
`c1661137e6c549444335b171a4e7e795beb13217a3ee9d4b93ff33acbeeae249`.
All lengths/hashes were verified, and recorded project/vendor source bytes
matched this checkout. The whole parent root was copied to a fresh private
root, and only the private copy was unsealed.

The missing LevelSemantics source/object pair came from
`/tmp/lean4lean-addability42-shared-v1`. Its 58-record dependency manifest was
verified in full, SHA256
`021fd786e9b6f71a5dc9e62bb9f9c26928965386021d1f5c12bf660b7aa6111b`.
The added source matches this checkout. The original shared manifest is
retained. All 55 private input files are bound by
`tree-value-input-identities.json`, SHA256
`720bda0cee5fe926a6e8163b04f22d05fc5d8d397360de42cd0bc20dda6d65e8`.
Both original manifests and every private input were verified again after
compilation/probes. No dependency source/object was changed or recompiled.

No Lean command ran until the coordinator explicitly granted lease 1.
Every compile/probe used pinned Lean 4.24.0, commit
`797c613eb9b6d4ec95db23e3e00af9ac6657f24b`, executable
`/home/agent/.local/share/vesper-lean/bin/lean`, SHA256
`8fadc3ed92cc9decb94c96ee9c0876e91e03a3a5e69138f32114f02789e2766c`.
No Cargo command ran. The lease was explicitly released immediately after
the successful probes/postchecks, before this report/commit.

The exact compile command ran from the respective complete private directory
with N=1 (exit 1) and N=2 (exit 0):

```sh
LEAN_PATH=/tmp/lean4lean-reconstruction42-tree-value-vN LEAN_NUM_THREADS=1 \
  /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 \
  -o Lean4LeanLevel42/TreeReifyValue.olean Lean4LeanLevel42/TreeReifyValue.lean
```

V1 needed the original induction hypothesis after simplifier cancellation,
an explicit zero/nonzero valuation split for the syntactic-zero child,
an explicit `tree_reify_value ρ t` rewrite to avoid rewriting the child
reification instead of its parent, and the explicit Nat.le_max_right proof
for the edge bound. Two unused simp arguments were also removed. None of
the definitions, hypotheses or conclusions changed. V1 source and full
diagnostics remain immutable; no failed object was consumed.

| Failed v1 file | Bytes | SHA256 |
| --- | ---: | --- |
| TreeReifyValue.lean | 6419 | `ebb1f8539031e441765a1b3083813f34fb44c039a980dd3bfa467de321af9e19` |
| compiler.log | 1967 | `13a7f7b56e6aa5b397b9d290b179553a41f788b772e1b521a3d25b05c2748508` |

From v2, both exact probe commands exited 0:

```sh
LEAN_PATH=/tmp/lean4lean-reconstruction42-tree-value-v2 LEAN_NUM_THREADS=1 \
  /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 TreeValueAudit.lean
LEAN_PATH=/tmp/lean4lean-reconstruction42-tree-value-v2 LEAN_NUM_THREADS=1 \
  /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 TreeValueInventoryAudit.lean
```

The first fixed handwritten probe prints axioms for all twenty authored
public roots (two definitions/eighteen theorems) and the actual module
declaration inventory. The second fixed probe calls actual Lean.collectAxioms
for every one of the 65 original module declarations, including generated
equations, matchers and proof helpers. Every declaration is safe and has an
axiom set contained in the permitted standard three. There are no new
unsafe_rec/unsafeRec declarations or requested compiler-helper omissions.
The original module contains safe generated helpers such as
`levelValue_addOffsetAux._proof_1_12` and `tree_reify_le.match_1_1`; they are
included in that inventory/probe. A postprocessor initially expected one
printed line per declaration; handling wrapped axiom arrays corrected that
local assertion. The original successful Lean transcript was unchanged.

## Frozen artifacts and scope

Both private versions are read-only. The final complete root is
`/tmp/lean4lean-reconstruction42-tree-value-v2`.
Its `tree-value-identities.json` binds all 67 pre-manifest files, including
all actual sources/objects/companions, original manifests, command results,
toolchain identity, and the two probe inputs/transcripts. Manifest SHA256:
`d10369d67e9e2d3d37374996f9e8bb4bf0f4bd9a7656722d677eadea94ab098a`.

| Final file | Bytes | SHA256 |
| --- | ---: | --- |
| TreeReifyValue.lean | 6482 | `199feb2fb78e83b6cfa1e9df64ff54a5d2b37121e3973d5c78def294e03ed952` |
| TreeReifyValue.olean | 301112 | `7e28e2e909e6a4ef2ecc00b4df26526c3c440875e71c3814b29adb57758f422e` |
| compiler.log | 0 | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| TreeValueAudit.lean | 1499 | `841efeca0e7e09fbfb03d923b39bfa6fea96f404ea9938b3dc820c4599de82d9` |
| audit.log | 7380 | `5bebd1413d26e5f237fe7dd20a996a6ce20c3d9c400e27d6cc6721dcc96b95d7` |
| TreeValueInventoryAudit.lean | 394 | `bfd0d7f80e424c5d95244b336f843759d64f9b7774a435aacdafb36b7fc9be12` |
| inventory-audit.log | 7077 | `b9ab10556db8bfa4414856275d54658367534b2990ec88a8bd5359933c39d4ed` |

`git diff --check` passed. The coordinator's full projected audits/matrices
remain separate integration work. This lane establishes tree primitives;
it does not prove NormLevel.toTree correctness, normalized reconstruction,
minimality/canonicality, opaque Level equality reflection, whole-checker or
native equivalence. No production, parent, BUILD, auditor or runner file was
changed, and no push/PR action ran.
