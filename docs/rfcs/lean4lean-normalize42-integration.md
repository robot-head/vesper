# Actual recursive level construction (#424–#429; PR #432)

All five ordinary matrices and all five actual regular-copy launcher matrices
pass. The new packet proves exact denotation of the actual unchanged
`Normalize.normalizeAux`, before its caller performs subsumption. It adds
32 public theorems without changing any actual normalizer helper or parent
proof statement.

## Actual domain and statements

`LevelSemantics` defines a total structural interpretation of actual
`Lean.Level`, structural `NoMVars`, and a constructor-count proof measure.
Its seventeen roots prove evaluator equations, positive/decreasing measures
for the fresh imax expressions and the required Nat.imax/max/offset algebra.
The mvar case merely totalizes the interpretation to zero. Correctness
excludes metavariables structurally; it does not use cached hasMVar flags,
opaque mkData bridges, or old Verify/Axioms assumptions.

`NormalizeInvariants` has four roots. A generic induction over the actual
recursive branches proves preservation of any map property preserved by
the actual primitive updates, with actual insertion membership. Its concrete
instances preserve PathPrimed at an ARBITRARY independent path and MapWF for
ALL levels. Neither theorem needs a metavariable-free/current-priming domain;
the latter yields actual empty-map construction WF.

`NormalizeValue` has eleven roots: the actual parameter-branch helper, main
equation, empty-map corollary and eight witnesses. The main equation is:

```lean
NoMVars l → PathPrimed p acc →
mapValue ρ (Normalize.normalizeAux l p k acc) =
  max (mapValue ρ acc)
    (if enabled ρ p then levelValue ρ l + k else 0)
```

It covers arbitrary valuations/maps/offsets with ONLY those two premises;
no sorting, MapWF or assumed semantic-preservation result. Strong recursive
proofs use structural levelSize, including reconstructed imax calls.
Sequential calls use arbitrary-path priming preservation; zero-offset
member branches use actual existing-variable coverage, and positive offsets
use actual current-row presence for modify.

Witnesses cover both fresh imax rewrites, zero/positive member offsets,
disabled activation, arbitrary initial-map retention, a genuinely primed
duplicate current key, and necessary domain exclusion. Actual construction
of succ(mvar) at empty path/map gives zero while the totalized evaluator gives
one. This is a real limitation of the unrestricted statement.

Actual `Normalize.normalize` subsequently calls `.subsumption`; its complete
correctness is NOT established here. Canonicality, `.le`, reification,
cached-flag correspondence, fresh-name context priming, whole-checker
implementation proofs and compiler/native equivalence remain separate.
Existing implementation-proof audit fields remain unchanged.

## Strict ordinary execution

Executed source: `c26445c6e790be35756352bdaec1dc8923136e4f` on
`lean4lean-normalize42/integration`. Five Rust targets built in 1m05s.
Every complete ordinary matrix passes, zero failures/ignores/filtered cases:

| Packet | Authored roots audited | Canonical declarations replayed | Matched compiler helpers | Complete seconds |
| --- | ---: | ---: | ---: | ---: |
| Construction | 37 | 337 | 13 | 123.74 |
| Foundations | 49 | 356 | 14 | 134.72 |
| Map | 99 | 448 | 13 | 144.88 |
| Recursive construction | 131 | 564 | 16 | 174.62 |
| Order | 64 | 371 | 13 | 135.84 |

The new fixed fourteen-module packet audits all 131 authored roots in its
closure: 32 new plus 99 imported. Thirty names are presence guards, never
the entire audit inventory. The existing 128-root batching runs two real
strict batches of 128 and 3; ALL actual alternate-proof aliases remain roots.
No new policy/batch/cap or 300-second budget setting was needed.

Its exact projection preserves 564 of 580 original declarations, inventories
sixteen matched partial native compiler helpers, has zero actual aliases,
and visits 1,709,137 logical expression nodes. Canonical ConstantInfo equality,
kernel replay, fresh reimport and empty extensions/IR all pass.

