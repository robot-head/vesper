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
  `lean.ves`): `src/lsp/proof_model.rs`, part 3 (#1828);
- the Lens ownership facts and live linear obligations (`lsp_lens_facts`,
  `ownership.ves`): the `facts` walker, `live` and `resolve` of
  `src/lsp/lens.rs`, part 4 (#1828);
- the repaired Lens view of syntax-damaged source (`lsp_lens_repair`,
  `repair.ves`): `repair`, `candidates` and `open_at` of `src/lsp/lens.rs`,
  part 5 (#1828);
- the Lens views of semantic-error and repaired text and their queries
  (`lsp_lens_view`, `ownership.ves`): `View::from_text`, `outstanding` at
  the cut, `poisoned`, `binding`, `complete_member`, `signature` and
  `rank_methods_within`, part 6 (#1828);
- the Lens request layer of the server (`lsp_lens_requests`,
  `ownership.ves`): poison notices and Lens metadata, completion items,
  obligation hover text and inlay hints of
  `src/lsp/session/lens_requests.rs`, part 7 (#1828);
- the proof code lenses, proof ledgers, `vesper.proofAudit` results and
  background `vesper/proofStatus` notices (`lsp_proof_lenses`,
  `proof_lenses.ves`): `code_lenses_of`, `record_of`, `proof_result_of`
  and `proof_status_of` of `src/lsp/session/lens_requests.rs` and
  `ProofLedger` of `crates/vesper-lsp/src/lens.rs`, part 9 (#1828).

The source is parsed with `@vesper/front` and `@vesper/ast`, positions use
`@vesper/span`, and the JSON is written byte for byte as serde_json writes
it. The package does not depend on `@vesper/serde` (see the lane 16 report,
compiler gap 1).

See docs/parallel-results/self-host-lsp-features.md.
