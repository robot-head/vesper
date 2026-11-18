#!/bin/sh
# The Vesper port of vesper-semver (port wave P1 lane 15, #912) under Cargo,
# for the macOS and Windows checks; Linux runs the same checks as Bazel
# targets (crates/vesper-semver/BUILD.bazel). The crate's tests must pass
# against the port-backed package and the Rust seed copy, and the divergent
# port (one edit in each ported module, see build.rs) must fail that
# module's test while the others still pass.
set -eu
cd "$(dirname "$0")/../../.."
cargo test --locked -p vesper-semver -p vesper-semver-seed
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
# test file, the test the divergent build must fail, a test it must pass
for check in \
    "interface function_and_produced_value_modes_are_covariant identical_interface_is_patch_and_round_trips" \
    "release component_resets_are_allowed_only_when_precedence_increases invalid_and_overflowing_versions_return_typed_errors"; do
    set -- $check
    if cargo test --locked -p vesper-semver --features vesper-port-divergent --test "$1" -- --test-threads=1 >"$log" 2>&1; then
        cat "$log"
        echo "the divergent Vesper port passed tests/$1.rs" >&2
        exit 1
    fi
    if ! grep -q "test $2 \.\.\. FAILED" "$log" || ! grep -q "test $3 \.\.\. ok" "$log"; then
        cat "$log"
        echo "the divergent Vesper port failed tests/$1.rs for an unexpected reason" >&2
        exit 1
    fi
done
echo "divergent Vesper semver detected (one failing test per ported module, the rest agreed)"
