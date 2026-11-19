#!/bin/sh
# Port P3a lane 6 (#1041), sourced by scripts/test-port-harness.sh (which
# defines `divergent`): vesper-ebpf's tests pass against its Rust build (the
# default, the same code as the seed copy vesper-ebpf-seed) and against the
# Vesper port, and each divergent edit (EDITS in crates/vesper-ebpf/build.rs,
# applied one at a time) fails a test of its module while another test of
# the same file still passes.
set -eu
cargo test --locked -p vesper-ebpf -p vesper-ebpf-seed
cargo test --locked -p vesper-ebpf --features vesper-port
# Each line is one divergent edit (its index into EDITS) and the test of its
# module that catches it. A build of the port takes minutes, so the edits
# come in groups that share one build: the edits of a group live in
# different modules, and every check of a group still shows its own test
# failing while another test of the same file passes.
for group in 0,1,4,7,9 2,5,8,10 3,6,11; do
    export VESPER_EBPF_DIVERGENT="$group"
    while read -r index module file failing passing; do
        case ",$group," in *",$index,"*) ;; *) continue ;; esac
        divergent "eBPF port ($module)" "$failing" "$passing" \
            -p vesper-ebpf --features vesper-port-divergent --test "$file" </dev/null
    done <<'CHECKS'
0 insn codec every_family_round_trips malformed_input_is_a_typed_error
1 scalar scalar_soundness alu_transfer_functions_contain_concrete_results branch_refinement_keeps_every_value_on_its_branch
2 interp scalar_soundness alu_transfer_functions_contain_concrete_results branch_refinement_keeps_every_value_on_its_branch
3 verifier scalar_soundness branch_refinement_keeps_every_value_on_its_branch alu_transfer_functions_contain_concrete_results
4 helpers port_surface helper_table_matches_the_kernel_protos map_definitions_are_validated_rule_by_rule
5 program port_surface map_definitions_are_validated_rule_by_rule helper_table_matches_the_kernel_protos
6 callgraph port_surface stack_bound_reports_every_outcome map_definitions_are_validated_rule_by_rule
7 wire mitigator wire_layouts_are_exact verified_within_the_stack_budget
8 explore verifier_rules corpus_verdicts every_rule_is_covered
9 btf btf_port port_matches_rust_on_mutated_btf port_matches_rust_on_every_kind_and_split_btf
10 core core_port port_matches_rust_on_random_relocations port_matches_rust_on_patch
11 btf_ext ext_port port_matches_rust_on_objects_and_mutations port_matches_rust_on_local_types_of_host_btf
CHECKS
done
unset VESPER_EBPF_DIVERGENT
