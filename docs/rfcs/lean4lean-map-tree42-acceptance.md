# Raw map-to-tree denotation acceptance (#560)

The thirteenth fixed packet `Packet::MapTree` is authored against contract
`lean4lean-map-tree42-contracts.md` at
`75ced351d75cd8dde4d5495d90cdefcbb4e9ab85`, whose parent is own PR #556.
This lane ran static byte/source checks, direct rustfmt and BUILD
synchronization only. It ran no Cargo, Lean, Bazel or genuine matrix.
The two proof leaves are still being written: their final actual source
inventory, complete import graph and genuine acceptance remain pending.

Fixed IDs are label `lean4lean_map_tree42`, final module
`Lean4LeanLevel42.MapTreeValueFinal`, projection
`Lean4LeanMapTree42Kernel`, scope `audited_raw_map_to_tree_denotation` and
marker `LEAN4LEAN_MAP_TREE42_EVIDENCE`. This lane supplies the imports-only
final module, new fixed test, narrow runner/BUILD additions and this report.
There is no arbitrary source/module/proof selector, supplied mathematical
answer or weaker-policy input.

The proof authors confirmed these seven public required guards in namespace
`Vesper.Lean4LeanLevel42`:

- Primary endpoint: `norm_toTree_reify_value`.
- Path interfaces: `tree_empty_payload_sorted`,
  `tree_modify_payload_sorted`, `tree_modify_payload_frame`,
  `tree_modify_payload_mono`, `tree_modify_payload_lower` and
  `tree_modify_payload_upper`.

The intended endpoint is
`levelValue ρ s.toTree.reify = mapValue ρ s` for the unchanged actual
algorithms, with independent sorted stored-key and `MapCompletes s`
premises. Authors still must establish their sufficiency; any additional
structural premise must remain explicit. No final premise may supply the
desired denotation, unexplained semantic bounds, target freshness or
successful-search flags. This report records the contract, not an executed
proof of it.

Actual existing source imports give the twenty-one-module dependency union
of TreeStructure, TreeReifyValue and LexChainCompletion. Authors confirmed
TreePathValue will import only TreeStructure and TreeReifyValue, and
MapTreeValue only TreePathValue and LexChainCompletion. The actual final
module imports only MapTreeValue. These constraints give the following
24-module topological specification, fitting the unchanged cap exactly:

1. `Lean4Lean.List`
2. `Lean4Lean.Level`
3. `Lean4LeanLevel42.Semantics`
4. `Lean4LeanLevel42.ListProofs`
5. `Lean4LeanLevel42.OrderedInsert`
6. `Lean4LeanLevel42.VarInsert`
7. `Lean4LeanLevel42.NameOrder`
8. `Lean4LeanLevel42.SortedKeys`
9. `Lean4LeanLevel42.MapValue`
10. `Lean4LeanLevel42.LevelSemantics`
11. `Lean4LeanLevel42.MapAll`
12. `Lean4LeanLevel42.MapEquality`
13. `Lean4LeanLevel42.SortedSubset`
14. `Lean4LeanLevel42.MapAny`
15. `Lean4LeanLevel42.Addable`
16. `Lean4LeanLevel42.Completion`
17. `Lean4LeanLevel42.Feasible`
18. `Lean4LeanLevel42.ReifyCongruence`
19. `Lean4LeanLevel42.LexChainCompletion`
20. `Lean4LeanLevel42.TreeReifyValue`
21. `Lean4LeanLevel42.TreeStructure`
22. `Lean4LeanLevel42.TreePathValue`
23. `Lean4LeanLevel42.MapTreeValue`
24. `Lean4LeanLevel42.MapTreeValueFinal`

The two leaf files are not yet frozen, so this is an existing-source graph
plus confirmed leaf import constraints, not a claim that final new source
traversal passed. In-progress leaf drafts do not substitute for final source
checks. NormalizedCompletion is a branch parent only and is outside this
packet's import graph, as are the heavy reconstruction/normalization/Shape
aggregates and VariableSupport. Before genuine execution, final actual
imports and every guard must be checked against the frozen source.

