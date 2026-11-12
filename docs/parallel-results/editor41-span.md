# #79 bounded shared editor spans

Owned slice: new `crates/vesper-span/**` and this report on `editor41/span`,
base `593bbc311a1c0693aa08732b40cec813e2cda436`. Read issue #79, the editor41
contracts and current specification §21. Existing compiler diagnostics and all
compiler/database/protocol/root code remain unchanged.

The implemented, dependency-free, `#![forbid(unsafe_code)]` crate supplies:

- `Position { line: u32, character: u32 }`, `Range { start, end }`,
  `ByteSpan { start: usize, end: usize }` (Copy/Debug/Eq/Hash).
- `LineIndex::new(Arc<str>, Limits) -> Result<Self, Error>`, `text() -> &str`,
  `offset(Position) -> Result<usize, Error>`, `position(usize) -> Result<Position,
  Error>`, `range(ByteSpan) -> Result<Range, Error>`, and
  `span(Range) -> Result<ByteSpan, Error>`.
- Typed `Error`/`Resource`, with Display/Error implementations and no clamping,
  unchecked string slicing or lossy integer casts. UTF-8 interiors, UTF-16
  surrogate interiors, reversed/out-of-bounds ranges and CRLF interiors fail.
- `SourceEdit { span, expected, replacement }` and immutable snapshot-bound
  `SourceEdits::new(&LineIndex, &[SourceEdit], Limits)`, `source()`, `edits()`,
  `apply() -> Result<String, Error>`. All checks precede owned copies and output
  construction. Expected replaced bytes, unordered edit sorting, overlaps,
  insertion/boundary collisions, output and aggregate metadata/work are checked.
- Supplied `DiagnosticData`, `Label`, `Fix`, `Severity` and checked immutable
  `Diagnostics::new(&LineIndex, &[DiagnosticData], Limits)`, `source()`, `data()`.
  Multi-span labels and independent fix alternatives are validated against the
  exact same source. Counts/text/metadata/work aggregate across diagnostic fixes.

CRLF, lone CR and lone LF each terminate one line; Unicode U+2028/U+2029 do not.
The line terminator's initial byte offset maps to the preceding content end.
The CRLF midpoint cannot map to a distinct LSP position and is rejected. After a
terminator is next-line position zero; trailing terminators add an empty final
line. EOF is always representable on a successfully constructed index.

Default Limits public usize fields are `max_source_bytes=131072`,
`max_lines=32769`, `max_metadata_bytes=1048576`, `max_work=2097152`,
`max_edits=256`, `max_output_bytes=262144`, `max_diagnostics=256`,
`max_labels=1024`, `max_fixes=256`, `max_text_bytes=131072`.
`Limits::validate()` rejects fields above public `Limits::HARD` respectively:
1048576, 262145, 16777216, 67108864, 4096, 2097152, 4096, 16384, 4096, 2097152.
Zero caps are accepted; the empty source still requires one line. These are
logical payload budgets, not RSS/allocator/OOM/time guarantees. Existing source
Arc storage is caller-owned; each new batch has its own metadata budget and
sharing an index does not charge the existing line table again. Bounded count
caps additionally constrain fixed metadata traversal/binary-search overhead.
The crate README describes byte-work charges, conservative metadata accounting,
and exact insertion/stale-source semantics.

Validation in private `/tmp/vesper-target-editor41-span` with one job, debug=0,
incremental=0:

- `cargo fmt --manifest-path crates/vesper-span/Cargo.toml --check`: pass.
- `cargo test --manifest-path crates/vesper-span/Cargo.toml`: 14 integration
  tests pass, plus empty unit/doc suites. Exhaustive three-element strings over
  ASCII, BMP, astral, CR and LF exercise every byte boundary, alongside concrete
  malformed coordinates, usize/u32 extremes, EOF, exact index/edit caps,
  transactional failure, collision/order and aggregate diagnostic regressions.
- `cargo clippy --manifest-path crates/vesper-span/Cargo.toml --all-targets --
  -D warnings`: pass.
- `cargo run --manifest-path crates/vesper-span/Cargo.toml --example source_edits`:
  actual Unicode/transactional assertions pass.
- `git diff --check`: pass.

The leaf manifest includes standalone `[workspace]` for isolated checks; root
registration removes that table and adds the leaf to root members/dependencies.
No registry dependencies and no leaf lockfile committed. Root/protocol/navigation/
actions owners received the frozen API before integration.

Scope is one immutable source per index/diagnostic batch. This DTO validates
coordinates and payloads, not compiler provenance, causal assertions, SemVer,
fix compilation, cross-file source identity, live obligations, incremental
reparsing, proof badges or any §21 latency claim. No terminal renderer,
filesystem access, serialization or compiler execution is introduced. A batch
insertion's empty expected text does not prove an earlier client version;
higher-level version/source checks remain necessary before construction.
