# Issue-backed continuation and Appendix A integration

This batch implements the work claimed in issues
[#46–#52](https://github.com/robot-head/vesper/issues?q=is%3Aissue+%22Codex+claimed%22)
and [#54](https://github.com/robot-head/vesper/issues/54). The coordinator checked
open issues/PRs before posting claims and starting each worktree. The external
Claude Warp issues #36–#45 and their implementation PRs remain outside this
batch's production paths. [Ownership contracts](../rfcs/continuation37-contracts.md)
record the exact boundaries.

The prior [foundation PR #35](https://github.com/robot-head/vesper/pull/35) merged
as `7ed4775` after required native Linux, Windows and macOS checks passed. This
continuation was rebased onto that published main. The user's rewritten README
was preserved when resolving the prior PR's conflict; new host-tool links were
retained. Original implementation histories remain on local archive branches.

## Executing implementation slices

| Issue | Integrated commits | Result and detailed evidence |
| --- | --- | --- |
| #46 | `eb89a0d` | Real scalar StreamVByte plus fully validated VSPA v1/tree-schema-1 relative archives; [report](continuation37-serde-binary.md) |
| #47 | `cf5df3b` | Standard Arrow IPC V5 span export with 12 fixed typed columns and success-only FIFO consumption; [report](continuation37-beacon-arrow.md) |
| #48 | `9299e09` | Real 16 KiB slotted pages, exact headers/CRC32C and recoverable shared/exclusive pool pins; [report](continuation37-quarry-pages.md) |
| #49 | `03c5388` | Actual 16-token BF16 K/V storage, sequence/page generations, speculative rollback and zeroed reuse; [report](continuation37-tensor-kv.md) |
| #50 | `fe12b1e`, `86d9577` | Actual CDC frames, transactional offsets/ticks and complete local checkpoint images that resume in a fresh process; [report](continuation37-flux-checkpoints.md) |
| #51 | `1dce29c` | VirtIO 1.2 direct packed-chain byte capture, wrap phases, permission checks and recoverable completion leases; [report](continuation37-aether-virtio.md) |
| #54 | `40467be` | Repaired, compiled and axiom-audited Appendix A model without changing its definitions or theorem statements; [report](continuation37-metatheory.md) |

The new allocating `vesper-beacon-arrow` adapter is registered in workspace
members/default-members. Beacon's core remains dependency-free and no_std.
There are **37 registered packages** at this integration base. The root lock
adds the adapter/Arrow dependencies and Flux's existing-version BLAKE3 edge;
no previously locked registry package version was migrated.

The [foundation workflow](../../.github/workflows/expansion-foundations.yml)
now includes the adapter and all twelve actual acceptance examples, plus the
new pinned Lean model. It continues to run focused tests/strict Clippy/examples
on native Ubuntu, Windows and macOS. Model and host evidence stay separate.

## Independent reviews and concrete correction

Six cross-lane read-only reviews checked the binary codecs, Arrow representation
and writer budget, page/pin lifecycle, KV lifecycle, CDC/checkpoint state and
the authoritative packed-ring rules. No production ownership was duplicated.

Review found a real Flux budget mismatch: checkpoint creation with 24 work units
could succeed for an empty circuit, while identical-limit restore needed 33.
Creation now preflights exact version-1 restore reads before allocating an image;
rows and source bindings also finish charged reads before growth. Regressions
exercise rejecting 24/32 and accepting 33 units, populated raw 34/35 boundaries,
and source-backed 49/50 and 51/52 boundaries. Errors preserve prior live state.

The coordinator independently compiled the Appendix and compared all original
22 theorem headers and 43 data/instance declarations against the exact source;
none changed. Twenty-nine theorem audits include the seven concrete applicability
examples. This validates the stated small model, not compiler soundness.

## Design refresh

The native Google Doc now contains **sections 1–41 and Appendix A**, revision/time
and complete digests recorded in [source metadata](../specification-source.json).
The coordinator read sections 38–41 and the Appendix and compared every prior
section. Active library requirements did not change. The
[review](../rfcs/continuation37-review.md) records the new requirements, actual
Appendix compilation failures and unsupported scaffold claims. The upstream
Google Doc was not edited.

Inventory schema 2 checks explicit numbered coverage and appendix identity/hash
alongside heading order and source bytes. Legacy schema 1 still validates the
historical 37-section layout. Local consistency is not source authentication or
a freshness guarantee.

## Local validation

- **1,509 workspace tests/doctests across 177 suites passed**, with zero failures
  or ignored tests, on the combined 37-package workspace at `86d9577`.
- Strict workspace all-target Clippy, formatting and diff whitespace passed.
- All **twelve** self-checking examples executed through
  `scripts/test-expansion-foundations.sh`, including real independent Arrow
  decoding, actual page/KV bytes and a newly spawned checkpoint-resume process.
- Windows GNU all-target strict Clippy passed for all seven libraries using
  portable BLAKE3. This is cross-compilation evidence, not native Windows runtime.
- Actual Lean 4.24 compiled the Appendix file without errors/warnings. All 29
  theorem footprints are empty, `propext`, or `propext` plus `Quot.sound`; no
  admissions, new axioms, Classical.choice or compiler-trust tactics were added.
- The recorded snapshot check and fourteen Python regressions passed, including
  legacy success, schema/count validation, latest-section and appendix omission/
  substitution, numbered identities, order, content and digests.
- Official Actionlint 1.7.7 and shell syntax checks passed for the updated workflow.
- The Serde owner additionally compiled the pinned upstream scalar C StreamVByte
  reference: exact agreement across **513 frames and 131,328 values**. Arrow
  export tests independently decode every column using arrow2, separate from
  the producing arrow-rs implementation.

Local logs: `/tmp/vesper-continuation37-{workspace,clippy,examples,windows}.log`,
`/tmp/vesper-continuation37-metatheory-lean.log`. Compiler targets are private
to the root checkout; source-divergent agent worktrees use separate targets.
Hosted results for the final PR head are tracked on the PR and its workflow
runs after publication; the earlier PR #35 results do not certify this new batch.

The first [published foundation run](https://github.com/robot-head/vesper/actions/runs/43195786583)
at `f862902` passed native Ubuntu/macOS contracts, Clippy and all examples, plus
the Lean model job. Windows caught a portability error in the new legacy Python
fixture: text-mode output translated LF to CRLF after computing an LF hash.
The fixture now writes explicit UTF-8 bytes, preserving canonical bytes on every
platform. Its fourteen regressions pass locally; updated hosted results are
tracked on [PR #61](https://github.com/robot-head/vesper/pull/61).

## Remaining boundaries

The archive supports its explicit host tree schema; source derives and general
object graphs remain separate. Arrow IPC export is not OTAP transport. Pool/KV/
queue owners do not impose Vesper compile-time exhaustion on Rust callers.
Storage pages are not a durable WAL/B+Tree implementation; KV storage is not a
complete inference engine. CDC uses an explicit consecutive-record-LSN policy;
checkpoints are local images, not distributed/durable exactly-once commits.
Checksums are byte integrity, not provenance or rollback protection. Captured
VirtIO metadata is not live DMA, pinned backing, VM execution or device isolation.
Appendix A uses one optional binding, requires an empty residual let frame and
proves resource counts conditional on an evaluation, not full-language progress,
identity-sensitive ownership or production compiler correctness. No benchmark,
hardware timing, zero-RSS/allocation or cryptographic isolation claim is inferred.
