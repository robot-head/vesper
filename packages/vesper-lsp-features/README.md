# @vesper/lsp-features

The editor features of `vesper lsp` (`src/lsp/**`) in Vesper: self-hosting
wave SH5, lane 17 (#1340). Rust stays the default: the language server still
runs `src/lsp`, and this package is checked against it by
`tests/self_host_lsp_features.rs`.

Part 1 ports navigation (`src/lsp/navigation.rs`):

- `textDocument/documentSymbol`, in both the hierarchical and the legacy
  (`SymbolInformation`) shape;
- `workspace/symbol`;
- `textDocument/definition` by lexical resolution. A cursor inside a `///`
  or `//!` comment with no lexical target is the documentation-link
  fallback (`navigation/doc_links.rs`), which needs `vesper_doc::links`;
  the entry reports it as deferred;
- `lsp_definitions`, many definition requests on one document with the
  parse kept, as the server keeps its checked snapshot.

The source is parsed with `@vesper/front` and `@vesper/ast`, positions use
`@vesper/span`, and the JSON is written byte for byte as serde_json writes
it. The package does not depend on `@vesper/serde` (see the lane 16 report,
compiler gap 1).

See docs/parallel-results/self-host-lsp-features.md.
