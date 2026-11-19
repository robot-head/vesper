#!/bin/sh
# Port wave P3a lane 7 (#1042), sourced by scripts/test-port-harness.sh
# (which defines `divergent`): Silicon's tests pass against the port-backed
# package and the Rust seed copy, and each divergent edit (EDITS in
# crates/vesper-silicon/build.rs, applied one at a time) fails a test of its
# module while another test of the same binary still passes.
set -eu
cargo test --locked -p vesper-silicon -p vesper-silicon-seed
while read -r index area file failing passing; do
    export VESPER_SILICON_DIVERGENT="$index"
    divergent "Silicon port ($area)" "$failing" "$passing" \
        -p vesper-silicon --features vesper-port-divergent --test "$file" </dev/null
done <<'CHECKS'
0 imsic parity imsic_files_match every_fixture_parses_identically
1 aplic parity aplic_domains_match imsic_files_match
2 plic parity interrupt_fabrics_match imsic_files_match
3 dtb dtb qemu_riscv_virt_memory_harts_and_plic malformed_headers_are_typed_errors
CHECKS
unset VESPER_SILICON_DIVERGENT
