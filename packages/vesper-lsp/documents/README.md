# @vesper/lsp-documents

`DocumentStore` of `crates/vesper-lsp/src/document.rs` in Vesper
(self-hosting wave SH5 lane 16, #1339): open, change (ranged and full
edits, `rangeLength`, UTF-16 positions through `@vesper/span`), close and
get, with every limit, refusal and revision rule of the Rust store. Rust
stays the default.

The store is flat: one `String` arena holds every URI and text, with
offsets per document, and is compacted once the dead bytes pass both
64 KiB and the live bytes.

It is its own package because a world with `@vesper/serde` (which
`@vesper/lsp` needs) cannot use the builtin `String`. `src/lib.ves` holds
the `lsp_documents` entry of `tests/self_host_lsp_core.rs`.
