# Actual normalized-map completion composition (#555)

This lane owns only NormalizedCompletion.lean, the imports-only
NormalizedCompletionFinal.lean, and this report. Its parent is own #553,
`3ed3984a66738e444484b3ced350470c07cbfe3f`. Source authorization followed the
coordinator's all-open-scope ownership preflight in public issue #555.

The first theorem composes actual empty construction completion with actual
subsumption completion preservation. The latter already uses both variable
support and final-key origin. The second theorem pairs that certificate with
actual map WF, using construction WF and subsumption WF. Actual unchanged
Normalize.normalize is linked definitionally to those operations.

Both statements hold for every Lean.Level. There is no NoMVars, sorting, WF
input, successful search, or cached-flag premise. There is no new definition,
recursion, checker/search implementation, opaque equality bridge, or helper
omission. NormalizeInvariants is imported directly rather than pulling in
NormalizeFinal and NormalizeValue solely to reuse the already-known WF fact.

The exact topological module order is Lean4Lean.List, Lean4Lean.Level, then
Lean4LeanLevel42.Semantics, ListProofs, NameOrder, MapValue, Completion,
VarInsert, MapHelpers, LevelSemantics, OrderedInsert, ConstructionCompletion,
Subsumption, NodeResidual, MapMinimize, MapSubsumption, VariableSupport,
PathPriming, NormalizeInvariants, NormalizedCompletion, and
NormalizedCompletionFinal. All names after the first two have the
Lean4LeanLevel42 prefix. This is exactly 19 parent modules plus the two new
modules, within the unchanged cap of 24.

The fresh minimal private root is
`/tmp/lean4lean-normalized-completion42-composition-v1`. Seventeen original
source/object pairs come from the priority immutable construction-v3 root;
only missing PathPriming and NormalizeInvariants pairs come from immutable
addability-shared-v1. All 75 and 58 records in those original manifests were
verified before copying, and every overlapping module matched byte for byte.
Every selected source matched the repository. `dependency-origins.json` and
the two original complete manifests retain the provider associations.

The private snapshot contains exactly the 38 original source/object files
needed for those 19 parents, with no extra project modules or project fallback
path. Its 41-record input manifest also binds the provenance metadata; SHA-256:
`1f87438384f1f47092dcc0393db72f9f4bc9576f59bed4274e559f7650ffbccb`.
All 41 input identities and both owned source associations were reverified
after compilation and probing.

The following pinned Lean 4.24.0 commands ran in that private directory, under
the coordinator's single granted compiler lease. Both returned exit status 0
with no diagnostics:

```sh
LEAN_PATH=/tmp/lean4lean-normalized-completion42-composition-v1 LEAN_NUM_THREADS=1 /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 -o Lean4LeanLevel42/NormalizedCompletion.olean Lean4LeanLevel42/NormalizedCompletion.lean
LEAN_PATH=/tmp/lean4lean-normalized-completion42-composition-v1 LEAN_NUM_THREADS=1 /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 -o Lean4LeanLevel42/NormalizedCompletionFinal.olean Lean4LeanLevel42/NormalizedCompletionFinal.lean
```

The handwritten `NormalizedCompletionAudit.lean` imports the final module,
prints both root axioms, enumerates actual original declarations by owning
module, checks their original kinds/safety, and collects every declaration's
axioms. Its final invocation returned exit status 0 without diagnostics:

```sh
LEAN_PATH=/tmp/lean4lean-normalized-completion42-composition-v1 LEAN_NUM_THREADS=1 /home/agent/.local/share/vesper-lean/bin/lean -j1 -M4096 NormalizedCompletionAudit.lean
```

There are exactly two original theorem declarations, both safe, nonpartial,
nonaxiomatic, and dependent only on `propext`, `Classical.choice`, and
`Quot.sound`. There are no additional aliases, definitions or recursion
helpers. The imports-only final module owns zero declarations. An independent
read-only reviewer verified the exact closure, all original provider/source
associations, all final manifest entries and these probe receipts.

The initial tool probe used resolving name quotations for module names and
failed with three unknown-constant errors at audit lines 14–16. Both theorem
axiom probes had already passed. Literal module-name quotations fixed the
introspection script; no theorem source or object changed. The failed script,
full 602-byte log (SHA-256
`0cf700b892514787a432bff8762e0cf52629c8f90b7a4b0c5dfc32cda6eb3858`)
and exact command/output-chunk associations remain retained. There was no
logical proof compilation failure. `tool-results.json` binds both successful
module compilations and both probe attempts.

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| `NormalizedCompletion.lean` | 769 | `9854e19964bd993135e8c54042bbde37c9060f1bee71fc36f6fe041c572f23ee` |
| `NormalizedCompletion.olean` | 26304 | `6b8459e9d9a586e8c6645c5ac7702965b5c78e82fde17cdf4c546bee51040cb0` |
| `NormalizedCompletionFinal.lean` | 45 | `343e1b320c96f2b91e2dadc485ec6eaa83aa3981eee9e5a3a0df1e6250e13b0e` |
| `NormalizedCompletionFinal.olean` | 20056 | `05d92a57d580d6e0bc31405971be9cece23069f885e068029939258c81ed8025` |
| `audit.log` | 851 | `ae950faa8eaed5ae06e829098446236cc2da5c7508bd18eac5ca86c02877a070` |
| `original-declarations.json` | 396 | `3b55f1a400b8f9495ddf1d7442f9bf70b9f6b046f98a098ffd31ae1df4a66b20` |

The complete 54-file `normalized-completion-identities.json` has SHA-256
`8b7e01102fba48b8a30f21b8cb229bdec7f6962568f12120f628a23941f012d9`.
The private root is read-only and the compiler lease was explicitly released
after the successful probes. No Cargo, Bazel, push or PR command ran here.
The coordinator owns full matrix, projected-alias and actual Bazel validation.
These results make no feasible/lexChain, reification, canonicality, opaque
Level equality or whole-checker/native-equivalence claim.