Recursive object: 7,098,416 bytes, BLAKE3
`fadd3f5b0299597a889c6646ccaf88ca502ac4df49def2458c937caecd846c59`.
All FOUR prior projected objects remain byte-identical to their checkpoints.
Every matrix completes actual source/provenance, missing/corrupt original/
projection and clean-canonical versus forbidden-alternate-alias controls.
The actual zero-alias count does not skip the malicious alias fixture.

All audits are accepted/kernel-replayed, caching disabled and forbidden
axioms empty. The actual union is Classical.choice, Quot.sound and propext
under the unchanged standard-three policy; no axiom-free claim is made.

Strict five-target Clippy passes in 26.90s. Formatting, Bazel BUILD-file
synchronization, whitespace, exact source associations and independent
read-only mathematical review pass.

That Clippy result is for the frozen executed source above. Subsequent parent
integration preserves all 52 owned proof/test/tool file bytes but imports two
pre-existing Rust 1.99 Kinetic range-loop lints. Separate PR #434 fixes them
without changing arithmetic order. With exactly its two-file patch applied,
the integrated tree passes strict five-target Clippy in 19.02s. That temporary
patch is removed from this proof branch; the compatibility change remains in
its own PR. Receipts and both the genuine failure and successful composite
logs are retained under the normalize42 post-parent/kinetic433 evidence names.
Hosted integration of that dependency remains a separate gate.

## Confirmed runfiles portability

#407's macOS failure revealed that Bazel leaf-symlink inputs correctly fail
the unchanged regular-source guard. #429 selects the repository's existing
regular-file copy launcher for these fixed targets. No Rust/Lean validator,
proof, source manifest, auditor/projector/profile or workflow code changes.
Every original catalog/specification and the complete POLICY-through-EOF
validator/control/policy/budget suffix remain byte-identical (47,588 bytes,
SHA256 `2162a7e23f7de857d06542325e26be039d9e589ea117f6a93a7f62906b4da0b9`).

The ACTUAL unmodified launcher and real test binaries compiled with manifest
`.` and bare CLI PATH binding reproduce the remote panic without copy mode.
With copy mode, ALL FIVE full matrices pass on 105 leaf-symlink inputs:
foundations 134.40s, construction 132.97s, order 132.71s, map 144.14s and
recursive construction 181.31s (181.361s including its launcher copy).
All five output objects are byte-identical to ordinary execution. All real
refusal controls still execute, including artifact symlink rejection after
startup. No expected answer or weaker acceptance flag is exposed to tests.

[Runfiles evidence](lean4lean-proof-packets-runfiles.md) distinguishes this
real-launcher validation from a complete local Bazel invocation and the
separate hosted retry. Parent #407's BUILD fix is pushed at 5e19c99; descendant
construction/order/map branches have their corresponding regular-input rows.

Ordinary evidence: `/tmp/lean4lean-normalize42-root-acceptance.log`, separate
per-packet evidence JSON, root build/Clippy logs. Launcher evidence:
`/tmp/lean4lean-proof-packets-runfiles-429/`, with exact compiler/dependency/
input/launcher/binary receipts, genuine failure, five logs/evidence JSON and
summary. New source hashes match the genuinely compiled immutable lane roots:

- LevelSemantics: 27e0aeed8b8f8e16cbd4d19f374687313a04a5f995a87d650775bb9b4e1edc9f.
- NormalizeInvariants: a4318b8b2d9327d1eec069717379e5b625d45a29faba7093dc2e798e901575df.
- NormalizeValue: 94826b3eb799c1c67eadd22efd75864492d7cf558fbc418b226bf5ade6dafbef.

The PR is stacked on #423; hosted checks and parent merge gates remain
separate. Auto-merge is not enabled onto an unprotected parent branch.
