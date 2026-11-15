# Raw map-to-tree denotation contracts (#557)

This is an implementation contract, not evidence of a completed proof. The
parent is own PR #556, `2eaea8909c4bfa4e76344608eb4b128375aed2ec`.
Ownership was checked against all 82 open issue bodies and all 27 open PRs'
complete changed-file lists before public claims #557–560 and source edits.

The intended endpoint `Vesper.Lean4LeanLevel42.norm_toTree_reify_value`
states `levelValue rho s.toTree.reify = mapValue rho s` for the actual
unchanged algorithms. Stored-key sorting and `MapCompletes s` are the intended
independent premises. Authors must prove their sufficiency; any additional
structural premise must remain explicit. No final hypothesis may supply the
desired denotation, unexplained semantic bounds or successful-search flags.
This is a raw-map theorem; normalized-level composition remains separate.

## Two source lanes

Lane A (#558) owns `TreePathValue.lean` and the path-value report. It imports
only `TreeStructure` and `TreeReifyValue`. Agreed nonrecursive observers are
`treePathAt` (root-first existing List.foldl), `treePayloadAt` (actual node
fields through Option.map/getD), `treeWithPayload` (child-preserving field
assignment), `TreeChildrenSorted` (observed sibling order) and `treePathGate`
(the existing enabled predicate gating a Nat).

Its six interface theorems are `tree_empty_payload_sorted`,
`tree_modify_payload_sorted`, `tree_modify_payload_frame`,
`tree_modify_payload_mono`, `tree_modify_payload_lower` and
`tree_modify_payload_upper`. They must establish actual default/base behavior,
target existence and payload, off-target payload framing, sibling ordering,
zero-old-target value monotonicity, gated new payload/innermost-edge lower
bounds, and an atomic upper bound from the old value, new gated payload and
all root-first prefix-edge bounds. Frame claims must include newly created
zero ancestors and existing descendants. Arbitrary functions may be used in
source reassociation equations, never in payload-preservation claims.

Lane B (#559) owns `MapTreeValue.lean` and the map-value report. It imports
only `TreePathValue` and `LexChainCompletion`. It proves sorted-key chain
injectivity, actual certificate-derived edge bounds, actual zero-offset
pruning recovery and invariants of the original map's actual toList fold.
Maintain the fixed original-map upper bound, processed-row lower bounds and
zero payload at pending distinct targets. A processed-prefix value equality
is false: an early update can create an edge supported by a later row.
Do not assume target freshness or forget the inside-out chain reversal.

All executable operations remain the existing definitions. Proof induction
is allowed; new recursive specifications, updaters, evaluators, algorithm
copies, helper omissions, axioms, sorry/admit, unsafe/partial/native_decide,
opaque Level.beq bridges and cap/policy relaxations are excluded.

## Fixed acceptance lane

Lane C (#560) owns the imports-only `MapTreeValueFinal.lean`, new fixed test,
narrow runner/BUILD additions and acceptance report. Identifiers are
`Packet::MapTree`, label `lean4lean_map_tree42`, final module
`Lean4LeanLevel42.MapTreeValueFinal`, projection `Lean4LeanMapTree42Kernel`,
scope `audited_raw_map_to_tree_denotation` and marker
`LEAN4LEAN_MAP_TREE42_EVIDENCE`. The primary required root is the final
`norm_toTree_reify_value`; all actual authored theorems and aliases are
inventoried and audited, beyond required-name guards.

The actual existing union of TreeStructure, TreeReifyValue and
LexChainCompletion contains 21 project modules. Two proof leaves and one
imports-only final must fit the unchanged cap of 24. Do not import the heavy
reconstruction/normalization/Shape aggregates or hide dependencies. Preserve
all twelve existing packet catalogs, specifications, sources and the full
validator. Keep the existing copy, 5120 MB/one CPU and macOS scheduling rules.

Use complete immutable versioned private dependency roots, no project-prefix
fallback, at most two Lean compilers globally and an exclusive shared Cargo
lease. Final validation must execute a genuine new fixed packet, projection,
reimport, strict axiom policy and unchanged corruption/forbidden-alias
controls, plus scoped lint/format/BUILD checks and actual Bazel packaging.
Earlier runtime evidence keeps its original source identities. No normalized
reification, canonicality, whole-checker/native or campaign result is claimed
by this contract. Do not auto-merge a stack child onto its unprotected parent.
