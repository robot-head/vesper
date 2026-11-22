# @vesper/memo

Memo tables with salsa's invalidation, in Vesper (self-hosting wave SH5
lane 16, #1339), for the language server's incremental queries
(`src/database.rs`): inputs whose setters start a new revision, derived
queries memoized per (query, input), deep verification of a memo's reads
in order, re-execution, and early cutoff (backdating) for queries whose
values compare (salsa's default) but not for `no_eq` ones.

What a query reads and returns comes from an oracle (`Oracle` in
`src/memo.ves`) that the differential records from the actual queries; a
Vesper language server would put the query functions there. The
differential (`memo_invalidation_matches_salsa` in
`tests/self_host_lsp_core.rs`) compares the order of salsa's
`WillExecute` and `DidValidateMemoizedValue` events with this package's
log on every recorded edit. Rust (salsa) stays the default.
