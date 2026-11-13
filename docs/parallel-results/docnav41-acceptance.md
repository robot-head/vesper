# Current-source intra-doc definition executable acceptance

Claim [#191](https://github.com/robot-head/vesper/issues/191) owns only the new
executable Rust tests, fixtures, Python harness and this report. The published
`docs/rfcs/docnav41-contracts.md` contract preceded all writes. No production,
Session, source documentation, Cargo/build/workflow or test-wrapper files change.

Five independent Rust executable tests and `scripts/check-doc-navigation.py`
exercise the existing `textDocument/definition` route through actual framed
stdio. Rust accepts `VESPER_DOC_NAVIGATION_BINARY` with a Cargo executable
fallback; Python takes `--binary`. Each process has a finite timeout/output cap.
No fake checked source, prefix program, parser, World or proof report is used.

The local corpus contains global functions, a linear type and its inherent
method, effect names/operations, a trait name, another type's same-spelled method
and an explicit primitive-owner method (`i64::score`). Independent expected
Locations select exact original declaration-name ranges, using UTF16 positions
computed from unsaved Unicode/LF and CRLF text. They cannot select fake declaration
spellings inside comments, same-named globals, or a method on another owner.
Opening/closing brackets and internal backticks hit the link; its exclusive end
does not. Ordinary lexical navigation retains local-shadow priority; hover over
doc references remains null.

Other fixtures cover matching inline-code/fence/escape/Markdown exclusions,
comment/string lookalikes, genuine type/effect namespace ambiguity, unknown,
Self, generic, external and builtin-only references. A resolved occurrence still
navigates when another occurrence makes the link report rejected. Requesting an
excluded or unresolved reference returns null without manufacturing an accepted
Document or semantic overlay.

Every session opens a current unsaved source under a file URI whose on-disk
program deliberately differs; those disk bytes must remain unchanged. The
lifecycle case renames declarations/references, preserves current locations after
stale changes and invalid surrogate edits, accepts a later transaction using the
rejected edit's version, preserves state after an oversized rejected change, and
checks close/reopen. Strict UTF16 surrogate-interior request positions fail.

An actual 1025-reference input must return the existing resource error for its
doc link while an ordinary code reference still navigates. Successful explicit
macro preview cannot authorize definitions on original macro source: navigation
remains unavailable with the real parse classification. Actual type errors and
damaged source retain semantic/parse unavailable errors.

Actual validation against the provider-integrated executable at
`/workspace/vesper/target/parallel-next/debug/vesper`, built from coordinator
source `bd392e1` (integration head `6793b3c`), passes:

- All five independent Rust executable tests, zero failures and zero ignores.
- Nine Python stdio sessions with 86 JSON-RPC requests, including exact current
  original-source Locations and every rejection/null/resource distinction above.
- Standalone Rust compilation/strict Clippy, rustfmt, Python syntax and whitespace
  checks. No source, test expectation or production limit needed adjustment after
  these actual feature runs.

All three ordinary fixture programs additionally pass the actual compiler; the
pre-integration framed baseline confirmed existing guarded analysis and lexical
shadowing before the fallback existed. Those were fixture baselines, separate
from the new feature results above. Root performs integrated Cargo verification
and independent provider review separately. These are local Linux results;
hosted native and BuildBuddy evidence remain separate. No program/guest/Lean
execution, proof certificate, timing or RSS guarantee is claimed.

Final provider hardening rerun: the actual executable rebuilt by the integrated
Cargo checks at coordinator head `4a691dc` (provider code `bd392e1` plus
`94e084e`) again passes all five Rust executable tests with zero failures/ignores
and all nine Python sessions/86 requests. The unsupported-owner inventory change
requires no acceptance expectation changes. This supplement changes only the
report; no builds, cache writes, production edits or limit changes occurred in
this lane.
