# Scalable vectors under QEMU (wave eight, lane 9)

Claim: #458. Design: §14.3 (`VVec[P, T]`, `VMask[P, T]`), §20.3 (stage 1).
Baseline: `1094e649c95e4d81eef4e705d13882ab1339542d` (plan PR #477 merge),
a descendant of the required `ea1ed29`.

## What was built

### `vesper_simd::vvec` (crates/vesper-simd/src/vvec/)

* Profiles (sealed `IsaProfile` trait): `Scalar::new(lanes)` for any 1..=256
  lanes (the portable reference, including non-power-of-two widths),
  `Sve::detect()` (lanes = `cntb`) and `Rvv::detect()` (lanes = `vlenb`, `e8,
  m1`). Hardware tokens have private fields, exist only after run-time
  detection (`is_aarch64_feature_detected!("sve")`; `V` in `AT_HWCAP` read from
  `/proc/self/auxv`) and are `!Send`: the SVE vector length is per-thread.
  Vectors wider than 256 bytes are `VvecError::VectorTooLong`.
* Governing predicate `active_mask(offset, total_len)`: lanes
  `0..min(lanes, total_len - offset)` (SVE `whilelo`; RVV `vid.v` + `vmsltu.vx`
  at `e16, m2` so 256 lanes are exact).
* `VVec::load_masked(data, offset, &mask, passthru)` and `store_masked`:
  SVE `ld1b`/`sel`/`st1b` under the predicate, RVV `vle8.v`/`vse8.v` under
  `v0.t` with mask-undisturbed passthru. Only active lanes are accessed, so a
  strip may extend past the slice; an active lane past it is
  `VvecError::OutOfBounds` (checked before any access, overflow included).
* Lane ops `splat`, `simd_eq`, `simd_lt` (unsigned), `bit_xor`, `bit_and`,
  `bit_or`, `wrapping_add`, `reduce_xor` (SVE `eorv`, RVV `vredxor.vs`); masks
  are bit sets (`and`, `or`, `xor`, `and_not`, `!`, `count_ones`,
  `first_active`, `last_active`, `to_bitmask64`) in the layout `str p` and
  `vsm.v` write.
* `scan_record_delimiters(profile, data, delim)`: the §14.3 scanner written
  against the API exactly as the design writes it (no scalar remainder).
* `kernels::scan(&profile, data, delim)`: the same scanner as one strip-mined
  `asm!` loop (SVE `whilelo`/`incb`/`incp`; RVV `vsetvli` advancing by the
  granted `vl`, tail-undisturbed XOR accumulator).

### `vesper_serde::simd::scalable` (crates/vesper-serde/src/simd/scalable/)

* `ScalableIsa::{Sve, Rvv}` with `is_available`, `vector_bytes`, `available`.
* `block_masks_scalable(input, isa)` and `StructuralIndex::build_scalable`:
  stage 1 with the same masks, positions, limits and errors as the existing
  paths. The existing `Isa` enum and its dispatch are unchanged; the only edit
  outside the new directory is the `pub mod scalable;` line in
  `src/simd/mod.rs`. An index built here reports `isa()` as `None`.
* Kernel per 512-byte chunk, strip-mined at the hardware length: five byte
  classes (backslash, quote, operators via `b | 0x20` folding, whitespace,
  control) computed as predicates and stored with `str p` / `vsm.v` at the
  strip's bit offset, so 64-bit block words are identical at every length,
  including 384-bit SVE where strips straddle blocks. The same pass runs the
  Keiser–Lemire UTF-8 lookup check (`tbl` / `vrgather.vv` over the three
  nibble tables, previous bytes via `insr` / `vslide1up.vx`, three bytes of
  history across chunks). Prefix XOR uses shifts (carry-less multiply is
  optional in both ISAs). The final partial block is padded with spaces as on
  the other paths; a sequence cut off exactly at a block-aligned end is
  checked from the last three bytes.

### CI

`.github/workflows/scalable-vectors.yml` (new): cross-compiles both test
targets in release mode with `aarch64-linux-gnu-gcc` / `riscv64-linux-gnu-gcc`,
then runs them under `qemu-user` (Ubuntu's QEMU 8.2) at every length below
with `VESPER_REQUIRE_SVE=1` / `VESPER_REQUIRE_RVV=1`, and once with the vector
unit off to check the typed `Unavailable` / `IsaUnavailable` errors.

## Vector lengths tested (all bit-identical to the scalar reference)

| ISA | QEMU `-cpu` | Vector length |
| --- | --- | --- |
| SVE | `max,sve128=on` | 128 bits |
| SVE | `max,sve256=on` | 256 bits |
| SVE | `max,sve384=on` | 384 bits (not a power of two) |
| SVE | `max,sve512=on` | 512 bits |
| SVE | `max,sve2048=on,sve-default-vector-length=256` | 2048 bits |
| RVV 1.0 | `rv64,v=true,vlen=128` | VLEN 128 |
| RVV 1.0 | `rv64,v=true,vlen=256` | VLEN 256 |
| RVV 1.0 | `rv64,v=true,vlen=512` | VLEN 512 |
| RVV 1.0 | `rv64,v=true,vlen=1024` | VLEN 1024 (QEMU 8.2's maximum) |

Scalar profiles run on every host at 1, 3, 16, 17, 32, 48, 64, 100, 128, 192
and 256 lanes. SVE with `max,sve=off` and RVV with `rv64,v=false` were run
too: the hardware tests report unavailable and the typed errors hold.

What the tests compare: `tests/vvec.rs` checks predicates, masked loads and
stores with random masks and strips past the slice end, typed out-of-bounds
errors, every lane op and reduction, and both scanners at offsets 0–31,
lengths 0–299 plus 1000 and 4096, delimiters 0, 7, 10 and 255, against plain
Rust at the profile's own length, and each hardware op against the scalar
profile of the same width. A guard-page test (Unix) puts strip tails on a
`PROT_NONE` page. `tests/simd_scalable.rs` ports the stage 1 differential
suite: every byte value at every lane position, random inputs up to three
chunks, backslash runs and quotes across block and chunk edges, exhaustive
two-byte UTF-8 pairs at block and chunk edges, three- and four-byte sequences,
every 97th scalar value and truncations, and structural index positions,
escapes, string-control, unclosed-string and truncation results (also with
`max_entries = 17`). Mutating one SVE and one RVV instruction made these
tests fail, so they exercise the kernels.

## Checks (local, x86_64 host, Rust 1.97 and 1.100)

```sh
cargo fmt --all -- --check                                   # ok
cargo +1.100.0 clippy -p vesper-simd -p vesper-serde --all-targets --locked \
  --target {x86_64-unknown-linux-gnu,aarch64-unknown-linux-gnu,riscv64gc-unknown-linux-gnu} \
  -- -D warnings                                             # ok, all three
cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings
cargo test --workspace --locked
# QEMU matrix as in the workflow, release builds: vvec 5/5 and simd_scalable
# 8/8 at each of the nine lengths above.
```

Results of the workspace runs are in the PR description.

## Decisions

* Stable `asm!`, not intrinsics: Rust's SVE/RVV intrinsics are unstable. SVE
  blocks sit in `#[target_feature(enable = "sve")]` functions; RVV blocks
  enable V per block with `.option arch, +v`, so the crate still builds for
  plain `riscv64gc`. `unsafe` is confined to `src/vvec/{sve,rvv}.rs` and
  `src/simd/scalable/{sve,rvv}.rs` (serde keeps `#![deny(unsafe_code)]`
  elsewhere), each with a written register and memory contract.
* Values cross `asm!` blocks through memory (scalable registers cannot be
  operands), so the per-op `VVec` API is the reference shape; whole loops
  (`kernels::scan`, the classifier) stay in registers.
* Memory safety does not depend on the vector length staying fixed: every
  access is bounded by a predicate or `vl` built from the recorded lane count,
  and predicate stores fit the 32-byte mask buffers.
* RVV strips in the classifier request `min(remaining, VLMAX)` so `vl` is
  exact and mask bytes align; the scanner instead accepts whatever `vl`
  `vsetvli` grants, as RVV allows.
* RVV detection reads `AT_HWCAP` from `/proc/self/auxv` with `std` (no new
  dependency); `is_riscv_feature_detected!` is unstable.
* The new SVE path uses only base SVE instructions, so SVE2 hardware runs it;
  the design's profile name is `Sve2`.

## Remaining gaps

* No performance claims: QEMU is functional validation only, and no SVE or
  RVV hardware was available. None of this ran on real silicon.
* Only `u8` lanes. Wider element types, `Vec[T, N]` fixed-width vectors,
  AVX-512/AVX2 masked tails through this API and LMUL > 1 are not done.
* The source-level `VVec`/`Isa[P]` syntax, typing and lowering, and the
  Cranelift SVE/RVV backends (`IsaProfileKind::Sve2`/`Rvv1` in the JIT) are
  not implemented; `profile_support` still rejects them.
* Stage 2 (`IndexedJson::parse_simd`, `SimdParser`) is not wired to the
  scalable paths, because that would change the existing `Isa` dispatch; a
  caller builds the index with `build_scalable`.
* No Lean proof covers the SVE/RVV kernels; equivalence is differential
  testing against the scalar reference at the lengths listed.
* VLEN above 1024 (QEMU 8.2's limit) is untested; profiles reject vectors over
  2048 bits, while the kernels have no width limit.
