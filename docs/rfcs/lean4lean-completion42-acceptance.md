# Completion prerequisites and reconstruction congruence acceptance (#543)

The final Completion matrix passed at
`19d0d39380e9376af5fff5e5ece3903be629673a`, both through the exact-source
Rust 1.100 harness and through the actual uncached Bazel proof target. The
nine prior packet regressions passed at
`9f61536f94d4567884b6ed592f0317f8575a06bc`. Their 41 distinct selected Lean
sources, Rust test sources and common runner are byte-identical at 19d, so
those results retain their source association. The runs are not described
as all having executed at 19d. The coordinator ran the tools; this lane
independently checked source, object, executable, log and report identities
without running Cargo or Lean.

| Packet and execution | Authored/audited roots | Batches | Observed seconds |
| --- | ---: | --- | ---: |
| Foundations, 9f | 49 | 49 | 175.787 |
| Construction, 9f | 37 | 37 | 172.805 |
| Order, 9f | 64 | 64 | 175.071 |
| Map, 9f | 99 | 99 | 179.423 |
| Normalize, 9f | 131 | 128 + 3 | 229.888 |
| Normalization, 9f | 173 | 128 + 45 | 231.979 |
| Comparison, 9f | 191 | 128 + 63 | 241.173 |
| Shape, 9f | 226 | 128 + 98 | 255.425 |
| Addability, 9f | 179 | 128 + 51 | 202.524 |
| Final Completion manual harness, 19d | 203 | 128 + 75 | 238.664 |
| Final Completion actual Bazel test, 19d | 203 | 128 + 75 | 227.52 |

Manual entries are measured harness-process times. The Bazel entry is the
actual Rust test time; its enclosing test command took 262.100 seconds.
Each matrix reported one passed test, zero failed and zero ignored tests,
including all actual refusal controls. The internal whole-matrix limit
remained 300 seconds.

Manual execution used exact repository test sources compiled directly by
CI-pinned Rust 1.100.0 with real dependency artifacts, invoking the archived
production Bazel-built CLI. These are direct-rustc harnesses, not Cargo-built
or Bazel-built test binaries. The separate actual Bazel run built and ran
`//:test/lean4lean_completion42`, with its actual production CLI and regular
copied source inputs. It was not a cached test result.

The fixed IDs are label `lean4lean_completion42`, source module
`Lean4LeanLevel42.CompletionFinal`, projection
`Lean4LeanCompletion42Kernel`, scope
`audited_completion_prerequisites_and_reconstruction_congruence` and marker
`LEAN4LEAN_COMPLETION42_EVIDENCE`. The coordinator owns the imports-only
final module, new test entry point, BUILD registration and integration report.
This closed packet adds no arbitrary runtime source/module/proof selector,
supplied answer or weaker-policy mode.

Traversing the actual final source imports recovers exactly these twenty-three
modules, in dependency order:

1. `Lean4Lean.List`
2. `Lean4Lean.Level`
3. `Lean4LeanLevel42.Semantics`
4. `Lean4LeanLevel42.ListProofs`
5. `Lean4LeanLevel42.Subsumption`
6. `Lean4LeanLevel42.OrderedInsert`
7. `Lean4LeanLevel42.VarInsert`
8. `Lean4LeanLevel42.NameOrder`
9. `Lean4LeanLevel42.SortedKeys`
10. `Lean4LeanLevel42.MapValue`
11. `Lean4LeanLevel42.MapAll`
12. `Lean4LeanLevel42.MapEquality`
13. `Lean4LeanLevel42.SortedSubset`
14. `Lean4LeanLevel42.MapAny`
15. `Lean4LeanLevel42.Addable`
16. `Lean4LeanLevel42.NodeResidual`
17. `Lean4LeanLevel42.MapMinimize`
18. `Lean4LeanLevel42.MapSubsumption`
19. `Lean4LeanLevel42.Completion`
20. `Lean4LeanLevel42.Feasible`
21. `Lean4LeanLevel42.VariableSupport`
22. `Lean4LeanLevel42.ReifyCongruence`
23. `Lean4LeanLevel42.CompletionFinal`

The existing dependencies contribute eighteen modules; the core, three
proof modules and imports-only final module add five. The actual support
source imports only Completion and MapSubsumption, so MapHelpers is not
needed. PathPriming, LevelSemantics, recursive normalization, Shape/ShapeFinal
and ReifyCycle are outside this closure. Every dependency precedes its user
in the fixed order, which fits the unchanged hard source-count bound of 24.
The unchanged import-closure check requires every selected source to enter
the genuine original artifact closure.

