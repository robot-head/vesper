# @vesper/lsp-features

The editor features of `vesper lsp` (`src/lsp/**`) in Vesper: self-hosting
wave SH5, lane 17 (#1340). Rust stays the default: the language server still
runs `src/lsp`, and this package is checked against it by
`tests/self_host_lsp_features.rs`.

Part 1 ports navigation (`src/lsp/navigation.rs`); part 1b (#1828) brings it
under the 1.5x bar, and part 2 adds hover and the documentation links:

- `textDocument/documentSymbol`, in both the hierarchical and the legacy
  (`SymbolInformation`) shape;
- `workspace/symbol`;
- `textDocument/definition` by lexical resolution. A cursor inside a `///`
  or `//!` comment with no lexical target is the documentation-link
  fallback (`navigation/doc_links.rs` over `vesper_doc::links`), in
  `doc_links.ves`, `links.ves` and `markdown.ves`. It needs the
  declaration facts, so the single entry reports it as deferred;
- `lsp_definitions`, many definition requests on one document with the
  parse kept, as the server keeps its checked snapshot, with the doc facts;
- `textDocument/hover` (`lsp_hovers`, `hover.ves`) over the checker's facts
  in the `@vesper/check-data` harness layout;
- the proof-lens models (`lsp_proof_models`, `proof.ves`, `proof_types.ves`,
  `lean.ves`): `src/lsp/proof_model.rs`, part 3 (#1828).

The source is parsed with `@vesper/front` and `@vesper/ast`, positions use
`@vesper/span`, and the JSON is written byte for byte as serde_json writes
it. The package does not depend on `@vesper/serde` (see the lane 16 report,
compiler gap 1).

See docs/parallel-results/self-host-lsp-features.md.
