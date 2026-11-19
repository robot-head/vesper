# Port wave P3b lane 8: vesper-kinetic, part 1 (seed split, Morton codes, fast math, colouring, broadphases)

Claim [#1070](https://github.com/robot-head/vesper/issues/1070). Base `main` at
8212ddf (2026-11-18). Plan: `docs/agent-prompts/port-p3b/08-kinetic.md`
(plan PR #1072).

This is the first part of the lane. It splits the crate into the seed copy and
the port-backed build. It then ports the leaf modules that the solvers build
on: `morton.rs`, the 63-bit codes of `lbvh.rs`, `fastmath.rs`, `coloring.rs`,
`lbvh.rs` and `sap.rs`. The world, rigid, soft and other modules call the
ported functions in the port-backed build, so the golden traces and world
tests check the port end to end. The rest of the lane's list (see "Not ported
yet") follows in later parts. Part 1 closes nothing; the last part closes
#1070.

## Builds

`vesper-kinetic` is in the compiler's dependency closure
(`src/native_verifiers.rs` runs `vesper_kinetic::detfloat::audit`), so it
follows the seed-copy rule of `docs/maintenance/porting-a-crate.md`:

| Package | Path | What it is | Who links it |
|---|---|---|---|
| `vesper-kinetic-seed` | `crates/vesper-kinetic/seed` | the Rust build of `../src`, library `vesper_kinetic`, no build script | the compiler (`vesper`), `vesper-gpu-compute`, `vesper-kinetic-gpu` |
| `vesper-kinetic` | `crates/vesper-kinetic` | `build.rs` links the Loom package `vesper/` and sets `cfg(vesper_port)` | its own tests and `examples/abba.rs` only |

Under the owner's rule, Rust stays the default: no dependent links the port,
and no Rust code is deleted. `vesper_kinetic::IMPLEMENTATION` says which
build a binary has (`examples/abba.rs` asserts both).

The dependent switch (root `Cargo.toml`, `vesper-gpu-compute`,
`vesper-kinetic-gpu`, the seed package and the lockfile) is its own commit.

## API inventory

| Module | src lines | Public API | This part |
|---|--:|---|---|
| `morton` | 170 | `spread`, `compact`, `encode_masked`, `encode`, `decode_masked`, `decode`, `spec_encode`, `encode_fast`, `decode_fast`, `arch::*` (BMI2) | all but `spec_encode` (the Lean model's bit-by-bit reference, kept as the test oracle) and the BMI2 intrinsics |
| `lbvh` | 423 | `spread21`, `compact21`, `morton64`, `demorton64`, `Aabb`, `Lbvh` (`build`, `overlapping_pairs`, accessors), `brute_force_pairs` | `spread21`, `compact21`, `morton64`, `Lbvh::build` (validation, centroid quantisation, sort, Karras construction, bounds) and `Lbvh::overlapping_pairs` |
| `sap` | 424 | `sortable`, `dispatch`, `SweepAndPrune` (`build`, `overlapping_pairs`, accessors) | `sortable`, `build` (validate, axis, keys, radix sort, gather) and `overlapping_pairs` (count, scan, write, radix sort) |
| `fastmath` | 546 | `exp`, `log`, `sin`, `cos`, `atan2` and the five binary32 functions, `ulp_distance*` | the ten functions |
| `coloring` | 410 | `Colorer::color`, `MultiColorer::color`, accessors, `islands_are_disjoint`, `sets_are_disjoint` | both `color` functions |
| `crmath`, `gjk`, `tile`, `rigid` + `rigid/**`, `soft`, `articulation`, `couple`, `diff`, `rigid_diff`, `rollback`, `snapshot`, `world` | 12,108 | | later parts |

The Vesper package is 2,112 lines in seven modules (`vesper/src/`): `lib.ves`
(the exported functions and the shared error enums), `morton`, `fastmath`,
`coloring`, `lbvh`, `sap` and `util` (total slice access and heap sorts).

### Boundary design

- The export ABI has no `f32`, so an `f32` crosses as its bit pattern (`u32`),
  as in `@vesper/tensor`. A box is six bit patterns (min x, y, z, max x, y,
  z).
- The Rust types keep their buffers (`Vec`s reserved at construction) and
  lend them to the port as `inout Slice[..]` of packed scalars, which the
  binding passes without a copy. Nothing in the port allocates, so the
  zero-allocation tests (`tests/alloc.rs`, `tests/alloc_arena.rs`) pass
  against the port. Inputs that are not packed (`&[ConstraintBodies]`,
  `&[Aabb]`) are flattened once per call into reserved scratch columns.
- Results that are Rust types (`Lbvh::keys()`, `Lbvh::nodes()`, the pair
  lists) are rebuilt from the port's packed words: a child is `k` for node
  `k` and `2^32 + i` for leaf `i`, a pair is `(a << 32) | b`.
- The capacity checks that size the lent buffers stay in the Rust wrapper
  (they return the same errors before any buffer is touched); the port
  checks again. Everything else, including the typed `InvalidBox`,
  `BodyOutOfRange`, `SelfConstraint`, `TooManyColors`, `BadArity` and
  `PairOverflow` errors, is decided by the port.
- `sin` and `cos` past the fast reduction (`|x| >= 2^20 pi/2`) call the
  correctly rounded `crmath.rs` through the `.vsys` library `Crmath`
  (`src/port.rs` `shim`), until `crmath.rs` is ported.
- The SAP kernels in Rust run over workgroups of 256 items in a GPU's shape;
  their outputs do not depend on the grouping, so the port runs each kernel
  as one loop and its radix sort uses one histogram per pass (same scatter
  positions). The LBVH and colouring sorts use heap sort: every key is
  unique (index tie-break), so any correct sort gives the Rust order.

## What stays Rust in this part, and why

| File | Reason | Unblocked by |
|---|---|---|
| `morton.rs` `arch` (BMI2 `pdep`/`pext`) | intrinsics; `encode_fast`/`decode_fast` still range-check through the port | source `Vector`/bit-deposit builtins, or never (measure) |
| `morton.rs` `spec_encode`, `lbvh.rs` `brute_force_pairs`, `demorton64`, `Aabb` methods, `coloring.rs` checkers | test oracles and one-line helpers that keep the tests independent of the port | port later if wanted |
| `crmath.rs` (called by the port's `sin`/`cos` fallback) | needs multiprecision integers (`num-bigint`) | part 2 (a word-array bignum in the package) |
| `detfloat.rs`, `simd.rs`, `vesper-kinetic-gpu` | per the lane plan (Cranelift IR scanner, intrinsics, GPU crate) | stays Rust |
| everything listed under "later parts" | not yet ported | parts 2+ |

Unsafe code: the port has none. The port-backed build adds the generated
binding (`unsafe` FFI calls, `#[allow(unsafe_code)]` on that module) and two
`#[no_mangle] extern "C"` shim functions. The seed has the same unsafe as
before (`morton::arch`, `simd`).

## Unit tests

`vesper-kinetic` has no in-crate `#[cfg(test)]` tests; all 30 test files are
under `tests/` and use the public API, so each runs unchanged against both
packages (`cargo test -p vesper-kinetic -p vesper-kinetic-seed`). The seed
package lists every one as a `[[test]]` with `path = "../tests/.."`.
`tests/lean_model.rs` finds `lean/` from either package directory (the only
test edit). In Bazel, the seed runs every test but `lean_model` (it needs the
regular-file copy of the Lean runfiles that `vesper_crate` sets up for the
port package).

## Results against both builds

Locally (Linux x86_64, `VESPER_PORT_COMPILER` set to the in-tree compiler),
`cargo test --no-fail-fast -p vesper-kinetic -p vesper-kinetic-seed`: every
test of both packages passes, 329 test cases in all, including:

- `tests/golden.rs`: the fixed scene and the batched/LBVH/arena/SIMD/thread
  variants match the committed golden hashes byte for byte.
- `tests/fastmath.rs`: the 20,000-input sweep hash
  (`28519723...0fac3`) matches; every function stays within its ULP bound.
- `tests/batches.rs`, `tests/soft.rs`, `tests/world.rs`: thread-count
  independence (1 to 64 threads) still holds with the ported colouring and
  broadphases inside the solvers.
- `tests/alloc.rs`, `tests/alloc_arena.rs`: zero allocations per step in
  steady state with the port (LBVH and SAP).

No `f32`/`f64` divergence was found.

## Divergence checks

`crates/vesper-kinetic/port/check.sh` (sourced by
`scripts/test-port-harness.sh`, shard 3) applies one edit at a time
(`EDITS` in `build.rs`, the same edits as the Bazel
`kinetic_vesper_divergent_*` objects) and asserts that a test of its module
fails while another test of the same binary passes. All five were run
locally:

| Module | Edit | Fails | Still passes |
|---|---|---|---|
| morton | a code with bit 30 set decodes | `out_of_range_inputs_are_typed_errors` | `every_axis_value_matches_the_bitwise_specification` |
| fastmath | the cosine kernel adds the reduction tail | `output_sweep_hash_is_identical_on_every_architecture` | `annex_f_special_values_match_the_correctly_rounded_path` |
| coloring | a constraint's first body is never marked busy | `colouring_is_greedy_lowest_free_in_body_order` | `chain_uses_two_colours_and_islands_are_disjoint` |
| lbvh | pairs of adjacent sorted leaves are dropped | `overlapping_pairs_equal_the_brute_force_set` | `morton64_round_trips_and_orders_by_interleaved_bits` |
| sap | `sortable` flips its sign test | `sortable_keys_preserve_float_order` | `malformed_inputs_are_typed_errors` |

## Performance (ABBA)

`examples/abba.rs` runs the Rust seed (`vesper_kinetic_rust`) and the port
in one process, in Rust, port, port, Rust order per epoch. Host: cloud
Linux x86_64 VM, Intel Xeon @ 2.10GHz, 4 vCPUs, release build;
`abba compare 5 8` (5 epochs of 8 runs). Instruction counts are callgrind
totals of `abba one <build> <workload> 1` minus the 452,980 instructions of
an empty run (process start-up).

| Workload | Rust ns/run | Port ns/run | Wall ratio (range) | Rust instr | Port instr | Instr ratio |
|---|--:|--:|--:|--:|--:|--:|
| `morton` (4,096 encode + decode) | 36,196 | 67,107 | 1.83x (1.68-1.99) | 467k | 938k | 2.01x |
| `fastmath` (512 x 10 calls) | 52,694 | 76,914 | 1.46x (1.35-1.57) | 453k | 884k | 1.95x |
| `colorer` (8,192 constraints) | 1,168,307 | 3,098,151 | 2.54x (2.19-2.68) | 6.75M | 18.47M | 2.74x |
| `multi-colorer` (4,096 sets) | 326,476 | 1,105,782 | 3.36x (2.73-3.43) | 3.59M | 9.99M | 2.78x |
| `lbvh` (4,096 boxes, build + pairs) | 1,680,128 | 4,080,619 | 2.43x (2.41-2.44) | 8.17M | 35.66M | 4.36x |
| `sap` (4,096 boxes, build + pairs) | 2,851,915 | 6,118,827 | 2.14x (2.12-2.16) | 14.14M | 40.01M | 2.83x |
| `scene` (golden scene, 16 ticks) | 7,122,915 | 9,430,202 | 1.32x (1.32-1.37) | 69.95M | 92.14M | 1.32x |
| `pile-lbvh` (1,024 spheres, 4 ticks, 1 thread) | 5,040,599 | 14,136,955 | 2.80x (2.75-2.83) | 58.24M | 136.60M | 2.35x |
| `pile-lbvh-4t` (same, 4 solver threads) | 4,964,226 | 14,239,811 | 2.87x (2.78-3.17) | | | |

The world workloads run the Rust solvers in both builds; only their
colouring, broadphase and Morton calls go to the port. Not parity, so Rust
stays the default everywhere.

Where the time goes:

1. **Sorting.** The Rust build sorts with `sort_unstable` (pdqsort, with
   tuple keys compared in registers); the port uses a heap sort over lent
   slices, two `try_get`s per comparison (LBVH: keys and primitives sort
   as two parallel slices). There is no sort in `@vesper-std`. This is most
   of the `lbvh`, `colorer` and `multi-colorer` gaps.
2. **Calls per scalar.** `morton::encode_masked` and the fast-math functions
   are a handful of instructions in Rust and inline into their callers; in
   the port each is a C-ABI call through the binding (the Morton-cell
   broadphase calls `encode_masked` once per body per tick). Fusing such
   calls needs the callers ported (part 2+), not a compiler change.
3. **Marshalling.** `&[Aabb]` and `&[ConstraintBodies]` are flattened into
   packed columns per call, and the pair lists are unpacked from words.

## Part 2: std sort

Part 2 (base `main` at 8e7d8b7, 2026-11-19) replaces the three heap sorts
of `util.ves` with `@vesper-std/sort` (#1096, PR #1110), a path dependency
of the package (Bazel: `//:std_sort_package`). The orders are unchanged,
since every sort is by a strict total order:

| Call site | Before | After |
|---|---|---|
| LBVH build, codes and primitives | `sort_pairs` (heap, by code then primitive) | codes go to the free traversal stack, `keyed::sort_by_key` orders the primitives (ties by index), then the codes are gathered; no allocation |
| LBVH pairs | `sort_u64` (heap) | `unstable::sort` (introsort) on `out[0..len]` |
| `Colorer` / `MultiColorer` | `sort_by_key` (heap) | `keyed::sort_by_key` on `order[0..n]` |

`keyed::sort_pairs` is not used: it is stable but allocates scratch.

| Workload | Part 1 wall | Part 2 wall (range) | Part 1 instr | Part 2 instr (Rust, port) |
|---|--:|--:|--:|--:|
| `colorer` | 2.54x | 1.99x (1.81-2.00) | 2.74x | 1.75x (6.76M, 11.81M) |
| `multi-colorer` | 3.36x | 2.78x (2.69-3.18) | 2.78x | 1.99x (3.59M, 7.13M) |
| `lbvh` | 2.43x | 2.08x (2.01-2.21) | 4.36x | 3.81x (8.17M, 31.17M) |
| `pile-lbvh` | 2.80x | 2.45x (2.42-2.51) | 2.35x | 2.24x (58.24M, 130.27M) |
| `pile-lbvh-4t` | 2.87x | 2.46x (2.43-2.67) | | |
| `scene` | 1.32x | 1.28x (1.20-1.30) | | |

Same host and method as above (`abba compare 5 8`, callgrind minus the
empty run). `morton`, `fastmath` and `sap` do not sort through `util.ves`
and are unchanged (1.85x, 1.44x, 2.03x wall). Still not parity, so Rust
stays the default. The LBVH gap that is left is the per-query traversal
(two `try_get`s per box lane) and the 2.7-4.3x of std sort against Rust's
`sort_unstable` reported in #1110.

## Part 3: crmath

Part 3 ports `crmath.rs`, the correctly rounded `sin`, `cos`, `exp`, `log`
and `atan2`, with the arbitrary-precision integers it needs
(`vesper/src/bignum.ves`, 32-bit limbs, standing in for `num-bigint`). Its
shifts round toward negative infinity and its division truncates, as
`num-bigint`'s do, so every fixed-point intermediate is the Rust build's.
Long division is Knuth's algorithm D, and square roots use Newton's
iteration. With the port in place, the fast path's out-of-range `sin` and
`cos` call it directly. The `.vsys` shim of part 1 is gone, and the
package's capability ceiling drops `Ffi`.

pi and ln 2 come from a cache that Rust lends (`inout Slice[u32]`, one per
thread, in `src/crmath.rs`), as the Rust build caches them per thread. The
cache holds the constants at the widest width seen so far, so its contents
depend on earlier calls. Every result is still correctly rounded, so
results never depend on call history. The fast path's fallback lends an
empty cache, which is never filled. The port does not count
`precision_cap_hits`.

Both packages pass every test. That includes the 400-bit `mpmath`
references bit for bit and the committed cross-architecture sweep hash.
Two divergence checks were added:

- **crmath:** the third-quadrant sine keeps its sign. `crmath` fails
  `matches_mpmath_references_bit_for_bit`, and `annex_f_special_values`
  still passes.
- **bignum:** a magnitude's low word drops its upper limb. `fastmath` fails
  `binary64_functions_stay_within_the_documented_ulp_bound`, and the
  binary32 bound still passes.

A first try broke long division instead. That divergence never settles: the
reduced argument grows, so the series does not end. It was dropped as a
check, because a check has to fail rather than hang.

| Workload | Rust ns/run | Port ns/run | Wall ratio (range) | Rust instr | Port instr | Instr ratio |
|---|--:|--:|--:|--:|--:|--:|
| `crmath` (64 x 5 calls), constants recomputed per evaluation | 1,415,386 | 8,084,111 | 6.02x (5.55-7.22) | 16.04M | 109.21M | 6.81x |
| `crmath`, lent cache (committed) | 1,420,882 | 3,587,424 | 2.51x (2.45-2.71) | 16.04M | 45.23M | 2.82x |

Not parity, so Rust stays the default. The rest of the gap:

- **Allocation.** Every bignum operation returns a fresh `ChunkSeq`, and
  `malloc`/`free`/`vesper_seq_make` were 27% of the uncached profile. In
  place, `inout` arithmetic would avoid most of it.
- **32-bit limbs.** These cost four times the limb products of
  `num-bigint`'s 64-bit limbs. There is no `u128`, and there is no 128 by 64
  division for algorithm D's estimate. `mul_wide` covers the product half.

## Part 4: GJK and EPA

Part 4 ports `gjk.rs`, the narrowphase (`vesper/src/gjk.ves`): GJK on the
shapes' cores with Johnson's sub-simplex search, EPA on the cores'
Minkowski difference, and the flat-difference fallback. Every binary64
operation keeps the Rust order with no fused multiply-add, so the port's
results are the Rust build's bits. The Rust `Shape` crosses the boundary as
a kind and its numbers: a sphere is `[centre, radius]`, a capsule
`[a, b, radius]` and a polytope its vertices (`as_flattened`, lent without
a copy). The support function that `gjk.rs` passes as `&dyn Fn` becomes a
match on the kind. Internal functions return a status, and the
`gjk_separation` export raises `Fail[GjkError]`, which the binding maps to
the crate's `GjkError`.

Both packages pass every test, including the contact, rigid-body and golden
scene hashes that run through the narrowphase. One divergence check was
added:

- **gjk:** a separated distance subtracts only the first radius. `gjk`
  fails `sphere_pairs_match_closed_form`, and
  `malformed_shapes_are_typed_errors` still passes.

| Workload | Rust ns/run | Port ns/run | Wall ratio (range) | Rust instr | Port instr | Instr ratio |
|---|--:|--:|--:|--:|--:|--:|
| `gjk` (256 random sphere, capsule and box pairs) | 267,351 | 448,359 | 1.69x (1.62-1.78) | 116.7M | 217.2M | 1.86x |

Not parity, so Rust stays the default. The simplex search runs on
`ChunkSeq` scratch (the 4 x 4 elimination matrix, the weights and the
simplex) where Rust uses stack arrays, so each access is a checked
`try_get`/`try_set` through a seq header. Fixed-size local arrays would
close most of it (the same gap as the bench port's subset-sum loop).

## Compiler gaps (with repros)

1. **Bit builtin on a reassigned mutable local fails IR validation.**
   `vesper build --lib` (optimized) rejects

   ```vesper
   pub fn f(v: u32) -> u32 {
       let mut x = v;
       x = wrapping_shl_u32(x, 16);
       x
   }
   ```

   with `optimized IR failed ownership validation: E0410 IR access to
   '$slot1_x' overlaps active loan '$slot1_x'`. The same happens for any of
   the suffix bit builtins (`wrapping_shl_k`, `wrapping_shr_k`, ...) applied
   to a `let mut` local that is assigned later, including a loop counter
   (`x = x | wrapping_shl_u32(1, i); i = wrapping_add(i, 1);`). `wrapping_add`
   in the same position is fine. Probably the native bit-builtin rewrite
   (`integer_widths::lower_bit_builtin_calls`, #988). Workaround in the port:
   constant shifts use `<<`/`>>` (total for constant amounts), and variable
   shifts go through one-line helpers (`util::shr_u64`, `shl_u64`,
   `shr_i64`), whose parameters are immutable.
2. **Integer literal range bounds are `i64`.** `for a in 0..3 { s.try_get(a) }`
   with `s: Slice[u64]` fails with `E0200 expected u64, found i64`; the range
   does not take its type from the use. Workaround: `0..u(3)` with
   `fn u(n: u64) -> u64`.
3. **No `while`.** `while cond { .. }` is a parse error; ports write
   `loop { if !(cond) { break; } .. }`.
4. **No sort in `@vesper-std`** (fixed by #1110, adopted in part 2), and no `f32` in the export ABI (bit patterns
   instead; known since the Tensor port).
5. **`.vsys` imports are visible only in the root module.** `fastmath.ves`
   cannot name `Crmath` (`E0111 module self has no item 'Crmath'`), so the
   crmath fallback sat in `lib.ves` (moot since part 3 removed the shim).
6. **No 128-bit integers** (part 3): bignum limbs are 32 bits because
   algorithm D needs a double-word by single-word division.
7. **A divergent edit that never terminates** (part 3) is not a usable
   check, and nothing in the harness bounds it; the check list avoids such
   edits.
8. **A read of an `inout` argument in a sibling argument is refused**
   (part 4): `gset(inout g, i, c, gm(in g, j, c))` fails `E0301` even
   though the read finishes before the call. Workaround: bind the read to a
   `let` first.
9. **No fixed-size local arrays** (part 4): small scratch matrices live in
   `ChunkSeq`s, with a bounds check and header load per access.

Std candidates: the total slice getters in `util.ves`.

## Not ported yet (later parts)

`tile.rs` with `world.rs` (the step, Morton cells, batching),
`rigid.rs` and `rigid/**`, `soft.rs` (their `std::thread::scope` fan-out
becomes `parallel_for` from PR #1062, merged 2026-11-18, with 1/2/4-worker
bit identity), `articulation.rs`, `couple.rs`, `diff.rs`, `rigid_diff.rs`,
`rollback.rs` (source atomics from #1052) and `snapshot.rs`.

## Platforms

Local: Linux x86_64 (Cargo, both packages, every test; the eight divergence
checks; `cargo +1.100.0 clippy --all-targets -D warnings` on
`vesper-kinetic`, `vesper-kinetic-seed`, `vesper-kinetic-gpu` and
`vesper-gpu-compute`). CI: Bazel on BuildBuddy, macOS AArch64 and Windows
x86_64 MSVC through the required checks of the PR, and the Cargo port harness
on the Linux shard 3.