The 41 unique required guards use frozen public ASCII theorem names in
namespace `Vesper.Lean4LeanLevel42`:

- Independent core certificates: `varSupport_refl`, `varSupport_trans`,
  `edgeCovered_acc_mono`, `edgeCovered_acc_congr`, `admissible_acc_mono`,
  `completes_acc_mono`, `edgeCovered_support`, `admissible_support`,
  `completes_support`, `mapCompletes_support`, `completes_nil`,
  `mapCompletes_empty`.
- Actual feasibility and exchange algebra: `addable_edgeCovered`,
  `addable_iff_edgeCovered`, `feasible_insert_getD_mem`,
  `feasible_insert_getD_sorted`, `admissible_erase`, `completes_erase`,
  `completes_nonempty_edge`, `feasible_go_sound`, `feasible_go_complete`,
  `feasible_sound`, `feasible_complete`, `feasible_iff_completes`,
  `feasible_duplicate_present_witness`.
- Actual support and key origin: `minimizeEntry_varSupport`,
  `minimizeEntry_key_origin`, `subsumption_varSupport`,
  `subsumption_key_origin`, `subsumption_mapCompletes`.
- Deterministic reconstruction congruence: `feasible_go_lookup_congr`,
  `feasible_lookup_congr`, `lexChain_lookup_congr`, `lexChain_perm`,
  `norm_toList_lookup_congr`, `norm_toTree_lookup_congr`,
  `norm_reify_lookup_congr`, `feasible_beq_congr`, `lexChain_beq_congr`,
  `norm_toTree_beq_congr`, `norm_reify_beq_congr`.

Guards only require presence. The unchanged source-command extractor derives
every actual public authored theorem in the selected helper closure,
including all seven additional support helpers outside the guard list. The
static source inspection observed twelve core, thirteen Feasible, twelve
VariableSupport and eleven ReifyCongruence declarations. These observed
counts are not acceptance caps or a golden root inventory. The unchanged
root union includes every actual alternate-proof alias and audits the full
union in existing 128-root batches. Genuine compilation, projection and
proof audit must establish the complete roots and original/projected
identities; static counts do not establish successful execution.

Static byte comparison against base
`3e0a98b6928bd1d79cd621d5ade7c438f4e3a2c2` confirmed that all nine older
required catalogs and complete Packet specification arms remain identical.
Measured including their terminating newline, the byte counts are:

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

The complete runner from `const POLICY` through EOF remains byte-identical:
47,588 bytes, SHA-256
`2162a7e23f7de857d06542325e26be039d9e589ea117f6a93a7f62906b4da0b9`.
Removing only the new enum variant, catalog, specification arm and
introductory nine-to-ten comment restores the base runner byte-for-byte.
Static assertions also passed for the actual twenty-three-module closure,
fixed dependency order, all five IDs, all 41 unique qualified guards and
their presence among actual public commands, the catalog association and
the unchanged source-count bound. The inspected import graph and source
hashes are retained in
`/tmp/lean4lean-completion42-acceptance-static-check.json`. Direct
`rustfmt --edition 2024 --check tests/common/lean4lean_proof_packet.rs` and
`git diff --check` passed. This lane changes only the narrow runner extension
and this report; it changes no BUILD entry or older scheduling policy.

The unchanged validator retains source/provenance binding, original and
projected source/object associations, occurrence/partition validation,
complete authored-root/alias discovery, strict audit checks, bounded
regular-file capture, missing/corrupt original and projected artifact
controls, changed-source refusal and the forbidden alternate-proof control.
The cap of 24 sources, 128-root batches, standard-three axiom policy, every
other cap and finite 300-second whole-matrix budget remain unchanged.

The proved mathematical scope uses independent certificates rather than
algorithm success or semantic correctness as definitions. EdgeCovered and
VarSupport retain arbitrary stored offsets. Support transports fixed-key
certificates, while whole-map MapCompletes transport additionally requires
that every target key originates in the source. VarSupport alone cannot
exclude newly introduced uncertified keys. Both actual minimizeEntry helpers
require the original node's actual lookup; no arbitrary insertion inherits
support or key origin.

