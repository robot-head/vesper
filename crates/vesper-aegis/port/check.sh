#!/bin/sh
# vesper-aegis port checks under Cargo (port wave P3a lane 8, #1043; SH6
# lane 8, #1403), run by scripts/test-port-harness.sh in the Linux "Vesper
# ports" shard; Bazel builds the same divergent objects
# (crates/vesper-aegis/BUILD.bazel). Every test must pass against the port
# (the only build since #1403; tests/port_differential.rs compares it with
# the last Rust build's recorded results), and each divergent build (one
# edit per ported module, DIVERGENT in build.rs, picked by
# VESPER_AEGIS_DIVERGE) must fail the test that guards its module while an
# unrelated one passes.
set -eu
cd "$(dirname "$0")/../../.."
cargo test --locked -p vesper-aegis
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
# check <module> <test file> <expected failing test> <expected passing test>
check() {
    if VESPER_AEGIS_DIVERGE=$1 cargo test --locked -p vesper-aegis --features vesper-port-divergent --test "$2" -- --test-threads=1 >"$log" 2>&1; then
        cat "$log"
        echo "the divergent aegis port ($1) passed tests/$2.rs" >&2
        exit 1
    fi
    if ! grep -q "test $3 ... FAILED" "$log" || ! grep -q "test $4 ... ok" "$log"; then
        cat "$log"
        echo "the divergent aegis port ($1) failed for an unexpected reason" >&2
        exit 1
    fi
    echo "divergent aegis port ($1) detected ($3 failed, $4 agreed)"
}
check reader parser_fuzz tdx_parser_never_panics_and_round_trips claims_wire_decoder_never_panics
check snp vendor_samples_rest snp_report_versions_and_reserved_fields real_quote_v5_td15_ex_verifies_end_to_end
# The vendor's TD 1.5 "ex" quote catches a wrong body size.
check tdx vendor_samples_rest real_quote_v5_td15_ex_verifies_end_to_end snp_report_versions_and_reserved_fields
check sgx sgx_quote_v3 real_sgx_quote_v3_verifies_end_to_end sgx_quote_mutations_never_panic
check wire port_differential claims_wire_agrees snp_reports_agree
check policy port_differential policy_decisions_agree snp_reports_agree
check time port_differential timestamps_agree snp_reports_agree
