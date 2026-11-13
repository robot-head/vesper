# Bounded AST rewrite checker (#232)

## Ownership and source

This lane owns only `src/optimize/witness.rs`, `tests/rewrite_core.rs`, and this
report. The entire frozen `a24896b` contract was read before source work. The
published policy amendment `3d41a56` was read before its candidate-span/capacity
requirements were implemented. Coordinator module registration/gate, producer,
adversarial/runtime tests, core AST/numeric, dependencies and backends are not
edited by this lane.

Owned commits so far:

* `b2d0417`: exact public certificate DTO/API and private budget/admission/builder
  seam; independent typed proof checker.
* `74fc8e9`: prepaid origin scans, candidate represented-data envelope and public
  certificate reserved-capacity accounting.
* `af21f22`: ten public core test cases, including all 24 rules across four kinds.
* `2fd0ba1`: three checker-local candidate/budget test cases.
* `4af398d`: canonical candidate value spans and actual retained AST capacities,
  plus two public regression cases with five retained-capacity variants.

The checker imports no egg API and forbids unsafe code. It validates every term
and proof, including unused records. Term children and proof/guard dependencies
must point backward; all terms use the actual source's closed integer kind;
constants fit; variable IDs bind the source-derived name inventory. Rules use
explicit bounded substitutions. Reflexivity is shallow, congruence connects
ordered child endpoints, and symmetry/transitivity cite previously validated
equalities. No e-class membership, canonicalization or producer union-find is
trusted.

The six erasing rules require the actual substituted erased term to be a leaf
or an earlier checked equality to a same-kind total leaf. Current/future guard
references and unrelated premises are rejected. The 24-rule whitelist includes
the six baseline direct folds. Checked multiplication uses checked i128
arithmetic; modular multiplication retains correct low bits even when a U64
product exceeds i128::MAX. Unknown IDs/versions and malformed substitutions
fail closed.

Admission walks actual AST occurrences iteratively, using its checked TypeMap.
Root/nonliteral kinds are closed; contextual IntLiteral metadata is permitted
only for fitting actual literal leaves. The metadata payload never supplies a
literal's numerical value. Intrinsics require original unshadowed callee spans,
correct conventions and the same source lexical island. The builder copies
those actual callee templates. All candidate value nodes use the source root
span, whose checked kind controls backend coercions.

One move-only budget persists through producer phases and the same full gate.
The public standalone verifier creates a fresh standalone budget. It charges
represented certificate capacities and working reservations, cumulative work,
names and actual retained candidate capacities. Term shape preflight counts
expanded occurrences/name bytes despite DAG sharing; output construction uses
explicit stacks and prepays boxes/vectors/strings and origin scans before
materializing payloads. Existing caller AST storage/allocator/RSS and physical
OOM retain the explicitly limited contract scope.

## Evidence and review

Twelve public tests cover 96 rule/kind combinations, equality composition,
prior totality, cyclic/irrelevant guards, signed/unsigned overflow and wide
modular multiplication, unused malformed records, versions/substitutions,
actual input/output values, contextual literal metadata, callee origin, every
limit field, certificate retained capacity, conflicting/noncanonical value
spans and five actual candidate retained-capacity fields. Three local tests
exercise shared-DAG expansion refusal, actual origin-preserving construction
with cumulative full verification, and reservation refusal before Vec growth.

Read-only reviewer `/root/builder_attestations` identified the span-indexed
backend coercion gap and the caller candidate retained-capacity gap. The
coordinator published `3d41a56` before dependent fixes. Focused rereview of
`4af398d` reported both findings closed. No assertion was weakened; positive
candidate fixtures already use canonical spans.

Executed in this authoring lane: `rustfmt --edition 2024` on owned Rust files
and `git diff --check`, both passed. Per the frozen build lease, **no compilation,
tests, Clippy or Miri were executed by this lane**. The following actual results
were executed and reported by the coordinator in the combined integration
worktree, rather than inferred from source review.

At integration head `2260c6a`, the corrected critical run passed **129 tests
across six targets**, with zero failures or ignored tests:

| Actual target | Passed |
|---|---:|
| Library, including three checker-local tests and genuine producer/adversarial tests | 71 |
| Existing checked arithmetic integration | 18 |
| Rewrite CLI | 5 |
| Public checker core, including 96 rule/kind combinations | 12 |
| Rewrite runtime/evaluator/native behavior | 8 |
| Independent public adversarial checker tests | 15 |

The actual log `/tmp/rewrite42-critical-fixed.log` explicitly records all three
checker-local cases and all twelve public core cases as passing. Both independent
review findings were fixed and executed: conflicting/noncanonical candidate
typing spans and caller-retained String/Vec capacities are rejected, including
the source-preserving actual gate tests. Checker source `4af398d` remained
unchanged during the subsequent producer accounting corrections.

Coordinator strict package all-target Clippy passed; actual output is retained
in `/tmp/rewrite42-clippy-fixed.log`. The independently authored Python driver
also passed **340 commands across 49 programs**, recorded in
`/tmp/rewrite42-python-fixed.log`; its stated scope is the actual default
witness-gated AST optimizer, evaluator and JIT. This is execution evidence,
not a formal certificate or performance claim.

The final full-root-package rerun was underway on the same code when this
report was updated; its result is left to the coordinator's integration report.
No unreported platform, Miri or broader-package success is inferred here.

## Limits

This is an audited Rust checker for the existing pure integer AST profile,
not a Lean refinement theorem, PlaceIR/effect witness system, backend proof,
linear-time proof, performance result or whole-process resource guarantee.
The producer and root gate remain separately reviewed integration components.
No generated Lean, raw/formatted Lean source or public approval/trust token was
introduced.
