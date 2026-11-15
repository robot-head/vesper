# Construction, chain and tree prerequisite acceptance (#552)

All eleven genuine manual matrices passed on frozen source
`3c526dac65c92e7f4c276b2e7255ca5398142068`, including Reconstruction in
224.895 seconds. The actual Bazel Reconstruction target also passed, with
one test and zero failures or ignored tests in 200.07 seconds.
The coordinator executes the harnesses and Lean processes; this lane
independently checks source, original/projected, executable and report
identities without running Cargo or Lean.

Manual execution uses exact repository test sources compiled directly with
CI-pinned Rust 1.100.0 and real dependency artifacts, invoking the archived
production CLI built by Bazel. These are direct-rustc harnesses, not
Cargo-built or Bazel-built test binaries. The separate actual Bazel target
build passed in 89.133 seconds, and its complete test command passed in
270.241 seconds. These are local executions, with no hosted/RBE claim.

The fixed IDs are label `lean4lean_reconstruction42`, module
`Lean4LeanLevel42.ReconstructionFinal`, projection
`Lean4LeanReconstruction42Kernel`, scope
`audited_construction_completion_admissible_chains_and_tree_primitives`
and marker `LEAN4LEAN_RECONSTRUCTION42_EVIDENCE`. The coordinator owns the
imports-only final module, test entry point, narrow BUILD registration and
integration report. This closed packet adds no arbitrary source/module/proof
selector, supplied expected answer or weaker-policy flag.

Traversing the actual final source imports recovers exactly these twenty-four
modules in dependency order:

1. `Lean4Lean.List`
2. `Lean4Lean.Level`
3. `Lean4LeanLevel42.Semantics`
4. `Lean4LeanLevel42.ListProofs`
5. `Lean4LeanLevel42.OrderedInsert`
6. `Lean4LeanLevel42.VarInsert`
7. `Lean4LeanLevel42.NameOrder`
8. `Lean4LeanLevel42.SortedKeys`
9. `Lean4LeanLevel42.MapValue`
10. `Lean4LeanLevel42.MapHelpers`
11. `Lean4LeanLevel42.LevelSemantics`
12. `Lean4LeanLevel42.MapAll`
13. `Lean4LeanLevel42.MapEquality`
14. `Lean4LeanLevel42.SortedSubset`
15. `Lean4LeanLevel42.MapAny`
16. `Lean4LeanLevel42.Addable`
17. `Lean4LeanLevel42.Completion`
18. `Lean4LeanLevel42.Feasible`
19. `Lean4LeanLevel42.ReifyCongruence`
20. `Lean4LeanLevel42.ConstructionCompletion`
21. `Lean4LeanLevel42.LexChainCompletion`
22. `Lean4LeanLevel42.TreeReifyValue`
23. `Lean4LeanLevel42.TreeStructure`
24. `Lean4LeanLevel42.ReconstructionFinal`

The existing import union contains nineteen modules; the four leaves and
imports-only final module add five. Every dependency precedes its user.
Completion uses the fold-based definition from the final 19d replay, source
SHA-256 `f89ca02776f406b633af9411aae7d0875e4067d927f0629c5d8f81025a678505`.
VariableSupport, subsumption proof modules, normalization invariants,
PathPriming, Shape/ShapeFinal and ReifyCycle are outside this closure. The
selected sources fit the unchanged hard cap of 24. The unchanged actual
import-closure check requires every selected source to enter the replayed
original artifact closure.

The 37 unique required guards use frozen public ASCII theorem names in
namespace `Vesper.Lean4LeanLevel42`:

- Construction completion: `normalizeAux_completion`,
  `normalizeAux_empty_completion`, `normalizeAux_preserves_completes`.
- Actual chain selection and orientation: `admissible_append_iff`,
  `completes_last_edge`, `lexChain_candidate_exists`, `lexChain_find_some`,
  `lexChain_no_nonempty_fallback`, `lexChain_nonempty_step`,
  `lexChain_reverse_admissible`, `lexChain_length_reverse_admissible`,
  `lexChain_reverse_order_witness`.
