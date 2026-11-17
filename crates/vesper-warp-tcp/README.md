# vesper-warp-tcp

Sans-IO RFC 9293 TCP endpoint for Vesper Warp (manuscript §35): wire format,
connection state machine, SACK loss recovery, NewReno, and a deterministic
lossy two-endpoint test harness.

**Frozen (bug fixes only) since 2026-11-17:** the crate is being ported to
Vesper in port wave P1 lane 11 ([#908](https://github.com/robot-head/vesper/issues/908)).
The Vesper build is the library in `vesper/` (one file per Rust module,
concatenated by build.rs), selected with the `vesper-port` feature;
the Rust implementation lives in `src/reference/`. See
`docs/parallel-results/port-p1-warp-tcp.md`.
