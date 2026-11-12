# Baseline128 SIMD scanner handoff

Implemented the bounded design §14.2–14.5 delimiter-count/XOR milestone in the
new `crates/vesper-simd` library. Existing compiler, runtime, allocator, FFI,
workspace, and external-agent files were not changed.

## Public boundary

- `Baseline128Capability::detect()` produces a copyable, thread-local typed
  capability with a private constructor after real host feature detection.
  Simulated discovery tests exercise a separate private decision function;
  callers cannot turn hypothetical CPU features into executable evidence.
- `JitScanner::compile(capability)` owns a real Cranelift JIT module and private
  C function pointer. `scan(&[u8], delimiter)` returns `ScanResult { matches,
  xor }` directly from native code. Clones share module ownership using `Rc`;
  the final drop frees code memory. Failed construction also frees allocations.
- `profile_support(kind)` and `JitScanner::compile_profile(kind)` explicitly
  reject Avx2, Avx512, Sve2, and Rvv1. No fixed-width substitute is presented as
  scalable-vector support.
- `ObjectArtifact::emit(ElfTarget::{X86_64,Aarch64}, Baseline128)` returns real
  Linux ELF relocatable bytes plus CLIF and `CodeMetadata`. It creates no
  executable capability and never executes a cross-target object.
- Metadata records profile, complete target triple, vector width, scalar tail
  policy, raw C ABI version, actual Cranelift version, all shared codegen
  settings, and enabled optional ISA flags. Cache consumers must additionally
  bind implementation identity.

On x86_64, runtime checks cover SSE3, SSSE3, SSE4.1 and SSE4.2. The ISA builder
starts from `isa::lookup`, not host-detected presets, and enables only the
`sse42` preset. The actual enabled flags are exactly `has_sse3`, `has_sse41`,
`has_sse42`, `has_ssse3`; POPCNT, AVX, BMI and FMA are not enabled. AArch64 probes
NEON and uses baseline AArch64 codegen without optional SVE features.

Both loops use actual 128-bit vector byte equality and XOR. The full-chunk
condition is `offset < (len & !15)`, so a 16-byte load is always within the
slice. Counts are reduced using 0/1 mask lanes without requiring POPCNT.
The explicit remainder loop checks `offset < len` before each byte load.
Unaligned inputs and zero-length slices are accepted. Code accepts no raw
pointer through its safe API, and no generated entry pointer escapes.

## Validation completed

Using the existing shared target cache and Cranelift 0.125.4,
target-lexicon 0.13, object 0.37.3:

```sh
CARGO_TARGET_DIR=/workspace/vesper/target cargo test --manifest-path crates/vesper-simd/Cargo.toml --jobs 2
CARGO_TARGET_DIR=/workspace/vesper/target cargo clippy --manifest-path crates/vesper-simd/Cargo.toml --all-targets --jobs 2 -- -D warnings
cargo fmt --manifest-path crates/vesper-simd/Cargo.toml -- --check
```

All seven unit tests and three doctests passed. Native x86_64 code executed on
this host. Tests cover all byte values and delimiter values, seeded data,
offsets 0–31, lengths 0–193, large slices through 1,048,577 bytes, delimiter
0/255, repeated matching and nonmatching bytes, clone lifetime after dropping
the original, and rejected profiles. Compile-fail doctests verify the private
capability constructor and scanner's `!Send` boundary. Unix guard-page tests
place slices at both ends of a readable page with inaccessible neighbors and
exercise lengths 0–95; no full-width tail overread or front overread occurred.

Both emitted objects parse as ELF relocatables of the correct architecture,
export a nonempty defined scanner symbol, and have no external runtime
symbols. Their generated CLIF contains the vector loads, equality and XOR.
The example below creates inspectable artifacts without executing them:

```sh
CARGO_TARGET_DIR=/workspace/vesper/target cargo run --manifest-path crates/vesper-simd/Cargo.toml --example emit_objects -- /tmp/vesper-simd-inspect
objdump -d /tmp/vesper-simd-inspect/x86_64.o
```

Actual x86_64 disassembly contains `movdqu`, `pcmpeqb`, `pxor`, `pshufb`, and
`pextrb`, with no AVX or POPCNT instructions. Using the installed AArch64
objdump, actual AArch64 disassembly contains `ldr q20`, `dup v21.16b`,
`cmeq v21.16b`, and `eor v19.16b`. The AArch64 object was not executed; this is
object-generation evidence, not an AArch64 native-runtime claim.

## Integration and remaining design work

The crate has a temporary standalone `[workspace]` and nested lockfile for
independent handoff. The coordinator should remove those, register it in root
`members` and `default-members`, update the root lock, and run workspace checks.
All production dependencies match versions already used in the repository;
Unix `libc` and `object` are test-only direct dependencies.

This is a standalone library, not source compiler integration. It does not
implement lexical source `eff [Isa[P]]` judgments, source vector types or
lowering, generic fixed-vector operations, AVX2/AVX-512/SVE2/RVV kernels,
egglog vectorization, float FMA equivalence, wire-layout integration, or Lean
SIMD proofs. It makes no performance, zero-heap, scheduling portability, or
compiler-correctness claim. These limits preserve the shared-contract audit's
separation of runtime ISA evidence, ownership, and thread portability.

## Coordinator integration

Registered the crate in both root workspace lists and removed its standalone
workspace/lockfile. The safe JIT capability now also rejects non-64-bit pointer
ABIs, including x86_64-x32. Added `vesper simd scan`, bounded streaming reads,
actual code metadata in JSON, and four CLI tests covering scalar comparisons,
chunk boundaries, explicit unsupported profiles and error paths.

`scripts/test-simd-aarch64.sh` now links the emitted object with a libc-free
assembly harness and executes it under QEMU. All 9,600 independently computed
scalar-reference cases passed: offsets 0–31, lengths 0–95 and four large sizes,
with delimiter bytes 0, 7 and 255. This supersedes the handoff's emission-only
AArch64 validation boundary; it does not establish hardware performance.

Integrated validation: 762 workspace tests/doctests passed across 75 suites with
the actual pinned Lean toolchain available; formatting and strict workspace
Clippy passed. The root CLI and all its targets also passed Windows GNU cross
compilation with the pure BLAKE3 feature; Windows runtime execution was not tested.
