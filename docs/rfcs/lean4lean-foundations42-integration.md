# Audited helper proof foundations (#402–#407)

The assembled packet passes genuine pinned-Lean compilation, exact logical
projection, complete authored-theorem/alias policy audit and adversarial artifact
controls. It adds conditional proofs about actual pinned helper definitions,
without changing the external checker, profiles or existing resource model.

## What is proved

There are **49 public authored theorems**: nine node-denotation facts, five
actual comparator/list-pruning/offset facts, thirteen node-subsumption facts and
concrete witnesses, and twenty-two Core fresh-name facts. The earlier informal
Semantics count of ten was corrected against the actual nine-name source
catalog; acceptance derives the full catalog rather than trusting that count.

Level proofs call the actual copied `Lean.Level.Normalize` definitions. Boolean
subset soundness and list-pruning max-union preservation require no sorting or
uniqueness assumption. Conditional node pruning preserves joint denotation with
only right-node condition-key well-formedness and key inclusion; the left node
needs no extra well-formedness premise. Self-same equality is proved separately.
The comparator is explicit `Name.cmp`, matching the implementation's file-local
instance. A genuine empty-key/zero-valuation counterexample changes the value
from one to zero without the invariant. Valid examples exhibit actual removal,
not merely branches that leave the node unchanged.

Names proofs use actual Core `NameGenerator.curr`/`next` and `FVarId`, with no
unsafe Expr or old Theory/Verify imports. They establish reservation/order,
finite index/prefix equations, injectivity, no alias with prior reservations,
and emitted-list length/Nodup/new reservations. Reservations are a caller
invariant and intentionally vacuous for other prefixes; this does not prove
the caller's context/cache-priming algorithm establishes that invariant.

## Exact implementation correspondence

The vendored helper files match both the actual sealed patched checker source
and the selected entries of the existing pinned source manifest:

| Module | Bytes | BLAKE3 |
| --- | ---: | --- |
| Lean4Lean.List | 176 | `508045af9f1a1590749506718fa43245c035a5d37fd06e7ae97ec9722a4e1027` |
| Lean4Lean.Level | 13,744 | `4b3f0b40448d255f3d7d48e03f2251610b73993d13beb6fdd372f0d511538139` |

`tools/lean4lean-foundations42/source-provenance.json` also binds the immutable
upstream/Lean versions, actual existing kernel patch and upstream license.
Acceptance recomputes BLAKE3 correspondence. Its SHA256 strings are adjunct
metadata, not an independently recomputed acceptance hash or proof of binary
derivation. No floating fetch or runtime external-checker setup is required.

## Actual final acceptance

Executed source: `98cf7a7` on `lean4lean-foundations42/integration`.
`cargo test --locked --test lean4lean_foundations42 -- --nocapture` **passed**:
one complete matrix, zero failures/ignores/filters, **127.08 seconds**, within
the original finite 300-second budget. The initial target build passed in
64 seconds. Strict target Clippy passed in 23.93 seconds; workspace formatting,
Bazel metadata synchronization and whitespace checks also passed.

The matrix freshly compiled all eight implementation/proof/aggregate modules,
binding sixteen actual source/object files. The unchanged snapshot retained
**356 canonical declarations** from 370 original constants, explicitly
inventorying fourteen narrowly matched partial native compiler helpers.
There were no duplicate occurrences or alternate proof aliases in this actual
packet. It visited 1,232,036 logical expression nodes, compared complete retained
ConstantInfos, kernel-replayed them and verified export/reimport. The observed
counts are evidence, not golden acceptance predicates.

The 5,058,968-byte standalone `Lean4LeanFoundations42Kernel.olean` has BLAKE3
`4d1483789427ed0be66c1245852d00c584ffc7d84a673cc4966142ae00459d7e`.
One actual strict audit checked all 49 exact authored roots and all 356 retained
declarations. `accepted` and `kernel_replayed` are true, caching is disabled,
and forbidden axioms are empty. The actual transitive standard axiom union is
`Classical.choice`, `Quot.sound`, `propext`, under the unchanged explicit policy.
No claim of axiom-free or whole-checker verification follows.

Actual controls passed: changed source-byte refusal; missing and corrupted
original Level object; missing and corrupted projected object; and the existing
fixed alternate-proof fixture. For that fixture, canonical-only audit genuinely
passes, but adding its actual alternate alias genuinely rejects the exact
forbidden project axiom. The harness always includes every actual alias in the
positive policy-root set; the empty positive alias list is not used to skip
the discriminating negative control.

Bounded evidence was emitted before later negative controls. Exact local
records are `/tmp/lean4lean-foundations42-root-acceptance.log` and
`/tmp/lean4lean-foundations42-root-evidence.json`; build and Clippy logs have
the corresponding `-root-build.log` and `-root-clippy.log` names. The generic
projector source is unchanged (BLAKE3
`bd2a748c9886b06e5419cf5aa01711dc46d8d3da916d9e184b543afaa7c27eb2`).

## Scope and merge gate

This is conditional helper soundness, not map-wide minimization, normalizeAux
or complete level-equivalence soundness, caller cache/context priming, whole
typechecker verification, native equivalence or compiler refinement. The old
Verify library's missing proofs/project assumptions are not imported or treated
as discharged. Existing external-profile audit status fields remain unchanged.

No existing kernel/profile, auditor/projector, metatheory, runtime, Cargo,
workspace registration or CI source was edited. No scheduling or cap increase
was required. PR #407 is a sibling of #399 stacked on #323; hosted checks and
the parent merge gate remain outstanding after successful local validation.
