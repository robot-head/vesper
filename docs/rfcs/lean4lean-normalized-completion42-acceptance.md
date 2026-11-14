# Normalized completion acceptance (#555)

The twelfth fixed packet, `Packet::NormalizedCompletion`, is authored.
This lane ran direct rustfmt, BUILD synchronization and static source/byte
checks only. It ran no Cargo or Lean compilation, genuine matrix, Clippy or
Bazel execution. The completed genuine matrix and scoped lint checks are
recorded in the [integration report](lean4lean-normalized-completion42-integration.md).
The prior eleven matrices remain historical evidence from frozen source
`3c526dac65c92e7f4c276b2e7255ca5398142068`; they are not relabeled as
executions of this additive packet extension.

This report records the coordinator's fixed acceptance contract. IDs are
label `lean4lean_normalized_completion42`, module
`Lean4LeanLevel42.NormalizedCompletionFinal`, projection
`Lean4LeanNormalizedCompletion42Kernel`, scope
`audited_normalized_completion_and_wellformedness` and marker
`LEAN4LEAN_NORMALIZED_COMPLETION42_EVIDENCE`. The test entry point invokes
only the fixed packet enum. There is no arbitrary source/module/proof flag,
supplied expected answer or weaker-policy input.

The two required public guards, in namespace `Vesper.Lean4LeanLevel42`, are:

- `normalize_mapCompletes (l : Level) : MapCompletes (Normalize.normalize l)`.
- `normalize_mapCompletes_WF (l : Level) :
  MapCompletes (Normalize.normalize l) ∧ MapWF (Normalize.normalize l)`.

These compose the actual construction certificate through actual subsumption,
then pair it with actual map well-formedness. They hold for every `Level`,
without NoMVars, sorting or prior-WF premises. The new combined endpoint has
its own identifier; it does not redeclare the existing `normalize_WF`.
The proof module imports ConstructionCompletion, VariableSupport and
NormalizeInvariants; its final module only imports NormalizedCompletion.
The two new sources define no algorithm, recursive specification or helper.
No newly generated project-helper omission is authorized. Genuine execution
must inspect the actual declarations rather than infer omission/duplicate
counts from another packet.

Traversing the author's actual source imports recovers exactly twenty-one
modules, with each dependency before its user:

1. `Lean4Lean.List`
2. `Lean4Lean.Level`
3. `Lean4LeanLevel42.Semantics`
4. `Lean4LeanLevel42.ListProofs`
5. `Lean4LeanLevel42.NameOrder`
6. `Lean4LeanLevel42.MapValue`
7. `Lean4LeanLevel42.Completion`
8. `Lean4LeanLevel42.VarInsert`
9. `Lean4LeanLevel42.MapHelpers`
10. `Lean4LeanLevel42.LevelSemantics`
11. `Lean4LeanLevel42.OrderedInsert`
12. `Lean4LeanLevel42.ConstructionCompletion`
13. `Lean4LeanLevel42.Subsumption`
14. `Lean4LeanLevel42.NodeResidual`
15. `Lean4LeanLevel42.MapMinimize`
16. `Lean4LeanLevel42.MapSubsumption`
17. `Lean4LeanLevel42.VariableSupport`
18. `Lean4LeanLevel42.PathPriming`
19. `Lean4LeanLevel42.NormalizeInvariants`
20. `Lean4LeanLevel42.NormalizedCompletion`
21. `Lean4LeanLevel42.NormalizedCompletionFinal`

The existing import union has nineteen modules; the new proof module and
imports-only final module add two. It fits the unchanged hard source cap of
24. Completion retains the final fold-based source SHA-256
`f89ca02776f406b633af9411aae7d0875e4067d927f0629c5d8f81025a678505`.
No import of the full ReconstructionFinal aggregate, Feasible, sorted-subset,
map-equality, lex-chain, tree-value or tree-structure proof modules is needed.
The unchanged artifact closure check requires every selected source to enter
the genuine replayed original closure.

The required catalog checks presence of the two actual public names.
Acceptance still extracts every actual public authored theorem from every
selected helper source and audits the complete authored-root/alternate-proof
alias union, using the unchanged batches of at most 128. The guard list does
not replace that inventory or supply accepted answers. Original constant
information, dependencies, occurrences, projections and matched compiler
helpers remain subject to the same validator and strict proof auditor.

Static comparison against base
`3ed3984a66738e444484b3ced350470c07cbfe3f` confirms all eleven prior required
catalogs and complete Packet specification arms remain byte-identical.
Byte counts include their terminating newline:

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

The entire runner from `const POLICY` through EOF remains exactly 47,588
bytes, SHA-256
`2162a7e23f7de857d06542325e26be039d9e589ea117f6a93a7f62906b4da0b9`.
Reversing only the new enum variant, required catalog, specification arm and
first-comment eleven-to-twelve change restores the base runner byte-for-byte.
The standard-three axiom policy, source cap 24, batch size 128, whole-matrix
300-second budget and every validator/control/policy cap remain identical.

BUILD adds the one fixed target to `copy_test_cwd` and the existing
5,120-MiB/one-CPU tag list, plus the same macOS-only incompatibility select as
the other fixed packets. Removing precisely these three additions restores
the original BUILD file byte-for-byte. Existing labels, attributes and
platform policies remain unchanged. Linux and Windows retain the full
semantic, audit and negative controls.

Static assertions passed for the actual 21-module graph and topological
order, all fixed IDs, the two unique guards and actual public theorem
commands, imports-only final module, absent algorithm definitions, unchanged
fold-based Completion identity, all eleven old catalogs/specifications and
full validator suffix. The receipt is
`/tmp/lean4lean-normalized-completion42-acceptance-static-check.json`.
Direct `rustfmt --edition 2024 --check` passed for the new test and common
runner; `python3 tools/bazel/sync_build_files.py --check` and
`git diff --check` passed. This lane owns only the new test/report and narrow
runner/BUILD additions.

Actual replay still must establish complete source/root coverage, original
and projected identities, exact constant/occurrence partitions, kernel replay,
verified reimport, standard-three actual axiom unions and cache eligibility
false. The unchanged negative controls include missing/corrupt originals and
projections, changed-source refusal, proof-alias policy rejection and strict
bounded regular-file capture. No synthetic successful audit or expected
mathematical report is introduced.

This wave establishes completion and row well-formedness of the actual final
normalizer. It does not establish normalized reification denotation,
map-to-tree semantic preservation, recursive tree shape/payload preservation,
minimality/canonicality, opaque native Level.beq reflection or whole-checker/
compiler/native equivalence. No implementation-proof audit status or
source-campaign completion claim changes. The coordinator validation plan
is the genuine new 21-module matrix with every control, strict CI-pinned
Rust 1.100 Clippy of all twelve exact test sources, static preservation of
the eleven earlier catalogs/specifications and validator, and actual Bazel
packaging. It does not schedule repeated runtime checks of the eleven earlier
packets when their selected inputs remain unchanged. The completed new
execution, Clippy, Bazel and integrated tool/source/object evidence are
recorded in the [integration report](lean4lean-normalized-completion42-integration.md).
