# @vesper/lsp-server

The LSP session loop in Vesper (#1828 part 15a). `serve` reads frames,
answers refused bodies, the request gate and the notification gate with
`@vesper/lsp`, and hands everything else to the engine seam
`lsp_engine_*` of `@vesper/driver-engines` (the `hc_lsp_engine_*`
functions of its `HybridCommands` import):

- `lsp_engine_frame(body)` runs one body and reports continue, clean
  exit, unclean exit or failure.
- `lsp_engine_take(buf)` copies the answer frames out.
- `lsp_engine_exit()` and `lsp_engine_error(buf)` report the outcome.
- `lsp_engine_read(buf, wait_ms)` and `lsp_engine_write(buf)` are the
  client's byte streams for `serve_live` (part 15b), and
  `lsp_engine_deadline()` and `lsp_engine_tick()` run the debounce timers
  and background proof runs while the loop waits.

`serve_stdio` is `vesper lsp` in the hybrid compiler (part 15c), whose
host is crates/vesper-hybrid/src/lsp_engine.rs. Today the engine is the
hidden Rust `vesper::lsp::Engine`. The
differential `server_loop_matches_rust` in tests/self_host_lsp_core.rs
binds it from the host and compares whole sessions with Rust `serve`;
`live_loop_matches_rust` feeds the same sessions to `serve_live` in
pieces, and `live_loop_runs_timers` checks a background proof run.

Gaps: `vesper.proofAudit` blocks the loop until its job finishes,
and work that is already running is not cancelled. See
docs/parallel-results/self-host-lsp-features.md.
