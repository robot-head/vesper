#!/bin/sh
# Run native AArch64 code under QEMU without a target libc or sysroot.
set -eu
cd "$(dirname "$0")/.."
. scripts/bazel-lib.sh
assembler=${AARCH64_AS:-aarch64-linux-gnu-as}
linker=${AARCH64_LD:-aarch64-linux-gnu-ld}
emulator=${QEMU_AARCH64:-qemu-aarch64}
artifacts=$(mktemp -d)
trap 'rm -rf "$artifacts"' EXIT HUP INT TERM
vesper="$(bazel_executable //:bin/vesper)"
cat > "$artifacts/start.s" <<'ASM'
.text
.global _start
_start:
    bl main
    mov x8, #93
    svc #0
ASM
"$assembler" "$artifacts/start.s" -o "$artifacts/start.o"
for case in fibonacci:55 transaction:42 projections:42 counter:3 fail:42 closures:42 resumptions:42 checked_math:42; do
    source=${case%:*}
    expected=${case#*:}
    "$vesper" emit "examples/$source.ves" --emit object --target aarch64-unknown-linux-gnu -o "$artifacts/$source.o"
    "$linker" "$artifacts/start.o" "$artifacts/$source.o" -o "$artifacts/$source"
    actual=0
    "$emulator" "$artifacts/$source" || actual=$?
    if [ "$actual" != "$expected" ]; then
        printf '%s: expected %s, got %s\n' "$source" "$expected" "$actual" >&2
        exit 1
    fi
    printf '%s: AArch64 exit %s\n' "$source" "$actual"
done
