# Completion prerequisites and reconstruction congruence acceptance (#543)

The tenth fixed `Packet::Completion` specification and required catalog are
authored. This lane ran direct rustfmt and static byte/source checks only.
It ran no Cargo or Lean command, genuine packet matrix, Clippy or Bazel
execution. The new packet and all nine earlier complete matrices remain
coordinator execution work; this report does not claim they passed.

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

The intended mathematical scope uses independent certificates rather than
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
the cycle. Existing implementation-proof audit status is unchanged. Genuine
Completion and all nine earlier complete matrices, strict targeted Clippy,
formatting, BUILD synchronization, actual Bazel packaging and final
source/object/tool evidence remain coordinator integration work.