- Actual tree reification primitives: `levelValue_ofNat`,
  `levelValue_addOffsetAux`, `levelValue_addOffset`, `reify_mkMax_value`,
  `reify_mkChild_value`, `reify_children_fold_value`,
  `reify_vars_fold_value`, `tree_reify_value`, `foldr_max_mem_bound`,
  `foldr_max_le`, `tree_reify_const_bound`, `tree_reify_vars_bound`,
  `tree_reify_payload_bound`, `tree_reify_var_bound`,
  `tree_reify_child_bound`, `tree_reify_edge_bound`,
  `tree_reify_leaf_value`, `tree_reify_le`.
- Actual list update structure: `treeEntriesLookup_cons`,
  `treeEntriesLookup_none_of_not_mem`, `modifyAt_keys`,
  `modifyAt_lookup_other`, `modifyAt_keysSorted`, `modifyAt_lookup_target`,
  `modifyAt_unsorted_target_witness`.

Guards only require presence. The unchanged source-command extractor derives
every actual public authored theorem in the selected helper closure,
including the additional construction helpers outside this guard list. The
static inventory observes 12 ConstructionCompletion, nine LexChainCompletion,
18 TreeReifyValue and seven TreeStructure theorem commands. The two
nonrecursive tree-value definitions and one nonrecursive lookup definition
are not theorem commands; their actual constant information and dependencies
remain part of the genuine original/projection checks. These counts are
observations, not a golden root inventory or acceptance cap. Every actual
alternate-proof alias joins the authored-root union and is audited in the
existing 128-root batches.

Static byte comparison against base
`ed18ce23b06d3ffe0fe8a1a3eb1f605e043945ac` confirmed all ten earlier required
catalogs and complete Packet specification arms remain identical. Including
their terminating newline, the byte counts are:

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

The complete runner from `const POLICY` through EOF remains byte-identical:
47,588 bytes, SHA-256
`2162a7e23f7de857d06542325e26be039d9e589ea117f6a93a7f62906b4da0b9`.
Removing only the new enum variant, catalog, specification arm and
introductory ten-to-eleven comment restores the base runner byte-for-byte.
Static assertions passed for the actual twenty-four-module graph and order,
all five fixed IDs, all 37 unique qualified guards and their presence in the
actual public theorem commands, the catalog association, unchanged source
cap and fold-based Completion identity. The inspected import/source receipt
is `/tmp/lean4lean-reconstruction42-acceptance-static-check.json`. Direct
`rustfmt --edition 2024 --check tests/common/lean4lean_proof_packet.rs` and
`git diff --check` passed. This lane changes only the narrow runner extension
and this report, with no test, BUILD or earlier platform-policy changes.

The unchanged validator retains actual source/provenance binding,
original/projected source/object associations, occurrence/partition checks,
complete authored-root/alias discovery, strict proof auditing, bounded
regular-file capture, missing/corrupt original and projected artifact
controls, changed-source refusal and the forbidden alternate-proof control.
The source cap of 24, batch bound of 128, standard-three axiom policy,
300-second whole-matrix budget and every other cap/control remain unchanged.
Genuine execution must inspect the actual declaration and helper inventory;
it cannot infer the new packet's omission or duplicate counts from an older
packet. The existing LevelSemantics generated helpers are historical inputs,
while no newly generated project-helper omission is authorized.

The construction claim preserves MapCompletes and VarSupport through
the actual normalizeAux rewrite branches from a complete input map and a
certificate for its current path. It stops before subsumption and adds no
sorting or NoMVars premise. The chain result uses sorted stored keys, a
sorted path, independent completion and the actual remaining-length fuel
bound, with inside-out orientation expressed by admissibility of the reversed
actual chain. Nonempty fallback exclusion concerns the actual candidate
find returning some value; equality between the returned chain and input
path does not by itself identify fallback. No lexicographic minimality claim
is inferred from admissibility.