Guards check required names only. The unchanged collector inventories every
actual public authored theorem and alternate-proof alias throughout the
selected helper closure, including additional leaf helpers. All such roots
must be audited in the existing batches of at most 128; none are replaced
by a golden answer or expected root count. The same original/projected
constant-information, source/object, occurrence/partition, provenance and
kernel checks still apply. No newly generated project-helper omission is
authorized; existing helper identities must be examined in actual evidence.

Static comparison against contract base
`75ced351d75cd8dde4d5495d90cdefcbb4e9ab85` confirms all twelve earlier
required catalogs and complete Packet specification arms are byte-identical.
Counts include each terminating newline:

| Prior packet | Required catalog | Complete specification arm |
| --- | ---: | ---: |
| Foundations | 1,243 | 796 |
| Construction | 1,656 | 777 |
| Order | 1,398 | 822 |
| Map | 1,445 | 920 |
| Normalize | 1,668 | 1,139 |
| Normalization | 1,293 | 1,426 |
| Comparison | 685 | 1,556 |
| Shape | 1,390 | 1,653 |
| Addability | 2,066 | 1,417 |
| Completion | 2,183 | 1,635 |
| Reconstruction | 2,054 | 1,739 |
| NormalizedCompletion | 165 | 1,633 |

The entire runner from `const POLICY` through EOF remains exactly 47,588
bytes, SHA-256
`2162a7e23f7de857d06542325e26be039d9e589ea117f6a93a7f62906b4da0b9`.
Removing only the new enum variant, catalog, specification arm and first
comment's twelve-to-thirteen change restores the base runner byte-for-byte.
The source cap 24, batch size 128, whole-matrix budget 300 seconds,
standard-three policy and every other policy/control/cap remain unchanged.

BUILD adds only the fixed test to `copy_test_cwd`, the existing
5,120-MiB/one-CPU tag list and the same macOS-only incompatibility select.
Removing those three additions restores the base BUILD file byte-for-byte.
Earlier labels, attributes and platform policies remain unchanged. Linux
and Windows retain the full logical audit and all negative controls.

Direct `rustfmt --edition 2024 --check` passed for the new test and common
runner. `python3 tools/bazel/sync_build_files.py --check` and
`git diff --check` passed. Static assertions verified all twelve prior
catalogs/specifications, the full validator suffix, additive reversibility,
the existing 21-module union, topological ordering under the confirmed leaf
imports, seven unique fixed guards, all fixed IDs and the actual imports-only
final module. The receipt
`/tmp/lean4lean-map-tree42-acceptance-static-check.json` explicitly records
pending final-new-source/guard assertions.

Final validation must run the genuine new fixed matrix with projection,
reimport, strict actual axiom policy, exact artifact identities and unchanged
missing/corrupt original/projection, changed-source and forbidden-alias
controls. Scoped lint/format/BUILD checks and actual Bazel packaging remain
coordinator work. Earlier runtime evidence retains its original source
identities; the twelve prior packets are not rerun unless selected inputs
change or a failure justifies it. No new execution success is inferred from
static preservation or historical passes.

The path contract concerns actual payload updates, sibling order, framing
through new zero ancestors and existing descendants, zero-old-target
monotonicity, gated payload/innermost-edge lower bounds and an upper bound
including all root-first prefix-edge bounds. Arbitrary reassociation
functions do not establish payload preservation. The map fold must keep the
original-map upper bound, processed-row lower bounds and zero payload at
pending distinct targets, with the actual chain's inside-out reversal.
Processed-prefix equality is not asserted: an early edge can depend on a
later row. All executable operations remain the existing definitions; no
replacement recursive tree evaluator, updater or specification is introduced
by this acceptance plumbing.

This raw-map theorem does not claim normalized-level reification,
canonicality, whole-checker/native/compiler equivalence or campaign
completion. No implementation-proof audit status changes. Final theorem
statements, full new-source inventory and identities, genuine packet results,
Clippy and Bazel evidence remain pending.