Actual feasibility soundness is unconditional. Completeness uses sorted
stored keys, a sorted accumulator and the stated remaining-length fuel bound.
There is no remaining-name nodup or name-freshness assumption; the closed
witness covers duplicate remaining names already present in the accumulator,
including legitimate orderedInsert-none behavior. Reconstruction congruence
uses pointwise lookup equality or the actual transparent map Boolean equality.
LexChain permutation includes fallback and duplicate inputs, without sorting
or WF premises. No physical TreeMap equality or opaque native Level.beq
reflection is assumed.

This packet prepares completion and support arguments. It does not prove
normalizeAux completion preservation, actual lexChain admissibility or
minimality, full tree denotation, normalized reification correctness,
canonicality, whole-checker/compiler/native equivalence or source-campaign
completion. Earlier raw-cycle results do not assert that normalization emits
the cycle. Existing implementation-proof audit status is unchanged. The
observed matrix, lint, format and Bazel results are recorded here; hosted CI
and stack/PR status remain separate coordinator reports.


The final actual source catalog contains 203 authored roots, exactly equal to
the audited set: 155 existing and 48 new roots (12 Completion, 13 Feasible,
12 VariableSupport and 11 ReifyCongruence). An independent inventory of every
selected helper source recovered those same names. The exact new inventory
is the 41 guards above plus `subsumeVars_var_support`,
`node_subsumeBy_var_support`, `node_subsume_var_support`,
`minimize_fold_var_support`, `minimize_vars_erased_support`,
`node_var_mem_noEmpty` and `subsumption_fold_varSupport`. The imports-only
final module adds no theorem. All authored definitions and theorem roots
remain in the canonical projection.

The final definition of Admissible uses the existing `List.foldr`, preserving
the exact nil and cons equations in the contract. All later core definitions
and the twelve core proof bodies are unchanged. This removes the additional
compiler recursion helper from the original environment before projection.
Fresh Core, Feasible, VariableSupport and Final objects compiled with empty
successful logs; all 48 original-root axiom probes used only the standard
three, and the original-environment probe explicitly rejected any presence
of `Vesper.Lean4LeanLevel42.Admissible._unsafe_rec`. The new fold manifest at
`/tmp/lean4lean-completion42-fold-v2/fold-identities.json`, SHA-256
`c1661137e6c549444335b171a4e7e795beb13217a3ee9d4b93ff33acbeeae249`,
binds fifty immutable files and the five current source/object pairs.
These objects supersede earlier core/dependent object identities in the
individual lane reports. The independent ReifyCongruence source is unchanged.

The final twenty-three-module packet contains 46 original source/object
records. Its 607 original constant occurrences represent 605 distinct
originals. There are 594 retained occurrences, 592 canonical output constants
and thirteen omitted generated helpers. The two repeated retained occurrences
are `Lean.Level.Normalize.NormLevel.feasible.go.eq_1` and `.eq_2`, each
realized in Feasible and ReifyCongruence with reason
`identical_safe_theorem_realization`. Both occurrences remain represented;
there are zero alternate-proof aliases. The complete thirteen omission
records match the old Map vendor-helper set exactly, all from Lean4Lean.List
or Lean4Lean.Level. No Vesper-generated helper or authored proof is omitted,
and no projector/validator rule, source cap or axiom policy was changed.

Evidence reports exact inventory, constant-info equality, empty extensions
and IR, successful kernel replay and verified reimport. Logical traversal
visited 1,751,028 expression nodes. The final
`Lean4LeanCompletion42Kernel.olean` is 7,628,088 bytes, BLAKE3
`0f471e17baf05fe8616a521af759b045323704397b6d86196b1ae2b6c3ac5725`.
Both disjoint audit batches, 128 and 75 roots, are accepted with status
`audited`, successful kernel replay, `cache_eligible = false`, 592 checked
constants and no forbidden axioms. Each batch's actual theorem axiom union
is exactly `Classical.choice`, `Quot.sound` and `propext`. Audit artifact
records match the exact projection, and every authored root occurs in the
retained canonical partition. The original module is
`Lean4LeanLevel42.CompletionFinal`; `whole_checker_verified` remains false.

