# vesper-diagnostics

A root-free bounded host adapter for actual compiler diagnostic metadata. It
performs no filesystem, compiler, native/JIT/WASM or proof execution. Validation
checks supplied data and its byte locations against one exact source; it does
not establish semantic causality or authorize fixes/proof claims.

```rust
use std::sync::Arc;
use vesper_core::ast::Diagnostic;
use vesper_diagnostics::{prepare, Limits};
use vesper_span::LineIndex;

let limits = Limits::default();
let index = LineIndex::new(Arc::from("let x = 0;\nconsume(x);"), limits.span)?;
let diagnostics = [Diagnostic::new("E0300", "already consumed", 19..20)
    .with_related(4..5, "binding declared here")
    .with_help("Consume each linear value exactly once.")];
let prepared = prepare(&index, &diagnostics, limits)?;
let lsp = prepared.to_lsp("untitled:example")?;
let terminal = prepared.terminal("example.vsp")?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

`prepare(index, &[vesper_core::ast::Diagnostic], Limits)` returns immutable
`Prepared<'_>` borrowing the exact LineIndex and owning checked diagnostic DTOs.
All primary/related spans, aggregate counts, code/message/help/label text, source
bytes/lines, metadata and work are validated before metadata copies. The DTOs
also pass `vesper_span::Diagnostics::new`; no fixes are created. Original primary
code/message and ordered related labels are retained. `data()` and `source()`
provide immutable inspection. Mutating caller diagnostics after preparation
cannot change the prepared result. Construction stays private.

`Prepared::to_lsp(uri)` returns a complete Vec<Value>: severity1/source=vesper,
original code/message and strict UTF-16 primary range; actual secondary labels
become standard relatedInformation locations with the same opaque URI and their
own range/message. relatedInformation is omitted when absent. Optional help
appears only in data.help (including Some(empty)); absent help omits data.
There are no code actions, synthetic locations or fix payloads. An exact compact
JSON-array UTF-8 byte count, including escaped strings, URI repetition and range
numbers, is checked before DOM copies. This cap covers the diagnostic array;
server envelope/framing limits belong to the caller. Work preflight charges
source endpoint scans, escaping, URI copies and DOM shape; metadata is charged
before constructing the returned DOM. No partial vector is returned on failure.

`Prepared::terminal(file)` returns a complete private Vec<u8>, rendering all real
labels/help with Ariadne0.5 configured explicitly for IndexType::Byte, stable
label ordering and colors disabled. There is no eprint/filesystem source cache;
Ariadne receives only the supplied source and private bounded writer. Invalid
UTF-8 boundaries, reversed/out-of-bounds spans and CRLF-interior endpoints fail;
nothing is min-clamped or remapped to a guessed location. LSP character offsets
are UTF-16; terminal columns use Ariadne's Unicode character display convention.

Two Ariadne integration details are explicit:

- After a real trailing CR/LF terminator, the shared index contains an empty EOF
  line. Ariadne normally omits it. A single display-only trailing blank makes
  that actual empty line addressable without changing any original byte or
  validated offset. Source identity remains the original index; no edit is
  published. Empty source and zero-width EOF without a terminator also work.
- Ariadne treats VT, FF, NEL, U+2028 and U+2029 as line separators, while the
  shared profile terminates lines only at CR/LF. Terminal rendering explicitly
  returns UnsupportedLineEnding for such sources; LSP conversion remains valid.
  A host can report raw code/message/byte offsets with an unavailable reason.

Filename and URI are opaque, nonempty labels, at most max_name_bytes UTF-8 bytes,
with controls rejected. They grant no filesystem authority. Diagnostic message,
label and help text is preserved; disabling colors does not sanitize arbitrary
escape/control bytes supplied inside that metadata.

## Limits

`Limits { span:vesper_span::Limits, max_name_bytes:usize }` has public validated
fields. Default span profile: 128 KiB source, 32,769 lines, 1 MiB logical metadata,
64 MiB work units, 256 KiB output, 256 diagnostics, 1024 total labels (primary
included), 128 KiB aggregate text, no edits/fixes; max_name_bytes4096. HARD uses
span HARD: 1 MiB source, 262,145 lines, 16 MiB metadata, 64 MiB work, 2 MiB output,
4096 diagnostics, 16,384 labels, 2 MiB text, and name4096. Span edit/fix fields are
validated but this adapter never creates edits/fixes. Configurations exceeding
any hard field fail. Zero fields are allowed; operations needing that resource
fail, including an empty source's one logical line.

Prepare charges the temporary plus final DTO capacities/text, source line lookup,
source×label endpoint work and copies before allocation. LSP conversion charges
its repeated range/escaped-string/URI work before scanning/copying. Terminal
preflights source lines and a conservative source/label shape score, metadata
and text before building Ariadne data. Ariadne's label layout includes nonlinear
work; peak label/source combinations can fail even when each individual ceiling
is met. Failure is explicit Resource/Output, not a partial diagnostic or false
success. A 1 MiB CLI source profile is supported, but cannot promise all label
counts, line widths or rendered outputs fit the same budgets.

These are logical per-operation bounds on this adapter's data and requested
staging. Input LineIndex storage and caller-retained outputs remain caller-owned.
Vec/String copies use fallible reserve; serde_json DOM/Ariadne internal bounded
allocations use their standard allocators. Metadata charges and shape scores
are not an exact allocator/RSS or CPU-time proof. No latency or hard timeout is
claimed. Errors implement Display/std::error::Error and include structured
Span errors, Name, InvalidLimits, Allocation, Output and UnsupportedLineEnding.

After workspace registration:

```text
cargo test -p vesper-diagnostics --all-targets
cargo clippy -p vesper-diagnostics --all-targets -- -D warnings
cargo run -p vesper-diagnostics --example causal_diagnostics
```

The example self-checks actual Unicode/CRLF primary and related locations, help,
byte-indexed terminal output and the real empty EOF line.
