# Actual construction, chains and tree primitives (#547–#552)

The final packet adds 46 public theorem roots in four modules:

- ConstructionCompletion (12): actual map updates preserve name support;
  the recursive normalizeAux preserves independent completion certificates
  and accumulator support. The main induction requires completion of the
  seed map and current path, without sorting or NoMVars hypotheses.
- LexChainCompletion (9): complete sorted paths have an actual candidate;
  the selected chain is reverse-admissible under explicit sorting and fuel
  bounds. Nonempty find-none fallback is excluded under those hypotheses.
- TreeReifyValue (18): actual Tree.reify equals max of its local payload and
  immediate child contributions. Each child contributes Nat.imax of its
  reified value and name valuation, including the syntactic-zero branch.
- TreeStructure (7): actual modifyAt preserves key projection, other lookup
  and sorted keys. Target lookup uses the real generic default and requires
  sorting; a closed unsorted witness gives actual value 1 versus proposed 3.

The three new observer/value definitions are nonrecursive. No replacement
normalizer, search, updater or recursive tree evaluator is introduced.
Independent read-only reviews found no missing assumptions in any lane.

## Exact source and inventory

All mathematical execution used source revision
`3c526dac65c92e7f4c276b2e7255ca5398142068`. Later commits change reports only.
The actual transitive closure contains 24 original modules and 48 selected
source/object records. Its complete authored/audited catalog has 237 roots:
191 earlier roots plus all 46 new theorem declarations. The 37 presence
guards do not replace automatic discovery of every helper and alternate alias.

The original snapshot contains 768 occurrences, 766 distinct constants and
750 retained canonical constants. Two identical safe realizations of
NormLevel.feasible.go.eq_1/eq_2 are retained, with zero alternate-proof aliases.
The sixteen omitted compiler helpers exactly match historical records:
thirteen vendor helpers and the three existing LevelSemantics helpers for
NoMVars, levelSize and levelValue. No newly authored module/helper is omitted.
Constant-info equality, exact inventory, kernel replay, verified reimport,
empty extensions and empty IR all pass. Logical traversal visits 1,747,729
expression nodes.

The two complete audit batches cover 128 and 109 roots, are accepted with
cache eligibility false, and have actual axiom unions exactly Classical.choice,
Quot.sound and propext. The standalone projection is 6,603,192 bytes, BLAKE3
`f4e3a91fe8c2e4e12a2fe0f3264a9a650977571b5c5efa69e85f6742da154245`.

The combined original import and all 46 new-root axiom probes also pass.
Its sealed 50-record manifest is
`/tmp/lean4lean-reconstruction42-integration-v1/reconstruction-integration-identities.json`,
SHA256 `2ce9d9be5126bbdd3799d060dbada87946faf0109f165e754509c60e7fc039d1`.
Individual lane reports retain failed attempts, actual original declarations,
dependency verification and object hashes. Their safety inventories include
generated helpers, not only source theorem commands.

## Observed validation

All eleven exact-source Rust 1.100 harness matrices passed at 3c526dac.
The new packet took 224.895 seconds. Every earlier authored/audited catalog,
original/projected fingerprint and partition/provenance record matches its
historical baseline. [Acceptance](lean4lean-reconstruction42-acceptance.md)
records every observed duration and source/executable association.

Those test binaries were compiled directly from the actual repository sources
with real dependency artifacts and invoke the genuine archived Bazel-built
production proof auditor. Their build mode is not described as Cargo- or
Bazel-built test binaries. Exact commands, source/binary hashes and the
separate auditor producer association are retained under
`/tmp/lean4lean-reconstruction42-rust100-harnesses-v1/`.

A separate actual Bazel 9.3.1/Rust 1.100 deployment passed on the same clean
source. The target `//:test/lean4lean_reconstruction42` built in 89.133 seconds
with 2,333 disk-cache hits, then executed one uncached test in 200.07 seconds,
zero failures or ignored tests. The full test command took 270.241 seconds.
It used the packaged executable, real production auditor and all actual
refusal/artifact controls. Evidence matches the manual replay completely
apart from 25 temporary paths: 24 original-object paths and the projection.

The actual deployed CLI SHA256 is
`6aef5208260f57c715fd879a69de027f43a7fa4670e69b742de969be53c63542`;
the test executable SHA256 is
`436d13dbe32fd4164265dbde29ae6ee365c0598fdfd98877229285b82cb8fce4`.
The receipt binds binaries, full stdout/XML, six auditor invocations and
evidence: `/tmp/vesper-bazel-reconstruction-v1-receipt.json`, SHA256
`25fc5ae6cdd114e4e281c30976c6567b08628d3785a9b23ad5b2a197b4f9bce1`.
Its test lease was granted only after ten of eleven manual tasks exited,
preserving the global maximum of two Lean processes. Local Bazel used batch
mode, existing system trust, a 2 GiB JVM and one build/test job.

Strict Clippy of all eleven actual Rust test sources passed under Rust 1.100
in 1.965 seconds via the minimal real-dependency package; this is targeted
linting, not a whole-workspace check. Rustfmt 1.100, BUILD synchronization and
whitespace checks pass. All ten older packet specifications/catalogs and the
complete 47,588-byte validator suffix remain byte-identical. Source cap 24,
128-root batches, 300-second matrix budget, standard-three policy and every
real refusal/artifact control remain unchanged.

## Remaining scope

Construction-plus-subsumption composition, recursive tree shape/payload
preservation, full map-to-tree denotation, normalized reification correctness,
minimality and canonicality remain separate obligations. No native Level.beq
reflection, whole-checker/compiler/native equivalence, source-campaign success
or changed implementation-proof audit status is claimed. Compiler-prefix
standard libraries/shared libraries remain outside individual project-artifact
attestation. Hosted checks remain the merge gate. This stack targets own #546
without automatic merging onto its unprotected branch.
