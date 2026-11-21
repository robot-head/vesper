#!/bin/sh
# The vesper-beacon-arrow port checks for Cargo (the Linux "Vesper ports" job
# runs this from scripts/test-port-harness.sh; BuildBuddy runs the Bazel
# targets in crates/vesper-beacon-arrow/BUILD.bazel). The span stream tests
# run on the port (the only build), and a build that shifts every u64 column
# up by one row (EDITS in build.rs) must fail the independent arrow2
# reader while the preflight test, which never reaches the writer, passes.
set -eu
cd "$(dirname "$0")/../../.."
cargo test --locked -p vesper-beacon-arrow --lib --test interoperability --test span_stream_differential
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
if cargo test --locked -p vesper-beacon-arrow --features vesper-port-divergent --no-fail-fast \
    --lib --test interoperability -- --test-threads=1 >"$log" 2>&1; then
    cat "$log"
    echo "the divergent vesper-beacon-arrow port passed its tests" >&2
    exit 1
fi
for want in "independent_arrow2_reader_compares_every_column_and_metadata ... FAILED" \
    "tests::hard_row_limit_precedes_attribute_walk_and_allocations ... ok"; do
    grep -q "test $want" "$log" || {
        cat "$log"
        echo "the divergent vesper-beacon-arrow port: expected \"$want\"" >&2
        exit 1
    }
done
echo "divergent vesper-beacon-arrow port detected (spans.ves)"