The tree-value equation is about actual Tree.reify: the maximum of its
constant, variable value and actual child contributions. An edge `(a,t)`
contributes `Nat.imax (levelValue ρ t.reify) (ρ a)`, including the syntactic
zero branch. Local payload, variable, child-contribution and incoming-edge
bounds do not treat an unguarded child payload as a parent bound. There is
no replacement recursive TreeValue function or map-to-tree denotation claim.
The structure APIs describe actual modifyAt key projection, other-key lookup,
sorted preservation and target lookup, with the stated sorting premise and
an actual unsorted target counterexample. The lookup definition delegates
to List.find?; it is not a replacement update algorithm or recursive shape
predicate.

Construction-plus-subsumption composition, recursive tree-shape/payload
preservation, map-to-tree denotation, normalized reification correctness,
minimality/canonicality, opaque native Level.beq reflection and whole-checker/
compiler/native equivalence remain separate obligations. No source-campaign
completion or implementation-proof audit status change is claimed.
Execution below is coordinator evidence independently inspected by this lane;
this lane itself ran no Cargo or Lean command.

Independent inventory of every selected helper source recovered exactly 237
public authored roots, equal to the audited set: 191 existing and 46 new
roots. The four leaves contribute 12 ConstructionCompletion, nine
LexChainCompletion, 18 TreeReifyValue and seven TreeStructure theorems. The
37 guards above plus nine additional construction helpers form the complete
new theorem inventory. The two tree-value definitions and nonrecursive lookup
definition are retained constants, not theorem commands. The fold-based
Admissible definition is also retained, with no generated recursion helper.

The actual closure has twenty-four modules and 48 original source/object
records. Its 768 original constant occurrences represent 766 distinct
originals, with 752 retained occurrences, 750 canonical output constants and
sixteen omitted compiler helpers. The complete omission records match the
historical Addability checkpoint exactly: thirteen vendor helpers and the
three pre-existing LevelSemantics helpers for NoMVars, levelSize and
levelValue. No new leaf/Core helper is omitted. The two repeated retained
occurrences are identical safe realizations of
`Lean.Level.Normalize.NormLevel.feasible.go.eq_1` and `.eq_2` in Feasible
and ReifyCongruence. There are zero alternate-proof aliases.

Evidence reports exact inventory, constant-info equality, empty extensions
and IR, successful kernel replay and verified reimport. Logical traversal
visited 1,747,729 expression nodes. The projection
`Lean4LeanReconstruction42Kernel.olean` is 6,603,192 bytes, BLAKE3
`f4e3a91fe8c2e4e12a2fe0f3264a9a650977571b5c5efa69e85f6742da154245`.
Disjoint audit batches of 128 and 109 roots cover all 237 source roots. Both
are accepted with status `audited`, successful kernel replay,
`cache_eligible = false`, 750 checked constants and no forbidden axioms.
Each batch's actual theorem axiom union is exactly `Classical.choice`,
`Quot.sound` and `propext`. Audit artifact records match the projection's
filename, byte count and BLAKE3. Original module associations consistently
name `Lean4LeanLevel42.ReconstructionFinal`, and evidence records
`whole_checker_verified = false`.

Every original record shared with historical packets matches: twenty with
Normalization, twenty-two with Comparison, twenty-six with Shape, thirty-two
with Addability and thirty-four with final Completion. The combined original
compile root `/tmp/lean4lean-reconstruction42-integration-v1` has fifty
verified immutable manifest records and five current new source/object pairs.
Its manifest SHA-256 is
`2ce9d9be5126bbdd3799d060dbada87946faf0109f165e754509c60e7fc039d1`.
That original compilation/probe evidence is separate from the full genuine
packet replay and refusal controls.

The build receipt is
`/tmp/lean4lean-reconstruction42-rust100-harnesses-v1/receipt.json`.
Independent checks verified the actual Rust 1.100 compiler version, eleven
source/binary hashes, common runner hash, empty compile logs and real
dependency arguments. Both original and archived production CLI files match
SHA-256 `8ee12bf3d266b6a0f1a47784ed1b5752b574134138d72444f6d14f46a7a31a32`,
as bound by `/tmp/vesper-proof-auditor-bazel-d635-v1/receipt.json`.
Proof command, Audit.lean and KernelSnapshot sources match parent, current
source and the CLI producer. All six proof-audit calls in the new packet's
actual log invoke that archive. The report's `auditor_blake3` identifies
embedded Audit.lean text; executable bytes have this separate receipt.

