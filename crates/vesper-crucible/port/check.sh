#!/bin/sh
# Port P1 lane 18 (#915), sourced by scripts/test-port-harness.sh (which
# defines `divergent`): Crucible's tests pass against the port-backed package
# and the Rust seed copy, and each divergent edit (EDITS in
# crates/vesper-crucible/build.rs, applied one at a time) fails the fixed-seed
# parity test of its area while another parity test still passes.
set -eu
cargo test --locked -p vesper-crucible -p vesper-crucible-seed
while read -r index area failing passing; do
    export VESPER_CRUCIBLE_DIVERGENT="$index"
    divergent "Crucible port ($area)" "$failing" "$passing" \
        -p vesper-crucible --features vesper-port-divergent --test parity </dev/null
done <<'CHECKS'
0 rng splitmix_outputs_match_the_rust_build tape_text_and_keys_match_the_rust_build
1 draws random_and_replayed_sources_match_the_rust_build tape_text_and_keys_match_the_rust_build
2 tape tape_text_and_keys_match_the_rust_build splitmix_outputs_match_the_rust_build
3 shrink shrink_sequence_matches_the_rust_build tape_text_and_keys_match_the_rust_build
4 fuzz fuzz_campaigns_match_the_rust_build tape_text_and_keys_match_the_rust_build
5 dpor explorations_match_the_rust_build tape_text_and_keys_match_the_rust_build
CHECKS
unset VESPER_CRUCIBLE_DIVERGENT
