# Kinetic fixed-axis Clippy repair

Claim: #433. Base: `28fbc3665f5ad46faf6003841e7352fd1575ee49`.

Strict Clippy reported `needless_range_loop` in two fixed three-axis loops:
`RigidWorld::kinetic_energy` in `rigid.rs` and `pair_portable` in `simd.rs`.
Both now consume the existing three-element array with
`into_iter().enumerate()`, naming each scalar `component`. Array iteration
retains axis order 0, 1, 2. The inertia condition, indexed companion arrays,
and write order remain unchanged.

The energy contribution remains
`0.5 * component * component / f64::from(s.inv_inertia[k])`, with the same
left-associated floating operations and accumulation order. The portable
normal remains `det::mul(component, inv_len)`. This changes no allocation,
layout, public API, numerical policy, lint allowance, or warning threshold.
Existing tests cover the numerical and allocation contracts; no tests that
repeat the two loops were added.

## Local validation

Host: `x86_64-unknown-linux-gnu`. Compiler:
`rustc 1.99.1 (58cf8f9c3 2026-10-13)`, LLVM 23.1.1.
Cargo: `1.99.1 (5f94df478 2026-08-27)`. The optional model differential used
Lean 4.24.0, commit `797c613eb9b6d4ec95db23e3e00af9ac6657f24b`.

Commands ran from the isolated checkout
`/workspace/vesper-worktrees/kinetic-clippy-fixed-axes`. All Cargo commands
used the following environment, under an exclusive shared-target lease:

```sh
export CARGO_TARGET_DIR=/workspace/vesper/target/parallel-next
export CARGO_BUILD_JOBS=1
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_PROFILE_TEST_DEBUG=0
export CARGO_INCREMENTAL=0
export VLB_CARGO_LOCK=/workspace/vesper-worktrees/kinetic-clippy-fixed-axes/Cargo.lock
cargo clippy -p vesper-kinetic --locked --all-targets -- -D warnings
env -u VESPER_LEAN cargo test -p vesper-kinetic --locked
VESPER_LEAN=/home/agent/.local/share/vesper-lean/bin/lean LEAN_NUM_THREADS=1 \
  cargo test -p vesper-kinetic --locked --test lean_model \
  lean_model_matches_rust_morton_and_colouring -- --exact --nocapture
rustfmt --edition 2024 --check crates/vesper-kinetic/src/rigid.rs crates/vesper-kinetic/src/simd.rs
git diff --check
```

Strict package Clippy passed in 17.39 s with no warnings. The package test
command reported 104 passed, zero failed, and three ignored timing benchmarks;
this includes the opt-in Lean differential's early return while `VESPER_LEAN`
was unset. Coverage includes unit, correctly rounded and fast math, zero
allocation with heap and arena storage, bitwise portable/SIMD agreement,
golden hashes across available backends and threads, rigid bodies, rollback,
and the Lean source audit. The package test build took 24.48 s.

After obtaining the shared Lean compiler lease, the targeted differential
command genuinely compiled the Lean model and checked Rust Morton/colouring
agreement: one passed, zero failed, one unrelated source-audit case filtered
out, in 2.74 s. Targeted formatting and whitespace checks passed. No validation
command failed. Shared Cargo and Lean compiler leases were released after
the commands completed.

Validation is local Linux coverage. Runtime SIMD tests exercise only backends
available on this host; no macOS, Windows, or AArch64 run is claimed.
The initial package invocation explicitly unset `VESPER_LEAN`; the separate
targeted invocation completed that optional coverage. No workspace-wide
acceptance claim is made.
