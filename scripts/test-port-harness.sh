#!/bin/sh
# Port harness (port wave P0 lane 6, #878; build modes and the seed-copy rule
# from port wave P1 lane 2, #899) under Cargo. Every port is compiled by one
# prebuilt `vesper` executable, VESPER_PORT_COMPILER (CI builds it first;
# this script builds it when unset).
#
# Where it runs (#1010, docs/ci-platforms.md): the Linux job of the
# Platforms workflow runs all of it. macOS and Windows run only the export ABI tier,
# the part that depends on the host's object format and calling convention.
# Bazel on Linux also builds every port and runs most of the same tests
# (BUILD.bazel of each crate).
#
# - The pilot's API tests must pass against its Rust build and its Vesper
#   build, and fail against a deliberately divergent Vesper build.
# - vesper-port-build links a multi-file Loom package with a path dependency,
#   and its divergent build must fail the tests.
# - vesper-beacon (Rust by default, its Vesper port under `vesper-port`)
#   and vesper-beacon-seed (the compiler's Rust copy) pass Beacon's tests;
#   vesper-beacon-arrow passes against both Beacon builds; the
#   closure guard (vesper-port-harness tests/closure_guard.rs) checks that no
#   port-backed package is in the compiler's dependency closure.
#
# Ports append their own lines at the end (after the ABI tier, so they run
# on Linux only), guarded by the crate's directory so a PR that changes only
# other ports skips them: `skip_port vesper-x || "$VESPER_ASPECT_BIN" crate x check`, or
# `if ported vesper-x; then ... fi` for several lines (#1103).
#
# Shards (#1019, #1100): the Linux job runs the ports in seven parallel
# shards, VESPER_PORT_HARNESS_SHARD=1 to 7 (unset runs all of them), each a
# run of consecutive blocks below: 1 the ABI tier through Ignis, 2 Quarry
# through pact, 3 semver through Crucible, 4 serde, warp-umem and aether, 5 aegis through bench,
# 6 eBPF and silicon, 7 prism-h3 and every line new ports append. With a
# debug compiler, the jobs took 21, 17, 19, 21, 24, 30 and 6 minutes on
# #1101 (November 2026); the shared optimized compiler (#1103) cuts most of
# that, since compiling Vesper sources is nearly all of it. When a shard
# passes about 35 minutes, move a shard boundary (or add a shard here and in
# the `ports` matrix of .github/workflows/platforms.yml).
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
shard=${VESPER_PORT_HARNESS_SHARD:-}
shards=7
case "$shard" in "" | [1-7]) ;; *) echo "VESPER_PORT_HARNESS_SHARD must be 1 to $shards" >&2; exit 1 ;; esac
[ "${VESPER_PORT_HARNESS_SCOPE:-all}" = abi ] && [ -n "$shard" ] && { echo "VESPER_PORT_HARNESS_SCOPE=abi takes no shard" >&2; exit 1; }
# part <n>: whether this run includes shard n.
part() { [ -z "$shard" ] || [ "$shard" = "$1" ]; }
# ported <crate dir>...: whether this run tests any of those crates.
# VESPER_PORT_HARNESS_CRATES (from scripts/ci/port_harness_selection.py on
# PRs) lists the crates/<dir> names to test; unset or "all" tests every port.
port_crates=" ${VESPER_PORT_HARNESS_CRATES:-all} "
ported() {
    [ "$port_crates" = " all " ] && return 0
    for c in "$@"; do case "$port_crates" in *" $c "*) return 0 ;; esac; done
    return 1
}
# skip_port <crate dir>: the inverse, for one-line blocks
# (`skip_port vesper-x || "$VESPER_ASPECT_BIN" crate x check`), which keeps a
# skipped last line from failing the script under `set -e`.
skip_port() { ! ported "$@"; }
[ "$port_crates" = " all " ] || echo "testing only the affected ports:$port_crates"
[ -n "$shard" ] && echo "port harness shard $shard of $shards"

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

log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
# Shard 1: the export ABI tier and the ports through Ignis.
if part 1; then
"${VESPER_ASPECT_BIN:-aspect}" ci test-port-abi
# End of the export ABI tier: the ports below are platform-neutral Rust
# tests over objects the ABI checks above already cover.
if [ "${VESPER_PORT_HARNESS_SCOPE:-all}" = abi ]; then
    echo "export ABI tier done (VESPER_PORT_HARNESS_SCOPE=abi); ports run on Linux"
    exit 0
