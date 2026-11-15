# Exact construction helper proofs (#408–#412)

The new packet and the extracted parent acceptance matrix both pass. It adds
23 public theorems about the actual pinned helper definitions, without changing
those definitions, the external checker/profile, runtime or existing parent
proof statements.

## Actual statements

Fourteen ordered-insertion theorems establish generic-comparator result
permutation, length and conditional Nodup; actual Name membership and one-way
none/nonmember facts; exact positive-key activation and WF transfer. They do
not assume sorted input or claim the false general member-to-none converse.
The actual numeric-name counterexample inserts a duplicate on `[1,0]`.

Nine variable-insertion theorems establish exact max-value preservation and
variable-name union for arbitrary lists, key membership and exact inner Node
record-update value/WF properties. Four numeric-name witnesses exercise before,
after, max-offset merging and repeated insertion with a trailing duplicate.
These are list/record facts, not whole-map or complete-normalizer correctness.

## Preserved shared acceptance

The tested runner now lives in `tests/common/lean4lean_proof_packet.rs`.
A closed `Packet` enum selects private static module bytes/names/required roots;
there is no runtime arbitrary-module or weaker-policy override. The original
eight-module source closure and required catalog remain fixed, so newly added
proof files cannot silently expand the parent packet. The construction closure
has seven modules and reuses fourteen existing semantic/list proofs.

Both variants retain all source/provenance, complete canonical/occurrence/
omission/alias, strict standard-axiom, bounded-capture and real refusal controls.
Every authored theorem in each imported proof closure and ALL actual aliases
are policy roots; every canonical declaration receives kernel replay.

## Genuine final execution

Executed code: `72ae6e4` on `lean4lean-construction42/integration`.
The combined Rust target build passed in 63 seconds. Both complete real matrices
passed with zero failures, ignores or filtered cases:

| Matrix | Authored roots audited | Canonical declarations replayed | Matched compiler helpers | Complete target seconds |
| --- | ---: | ---: | ---: | ---: |
| Construction | 37 (23 new + 14 imported) | 337 | 13 | 126.24 |
| Original foundations | 49 | 356 | 14 | 131.49 |

Both actual alias lists are empty; each matrix nevertheless executes the real
alternate-proof fixture: canonical-only audit passes and inclusion of the
actual forbidden alias rejects its exact project axiom. Missing/corrupted
original and projection objects and changed-source gates also pass. No
synthetic success or expected-acceptance input replaces these executions.

The construction snapshot retains 337 from 350 original declarations and visits
1,208,661 logical expression nodes. Its 4,268,008-byte object has BLAKE3
`2e27b0f4fa2b1d09632399bf0f346986f608989dafca251d4a94dbc7fd805c40`.
The parent snapshot again retains 356 from 370, visits 1,232,036 nodes, and is
byte-identical to its pre-extraction object: 5,058,968 bytes, BLAKE3
`4d1483789427ed0be66c1245852d00c584ffc7d84a673cc4966142ae00459d7e`.
Observed counts are evidence, not golden acceptance predicates.

Both strict audits report accepted/kernel-replayed, caching disabled and no
forbidden axioms. The actual standard axiom union is Classical.choice,
Quot.sound and propext under the unchanged explicit policy. No axiom-free or
whole-checker verification claim is made.

Strict two-target Clippy passed in 24.14 seconds. Workspace formatting, Bazel
synchronization, whitespace and final source-byte association checks pass.
The unchanged finite 300-second per-matrix budgets required no scheduling/cap
increase. No Cargo/workspace registration or CI source changed.

Actual evidence is in `/tmp/lean4lean-construction42-root-acceptance.log`,
the separately captured construction/foundations evidence JSON files, and the
corresponding root build/Clippy logs. Bounded evidence is printed before later
negative controls. Per-lane reports distinguish development compilation from
this assembled runtime validation.

PR #412 is stacked on #407 and remains subject to hosted checks and parent
merge gates. Existing implementation-proof audit fields are unchanged; caller
cache priming, map operations, normalization and whole-checker/compiler/native
correctness remain separate obligations.
