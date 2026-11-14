# Vesper SIMD scanner milestone

This library implements the delimiter-count and XOR example from design §14
using real Cranelift machine code. Its safe Rust entry accepts `&[u8]` and
returns `ScanResult { matches: u64, xor: u8 }`.

```rust
use vesper_simd::{Baseline128Capability, JitScanner};

# fn example() -> Result<(), vesper_simd::Error> {
let capability = Baseline128Capability::detect()?;
let scanner = JitScanner::compile(capability)?;
let result = scanner.scan(b"record\nrecord\n", b'\n');
assert_eq!(result.matches, 2);
# Ok(())
# }
```

`Baseline128Capability` has a private constructor and can be obtained only after
runtime feature detection. On x86_64 detection checks SSE3, SSSE3, SSE4.1, and
SSE4.2 before executable code creation. Codegen enables exactly that SSE preset;
it does not inherit host AVX, BMI, FMA, or POPCNT settings. AArch64 checks NEON and
uses the backend baseline without SVE extensions. Both the token and scanner
remain local to a thread; they do not provide a scheduling portability proof.

The compiled loop loads a full 128-bit chunk only when its starting offset is
below `len & !15`. It uses vector byte equality and vector XOR, with lane
extraction for reductions; no POPCNT feature is assumed. Remainders use scalar
loads individually guarded by `offset < len`. There is no speculative full-width
load for a short tail and no alignment precondition. The JIT module remains
owned across calls and clones, and its executable memory is freed on the final
drop, including cleanup after codegen errors. No executable pointer is exposed.

`ObjectArtifact::emit(ElfTarget, IsaProfileKind::Baseline128)` generates real
x86_64 or AArch64 Linux ELF relocatable objects without executing them or
asserting the host supports their ISA. `bytes()`, `clif()`, and `metadata()`
expose the object and its exact profile, target, ABI version, codegen version,
shared settings, enabled optional ISA flags, width, and tail strategy. Cache
consumers must also bind implementation identity. The exported raw C ABI and
foreign-caller obligations are documented on `ObjectArtifact`.

`profile_support` and `compile_profile` reject Avx2, Avx512, Sve2, and Rvv1 with
`UnsupportedProfile`, even on hardware that supports them: the Cranelift JIT and
object paths are Baseline128 only. Scalable vectors live in `vesper_simd::vvec`
(below). This crate does not implement 256/512-bit fixed vectors,
source-language `eff [Isa[P]]` typing or lowering, egglog vectorization, float
FMA rewrites, wire-layout integration, or Lean SIMD proofs. It makes no
performance, zero-allocation, or compiler-correctness claim.

Validation commands while isolated from the parent workspace:

```sh
cargo test --manifest-path crates/vesper-simd/Cargo.toml
cargo clippy --manifest-path crates/vesper-simd/Cargo.toml --all-targets -- -D warnings
cargo run --manifest-path crates/vesper-simd/Cargo.toml --example emit_objects -- /tmp/vesper-simd-objects
```

The tests cover seeded differential results, all byte values and delimiters,
every scalar tail, unaligned subslices, empty/large inputs, retained module
lifetime after cloning, rejected profiles, simulated feature checks that cannot
mint capabilities, actual ELF architecture/symbols, and inaccessible guard pages
on Unix. Native tests explicitly report unavailable Baseline128 hosts rather
than executing unsupported instructions. AArch64 object generation is tested
on x86_64; AArch64 native Rust execution requires a separate target runner.

## Scalable vectors (`vvec`)

`vesper_simd::vvec` implements §14.3's length-agnostic strip-mined predication
for `u8` lanes. A profile fixes the lanes per vector: `Scalar::new(lanes)` (any
1..=256, the portable reference), `Sve::detect()` (AArch64 SVE, `cntb` bytes) or
`Rvv::detect()` (RVV 1.0, `vlenb` bytes at `e8, m1`). Hardware tokens have
private fields, exist only after run-time detection and are `!Send` (the SVE
vector length is per-thread state).

```rust
use vesper_simd::vvec::{IsaProfile, Scalar, VVec, scan_record_delimiters};

let p = Scalar::new(48).unwrap();
let data = b"a,b,c";
let mask = p.active_mask(0, data.len()); // whilelo / vsetvli
let v = VVec::load_masked(data, 0, &mask, 0).unwrap();
assert_eq!(v.simd_eq(&VVec::splat(p, b',')).and(mask).count_ones(), 2);
assert_eq!(scan_record_delimiters(p, data, b','), (2, data.iter().fold(0, |a, b| a ^ b)));
```

`active_mask(offset, total_len)` is the governing predicate of a strip;
`load_masked`/`store_masked` touch only active lanes (SVE `ld1b`/`st1b` under a
predicate, RVV `vle8.v`/`vse8.v` under `v0.t`), so a strip may run past the end
of a slice, and an active lane past it is `VvecError::OutOfBounds`. A Unix guard
page test checks that inactive lanes are never read. Lane operations are
`simd_eq`, `simd_lt`, `bit_xor`/`and`/`or`, `wrapping_add` and `reduce_xor`;
masks are bit sets with `and`, `or`, `xor`, `and_not`, `!`, `count_ones`.
`vvec::kernels::scan` runs the whole scanner loop in one `asm!` block.

Rust's SVE and RVV intrinsics are unstable, so the backends are stable `asm!`
in `src/vvec/sve.rs` and `src/vvec/rvv.rs`, each with a register and memory
contract. Vector values cross operations through memory, so the per-op API is
a reference shape, not a fast path. The tests (`tests/vvec.rs`) compare every
operation and both scanners with the scalar reference at the profile's own
length; `.github/workflows/scalable-vectors.yml` runs them under QEMU user mode
at SVE 128/256/384/512/2048 bits and RVV VLEN 128/256/512/1024. QEMU runs show
functional behaviour only; no performance is claimed.
