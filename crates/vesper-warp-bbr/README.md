# vesper-warp-bbr

Sans-IO BBRv3 congestion control and earliest-departure-time pacing for
Vesper Warp (design §35). The crate documentation (`src/lib.rs`) describes
the API and what the crate does and does not claim.

**Port freeze (2026-11-17):** the Rust implementation is being ported to
Vesper in port wave P1 lane 10 ([#907](https://github.com/robot-head/vesper/issues/907)).
From then on the Rust code takes bug fixes only. The Vesper source is the
package in `vesper/`; the `vesper-port` feature builds the crate from it
through the generated binding `port/binding.rs`.