The authoritative final manual evidence and source/build/runtime receipt
are in `/tmp/lean4lean-completion42-fold-v2-harness/`. Core source SHA-256 is
`f89ca02776f406b633af9411aae7d0875e4067d927f0629c5d8f81025a678505`.
The harness binary hash is unchanged because the Rust source and shared
runner did not change; the harness reads and binds the actual Lean payload
at runtime. Its selected original source records and guards verify the new
body. Actual old-original, corrupt-original, missing/corrupt-projection,
changed-source and forbidden-alias controls completed successfully.

The actual Bazel receipt is `/tmp/vesper-bazel-completion-v1-receipt.json`,
SHA-256 `f7cf8ee9ed47b2178581edff249734c07d6991abb79a8b55d942466a136d0a7b`.
Its incremental final-source build passed in 74.514 seconds; the 262.100-second
test command passed with one real Rust test in 227.52 seconds, zero failures,
zero ignored tests and no cached test result. The earlier 9f bootstrap build
is separate from this final-source build/test. All fourteen recorded file
hashes, actual JUnit result and six proof-audit invocations were independently
verified. The actual Bazel CLI SHA-256 is
`6aef5208260f57c715fd879a69de027f43a7fa4670e69b742de969be53c63542`;
its test binary SHA-256 is
`501b941fc1ab787d6df93017acd40e98f1956cc25dd7a6abb3e4dabc1c00af8e`.
The complete manual and actual Bazel evidence agree in all fields except
twenty-four scratch paths: the 23 original module paths and projection path.
Their original/projected fingerprints, catalogs, duplicate/omission records
and full audit reports are identical.

The nine prior results remain at
`/tmp/lean4lean-completion42-all-matrices-receipt.json`. Independent checks
verified their executable and log hashes, complete catalogs, all original
source/object fingerprints and projected fingerprints against the previous
Addability-wave checkpoints. Every prior audit is accepted, kernel-replayed,
cache-ineligible and free of forbidden axioms, with complete root coverage
and actual per-packet standard-three axiom union. Individual batches use only
that allowed set; the final three-root Normalize batch uses `propext` alone.
All records shared with final Completion also match. The final new packet
shares 24 original records with Normalization, 26 with Comparison, 30 with
Shape and 28 with Addability, with no mismatch.

The manual build receipt is
`/tmp/lean4lean-completion42-rust100-harnesses-v1/receipt.json`; its actual
archived production CLI is independently bound by
`/tmp/vesper-proof-auditor-bazel-d635-v1/receipt.json`, producer
`d6357113e08805064e438498b81a81fa0dcf1122`, SHA-256
`8ee12bf3d266b6a0f1a47784ed1b5752b574134138d72444f6d14f46a7a31a32`.
That archive and the actual Bazel CLI above have different build origins.
The proof command, embedded Audit.lean and KernelSnapshot sources match the
parent and executed tree. All runtime log invocations use the corresponding
real CLI. `auditor_blake3` identifies embedded Audit.lean text; the separate
receipts bind the executable bytes.

Strict Rust 1.100 Clippy of all ten exact repository test sources passed in
1.622 seconds through the minimal real-dependency package with
`--tests -- -D warnings`. The manifest, log and ten source paths were checked
against `/tmp/vesper-completion-ci-clippy-1.100-v1/receipt.json`. Its Rust
sources remain unchanged after the fold, preserving that lint association.
This is targeted harness linting, not whole-workspace validation. Rustfmt,
BUILD synchronization and whitespace checks passed as recorded by the root;
no broader hosted result is inferred.

Historical evidence is retained: the 9f Completion run passed in 209.814
seconds but used the earlier recursive predicate and fourteen omissions,
including its generated Admissible helper. Its 7,628,368-byte projection,
BLAKE3 `1400d638c2ebbe189b69ccda1baebd682edce88313375eaf0f06bacea6e8871b`,
is superseded and is not mixed with the final 19d objects or counts. The
first fold attempt failed on reserved binder `prefix`; its diagnostics remain
at `/tmp/lean4lean-completion42-fold-v1-Completion.log` and are not a pass.
The corrected `prior` binder preserves the same equations, and the new
original/projection evidence above is authoritative.

Independent source, build, runtime and historical comparisons are retained
in `/tmp/lean4lean-completion42-fold-independent-check.json`,
`/tmp/lean4lean-completion42-fold-independent-build-check.json` and
`/tmp/lean4lean-completion42-bazel-independent-check.json`. This acceptance
lane made no final runner, proof, tool or BUILD edits and ran no Cargo or Lean
commands during this independent evidence review.
