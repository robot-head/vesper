# Actual normalizer equality and shape (#499)

This checkpoint proves properties of the unchanged finite normalizer from
the pinned Lean4Lean source. The new sources contain 41 public theorems:
eight in MapEquality, fourteen in ConstructionShape, fourteen in
SubsumptionShape, and five final composition theorems in ShapeFinal.

Actual derived VarNode and handwritten Node Boolean comparisons reflect
structural equality. The actual NormLevel comparison is equivalent to
pointwise `get?` equality and therefore implies equal mapValue. These map
theorems need neither sorted rows nor MapWF and do not equate the physical
TreeMap representations.

The actual recursive construction preserves sorted variable lists. Its
key-sorting proof explicitly carries a sorted current path through the
actual levelSize recursion, including the fresh imax rewrites. A genuine
counterexample records why that path premise matters. Subsumption preserves
both sorting properties. The separate empty-row result applies to every raw
map, without sorting, MapWF or denotation assumptions: every possibly empty
accumulator row must either be nonempty already or have a key awaiting a
source visit. Visiting a key cleans it, and visiting another key leaves it
unchanged. The generic fold proof permits duplicate and empty source rows.

The final full-normalization shape results hold for every Level, including
structural metavariable cases. The final normalized-map Boolean-equality
denotation theorem retains the structural NoMVars premises. It composes the
existing normalizeAux and subsumption value theorems directly. It does not
reflect the opaque native `Level.beq` branch of `isEquiv'`, prove semantic
completeness or canonicality, or verify reification or the whole checker.

The new fixed Shape packet uses the minimal actual closure of 24 modules,
within the unchanged source-count cap. All seven previous packet catalogs
and specifications and the complete 47,588-byte validator suffix remain
byte-identical. The suffix SHA256 is
`2162a7e23f7de857d06542325e26be039d9e589ea117f6a93a7f62906b4da0b9`.
The standard-three axiom policy, 128-root audit batches, 300-second whole
matrix budget and every real negative control remain unchanged.

The test uses the established regular-file Bazel copy launcher and Linux/
Windows logical replay scheduling. No new macOS full-replay claim is made;
the existing private proof-audit portability target remains enabled there.
The model, metatheory, runtime/compiler/native providers, profiles,
extractor/projector/auditor and Cargo files are unchanged.

## Validation status

All three new component modules compiled with genuine pinned Lean 4.24.0
against versioned immutable copies. Equality's eight and subsumption's
fourteen original-object roots also passed their explicit standard-three
axiom probes. The combined original proof sources and final composition
compiled successfully before the final module was moved into the
inventoried namespace; that move preserved its source bytes exactly.
Independent read-only composition reviews found no actionable issue.

The first actual fixed-packet attempt failed before compilation because the
unchanged collector correctly refused to omit five required theorems placed
in a top-level aggregator. The final theorem module is now
`Lean4LeanLevel42.ShapeFinal`, matching the collector's existing prefix.
Only the new Shape specification's module/path was corrected; the collector
and policy were preserved. The failure is retained in
`/tmp/lean4lean-shape42-root-shape-v1.log`.

The corrected full Shape matrix passed in 205.99 seconds at actual code
`325b83972774c8c365173e6b8ac4ac8e3c72cbe0` (subsequent changes are documentation
only). Its complete source-derived catalog has 226 authored and audited
roots: 185 imported existing roots plus the 41 new roots. Both audit batches
(128 and 98 roots) reported audited status, kernel replay and no forbidden
axioms, with cache eligibility false. Their actual axiom union is exactly
Classical.choice, Quot.sound and propext.

The exact projection retained 697 canonical constants from 713 occurrences,
with sixteen matched compiler helpers, no duplicate constants or proof
aliases, 1,834,420 logical expression visits, constant-info equality,
verified reimport and empty extensions/IR. The projection is 9,772,016 bytes,
BLAKE3 `dcdcb30859e5bb7d0919d0a1164af835408c6c3209588b7a25fd7f3f8754730f`.
All 48 original source/object identities were recorded. Independently
comparing shared records found 34 identities unchanged from Normalization
and 36 unchanged from Comparison, including the exact same sixteen helper
omissions. Every actual corruption and forbidden-alias control passed.

Evidence is `/tmp/lean4lean-shape42-shape42-evidence.json`; independent checks
are `/tmp/lean4lean-shape42-independent-check.json`. Strict targeted Clippy
for all eight fixed targets passed in 29.26 seconds, and workspace formatting,
BUILD synchronization and whitespace checks passed. The seven previous full
matrix replays are still in progress; no all-eight completion is claimed.
Component reports retain their compiler attempts and artifact identities.