fi
if ported vesper-beacon vesper-beacon-arrow; then
cargo test --locked -p vesper-beacon -p vesper-beacon-seed
cargo test --locked -p vesper-beacon-arrow --test beacon_build
# Port wave P1 lane 4 (#901): Beacon against its Vesper port, the Arrow
# adapter against the port, and one divergent edit per ported module.
cargo test --locked -p vesper-beacon --features vesper-port
cargo test --locked -p vesper-beacon-arrow --features vesper-port
divergent "Beacon port (histogram.rs)" all_ranges_partition_u64_and_bound_error footprint_is_fixed \
    -p vesper-beacon --features vesper-port-divergent --test histogram
divergent "Beacon port (exponential.rs)" starts_at_the_finest_scale_and_downscales_to_fit \
    bucket_index_is_exact_at_small_scales -p vesper-beacon --features vesper-port-divergent --test exponential
divergent "Beacon port (traceparent.rs)" lengths_separators_lowercase_version_and_nonzero_are_strict \
    drain_wraparound_partial_zero_and_fifo_ownership -p vesper-beacon --features vesper-port-divergent --test behavior
divergent "Beacon port (spans.rs)" ids_never_wrap_or_repeat_and_imported_self_parent_is_skipped \
    drain_wraparound_partial_zero_and_fifo_ownership -p vesper-beacon --features vesper-port-divergent --test behavior
fi
# Port wave P1 lane 14 (#911): vesper-prism against its Vesper port.
skip_port vesper-prism || "${VESPER_ASPECT_BIN:-aspect}" crate prism test-port
# vesper-aura (port wave P1 lane 13, #910; the port is its only build since
# #979): the crate's tests pass, and one divergent edit per ported module
# fails that module's test.
if ported vesper-aura; then
cargo test --locked -p vesper-aura
divergent "Aura port (view.rs)" budgets_fail_before_backend_and_accepted_tree_remains_unmountable \
    local_thread_claim_does_not_require_or_assert_os_main_thread \
    -p vesper-aura --features vesper-port-divergent --test validation_close
divergent "Aura port (plan.rs)" keyed_insert_remove_reorder_and_stale_handle_rejection \
    changed_handler_disconnects_once_before_reconnect \
    -p vesper-aura --features vesper-port-divergent --test reconcile
divergent "Aura port (session.rs)" stale_and_foreign_plans_retain_input_and_do_not_call_backend \
    changed_handler_disconnects_once_before_reconnect \
    -p vesper-aura --features vesper-port-divergent --test reconcile
divergent "Aura port (recording.rs)" initial_mount_rejection_never_publishes_widgets_slots_or_registrations \
    changed_handler_disconnects_once_before_reconnect \
    -p vesper-aura --features vesper-port-divergent --test reconcile
divergent "Aura port (sim_aura.rs)" sim_aura_focus_events_atomic_wcag_gate_and_close_decision \
    wcag_audit_detects_missing_labels_and_hidden_subscribed_controls \
    -p vesper-aura --features vesper-port-divergent --test sim_aura