Strict Rust 1.100 Clippy of all eleven exact repository test sources passed
in 1.965 seconds through a minimal real-dependency package. Its receipt,
manifest/log hashes and eleven source paths were checked at
`/tmp/vesper-reconstruction-ci-clippy-1.100-v1/receipt.json`. This is targeted
harness linting, not whole-workspace validation. The retained build-only
receipt `/tmp/vesper-bazel-reconstruction-v1-build-receipt.json` records the
89.133-second build before the separate test lease. The subsequent actual
Bazel test executed the packaged Rust test and production CLI, with the same
source, catalog, artifacts, provenance and audit results as the manual run.
Only twenty-five scratch paths differ: twenty-four original-object paths
and the projection path. The actual test reported complete acceptance in
200.03 seconds and Rust success in 200.07 seconds; it was not a cached test.

The final receipt is `/tmp/vesper-bazel-reconstruction-v1-receipt.json`,
SHA-256 `25fc5ae6cdd114e4e281c30976c6567b08628d3785a9b23ad5b2a197b4f9bce1`.
All fourteen recorded file sizes and SHA-256 identities were verified,
including actual evidence, both command/test logs, test XML, lease, Lean,
Bazel, test executable and production CLI. Source was clean at build/test
start and completion. The packaged test SHA-256 is
`436d13dbe32fd4164265dbde29ae6ee365c0598fdfd98877229285b82cb8fce4`;
its actual production CLI SHA-256 is
`6aef5208260f57c715fd879a69de027f43a7fa4670e69b742de969be53c63542`.
All six actual proof-audit calls invoke that CLI. This separate production
package execution supplements the direct-rustc manual harness executions;
it does not relabel those earlier harnesses as Bazel test binaries.

The completed manual receipt is
`/tmp/lean4lean-reconstruction42-all-matrices-receipt.json`; progress ends
`ALL ELEVEN MATRICES PASSED`. Every executable and full log SHA-256 was
verified, with one passing test, no failed/ignored tests and genuine controls.
All eleven use the same frozen source commit; this wave has no mixed-source
fold replay.

| Packet | Seconds | Authored/audited roots | Canonical constants |
| --- | ---: | ---: | ---: |
| Completion | 224.837 | 203 | 592 |
| Reconstruction | 224.895 | 237 | 750 |
| Addability | 207.719 | 179 | 633 |
| Shape | 237.992 | 226 | 697 |
| Construction | 149.375 | 37 | 337 |
| Comparison | 225.170 | 191 | 659 |
| Foundations | 142.358 | 49 | 356 |
| Map | 152.837 | 99 | 448 |
| Normalization | 199.734 | 173 | 623 |
| Normalize | 185.417 | 131 | 564 |
| Order | 152.660 | 64 | 371 |

For all ten prior packets, independent comparison with the Completion-wave
checkpoints confirms exact authored/audited catalogs, every original and
projected byte/BLAKE3 fingerprint, provenance, scope, snapshot tool identity,
partition records and logical expression counts. Completion is compared with
its final fold-v2 evidence, not its superseded recursive-definition run.
Each audit has complete disjoint root coverage, successful kernel replay,
no forbidden axioms and cache eligibility false. Each packet's actual axiom
union is exactly the standard three; a single batch may use only a subset.
Every shared original record also matches Reconstruction. The five
historical shared-record comparisons described above additionally match the
older Normalization, Comparison, Shape, Addability and final Completion
checkpoints directly.

Independent checks are retained in
`/tmp/lean4lean-reconstruction42-independent-check.json`,
`/tmp/lean4lean-reconstruction42-independent-build-check.json`,
`/tmp/lean4lean-reconstruction42-prior-independent-check.json` and
`/tmp/lean4lean-reconstruction42-bazel-independent-check.json`.
