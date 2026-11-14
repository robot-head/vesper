# Actual level-comparison integration (#449)

The genuine Comparison matrix passed on executed revision
`788a17a9226e6257e3614905e6c5d62651e3bbd7`, which includes current main
`b2bd14eda54ace6b9e666c5b5a895ccd5c421e36`. Current-main ancestry was
independently checked. All seven genuine matrices completed successfully; the table below records
complete runs, including every actual negative control.
The coordinator runs Cargo/Lean; the acceptance lane reads the actual
structured evidence, execution receipt and logs without executing either.

| Genuine matrix | Status at this checkpoint | Test duration |
| --- | --- | ---: |
| Comparison | Passed, including real negative controls | 195.48 s |
| Construction | Passed, including real negative controls | 128.77 s |
| Foundations | Passed, including real negative controls | 134.99 s |
| Map | Passed, including real negative controls | 148.66 s |
| Normalize, before subsumption | Passed, including real negative controls | 187.33 s |
| Normalization, including subsumption | Passed, including real negative controls | 196.44 s |
| Order | Passed, including real negative controls | 144.73 s |

The Comparison test reported one passed, zero failed, zero ignored and zero
filtered tests. Its complete acceptance timing was 195.45 seconds; the test
harness reported 195.48 seconds. The unchanged whole-matrix budget is 300
seconds. These observations come from
`/tmp/lean4lean-comparison42-root-acceptance.log`; its structured evidence is
`/tmp/lean4lean-comparison42-comparison42-evidence.json`. Source identities
and conditional Clippy execution are recorded in
`/tmp/lean4lean-comparison42-execution-receipt.json`.

The new kernel projection replayed twenty-two actual source modules and
retained 659 canonical constants from 675 original constant occurrences.
Sixteen matched partial/unsafe compiler recursion helpers were accounted for
under the existing projection rule. The evidence records zero duplicates,
zero alternate-proof aliases, exact inventory, constant-info equality, empty
extensions and IR, successful kernel replay and verified reimport. Projection
traversal visited 1,841,081 logical expression nodes.
`Lean4LeanComparison42Kernel.olean` is 9,796,024 bytes, BLAKE3
`5ca9dbea7be9c8df4ed867c80a11d5470b691ffff86726f47f7e5106033959f5`.

Every one of the 191 actual public authored roots was audited in two batches
of 128 and 63. The disjoint batch theorem sets exactly match both the authored
and audited catalogs. Both reports were accepted with status `audited`,
`kernel_replayed = true`, `cache_eligible = false`, 659 checked constants
and no forbidden axioms. The union of actual theorem axioms is exactly
`Classical.choice`, `Quot.sound` and `propext`, matching the unchanged
standard-three policy. These facts were checked directly from the JSON;
there is no supplied expected answer or weakened-policy mode.

The exact root accounting is 173 roots from the previous full-normalization
closure plus eighteen new public theorems: four in `MapAll`, three in
`CompareFold` and eleven in `MapComparison`. The new names were extracted
from the actual public theorem commands and compared with the evidence.
This is an observed inventory rather than an acceptance cap or golden
count. The unchanged extractor still discovers all public authored roots
and all actual alternate-proof aliases; none were discovered in this run.

The unchanged runner verifies original and projected source/object hashes
before evidence emission and again after the real refusal controls. Complete
acceptance therefore covers deliberate changed-source rejection,
missing/corrupt original and projection artifacts, forbidden alternate-proof
rejection and restored original identities. Independently comparing this
packet with the prior full-normalization evidence found identical
fingerprints for all thirty-six shared original `.lean` and `.olean`
records. SHA-256 checks of the three new proof sources also matched the
execution receipt. The original objects remain distinct from the projection.

`norm_le_sound` proves soundness of the actual unchanged `NormLevel.le` for
arbitrary left maps and well-formed right maps. It needs no left-map WF or
sorting assumption. The proof connects actual short-circuit traversal and
lookup to each left row, then follows the actual Option early-exit fold over
the right map. Actual right-row lookup supplies its WF premise and semantic
bound in that same map. It uses the real node residual union and empty-value
facts, rather than replacing comparison with minimization.

`geq_sound` applies that map theorem to the actual unchanged `Lean.Level.geq'`
and rewrites the actual normalizations with their existing value/WF proofs.
Both level arguments satisfy structural `NoMVars`. The four audited public
witnesses demonstrate the right-WF requirement through a zero-valued variable,
successful discharge by distinct right rows, incompleteness for an explicit
raw empty left row against an empty map, and actual `geq'` success for upper
`max 2 u` and lower `imax 2 u` with its semantic consequence.

No fast-path bridge for the opaque extern `Level.beq` is assumed. This work
makes no unconditional `isEquiv'` claim and establishes neither comparison
completeness, canonicality, reification, whole-checker correctness nor
compiler/native equivalence. The explicit raw-map witness demonstrates why
semantic inequality alone does not imply successful arbitrary-map comparison.
Evidence records `whole_checker_verified = false`; the implementation's
proof-audit status remains unchanged.

All seven Rust targets were rebuilt against the executed current-main
integration; the build log reports 1 minute 42 seconds, and the final Cargo
acceptance recheck reports 40.55 seconds. Strict targeted seven-test Clippy
passed in 58.57 seconds with the exact two-file PR #434 compatibility patch
applied temporarily and then removed. This is conditional compatibility
validation, not a claim that the unpatched inherited runtime passes. The
coordinator reports repository formatting, BUILD synchronization, whitespace
and source-association checks passed. The six earlier specifications and the
complete validator suffix remain frozen: 47,588 bytes, SHA-256
`2162a7e23f7de857d06542325e26be039d9e589ea117f6a93a7f62906b4da0b9`.

All seven complete matrices pass with zero failed, ignored or filtered tests.
Every prior projected object remains byte-identical to its earlier checkpoint.
Actual source/object associations, strict audit results and all refusal checks
pass on the executed tree. The inherited #443 macOS-only scheduling select
preserves Linux/Windows execution, every Rust/Lean check and internal budget.
No local full Bazel invocation is claimed: BUILD synchronization and static
configuration checks pass, while hosted CI is reported separately.

Separate Kinetic compatibility PR #434 subsequently merged as
`085a7c9c1bd1aadcdba6ba63d1d3fe7917e7126d`. The coordinator imports that already
merged change into the proof stack, retaining all proof/test/tool bytes.
Its two runtime files must exactly match the composite patch used for the
strict seven-target Clippy check above; no temporary patch remains. The
seven genuine matrix results remain explicitly associated with executed
revision `788a17a`, before that import. The Kinetic fixed-axis loop change
has its own complete package and platform validation; it changes no proof,
auditor, projector or model source.