fi
# Port P1 lane 10 (#907): vesper-warp-bbr, both builds, and its divergent
# build fails exactly one unit test per ported module.
if ported vesper-warp-bbr; then
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
fi
skip_port vesper-warp-tcp || "${VESPER_ASPECT_BIN:-aspect}" crate warp-tcp test-port
# Port wave P1 lane 3 (#900): vesper-warp-wheel.
skip_port vesper-warp-wheel || "${VESPER_ASPECT_BIN:-aspect}" crate warp-wheel test-port
# Rust exit SH6 lane 4 (#1399): vesper-warp-shard.
skip_port vesper-warp-shard || "${VESPER_ASPECT_BIN:-aspect}" crate warp-shard test-port
skip_port vesper-ignis || "${VESPER_ASPECT_BIN:-aspect}" crate ignis check  # port P1 lane 12 (#909): vesper-ignis, both builds and the divergence check
fi
# Shard 2: Quarry through pact.
if part 2; then
# Quarry planner port (port wave P1 lane 7, #904): the port's tests, and one
# failing test per ported module on the divergent build (DIVERGENT in
# crates/vesper-quarry/build.rs).
if ported vesper-quarry; then
cargo test --locked -p vesper-quarry --features vesper-port --test sql --test planner_spec --test vector --test vector_join --test port_memory
for t in "sql outer_join_null_extension_and_filter_placement" "sql outer_join_pushdown_differential_for_many_predicates_and_null_inputs" "sql sql_null_three_valued_logic_exhaustive" "sql ddl_atomic_deterministic_and_stale_revision_or_content_rejected" "planner_spec front_end_transcript_matches_golden" "planner_spec planner_chooses_the_golden_access_paths"; do set -- $t; divergent "Quarry port ($2)" "$2" "$([ "$1" = sql ] && echo real_where_aliases_literals_and_typed_bindings || echo golden_results_equal_filtered_index_lookups)" -p vesper-quarry --features vesper-port-divergent --test "$1"; done
fi
# vesper-loom-calculus (port wave P1 lane 16, #913): the port-backed package
# and the compiler's Rust seed copy pass the same tests (including the
# differential corpus digest), and one divergent edit per ported module
# (DIVERGENT in crates/vesper-loom-calculus/build.rs) fails a test.
if ported vesper-loom-calculus; then
cargo test --locked -p vesper-loom-calculus -p vesper-loom-calculus-seed
if cargo test --locked -p vesper-loom-calculus --features vesper-port-divergent --no-fail-fast --tests -- --test-threads=1 >"$log" 2>&1; then
    cat "$log"
    echo "the divergent loom-calculus port passed its tests" >&2
    exit 1
fi
for t in manifest_corpus_matches_the_rust_build semver_matrix_matches_the_rust_build authority_reaches_ancestors_that_sort_before_their_dependencies unsafe_ffi_policy_relaxation_is_rejected_for_existing_and_new_packages manifest_cases_match_the_toml_crate semver_cases_match_the_semver_crate; do
    grep -q "test $t ... FAILED" "$log" || { cat "$log"; echo "the divergent loom-calculus port did not fail $t" >&2; exit 1; }
done
grep -q 'test documented_manifest_is_declarative_and_secure_by_default ... ok' "$log" || { cat "$log"; echo "the divergent loom-calculus port failed an unchanged test" >&2; exit 1; }
echo "divergent loom-calculus port detected (one failure per ported module)"
fi
skip_port vesper-loom-solver || "${VESPER_ASPECT_BIN:-aspect}" crate loom-solver test-port  # port P1 lane 17 (#914): vesper-loom-solver, both builds and the divergence check
skip_port vesper-pact || "${VESPER_ASPECT_BIN:-aspect}" crate pact test-port  # port P3a lane 3 (#1038): vesper-pact, all builds and the divergence check
fi
# Shard 3: semver through Crucible.
if part 3; then
# Port wave P1 lane 15 (#912): vesper-semver, both builds and the divergence check.
skip_port vesper-semver || "${VESPER_ASPECT_BIN:-aspect}" crate semver test-port
# Port wave P1 lane 8 (#905): vesper-tensor and its Rust seed copy, and the
# divergence check (one failing test per ported module).
skip_port vesper-tensor || "${VESPER_ASPECT_BIN:-aspect}" crate tensor test-port
# Port wave P1 lane 6 (#903): vesper-alloc (the Vesper port of the region
# bookkeeping) and vesper-alloc-seed (the Rust copy every dependent links)
# both pass the crate's tests; one divergent edit per ported area fails that
# area's test (EDITS in crates/vesper-alloc/build.rs).
if ported vesper-alloc; then
cargo test --locked -p vesper-alloc -p vesper-alloc-seed
divergent "Strata port (placement)" rewind_reuses_bytes_and_keeps_spare_chunks size_arithmetic_is_checked \
    -p vesper-alloc --features vesper-port-divergent --test region
divergent "Strata port (watermarks)" rewinding_an_outer_watermark_invalidates_inner_ones size_arithmetic_is_checked \
    -p vesper-alloc --features vesper-port-divergent --test region
divergent "Strata port (vectors)" linear_elements_block_discard_until_drained capacity_arithmetic_is_checked \
    -p vesper-alloc --features vesper-port-divergent --test collections
fi
# Grammar port (port wave P1 lane 9, #906): both builds pass the crate's
# tests (including the 10^5-input no-panic run), and the divergent build
# (one edit per ported module, DIVERGENT in crates/vesper-grammar/build.rs)
# fails a test of each module while an unrelated test still passes.
if ported vesper-grammar; then
cargo test --locked -p vesper-grammar -p vesper-grammar-seed
while read -r module file failing passing; do
    divergent "Grammar port ($module)" "$failing" "$passing" \
        -p vesper-grammar --features vesper-port-divergent --test "$file" </dev/null
