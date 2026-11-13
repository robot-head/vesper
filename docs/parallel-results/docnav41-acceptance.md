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

Validation so far: standalone Rust compilation/strict Clippy, rustfmt, Python
syntax and whitespace checks pass. All three ordinary fixture programs pass the
actual compiler; a baseline framed stdio request confirms the local fixture is
admitted by existing guarded analysis and resolves its shadowed local variable.
Those baseline checks do not claim the new doc-navigation fallback executes.

**Feature runtime results are pending the provider-integrated executable.**
The coordinator will supply that actual binary before both harnesses run. No
URI filesystem reads, program/guest/Lean execution, proof certificate, timing,
RSS, BuildBuddy or other-platform execution is claimed.
