# vesper-warp-wheel

The Vesper Warp timing wheel (design §35): a hierarchical hashed timing
wheel, the PTO lease table and the EDT launch queue. See the crate docs in
`src/lib.rs`.

**Frozen for the Vesper port** (port wave P1 lane 3, claim
[#900](https://github.com/robot-head/vesper/issues/900)): from 2026-11-17 the
Rust implementation takes bug fixes only. The Vesper source is in `vesper/`;
`--features vesper-port` runs the same API against it. The report is
`docs/parallel-results/port-p1-warp-wheel.md`.
