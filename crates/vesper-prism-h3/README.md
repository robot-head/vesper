# vesper-prism-h3

QPACK (RFC 9204) and HTTP/3 (RFC 9114) for Vesper Prism over Vesper Warp.

> **Vesper port** (port wave P3b lane 5, [#1067](https://github.com/robot-head/vesper/issues/1067); the only build since SH6 lane 5, [#1400](https://github.com/robot-head/vesper/issues/1400)): the protocol core is the Vesper library in `vesper/`. The connection drivers (`connection.rs`, `client.rs`, `session.rs`, `transport.rs`, `warp.rs`, `prism.rs`) are still Rust. See `docs/parallel-results/port-p3b-prism-h3.md` and `docs/parallel-results/rust-exit-prism.md`.

## Builds

- `cargo test -p vesper-prism-h3`. QPACK (Huffman code, prefixed integers, static and dynamic tables, encoder, decoder), frames, priorities, capsules and the message rules run in Vesper through `port/binding.rs`.
- Port checks, the tests and one divergence check per ported module: `sh port/check.sh`.
- ABBA benchmark against the last Rust build: `sh scripts/bench.sh` (`CALLGRIND=1` adds instruction counts).
