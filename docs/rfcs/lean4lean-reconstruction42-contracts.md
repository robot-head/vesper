# Construction, chain and tree prerequisites (#547–#552)

Base own #546 `da7d28da59f9bace3c352a4f49e171c2697c90b7`.
Before edits, all 87 open issue bodies and all 38 PR metadata/actual
paginated file lists were reviewed: 744 path occurrences, 699 distinct,
matching ascending/descending inventories and file counts, zero API errors.
Evidence `/tmp/lean4lean-reconstruction42-ownership-20261114T1509.json`, SHA256
`628e81b56de5baf41b62492d13b017e40361b22d138f0906fb882adb6b628082`.
Supplemental TreeStructure scan found zero mentions and no existing file.
No external conflict; source/reference #453/510 and extractor #461/520 remain
read-only. Live design metadata still reads 2026-11-13T04:46:25.948Z.

## Frozen ownership and interfaces

Use namespace `Vesper.Lean4LeanLevel42` and unchanged actual compiler
definitions. Existing Completion predicates are independently defined;
Admissible uses List.foldr and has the exact nil/cons equations. Do not
replace a certificate by search success or semantic correctness.

- #548 owns NEW ConstructionCompletion.lean and reconstruction42-construction
  report only. Imports: Completion, MapHelpers, LevelSemantics, OrderedInsert.
  Prove VarSupport for actual addConst/addNode/addVar. Prove addConst completion
  from completion of its inserted path; addVar introduces no new key. For
  successful actual orderedInsert, use the actual addNode variable witness,
  transport the old path certificate and append the inserted name to obtain
  the new path certificate. The main `normalizeAux_completion` target is:
  from `MapCompletes acc` and `Completes acc [] p`, obtain both
  `MapCompletes (normalizeAux l p k acc)` and
  `VarSupport acc (normalizeAux l p k acc)`. Keep the actual rewrite branches
  and levelSize recursion. Empty-map/path and seed-path corollaries follow.
  Do not assume sorting/NoMVars unless a genuine obstruction is reported.
  Do NOT import VariableSupport, MapSubsumption, NormalizeInvariants,
  PathPriming, Shape or ConstructionShape. This lane stops before subsumption.
- #549 owns NEW LexChainCompletion.lean and reconstruction42-chain report.
  Imports: Feasible, ReifyCongruence. Prove an actual find? candidate exists
  for complete nonempty paths, with sorted stored keys and KeySorted path.
  Then prove `Admissible s [] (s.lexChain fuel p).reverse` under those sorting
  premises, independent `Completes s [] p`, and `p.length ≤ fuel`. Preserve
  inside-out orientation. The actual feasible predicate supplies the selected
  remainder certificate; use actual permutation for its prefix membership.
  State nonempty fallback exclusion precisely. Admissibility alone is not
  lexicographic minimality or canonicality. Publish final ASCII theorem names.
- #550 owns NEW TreeReifyValue.lean and reconstruction42-tree-value report.
  Imports: LevelSemantics, MapValue only. Prove actual ofNat/addOffset/mkMax
  and Option-fold denotations as needed. The main actual Tree.reify equation
  describes max of its const, varsValue and child contributions. A child
  `(a,t)` contributes `Nat.imax (levelValue ρ t.reify) (ρ a)`, including the
  actual syntactic-zero branch. Derive local payload/individual edge bounds,
  leaf equality and boundedness from actual contribution bounds. No replacement
  recursive TreeValue function and no claim about NormLevel.toTree yet.
- #551 owns NEW TreeStructure.lean and reconstruction42-tree-structure report.
  Imports: SortedKeys/NameOrder, plus Std lemmas if needed. A nonrecursive
  `treeEntriesLookup` using actual List.find? is allowed. Prove actual
  modifyAt key projection equals orderedInsert.getD old keys, other-key lookup
  preservation, strict sorted-key preservation and target lookup as
  `some (f ((treeEntriesLookup n xs).getD default))` with necessary sorting.
  Keep an actual closed unsorted counterexample if sorting is necessary.
  No replacement update algorithm or recursive tree-shape predicate.
- #552 owns only the additive Packet::Reconstruction runner extension and NEW
  reconstruction42-acceptance report. Root owns imports-only ReconstructionFinal,
  tests/lean4lean_reconstruction42.rs, narrow BUILD copy/reservation/platform
  registration and integration report. Guard catalogs wait for authors' actual
  final public theorem names; every authored helper and alternate alias remains
  automatically audited.

No author edits another lane's files, existing helper modules, production
compiler/runtime/model/profile or Cargo metadata. If an extra import or API
is needed, report it before breaking the frozen minimal closure.

## Exact scope and execution

The existing import union of Completion, MapHelpers, LevelSemantics,
OrderedInsert, Feasible, ReifyCongruence, SortedKeys and NameOrder has nineteen
modules. The four new proof files and final module give exactly 24, preserving
the hard cap. Do not import subsumption/normalization invariant/shape modules
or ReifyCycle into this packet.

Fixed IDs: label `lean4lean_reconstruction42`, source module
`Lean4LeanLevel42.ReconstructionFinal`, projection
`Lean4LeanReconstruction42Kernel`, scope
`audited_construction_completion_admissible_chains_and_tree_primitives`, marker
`LEAN4LEAN_RECONSTRUCTION42_EVIDENCE`.

Preserve all ten earlier catalogs/specifications and the complete validator
suffix byte-for-byte: 47,588 bytes, SHA256
`2162a7e23f7de857d06542325e26be039d9e589ea117f6a93a7f62906b4da0b9`.
Source cap 24, batches 128, whole-matrix budget 300 seconds, standard-three
policy and every actual refusal/artifact control remain unchanged. No new
project-helper omission, supplied expected answer, arbitrary selector or
weaker mode. No axioms, sorry/admit, unsafe/partial or native_decide in new
proof source. Prefer existing folds/recursors for new specification predicates
so compiler helpers do not create another omission. Recursive theorem proofs
are allowed but their actual original declaration inventory must be inspected.

At most two coordinated Lean compiler leases globally, with pinned Lean4.24.0,
`-j1 -M4096` and LEAN_NUM_THREADS=1. One Cargo/shared-target lease. Each attempt
uses a complete private immutable dependency root and one LEAN_PATH; project
namespace prefixes shadow fallback roots. Retain failed attempts, exact
original source/object/tool manifests and all actual public-root axiom probes.
Audit the new packet and all ten prior full matrices; actual Bazel packaging
is validated separately. No auto-merge onto own unprotected stack parent.

Remaining obligations include construction-plus-subsumption composition,
recursive tree-shape/payload preservation, map-to-tree denotation, normalized
reification correctness, minimality/canonicality, opaque native Level.beq
reflection and whole-checker/compiler/native equivalence. No source-campaign
completion or implementation-proof audit status change is implied.
