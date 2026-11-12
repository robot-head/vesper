# §39 bounded lossless formatter (#64)

Implemented claimed [issue #64](https://github.com/robot-head/vesper/issues/64)
on `tooling41/fmt`, starting at `a1a8d938dd8103803219a05c8364cb7a198be8d9`.
Read the issue, `docs/rfcs/tooling41-contracts.md`, full §39 and the existing lexer/
Rowan parser before design. All writes are exclusively new `crates/vesper-fmt/**`,
`src/fmt_commands.rs`, `tests/fmt_cli.rs` and this report. The existing syntax,
core/checker/native, Warp, root manifests/lock/CLI/CI are unchanged. No temporary
root wiring is included.

## Actual formatting and validation

The root-free leaf depends only on existing `vesper-syntax` in production. It
lowers the actual lossless Rowan CST's source-anchored tokens into a bounded Doc
arena. It checks every CST token's byte range, kind and text against lexer
identity before lowering. Whitespace becomes canonical grouping, four-space
nesting, member-chain breaks and hard lines around statements/blocks/comments.
The fixed ribbon is 100 Unicode scalar columns. Group fitting accounts for
following punctuation and a block's opening brace. Indivisible token/comment
payloads are preserved verbatim and may exceed the ribbon; grapheme/display-cell
width is not claimed. Blank lines outside tokens normalize to the fixed layout.

Every successful `format_source` result reparses with the actual production
parser, conserves the exact combined non-whitespace kind/text sequence including
all comment bytes and their order relative to code, and formats identically on
a second pass. Line comments force a boundary before every following token or
closing delimiter. Strings, Unicode, nested block comments and CR bytes remain
unchanged inside their token. No checker or semantic autofix is invoked.

Invalid/incomplete source fails without output replacement. The current source
parser does not support general macro invocation/quotation payloads. Conservative
identifier-`!` candidates and quote/quasiquote/syntax_quote spellings yield
`OpaqueMacro`; the entire original buffer remains unchanged. This rejects some
ordinary identifiers too. It does not claim formatting around unknown macros or
trivia-insensitive macro expansion.

The public Doc algebra supplies nil, text, comment, hard/soft lines, nest, concat
and group. Origin-bound opaque IDs refer only to already built nodes, with checked
width/depth/indentation and work/node/output limits. Rendering/lookahead is
iterative and work-charged; arena destruction does not recursively drop Doc trees.
No O(N), speed, zero-allocation or comment-conservation theorem is asserted.

## Budgets and file replacement

Default source limits: 256 KiB input, 1 MiB output, 8,192 lexer tokens, 100,000 Doc
nodes, 24 delimiter levels, 128 Mi deterministic work units. Token/depth hard caps
remain 8,192/24. A path-chain budget of 128 components counts at each delimiter
level; child separators cannot reset ancestor chains (including repeated calls
with comma-containing arguments or repeated brace operands). This conservatively
bounds inputs handed to the existing recursive parser/AST. Valid complex input
can be rejected. Doc depth is capped at 256. Counts use checked arithmetic. Work
units include scans, parser preflight, construction, fitting/lookahead and emitted
bytes; they are not actual CPU timing or a total heap/RSS/OOM bound. Existing
lexer/parser internal allocations remain outside fallible Doc allocation handling.
A separately claimed bounded-parser lane (#70) may provide tighter APIs later;
this branch does not depend on or modify that work.

The adapter accepts `vesper fmt FILE [--check] [--json]`, a single bounded regular
UTF-8 file, and rejects final symlinks/nonregular files. Check never writes and
returns an error when changes are needed. Formatting completes all parse,
conservation, idempotence and resource checks before creating output staging.
It creates a short exclusive staging name in the destination directory, writes
and syncs complete output, preserves permissions, rejects observed source edits,
and commits with one rename. Returned errors before commit leave input unchanged;
cleanup only owns a successfully created staging file, so collisions cannot
delete existing files. There are no fallible returned checks after rename.

Atomic rename is not a filesystem compare-and-swap: external edits racing the
last identity/content check and rename require caller coordination. Directory
fsync/crash durability and ownership/xattr metadata retention are not claimed.

## Validation

All builds used private `/tmp/vesper-tooling41-fmt-target`, one job, development/
test debug info zero and incremental off. Target footprint was about 120 MiB.

- `cargo test --manifest-path crates/vesper-fmt/Cargo.toml --all-targets`: source/
  Doc and source-adapter tests pass (19 total: 11 leaf plus eight adapter cases). Formatter tests reparse, compare the full
  kind/text/comment sequence and assert idempotence on nine real repository
  fixtures (projections, closures, effects/transactions, Fibonacci, resumptions,
  checked arithmetic, traits and native-Pulse syntax). Additional cases cover
  exact canonical output, Unicode/strings/CR/nested comments, line-comment
  boundaries, generics/effect rows/projections, whitespace perturbations,
  100-column brace boundaries/member chains, indivisible long strings,
  incomplete/invalid/opaque buffers and size/token/depth/work/node errors.
- Doc tests exercise flat/broken groups, Unicode widths, shared subtrees,
  cross-document IDs, depth caps and output/work exhaustion. Chain guards reject
  both long unary expressions and nested-call/brace separator bypass attempts.
- Source adapter tests cover unchanged check mode, successful replacement,
  second check/idempotence, JSON report states, UTF-8/parser/macro/budget failures,
  missing/invalid options and files, nonregular/symlink rejection, permission
  preservation, staging collision ownership and observed concurrent edits.
- `cargo clippy --manifest-path crates/vesper-fmt/Cargo.toml --all-targets -- -D warnings`:
  passes. Cargo fmt, focused adapter/test rustfmt and git whitespace checks pass.
- `cargo run --manifest-path crates/vesper-fmt/Cargo.toml --example canonical`:
  formats the actual projections fixture and asserts idempotence before printing.

The standalone leaf test registration compiles the identical `src/fmt_commands.rs`
and exercises its real file behavior directly, avoiding duplicate builds of the
native facade. Root `tests/fmt_cli.rs` automatically invokes the actual Vesper
subprocess when Cargo provides `CARGO_BIN_EXE_vesper`. Actual root command routing
is coordinator-owned and must be validated after integration; this branch does
not claim it was already executed. Validation here is local Linux, not Windows/
macOS execution, a formal proof or a performance benchmark.

## Stable API and coordinator wiring

`vesper_fmt::format_source(&str, FormatLimits) -> Result<FormatResult, FormatError>`;
`FormatResult { output: String, changed: bool }`. Aliases `FormatLimits=Limits`,
`FormatError=Error`. Public `Document`, `Doc`, `DocId`, `DEFAULT_LINE_WIDTH=100`.
The source adapter is `fmt_commands::run(Vec<String>) -> Result<(), String>`.

Remove the leaf's temporary `[workspace]` block, add `crates/vesper-fmt` to root
members/default-members, and add root path dependency `vesper-fmt`. In main add
`mod fmt_commands;`, help entry `fmt     Canonically format source (see fmt --help)`,
and the existing early dispatch style:

```rust
if command == "fmt" {
    return fmt_commands::run(args.collect());
}
```

No root lib module or new production dependency besides the leaf is needed.
Leaf standalone tests use dev-only serde_json/libc to compile the source adapter;
root already has those dependencies. Run actual root CLI tests with
`cargo test -p vesper --test fmt_cli`, then the normal combined checks.


## Follow-up: readonly staging cleanup

Peer review found that a stage inheriting readonly permissions could remain after
a later precommit error on Windows, whose unlink rejects readonly files. The
invocation-owned staging guard now clears readonly on Windows before attempting
unlink; it leaves Unix mode bits unchanged. The guard is still constructed only
after successful exclusive creation and disabled after commit, so preexisting
collisions are never made writable or deleted. Cleanup is best effort when OS
permission/sharing/device operations fail; no unconditional cleanup on process
abort or arbitrary OS errors is claimed.

Two focused cross-platform regressions use the actual replacement helper. One
captures readonly input permissions, changes the source, then exercises the
precommit drift failure after the stage inherited readonly and verifies its
removal/source preservation. The other verifies that a readonly colliding stage
keeps both bytes and readonly permission. Linux executes the Unix behavior;
Windows-specific clearing still requires the coordinator's Windows CI execution.

Follow-up validation: all 21 standalone tests pass (11 formatter plus 10 adapter),
strict all-target Clippy, formatting and whitespace checks pass on Linux.
