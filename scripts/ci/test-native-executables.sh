#!/bin/sh
# Exercise the native object emitter and the host linker on the actual OS.
set -eu
cd "$(dirname "$0")/../.."
: "${VESPER_NATIVE_BINARY:?set VESPER_NATIVE_BINARY}"
cases=$(sed -n 's/^for case in \(.*\); do$/\1/p' scripts/test-native-host.sh)
test -n "$cases"
artifacts=$(mktemp -d)
trap 'rm -rf "$artifacts"' EXIT HUP INT TERM
for case in $cases; do
    source=${case%:*}
    expected=${case#*:}
    output="$artifacts/$source${VESPER_NATIVE_SUFFIX:-}"
    "$VESPER_NATIVE_BINARY" build "examples/$source.ves" -o "$output"
    actual=0
    "$output" || actual=$?
    if [ "$actual" != "$expected" ]; then
        printf '%s: expected %s, got %s\n' "$source" "$expected" "$actual" >&2
        exit 1
    fi
    printf '%s: host exit %s\n' "$source" "$actual"
done
