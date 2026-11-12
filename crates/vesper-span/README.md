# vesper-span

Dependency-free, safe Rust source coordinates and checked editor data. There is
no filesystem access, compiler execution, rendering, serialization, provenance
inference, or performance claim.

`LineIndex::new(Arc<str>, Limits)` retains an immutable source snapshot. `text()`
borrows it; `offset(Position)` maps a zero-based UTF-16 line/character to a byte
offset; `position(usize)` reverses that mapping. `range(ByteSpan)` and
`span(Range)` validate both endpoints and ordering. Every conversion returns
`Result<_, Error>` and never clamps. Public coordinate fields are transport data;
they become validated only when consumed by these methods.

CRLF, lone CR, and lone LF each terminate one line. The CR/LF byte at the end of
line content maps to that content's final UTF-16 position. The byte boundary
between a CR and its following LF is rejected (`LineEndingInterior`), since it
has no distinct LSP position. A trailing terminator creates an empty final line.
EOF is addressable. A byte inside a UTF-8 code point or a UTF-16 position between
an astral character's surrogate pair is rejected. Unicode line separator and
paragraph separator remain ordinary characters.

`SourceEdit { span, expected, replacement }` is unchecked input.
`SourceEdits::new(&LineIndex, &[SourceEdit], Limits)` validates all edits and
budgets before copying them; `apply()` creates a separate resulting `String`.
Its immutable original snapshot remains available through `source()`. Expected
bytes must match each replaced span; an insertion has no old bytes to compare.
The constructor binds insertions to its snapshot, but does not independently
prove an earlier client revision. Unordered edits are sorted by byte offsets.
Adjacent nonempty spans are allowed. Overlapping spans, duplicate insertions,
and insertions at either endpoint of another edit are rejected, so results do
not depend on caller ordering. Editing half of a CRLF is rejected. Batch edits
refer to the same original snapshot; sequential LSP changes must construct a
new index for each intermediate source in a transactional higher-level store.

`DiagnosticData`, `Label`, `Fix`, and `Severity` represent supplied data.
`Diagnostics::new(&LineIndex, &[DiagnosticData], Limits)` validates all labels and
fix batches before copying, retaining an exact source snapshot. Each diagnostic
requires a nonempty code/message and at least one primary label; fixes require
nonempty IDs/titles/edit lists. All spans refer to that same source, and each fix
is an independent alternative. Counts, metadata, text and work are aggregated
across the batch. `data()` exposes immutable checked data. This does not alter
old compiler diagnostics, establish causal or semantic evidence, or guarantee a
fix compiles or preserves SemVer.

Limits are logical payload/operation budgets, not process RSS, allocator
capacity, elapsed-time, or OOM guarantees. `Limits::validate()` rejects values
above `Limits::HARD` before traversal/allocation. Zero is allowed, though an
empty source still needs one line. Existing caller-owned source/input buffers
are not copied before checks. New line tables, strings, edit arrays, diagnostic
arrays, labels and fixes use fallible reservations after validation. Metadata
counts stored element sizes and string bytes (diagnostic fix text is counted
conservatively twice); allocator headers/capacity rounding and fixed stack
objects are excluded. Work charges byte traversal/copy upper bounds and edit
comparison/sort allowances; bounded metadata loop/binary-search overhead is
additionally constrained by the hard count caps. Repeated API calls each receive
the same per-call budget, not a shared lifetime quota. `apply()` uses the output
and work bounds established at construction. Source budgets are checked on all
constructors; line/index budgets apply to `LineIndex` construction. Each new
batch has its own metadata budget; sharing an existing index does not charge its
line table again.

| Limit | Default | Hard cap |
| --- | ---: | ---: |
| source bytes | 128 KiB | 1 MiB |
| lines | 32,769 | 262,145 |
| metadata bytes | 1 MiB | 16 MiB |
| work | 2 MiB | 64 MiB |
| edits per batch (aggregate across diagnostic fixes) | 256 | 4,096 |
| output bytes per alternative | 256 KiB | 2 MiB |
| diagnostics | 256 | 4,096 |
| labels | 1,024 | 16,384 |
| fixes | 256 | 4,096 |
| text bytes | 128 KiB | 2 MiB |

Run `cargo run --example source_edits` for a self-checking Unicode edit example.
