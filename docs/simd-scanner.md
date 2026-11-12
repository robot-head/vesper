# Native SIMD scanner

`vesper simd scan input.bin --delimiter 10 --json` counts matching bytes and XORs
all bytes in a file using generated Baseline128 native code. The command reads
64 KiB chunks, retains bounded input memory, and reports results only after EOF.
The JSON report includes target, ABI, Cranelift version, exact settings and enabled
ISA flags. Without `--json`, it prints the byte count, delimiter count and XOR.

The `vesper-simd` library also emits real x86_64 and AArch64 Linux ELF objects.
Its safe JIT API accepts borrowed slices and keeps executable memory alive across
cloned handles. Handles and CPU capability tokens remain on their creating thread.
On x86_64, runtime discovery checks SSE3, SSSE3, SSE4.1 and SSE4.2; code generation
uses that explicit baseline rather than inheriting the host's higher features.
AArch64 checks NEON. JIT execution requires a 64-bit pointer ABI. Unsupported
hosts and AVX2, AVX-512, SVE2 and RVV profiles return explicit errors.

Full 16-byte chunks use unaligned vector loads; the remainder uses checked scalar
loads. Native tests compare every byte value and delimiter, unaligned inputs,
all remainder lengths and larger inputs against a scalar reference. Unix guard
pages check both ends of slices. Object tests inspect both target formats and
profile metadata. With AArch64 binutils and QEMU installed,
`scripts/test-simd-aarch64.sh` executes the emitted scanner without a target libc
and checks 9,600 cases against independently computed scalar results. This covers
offsets 0–31, lengths 0–95 and large buffers, with delimiters 0, 7 and 255.
QEMU execution is functional validation, not Arm hardware performance evidence.

This implements the delimiter/XOR example from specification §14 as a host library
and command. Source `Isa[P]` effects, general vector syntax, scalable vectors,
egglog vectorization and Lean equivalence proofs remain unimplemented. No
throughput claim is made.
