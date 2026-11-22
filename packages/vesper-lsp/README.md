# @vesper/lsp

The JSON-RPC transport of `crates/vesper-lsp` (`src/transport.rs`) and the
transport half of the stdio loop of `src/lsp.rs`, in Vesper (self-hosting
wave SH5 lane 16, #1339). Rust stays the default: the language server runs
`crates/vesper-lsp`; this package is checked against it by
`tests/self_host_lsp_core.rs`.

- `src/limits.ves`: `ProtocolLimits` and `ProtocolErrorKind` (`Perr`, each
  variant prefixed `P` because variant names share one namespace).
- `src/frame.ves`: `read_frame`, bounded headers and the body.
- `src/envelope.ves`: `parse_envelope` over the serde port's token tape
  (`@vesper/serde`), with the Rust preflight's check order and
  serde_json's number rules.
- `src/encode.ves`: `encode_frame`: the output budget, serde_json's compact
  writer write by write (the output limit is checked per write, as Rust's
  `Output` does) and ryu's float text.
- `src/lib.ves`: the JIT entries the differential calls (`lsp_frames`,
  `lsp_envelope`, `lsp_encode`, `lsp_serve`).

The document store is the sibling package [`documents/`](documents/): a
world that contains `@vesper/serde` cannot name the builtin `String`
(serde's `Kind::String` takes the name), and the store needs it.

See docs/parallel-results/self-host-lsp-core.md for the numbers and gaps.
