# Actual comparison and sorted-key proofs (#413–#417)

The new Order packet and both preserved packets pass their complete strict
matrices. This adds 27 public theorems about the actual pinned normalizer
helpers without changing those helpers or any parent proof statements.

## Actual statements and boundaries

`NameOrder` contributes sixteen roots: structural laws for explicit
`Lean.Name.cmp`, equality reflection/converse, strict and isLE transitivity,
theorem-backed lawful comparator instances, pinned core lifting to explicit
`List.compareLex Name.cmp`, and four real numeric/string/prefix computations.
The prefix-first comparator is distinct from global Name Ord's `quickCmp`.
The constructor-step proof uses the recursively established prefix relation
and existing Nat/String scalar laws, without a project order axiom.

`SortedKeys` contributes eleven roots. Strict Pairwise ordering of condition
Names implies Nodup and is preserved by actual `orderedInsert`. Sorted input
also supplies the previously missing member-to-none direction of duplicate
detection. Actual `VarNode.addVar` preserves strict order by variable name,
including its max-offset merge, so output variable names remain unique.
Four computations exercise insertion, duplicate detection past the head,
offset merging and the real unsorted-key failure of the sorted guarantee.
Offsets do not participate in the name-uniqueness invariant.

These statements have real representation premises. They do not establish
that arbitrary keys inside a valid TreeMap are sorted, nor map construction,
minimization/normalization correctness, caller path priming, whole-checker
implementation proofs, compiler refinement or native equivalence. Existing
implementation-proof audit fields remain unchanged.

## Preserved strict acceptance

The closed `Packet` enum gains one static Order variant and a fixed
nine-module source closure. Twenty-six required API/witness names are presence
guards. The unchanged source-command catalog discovers all 64 authored roots
in that closure: 27 new and 37 imported. In particular, the additional public
constructor-step lemma is audited even though it is not a required-name guard.
ALL actual alternate-proof aliases remain policy roots.

Independent byte comparisons confirm both existing required arrays and packet
specifications, and the complete runner from `const POLICY` to EOF, are
unchanged. Every source/provenance, occurrence/partition, original/projection
association, strict axiom, bounded capture, corruption and forbidden-alias
check remains. There is no runtime arbitrary-module, expected-answer or
weaker-policy input. The source and artifact caps and finite 300-second
per-matrix budget are unchanged.

## Genuine integrated execution

Executed source: `304ceb65633376fd72b332e47de7494ee1937694` on
`lean4lean-order42/integration`. The three Rust targets built in 1m03s.
The combined real execution passed all three cases, with zero failures,
ignores or filtered cases:

| Matrix | Authored roots audited | Canonical declarations replayed | Matched compiler helpers | Complete target seconds |
| --- | ---: | ---: | ---: | ---: |
| Original construction | 37 | 337 | 13 | 123.56 |
| Original foundations | 49 | 356 | 14 | 130.46 |
| New Order | 64 | 371 | 13 | 132.27 |

The new projection preserves 371 of 384 original declarations, visits
1,228,719 logical expression nodes, has zero actual proof aliases, and is
reimport-verified with equal canonical ConstantInfo. Its extensions and IR
are empty. The 13 matched partial native compiler helpers are separately
inventoried under the unchanged exact projector policy.

Projected Order object: 4,473,304 bytes, BLAKE3
`11dd016c1c16acdfebe41b6163c9ef23ef43f47d6cd63644df9ba7ca5e42c595`.
The original construction object remains byte-identical: 4,268,008 bytes,
BLAKE3 `2e27b0f4fa2b1d09632399bf0f346986f608989dafca251d4a94dbc7fd805c40`.
The original foundations object also remains byte-identical: 5,058,968 bytes,
BLAKE3 `4d1483789427ed0be66c1245852d00c584ffc7d84a673cc4966142ae00459d7e`.
These are measured evidence, not golden acceptance counts or digests.

All strict audits report accepted/kernel-replayed, caching disabled, and
no forbidden axioms. The actual axiom union remains `Classical.choice`,
`Quot.sound` and `propext` under the explicit standard-three policy. This is
not an axiom-free claim. Each matrix completes its real changed-source,
missing/corrupted original/projection, provenance, and clean-canonical versus
forbidden-alternate-alias controls; the actual zero-alias count does not skip
the adversarial alias fixture.

Strict three-target Clippy passed in 24.71s. Workspace formatting, Bazel
BUILD-file synchronization, whitespace and source-byte associations pass.
No budget/scheduling, Cargo/workspace, CI, projector/auditor, external profile,
runtime or compiler changes were needed. Independent read-only review of all
27 new theorems and the static packet found no actionable defect.

Evidence: `/tmp/lean4lean-order42-root-acceptance.log`, the separately captured
`lean4lean-order42-{construction42,foundations42,order42}-evidence.json` files,
and the corresponding root build/Clippy logs. Per-lane reports distinguish
their development compilation and preserved failed attempts from this final
assembled matrix.

The two new proof sources match their compiled lane copies exactly:

| Source | Bytes | SHA256 |
| --- | ---: | --- |
| NameOrder.lean | 4,382 | 932a487b98f1c895cdde9ccc5771cc9cda874eabca09ab40e236d987eac6ace9 |
| SortedKeys.lean | 6,772 | be0e9a9c7fffd9de49a231c7771618a04ecc78652627b442534b8150877baf38 |

PR #417 is stacked on #412. Hosted checks and the parent merge gates remain
separate from these local results; auto-merge is not enabled onto an
unprotected parent branch. Actual map construction and erased-key-context
minimization proofs are the next obligations. In particular, `NodeWF` alone
does not justify `addConst`'s skipped constant one, and union with an old map
that still contains the original node can mask an invalid replacement.
