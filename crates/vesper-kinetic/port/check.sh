#!/bin/sh
# Port wave P3b lane 8 (#1070), sourced by scripts/test-port-harness.sh
# (which defines `divergent`): Kinetic's tests pass against the port-backed
# package and the Rust seed copy, and each divergent edit (EDITS in
# crates/vesper-kinetic/build.rs, applied one at a time) fails a test of its
# module while another test of the same binary still passes.
set -eu
cargo test --locked -p vesper-kinetic -p vesper-kinetic-seed
while read -r index module file failing passing; do
    export VESPER_KINETIC_DIVERGENT="$index"
    divergent "Kinetic port ($module)" "$failing" "$passing" \
        -p vesper-kinetic --features vesper-port-divergent --test "$file" </dev/null
done <<'CHECKS'
0 morton morton out_of_range_inputs_are_typed_errors every_axis_value_matches_the_bitwise_specification
1 fastmath fastmath output_sweep_hash_is_identical_on_every_architecture annex_f_special_values_match_the_correctly_rounded_path
2 coloring coloring colouring_is_greedy_lowest_free_in_body_order chain_uses_two_colours_and_islands_are_disjoint
3 lbvh lbvh overlapping_pairs_equal_the_brute_force_set morton64_round_trips_and_orders_by_interleaved_bits
4 sap sap sortable_keys_preserve_float_order malformed_inputs_are_typed_errors
CHECKS
unset VESPER_KINETIC_DIVERGENT
