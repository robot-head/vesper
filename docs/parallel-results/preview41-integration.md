# Explicit current-source macro preview — issues #152–155

Vesper Lens now advertises the opt-in `vesper/macroPreview` request. Its only
input is an open document URI and optional expected current version. It compiles
the complete current unsaved text with the actual pure Tier1 Forge pipeline and
returns an expanded-source-only artifact or the actual compiler rejection.
Client source, policy, package, World and approval fields are rejected.

The sealed Salsa snapshot directly observes the full SourceFile text, bypassing
legacy parse/check and signature cutoffs. Unchanged input queries retain the
actual immutable compiler Arc; helper-body, definition and comment edits rerun
the full compiler, authority gate and IR validation. Byte-identical editor
changes keep the existing SourceFile input while updating the document version.
Direct callers forcing the Salsa setter create a new revision even for equal
bytes. Existing 64-revision resets and close/reopen rebuild all input IDs.

The provider validates exact retained original bytes and the actual accepted
artifact's source/hash. It borrows provenance and audit DTOs without cloning
Worlds, serializes through a capped sink, and only then materializes a JSON Value.
The response is capped at1MiB with finite prepaid serialization/copy/parse work;
other existing protocol depth/node/string limits can independently refuse a
frame. These are logical resource guards, without an RSS or latency claim.

Successful maps remain UTF8-byte provenance anchors, including distinct expanded
ranges for repeated arguments. They do not assert lexeme equality after alpha
renaming. Compiler failures carry no accepted expanded hash/counts/maps/audits;
diagnostic coordinates are explicitly original, actual rejected expanded source,
or none. Provider or association errors return a bounded unavailable error.

Normal diagnostics, hover and other semantic requests continue to use
guarded_analysis on original source. A successful preview neither manufactures
an original-source CheckedSource nor grants entry execution. No URI filesystem
read, component execution, Lean or user program execution occurs during preview.

## Coordination and validation

Claims #152–155 preceded source writers. Fresh open issues and actual open-PR
filenames showed no database/LSP collision; Claude wave-five #121–136 remains
reserved. The exact contract and APIs were published before three exclusive
worktree writers. The live document was rechecked at41sections/AppendixA,
modified2026-11-12T09:18:30.204Z; §§16.4/21.3 were reread. Independent read-only
review found no actionable source-association, authority or serializer bypass.

Actual early tests exposed that the proposed256KiB syntax metadata envelope
refused small helper/linear fixtures before their intended authority/checker
stages. The published contract was amended before dependent changes to the
existing compiler's2MiB metadata default. All other compiler caps/work remain
fixed; the separate response cap stays1MiB. Store refusal and compiler resource
refusal also retain their distinct existing semantics in acceptance.

Local integrated validation on Rust1.99.1 passed133tests in12suites, zero ignored:
42root library,6real Salsa queries,7provider,5actual preview stdio, and73existing
editor, recovery, signature, CLI, authority and lowering regressions. Strict
workspace all-target Clippy, formatting and whitespace checks passed. Existing
executable editor acceptance passed19sessions; recovery passed15stdio/3batch
checks against the same actual integration binary.

The independent new executable harness passed20commands,8stdio sessions and
70JSON-RPC requests. Three separately checked expanded programs explicitly ran
to42. A fourth accepted fixture preserves a declared unhandled effect row and
its separate normal execution correctly refused. All four accepted artifacts,
hashes, maps and audits matched separate actual Forge checks. Preview itself did
not execute those programs. A second standalone Rust harness passed all5tests.

The dedicated native Linux/macOS/Windows workflow retains actual query, codec,
stdio, existing guard and strict checks. Hosted gates are pending publication;
local Linux results do not claim other-platform execution. This draft stacks on
PR142/115/110 and must be retargeted after the parents land.

## Remaining design scope

This memoizes a complete fixed-context pure compilation, rather than individual
macro functions or guest approvals. Source Tier2, other macro categories,
cross-package linking, implicit original-source semantic overlays, incremental
minimal rechecking and latency/RSS guarantees remain future work. The broader
design implementation goal remains active.
