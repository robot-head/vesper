#!/bin/sh
# Execute emitted AArch64 Baseline128 code without a target libc/sysroot.
set -eu
assembler=${AARCH64_AS:-aarch64-linux-gnu-as}
linker=${AARCH64_LD:-aarch64-linux-gnu-ld}
emulator=${QEMU_AARCH64:-qemu-aarch64}
artifacts=$(mktemp -d)
trap 'rm -rf "$artifacts"' EXIT HUP INT TERM
cargo run -p vesper-simd --locked --example emit_objects -- "$artifacts"
python3 - "$artifacts" <<'PY'
import pathlib
import struct
import sys

directory = pathlib.Path(sys.argv[1])
data = bytes((i * 131 + 7) % 256 for i in range(131120))
directory.joinpath("input.bin").write_bytes(data)
xors = [0]
counts = {delimiter: [0] for delimiter in (0, 7, 255)}
for byte in data:
    xors.append(xors[-1] ^ byte)
    for delimiter, prefix in counts.items():
        prefix.append(prefix[-1] + (byte == delimiter))
cases = bytearray()
for offset in range(32):
    for length in [*range(96), 65535, 65536, 65537, 131089]:
        for delimiter, prefix in counts.items():
            end = offset + length
            cases.extend(struct.pack("<5Q", offset, length, delimiter,
                                     prefix[end] - prefix[offset], xors[end] ^ xors[offset]))
directory.joinpath("cases.bin").write_bytes(cases)
PY
cat > "$artifacts/start.s" <<'ASM'
.text
.global _start
_start:
    adrp x19, cases
    add x19, x19, :lo12:cases
    adrp x20, input
    add x20, x20, :lo12:input
    mov x21, #9600
next_case:
    ldp x0, x1, [x19]
    add x0, x20, x0
    ldr x2, [x19, #16]
    adrp x3, xor_out
    add x3, x3, :lo12:xor_out
    bl vesper_simd_scan_baseline128_v1
    ldr x9, [x19, #24]
    cmp x0, x9
    b.ne failure
    adrp x3, xor_out
    add x3, x3, :lo12:xor_out
    ldrb w10, [x3]
    ldr x9, [x19, #32]
    cmp x10, x9
    b.ne failure
    add x19, x19, #40
    subs x21, x21, #1
    b.ne next_case
    mov x0, #0
    b exit
failure:
    mov x0, #1
exit:
    mov x8, #93
    svc #0
.section .rodata
.balign 16
input:
    .incbin "input.bin"
.balign 8
cases:
    .incbin "cases.bin"
.bss
xor_out:
    .skip 1
ASM
(cd "$artifacts" && "$assembler" start.s -o start.o)
"$linker" -z noexecstack "$artifacts/start.o" "$artifacts/aarch64.o" -o "$artifacts/scanner"
"$emulator" "$artifacts/scanner"
printf 'AArch64 SIMD scanner: 9600 scalar-reference comparisons passed\n'
