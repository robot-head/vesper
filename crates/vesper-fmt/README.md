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

Invalid or incomplete buffers return errors; there is no best-effort rewrite of
unparsed code. The current source parser has no general macro-invocation or
quotation grammar. Identifier-`!` candidates and `quote`/`quasiquote`/
`syntax_quote` tokens conservatively return `OpaqueMacro`. This preserves the
*entire original input* (including observable payload trivia), rather than
formatting inside or around an unsupported macro. These spellings may also reject
ordinary identifiers; this is an explicit conservative boundary, not macro
expansion or grammar support.

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

`src/fmt_commands.rs` provides `vesper fmt FILE [--check] [--json]` once the
coordinator adds root routing. Check mode never writes and reports noncanonical
source with nonzero exit. Regular UTF-8 input is bounded to 256 KiB; final
symlinks/nonregular files are rejected. Success stages output with exclusive
creation beside the destination, preserves file permissions, syncs the staging
file, checks for observed source changes, then renames as the single commit.
All returned precommit errors preserve the input and remove only staging files
owned by this invocation. A colliding existing staging name is never deleted.
This is not crash-durable directory fsync, metadata/xattr/ownership preservation,
or a filesystem compare-and-swap against an uncoordinated concurrent writer
between the final check and rename. Callers must coordinate concurrent edits.

Run the independent example with:

```sh
cargo run --manifest-path crates/vesper-fmt/Cargo.toml --example canonical
```

It formats the repository's projection fixture and asserts idempotence. The leaf
manifest also runs the real source-adapter file tests without building the native
facade. When the same root `tests/fmt_cli.rs` runs with Cargo's Vesper binary, it
uses subprocess `vesper fmt` calls instead; the coordinator validates that route.
