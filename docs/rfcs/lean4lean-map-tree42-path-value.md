# Observed tree payload updates and gated value bounds (#558)

This lane owns only `TreePathValue.lean` and this report, from contract commit
`75ced351d75cd8dde4d5495d90cdefcbb4e9ab85`. The source imports exactly
TreeStructure and TreeReifyValue. No existing compiler/helper source changed.

The five new definitions are nonrecursive: root-first `treePathAt` uses the
existing List.foldl/Option.bind and actual child lookup; `treePayloadAt` reads
actual node fields through map/getD with zero default; `treeWithPayload`
assigns const/var while preserving children; TreeChildrenSorted quantifies
actual observed nodes; treePathGate uses the existing enabled predicate.
There is no new recursive tree specification, updater or evaluator.

## Six proved interfaces

- `tree_empty_payload_sorted` proves actual default Tree sibling sorting and
  zero payload at every observed/missing route. Actual default equals the
  literal zero/empty tree by the independently proved `tree_default_eq`.
- `tree_modify_payload_sorted` preserves observed sibling order under the
  unchanged actual Tree.modify with the child-preserving payload setter.
- `tree_modify_payload_frame` proves target observation exists, its payload
  equals the supplied Node, and every distinct route retains its old payload.
  The joint observation induction explicitly handles missing children through
  actual default, new zero ancestors, existing targets and descendants. It
  does not assume target freshness or equality of old/new subtree objects.
- `tree_modify_payload_mono` proves actual reified value cannot decrease when
  the old target payload is zero. The target may be missing or already present
  with children; zero-payload observation is an independent structural premise.
- `tree_modify_payload_lower` bounds the new target's gated payload by the
  actual updated value. For a nonempty inside-out path a::tail, it also bounds
  the innermost incoming edge valuation gated by the full reversed path.
- `tree_modify_payload_upper` proves an atomic upper bound from the old tree
  value, the new path-gated payload, and all root-first prefix-edge bounds.
  These are separate contribution bounds, not the desired updated-tree result.

The upper proof generalizes an internal prior gate for path induction. A
selected child continuation is bounded through the actual imax decomposition:

```text
gate prior (imax z (rho a)) =
  max (gate prior (rho a)) (gate (prior ++ [a]) z)
```

The old full tree bounds unmodified payload/children. Recursive update bounds
use the actual selected child or actual default, and the corresponding suffix
of the independently supplied path-edge bounds. Local modifyAt lemmas account
for all child entries using actual sorted lookup/membership, not a replacement
update function. Monotonicity similarly compares the actual full child folds.

Source reassociation (`tree_modify_append`/`tree_modify_rootfirst`) permits
arbitrary callback functions because it is an equation of the original
operation. Frame, sorting, monotonicity and the six payload interfaces use
only the actual child-preserving `treeWithPayload` setter. The inside-out
Tree.modify path is explicitly reversed for root-first observations.

The module has 43 authored public theorem roots and five nonrecursive
definitions. Their exact complete inventory and axiom sets are in the
immutable manifest; every original module declaration is also checked.

## Genuine compile and original-object checks

Final complete read-only root: `/tmp/lean4lean-map-tree42-path-value-v4`.
The entire immutable reconstruction integration provider was copied into each
fresh private version. All 50 original source/object records from
`/tmp/lean4lean-reconstruction42-integration-v1/reconstruction-integration-identities.json`
were verified before each compile and after final probes. All corresponding
project/vendor source bytes match this checkout, and no dependency was edited
or recompiled. The complete private namespace prevents project-prefix fallback.
Unused provider modules are preserved as provenance; the packet's actual
import union remains the contract's 24 modules after both new leaves/final.

Pinned compiler is Lean 4.24.0, commit
`797c613eb9b6d4ec95db23e3e00af9ac6657f24b`, at
`/home/agent/.local/share/vesper-lean/bin/lean`. Actual final commands from v4:

```text
/home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 -o Lean4LeanLevel42/TreePathValue.olean Lean4LeanLevel42/TreePathValue.lean
/home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 TreePathAudit.lean
```

Exact environment: `PATH=/usr/bin:/bin`, `LANG=C.UTF-8`, `LEAN_NUM_THREADS=1`,
`LEAN_PATH=/tmp/lean4lean-map-tree42-path-value-v4:/home/agent/.local/share/vesper-lean/lib/lean`.
Each command had a 180-second outer deadline. Compilation exited zero in
1.741 seconds with empty diagnostics. The actual original-object audit exited
zero in 5.704 seconds, with 16,670 output bytes.

The probe prints axioms for all 43 authored theorem roots plus all five
new definitions, and independently enumerates every declaration attributed
to the imported source module. All 60 actual declarations, including generated
equations/splitters/proof aliases, were safe, nonpartial and nonaxiomatic.
Every axiom closure was contained in exactly the allowed set propext,
Classical.choice, Quot.sound. The five definitions are axiom-free. There is
no new unsafe implementation helper or helper omission. Generated safe
aliases remain in the actual proof artifact and inventory.

The final source-only publication commit is
`48c554145f0a5cb5abe1982f36087c0bfe11ec66`. The same source/object snapshot
was published to the dependent map-to-tree author before this report. No
proof source change followed publication.

| Artifact | Bytes | SHA256 | BLAKE3 |
| --- | ---: | --- | --- |
| TreePathValue.lean | 23339 | 85862113ff58b0ee4111e69dbe76b5edc1ffc89e99d3b04422f7550dbb339a2b | f3e0a0176f43016c8b2ce837ec6b83fb0d4f5441ca984bf484084316dff47f39 |
| TreePathValue.olean | 506504 | bf5744df1f750b834e7b67b25af458c56cef8449de3c9da172c872f528f86209 | b29f848564638f1e18846e693f022cabb110987ddc87e5364a04cb0d0c8475ce |
| path-value-identities.json | 11889 | 8bdeb106a055fe6cab00d0294e9fda97f5d0ed3e188537f984da241a3e9683cd | 6186e39d9040d87e8c6e5480a60b093f4148aa2afe887a4020d27a0d51a7454e |
| path-value-manifest.json | 19409 | a28993d82c9b5948c66f124e838024b040e808bb863308055485853329e3efa3 | 281343d2cbaa6b792f3e668c603535cef0925c318ca20e78a6b70ae486ab5d17 |

The identities file has 52 original source/object records: the unchanged 50
plus the new source and object. The full manifest also records the actual
compiler identity/version, command results, probe input/output, every named
root's axiom set, and source commit. Its own content hash is reported above.

## Preserved attempts and scope

Fresh read-only roots v1-v4 preserve all exact source/command/output history.
V1's observer proof attempt needed variable-shadowing, default unfolding and
let reduction corrections. V2 compiled the observer/frame milestone cleanly;
its actual audit found 30 safe declarations with permitted axiom closures.
V3's full value draft exposed three elementary Nat proof issues: max
commutation, explicit imax monotonicity and an unavailable Nat lemma name.
V4 resolved these with actual max/imax proofs and passed the complete audit.
No failed object was imported, no interface was weakened and no proof
admission or new project axiom was used.

The compiler lease was explicitly released after final probes, before the
map author resumed. `git diff --check` passed. No Cargo, Bazel, foreign edit,
push or PR action ran in this lane. Original compilation/axiom checks are
not a claim that the coordinator's projected kernel replay, full packet,
Bazel packaging or corruption controls have passed.

These are generic actual tree-update observation/value facts. Raw-map
reification denotation is the dependent lane's theorem; normalized reification,
canonicality, opaque Level.beq reflection, compiler/native equivalence and
source-campaign claims remain outside this lane.
