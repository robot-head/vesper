#!/bin/sh
# Test milestone crates that still carry their own temporary [workspace]
# (docs/parallel-work.md, rule 3). Remove a crate from this list in the same
# change that registers it in the root workspace, where `cargo test
# --workspace` picks it up on every CI lane.
set -eu
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target/standalone}"
for crate in vesper-fiber-abi vesper-ignis vesper-loom-cas vesper-loom-sandbox vesper-pact; do
    manifest="crates/$crate/Cargo.toml"
    # Crates with a committed nested lockfile are tested against it.
    locked=""
    if [ -f "crates/$crate/Cargo.lock" ]; then locked="--locked"; fi
    printf '== %s\n' "$crate"
    cargo fmt --manifest-path "$manifest" -- --check
    cargo clippy --manifest-path "$manifest" $locked --all-targets -- -D warnings
    cargo test --manifest-path "$manifest" $locked
done
