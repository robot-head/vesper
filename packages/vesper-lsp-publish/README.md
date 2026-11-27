# @vesper/lsp-publish

The `textDocument/publishDiagnostics` payload of `vesper lsp` for a failed
analysis, in Vesper (#1828 part 13b). It ports `diagnostic_payload`, the
analysis-unavailable warning and the publication filter of
`src/lsp/session.rs`:

- a parse or semantic failure is rendered by the Vesper port of
  vesper-diagnostics (`prepare_check`, then `lsp`), and its status is
  `parse_error` or `semantic_error`;
- a rendering error publishes one analysis-unavailable warning at the
  document origin, with the kind `invalid_compiler_span` for a span error
  and `diagnostic_metadata` for any other, and the error's `Display` text;
- a resource or unsupported failure publishes the same warning with its
  reason;
- the fields the client did not negotiate are removed: `relatedInformation`
  without related-information support, and `data` with help without
  help-data support.

`lsp_diagnostic_payload` reads the words of
`session::diagnostic_payload_words` and answers the JSON object of
`session::diagnostic_payload_answer`. The package is separate from
`@vesper/lsp-features` because `@vesper/diagnostics` and `@vesper/span`
both declare the variants of `Resource`, and enum variants share one
namespace across a program's packages.

`Diagnostics::new`'s checks, which the Rust build runs between `prepare`
and `to_lsp`, are not repeated: with no fixes, each is implied by a passing
`prepare_check` (the same span checks, a stronger work bound and twice the
metadata bound).

Tests: `diagnostic_payload_matches_rust` and the ignored
`diagnostic_payload_speed` in `tests/self_host_lsp_features.rs`.
