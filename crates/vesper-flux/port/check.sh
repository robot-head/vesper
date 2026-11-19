#!/bin/sh
# Port wave P3b lane 7 (#1069), sourced by scripts/test-port-harness.sh
# (which defines `divergent`): every vesper-flux test file passes (the Z-set
# kernels and the frontier are the Vesper port; tests/kafka.rs skips without
# a broker), and each divergent kernel (DIVERGENT in crates/vesper-flux/build.rs,
# applied one at a time) fails the differential test of its kernel while
# another test of the same binary still passes.
set -eu
cargo test --locked -p vesper-flux
while read -r kernel failing passing; do
    export VESPER_FLUX_DIVERGE="$kernel"
    divergent "Flux port ($kernel)" "$failing" "$passing" \
        -p vesper-flux --features vesper-port-divergent --test kernels </dev/null
done <<'CHECKS'
union consolidation_and_sums_match_the_rust_build frontier_operations_match_the_rust_build
filter circuit_ticks_and_batches_match_the_rust_build consolidation_and_sums_match_the_rust_build
consolidate consolidation_and_sums_match_the_rust_build frontier_operations_match_the_rust_build
join circuit_ticks_and_batches_match_the_rust_build consolidation_and_sums_match_the_rust_build
frontier frontier_operations_match_the_rust_build circuit_ticks_and_batches_match_the_rust_build
CHECKS
unset VESPER_FLUX_DIVERGE
