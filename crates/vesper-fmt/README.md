# Bounded canonical source formatter

This allocating host library uses the existing `vesper-syntax` lossless lexer
and Rowan CST. It formats the grammar currently accepted by that parser; it does
not implement the entire design's new surface grammar or semantic autofixes.
No checker/native/compiler production code is changed.

`format_source(&str, FormatLimits) -> Result<FormatResult, FormatError>` returns
`output: String` and `changed: bool`. Every success has passed all of:

1. Input lexing, delimiter/chain resource guards, and production parser/CST.
2. CST token ranges/kinds/text agree exactly with the lossless lexer.
3. Lowering to a bounded Doc arena and a fixed 100-column/four-space policy.
4. Exact combined non-whitespace token kind/text conservation, including every
   comment byte in its original order relative to both comments and code.
5. Reparse and a second formatting pass giving byte-identical output.

Line comments always have a hard newline before the next token, including closing
delimiters. Nested block comments, CR bytes in line comments, Unicode text and
string spelling are preserved. Whitespace and blank lines outside tokens are
canonicalized; a terminal LF is emitted. The ribbon counts Unicode scalar values,
not terminal cells/graphemes. Indivisible tokens and comment payloads are never
reflowed and may exceed 100 columns. Groups reserve following punctuation/brace
space; member chains have empty flat-mode break points.

`format_source` supports the fixed, nonrecursive expression-macro profile accepted
by the actual `vesper-syntax::macro_source` parser. It validates the complete
macro module before using recovery CST token locations for formatting. Formatted
output must pass the same profile again. The formatter never expands or executes
macros. Unknown invocations, other categories, repetitions, derives and nested
invocations remain errors. Bare `quote`/`quasiquote`/`syntax_quote` names retain
their conservative opaque boundary unless a complete supported macro module is
recognized. Comment and string spelling are conserved exactly.

Default source formatting still rejects invalid or incomplete buffers. The
separate `format_recovery_source(&str, Limits)` API offers syntax-only formatting
of independently complete top-level functions in a damaged buffer. It constructs
sealed recovery syntax, reparses eligible complete items, and formats only those
declaration spans. Every opaque/damaged gap and all bytes outside those spans
remain exact. Global lexical uncertainty rejects. There is no manufactured
checked program, semantic repair or diagnostics suppression.

The recovery result has private fields and getters for output, changed status,
original source hash, sorted disjoint byte edits and the number of safely
formatted declarations (including already canonical ones). All passes share a
cumulative work budget: parsing, fragment formatting, edit/output storage,
token/comment conservation, recovery-inventory comparison and a second complete
idempotence pass. Each replacement omits the formatter-added terminal LF so
outside whitespace cannot grow between passes. Valid large inputs can be refused
by conservative bounds.

The public `Document`/`Doc` algebra has nil, text/comment, hard/soft lines, nest,
concat and group. Opaque Doc IDs are origin-bound and reference only previously
inserted nodes, so cross-document references and cycles cannot silently resolve.
Cached flat widths and indentation arithmetic are checked. Iterative rendering
and bounded lookahead choose flat/broken groups. Both creation and rendering
charge work; repeated/shared subtrees cannot bypass output/work limits. The arena
avoids recursive Doc destruction. No O(N), timing or conservation theorem is claimed.

Defaults are 256 KiB input, 1 MiB output, 8,192 lexer tokens, 100,000 Doc nodes,
24 delimiter levels and 128 Mi deterministic work units. Token and delimiter
hard caps remain 8,192 and 24 even if larger limits are requested. A separate
128-component path/chain guard protects the existing recursive parser: child
commas/semicolons do not reset an ancestor's unary/assignment/postfix/type chain.
Root declaration boundaries reset their own path counter. Doc depth is capped
at 256. Valid but unusually complex source can be rejected by these conservative
guards. Work units count scans, conservative parser preflight, nodes, lookahead
and output bytes; they are not measured CPU instructions or an RSS limit.
Existing lexer/parser internal allocations are not converted into fallible arena
APIs. There is no OOM/crash-recovery guarantee or zero-allocation claim.

`src/fmt_commands.rs` provides `vesper fmt FILE [--recovery] [--check] [--json]`.
The explicit recovery flag selects the complete-declaration mode. JSON retains
schema_version1, adds mode source/recovery, and includes the edit count in recovery
mode. Check mode never writes and reports noncanonical
source with nonzero exit. Regular UTF-8 input is bounded to 256 KiB; final
symlinks/nonregular files are rejected. Success stages output with exclusive
creation beside the destination, preserves file permissions, syncs the staging
file, checks for observed source changes, then renames as the single commit.
All returned precommit errors preserve the input. Cleanup attempts to remove
only staging files owned by this invocation. On Windows it first clears an
inherited readonly bit on that owned stage; Unix permission bits are not changed.
A colliding existing staging name is neither made writable nor deleted. Cleanup
is best effort: an OS permission/sharing/device error can still prevent removal;
there is no process-crash or arbitrary OS-failure cleanup guarantee.
This is not crash-durable directory fsync, metadata/xattr/ownership preservation,
or a filesystem compare-and-swap against an uncoordinated concurrent writer
between the final check and rename. Callers must coordinate concurrent edits.

Editor formatting first tries ordinary/recognized-macro formatting and falls
back only for syntax/opaque errors. It reserves two 64 Mi work-unit stages from
one finite operation allowance; resource or conservation failures never retry
with a fresh budget. A recovery fallback offers only current-source UTF16 edits
for complete declarations, with syntax-only provenance. With no safe complete
declarations, formatting remains unavailable. No edit is automatically applied,
and no URI is treated as a filesystem path.

Run the independent example with:

```sh
cargo run --manifest-path crates/vesper-fmt/Cargo.toml --example canonical
```

It formats the repository's projection fixture and asserts idempotence. The leaf
manifest also runs the real source-adapter file tests without building the native
facade. When the same root `tests/fmt_cli.rs` runs with Cargo's Vesper binary, it
uses subprocess `vesper fmt` calls instead; the coordinator validates that route.
