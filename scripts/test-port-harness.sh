#!/bin/sh
# Port harness (port wave P0 lane 6, #878; build modes and the seed-copy rule
# from port wave P1 lane 2, #899) under Cargo, for the macOS and Windows
# checks; Linux runs the same checks as Bazel targets (BUILD.bazel of each
# crate below). Every port is compiled by one prebuilt `vesper` executable,
# VESPER_PORT_COMPILER (CI builds it first; this script builds it when unset).
#
# - The pilot's API tests must pass against its Rust build and its Vesper
#   build, and fail against a deliberately divergent Vesper build.
# - vesper-port-build links a multi-file Loom package with a path dependency,
#   and its divergent build must fail the tests.
# - vesper-beacon (port-backed) and vesper-beacon-seed (the compiler's Rust
#   copy) both pass Beacon's tests; vesper-beacon-arrow links the port; the
#   closure guard (vesper-port-harness tests/closure_guard.rs) checks that no
#   port-backed package is in the compiler's dependency closure.
#
# Ports append their own lines at the end.
set -eu
cd "$(dirname "$0")/.."
if [ -z "${VESPER_PORT_COMPILER:-}" ]; then
    cargo build --locked --bin vesper
    exe=vesper
    case "$(uname -s)" in MINGW* | MSYS* | CYGWIN* | Windows_NT) exe=vesper.exe ;; esac
    VESPER_PORT_COMPILER="${CARGO_TARGET_DIR:-$PWD/target}/debug/$exe"
fi
export VESPER_PORT_COMPILER
echo "compiling ports with $VESPER_PORT_COMPILER"

# divergent <description> <expected failing test> <expected passing test> <cargo test args...>
divergent() {
    what=$1 failing=$2 passing=$3
    shift 3
    log=$(mktemp)
    if cargo test --locked "$@" -- --test-threads=1 >"$log" 2>&1; then
        cat "$log"
        rm -f "$log"
        echo "the divergent $what passed its tests" >&2
        exit 1
    fi
    if ! grep -q "test $failing ... FAILED" "$log" || ! grep -q "test $passing ... ok" "$log"; then
        cat "$log"
        rm -f "$log"
        echo "the divergent $what failed for an unexpected reason" >&2
        exit 1
    fi
    rm -f "$log"
    echo "divergent $what detected ($failing failed, $passing agreed)"
}

cargo test --locked -p vesper-port-abi -p vesper-port-harness
cargo test --locked -p vesper-port-pilot
cargo test --locked -p vesper-port-pilot --features vesper-port
divergent "Vesper port of the pilot" bytes_and_booleans gcd_matches_euclid \
    -p vesper-port-pilot --features vesper-port-divergent --test api
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
# The kit (export ABI v2, #898): one failing test per form, and the leak
# check fails on the build that omits one Vesper-side free.
if cargo test --locked -p vesper-port-pilot --features vesper-port-divergent --test kit -- --test-threads=1 >"$log" 2>&1; then
    cat "$log"
    echo "the divergent kit passed its API tests" >&2
    exit 1
fi
for t in sequence_results slice_parameters strings owned_handles linear_handles recursive_enum_handles generic_instances vsys_imports; do
    if ! grep -q "test $t ... FAILED" "$log"; then
        cat "$log"
        echo "the divergent kit did not fail $t" >&2
        exit 1
    fi
done
grep -q 'test pattern_sink_closure ... ok' "$log" || { cat "$log"; echo "the divergent kit failed an unchanged test" >&2; exit 1; }
if cargo test --locked -p vesper-port-pilot --features vesper-port-divergent --test kit_memory >"$log" 2>&1; then
    cat "$log"
    echo "the kit leak check passed a build that leaks" >&2
    exit 1
fi
grep -qF 'leaked over 10^5 cycles' "$log" || { cat "$log"; echo "the kit leak check failed for another reason" >&2; exit 1; }
echo "divergent kit detected (one failure per form, and the leak)"
cargo test --locked -p vesper-port-build
divergent "package port" root_package_modules_are_linked dependency_modules_are_linked \
    -p vesper-port-build --features divergent --test package
cargo test --locked -p vesper-beacon -p vesper-beacon-seed
cargo test --locked -p vesper-beacon-arrow --test beacon_build
# Port P1 lane 10 (#907): vesper-warp-bbr, both builds, and its divergent
# build fails exactly one unit test per ported module.
cargo test --locked -p vesper-warp-bbr
cargo test --locked -p vesper-warp-bbr --features vesper-port
if cargo test --locked -p vesper-warp-bbr --features vesper-port-divergent --lib -- --test-threads=1 >"$log" 2>&1; then
    cat "$log"
    echo "the divergent warp-bbr port passed its unit tests" >&2
    exit 1
fi
for t in pacing::tests::pacing_delay_is_ceiling_and_defined_at_zero filter::tests::cycle_max_keeps_two_cycles bbr::tests::persistent_congestion_collapses_cwnd_then_floors_at_min_pipe; do
    grep -q "test $t ... FAILED" "$log" || { cat "$log"; echo "the divergent warp-bbr port did not fail $t" >&2; exit 1; }
done
grep -q 'test bbr::tests::inflight_at_loss_solves_for_the_threshold_crossing ... ok' "$log" || { cat "$log"; echo "the divergent warp-bbr port failed an unchanged test" >&2; exit 1; }
echo "divergent warp-bbr port detected (one failure per ported module)"
