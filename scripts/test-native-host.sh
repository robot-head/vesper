#!/bin/sh
# Build every executable example for the host target with `vesper build` and
# check its exit status. On Apple silicon this exercises the AArch64 native
# ABI on real hardware; on x86_64 Linux it covers the host backend end to end.
set -eu
artifacts=$(mktemp -d)
trap 'rm -rf "$artifacts"' EXIT HUP INT TERM
cargo build -p vesper --locked
for case in fibonacci:55 transaction:42 projections:42 counter:3 fail:42 closures:42 resumptions:42 checked_math:42; do
    source=${case%:*}
    expected=${case#*:}
    target/debug/vesper build "examples/$source.ves" -o "$artifacts/$source"
    actual=0
    "$artifacts/$source" || actual=$?
    if [ "$actual" != "$expected" ]; then
        printf '%s: expected %s, got %s\n' "$source" "$expected" "$actual" >&2
        exit 1
    fi
    printf '%s: host exit %s\n' "$source" "$actual"
done
