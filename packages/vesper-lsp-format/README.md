# @vesper/lsp-format

The `textDocument/formatting` action of `vesper lsp` (`src/lsp/actions.rs`)
in Vesper, part 11a of #1828. Rust stays the default: the language server
still runs `src/lsp/actions.rs`, and this package is checked against it by
`tests/self_host_lsp_features.rs`.

`lsp_formatting` runs the action the way the session does:

- it checks the action document (the line table, a URI of 1 to 4,096
  bytes, at most 128 KiB of source) and the expected version and BLAKE3
  source digest;
- it formats the document with `@vesper/fmt` and proposes one
  whole-document edit when the text changed;
- on a syntax error or an opaque macro it proposes the recovery
  formatter's edits to the complete declarations instead;
- it writes the edits, the workspace edit and the provenance as
  serde_json writes them.

It is its own package because `@vesper/fmt` links C BLAKE3
(`@vesper-std/hash-sys`), which a Wasm entry does not have, and
`@vesper/lsp-features` must still build under Stage 2. The source digest
uses the portable BLAKE3 of `@vesper-std/hash`, so the entry never reaches
a foreign call.

Modules:

- `lib.ves`: the input reader and the entry.
- `formatting.ves`: the checks, the formatter calls and the JSON.
