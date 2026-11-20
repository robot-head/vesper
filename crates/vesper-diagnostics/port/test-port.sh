#!/bin/sh
# The Vesper port of vesper-diagnostics (self-hosting SH1 lane 1, #1128)
# under Cargo, run by scripts/test-port-harness.sh; Bazel builds the same
# port (crates/vesper-diagnostics/BUILD.bazel). The crate's tests pass
# against the port-backed package and the Rust seed copy, the port matches
# the seed on generated reports (tests/differential.rs), and the divergent
# port, one area at a time (VESPER_DIAGNOSTICS_DIVERGE, EDITS in build.rs),
# fails a test that guards that area while a test it does not reach still
# passes.
set -eu
cd "$(dirname "$0")/../../.."
cargo test --locked -p vesper-diagnostics -p vesper-diagnostics-seed
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
while read -r area test failing passing; do
    if VESPER_DIAGNOSTICS_DIVERGE=$area cargo test --locked -p vesper-diagnostics \
        --features vesper-port-divergent --no-fail-fast --test "$test" \
        -- --test-threads=1 >"$log" 2>&1; then
        cat "$log"
        echo "the divergent diagnostics port ($area) passed its tests" >&2
        exit 1
    fi
    if ! grep -q "test $failing ... FAILED" "$log" ||
        ! grep -q "test $passing ... ok" "$log"; then
        cat "$log"
        echo "the divergent diagnostics port ($area) failed for an unexpected reason" >&2
        exit 1
    fi
    echo "divergent diagnostics port ($area) detected ($failing failed)"
done <<'CHECKS'
prepare render existing_index_must_fit_renderer_line_profile names_limits_and_empty_diagnostics
lsp render actual_utf8_bytes_become_utf16_ranges_and_all_actual_labels names_limits_and_empty_diagnostics
terminal differential generated_reports_render_identically the_builds_differ
CHECKS
