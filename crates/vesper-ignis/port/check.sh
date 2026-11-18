#!/bin/sh
# vesper-ignis port checks under Cargo (port wave P1 lane 12, #909), run by
# scripts/test-port-harness.sh in the Linux Platforms job; Bazel runs the same
# checks as Bazel targets (crates/vesper-ignis/BUILD.bazel). Every test must
# pass against the Rust build and the Vesper build, and the divergent Vesper
# build (one edit per ported module, EDITS in build.rs) must fail the test
# that guards each module (tests/divergence.rs runs the binaries and names
# the tests).
set -eu
cd "$(dirname "$0")/../../.."
cargo test --locked -p vesper-ignis
cargo test --locked -p vesper-ignis --features vesper-port
# The path of the divergent build's test binary for tests/$1.rs.
divergent() {
    cargo test --locked --color never -p vesper-ignis --features vesper-port-divergent --no-run --test "$1" 2>&1 |
        sed -n "s/.*Executable tests[/\\\\]$1\.rs (\(.*\))$/\1/p"
}
VESPER_IGNIS_DIVERGENT_PLANNER=$(divergent planner)
VESPER_IGNIS_DIVERGENT_MULTI_BACKEND=$(divergent multi_backend)
VESPER_IGNIS_DIVERGENT_LIFECYCLE=$(divergent lifecycle)
for binary in "$VESPER_IGNIS_DIVERGENT_PLANNER" "$VESPER_IGNIS_DIVERGENT_MULTI_BACKEND" "$VESPER_IGNIS_DIVERGENT_LIFECYCLE"; do
    [ -n "$binary" ] || { echo "a divergent vesper-ignis test binary was not built" >&2; exit 1; }
done
export VESPER_IGNIS_DIVERGENT_PLANNER VESPER_IGNIS_DIVERGENT_MULTI_BACKEND VESPER_IGNIS_DIVERGENT_LIFECYCLE
cargo test --locked -p vesper-ignis --test divergence -- --nocapture
echo "divergent vesper-ignis port detected (one failing test per ported module)"