done <<'CHECKS'
framing framing known_uleb_vectors_exact_prefix_and_transactional_output u24_big_endian_frames_roundtrip_and_preserve_remainder
arrow arrow checked_nullable_aggregation_matches_i128_prefix_oracle alignment_checks_distinguish_relative_offsets_from_actual_host_addresses
capnp framing capnp_stream_segments_are_borrowed_and_framing_is_exact u24_boundaries_fail_transactionally
capnp_packed capnp_packed official_encoding_vectors_and_raw_runs_with_zeroes every_short_output_preserves_the_whole_buffer
grammar grammar aliased_dag_expansion_cannot_bypass_capture_or_work_budgets ambiguity_duplicate_branch_ids_bad_references_and_recursion_fail
codec codec independently_constructed_numbers_have_known_canonical_encodings codec_limit_configuration_is_checked_before_all_operations
protobuf protobuf signed_twos_complement_varints_are_canonical_in_singular_and_packed_fields singular_presence_and_defaults_are_actual_distinct_logical_values
der der typed_set_of_sorts_complete_encodings_retains_duplicates_and_rejects_untyped_set utf8_string_is_strict_borrowed_and_transactional
ipc ipc independent_arrow_values_and_versions hostile_schema_and_union_discriminants
CHECKS
fi
skip_port vesper-simpulse || "${VESPER_ASPECT_BIN:-aspect}" crate simpulse check  # port P1 lane 5 (#902): vesper-simpulse, both builds and the divergence check
skip_port vesper-crucible || "${VESPER_ASPECT_BIN:-aspect}" crate crucible check  # port P1 lane 18 (#915): vesper-crucible, both builds and the divergence checks
fi
# Shard 4: serde, warp-umem and aether.
if part 4; then
skip_port vesper-serde || "${VESPER_ASPECT_BIN:-aspect}" crate serde check  # port P1 lane 19 (#916): vesper-serde, both builds and one divergence check per ported module
skip_port vesper-warp-umem || "${VESPER_ASPECT_BIN:-aspect}" crate warp-umem test-port  # self-hosting SH6 lane 14 (#1409): vesper-warp-umem, both builds, its consumers on the port and one divergence check per ported module
skip_port vesper-aether || "${VESPER_ASPECT_BIN:-aspect}" crate aether test-port  # self-hosting SH6 lane 14 (#1409): vesper-aether, both builds and one divergence check per ported module
fi
# Shard 5: aegis through bench.
if part 5; then
skip_port vesper-aegis || "${VESPER_ASPECT_BIN:-aspect}" crate aegis check  # port P3a lane 8 (#1043): vesper-aegis, the port (the only build, SH6 lane 8 #1403) and one divergence check per ported module
skip_port vesper-loom-audit || "${VESPER_ASPECT_BIN:-aspect}" crate loom-audit test-port  # port wave P3a lane 4 (#1039): vesper-loom-audit, both builds and the divergence check
skip_port vesper-bench || "${VESPER_ASPECT_BIN:-aspect}" crate bench check  # port P3a lane 5 (#1040): vesper-bench, both builds and one divergence check per ported module
fi
# Shard 6: eBPF and silicon.
if part 6; then
skip_port vesper-ebpf || "${VESPER_ASPECT_BIN:-aspect}" crate ebpf check  # port P3a lane 6 (#1041): vesper-ebpf, both builds and one divergence check per ported module
skip_port vesper-silicon || "${VESPER_ASPECT_BIN:-aspect}" crate silicon check  # port P3a lane 7 (#1042): vesper-silicon, both builds and one divergence check per ported module
fi
# Shard 7: every later port. New ports append their lines below.
part 7 || exit 0
skip_port vesper-warp || "${VESPER_ASPECT_BIN:-aspect}" crate warp check  # port P3b lane 4 (#1066): vesper-warp, both builds and one divergence check per ported module
skip_port vesper-prism-h3 || "${VESPER_ASPECT_BIN:-aspect}" crate prism-h3 check  # port P3b lane 5 (#1067), Vesper-only since SH6 lane 5 (#1400): vesper-prism-h3 tests and one divergence check per ported module
skip_port vesper-chorus || "${VESPER_ASPECT_BIN:-aspect}" crate chorus test-port  # port P3b lane 6 (#1068): vesper-chorus (the Vesper library is its only build) and one divergence check per ported module
skip_port vesper-resonance || "${VESPER_ASPECT_BIN:-aspect}" crate resonance test-port  # port P3b lane 9 (#1071): vesper-resonance, both builds, one divergence check per ported module and the realtime verifier proof
skip_port vesper-kinetic || "${VESPER_ASPECT_BIN:-aspect}" crate kinetic check  # port P3b lane 8 (#1070): vesper-kinetic, both builds and one divergence check per ported module
skip_port vesper-flux || "${VESPER_ASPECT_BIN:-aspect}" crate flux check  # port P3b lane 7 (#1069): vesper-flux on its Vesper kernels and one divergence check per ported kernel
skip_port vesper-span || "${VESPER_ASPECT_BIN:-aspect}" crate span test-port  # self-hosting SH0 lane 6 (#1128): vesper-span, both builds, the differential test and one divergence check per ported area
skip_port vesper-lean-syntax || "${VESPER_ASPECT_BIN:-aspect}" crate lean-syntax test-port  # self-hosting SH0 lane 6 (#1128): vesper-lean-syntax, both builds, the differential test and one divergence check per ported area
skip_port vesper-diagnostics || "${VESPER_ASPECT_BIN:-aspect}" crate diagnostics test-port  # self-hosting SH1 lane 1 (#1128): vesper-diagnostics, both builds, the differential test and one divergence check per ported area
skip_port vesper-fmt || "${VESPER_ASPECT_BIN:-aspect}" crate fmt test-port  # self-hosting SH1 lane 11 (#1154): vesper-fmt, all three builds, the differential test and one divergence check per ported area
skip_port vesper-highlight || "${VESPER_ASPECT_BIN:-aspect}" crate highlight test-port  # self-hosting SH1 lane 11 (#1154): vesper-highlight tier 0, all three builds, the differential test and one divergence check per ported area
skip_port vesper-forge || "${VESPER_ASPECT_BIN:-aspect}" crate forge test-port  # self-hosting SH1 lane 14 (#1157): vesper-forge hygiene core, both builds, the differential test and one divergence check per ported area
skip_port vesper-lint || "${VESPER_ASPECT_BIN:-aspect}" crate lint test-port  # self-hosting SH1 lane 13 (#1156): vesper-lint, all three builds, the differential test and one divergence check per ported area
skip_port vesper-sim-session || "${VESPER_ASPECT_BIN:-aspect}" crate sim-session test-port  # self-hosting SH5 lane 19 (#1342): vesper-sim-session, both builds, the transcript differential and the divergence check
skip_port vesper-fusion || "${VESPER_ASPECT_BIN:-aspect}" crate fusion test-port  # Rust-exit SH6 lane 18 (#1413): vesper-fusion on its Vesper engine (the only build) and one divergence check
skip_port vesper-warp-crypto || "${VESPER_ASPECT_BIN:-aspect}" crate warp-crypto check  # Rust-exit SH6 lane 17 (#1412): vesper-warp-crypto on its Vesper port (the only build), the differential against the Rust reference and one divergence check per ported area
skip_port vesper-loom-trust || "${VESPER_ASPECT_BIN:-aspect}" crate loom-trust check  # Rust-exit SH6 lane 17 (#1412): vesper-loom-trust on its Vesper port (its users outside the compiler's build; the tools link the Rust seed), the differential against the Rust reference and one divergence check per ported area
skip_port vesper-beacon-arrow || "${VESPER_ASPECT_BIN:-aspect}" crate beacon-arrow test-port  # Rust-exit SH6 lane 18 (#1413): vesper-beacon-arrow span stream on its Vesper writer (the only build) and one divergence check
skip_port vesper-loom-cas || "${VESPER_ASPECT_BIN:-aspect}" crate loom-cas test-port  # Rust-exit SH6 lane 16 (#1411): vesper-loom-cas (port-backed) and vesper-loom-cas-seed, both differentials, and one divergence check per module
skip_port vesper-vault || "${VESPER_ASPECT_BIN:-aspect}" crate vault check  # Rust-exit SH6 lane 17 (#1412): vesper-vault on its Vesper port (external users like vesper-aegis; the compiler and vesper-test link the Rust seed), the differential against the Rust reference, and one divergence check per ported area
