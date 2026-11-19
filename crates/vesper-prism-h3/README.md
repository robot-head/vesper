# vesper-prism-h3

QPACK (RFC 9204) and HTTP/3 (RFC 9114) for Vesper Prism over Vesper Warp.

> **Port freeze** (port wave P3b lane 5, [#1067](https://github.com/robot-head/vesper/issues/1067)): the Rust protocol core takes bug fixes only while it is ported to Vesper. The Vesper build lives in `vesper/` and is behind the non-default `vesper-port` feature. The Rust build stays the default until the port matches it in function and speed. See `docs/parallel-results/port-p3b-prism-h3.md`.

## Builds

- Default (Rust): `cargo test -p vesper-prism-h3`.
- Vesper port: `cargo test -p vesper-prism-h3 --features vesper-port`. QPACK (Huffman code, prefixed integers, static and dynamic tables, encoder, decoder) runs in Vesper through `port/binding.rs`. The connection driver stays Rust.
- Port checks, both builds and one divergence check per ported module: `sh port/check.sh`.
- ABBA benchmark against Rust: `sh scripts/bench.sh` (`CALLGRIND=1` adds instruction counts).
