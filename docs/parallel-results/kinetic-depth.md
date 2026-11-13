# Wave six lane 3: Kinetic depth (§36)

Claim: #245. Baseline: `07322e3ba47890e939a48967a1ea8f62fdd7aac1` (origin/main at
start, a descendant of the required `cda092f`). Branch:
`claude/project-thread-mr4ab4`. Earlier report: `docs/parallel-results/kinetic.md`.

Edited paths: `crates/vesper-kinetic/**`, `lean/KineticColoring.lean`,
`lean/KineticColoring/Fixture.lean`, this report, and the root `Cargo.lock`
(own commit, see "Root patch").

## What is implemented

| Item | Where | Evidence |
| --- | --- | --- |
| 1. Correctly rounded `sin`, `cos`, `atan2`, `exp`, `log` (f32 and f64), DetFloat allowlist | `src/crmath.rs`, `src/detfloat.rs` | `tests/crmath.rs` (3), `tests/detfloat.rs` (+1) |
| 2. 16-lane constraint batches over constraint AoSoA tiles | `src/world.rs` (`SolverMode::Batched16`) | `tests/golden.rs` (3) |
| 3. 64-bit Morton LBVH broadphase (Karras) | `src/lbvh.rs`, `BroadphaseMode::Lbvh` | `tests/lbvh.rs` (4), `tests/golden.rs`, `tests/world.rs` (+1) |
| 4a. GJK distance and EPA penetration | `src/gjk.rs` | `tests/gjk.rs` (5) |
| 4b. Coulomb friction (static/dynamic) and restitution in XPBD contacts | `src/world.rs` (`ContactMaterial`) | `tests/contacts.rs` (5), `tests/golden.rs` |
| 5. Region-scoped step arena with `vesper-alloc` | `World::step_in`, `World::step_arena_config` | `tests/alloc_arena.rs` (1), `tests/world.rs` (+1), `tests/golden.rs` |
| 6. Reverse-mode XPBD gradients as `vesper-tensor` host tensors | `src/diff.rs` | `tests/diff.rs` (4) |
| 7. Lean Morton-21 and LBVH key-order lemmas | `lean/KineticColoring.lean` | `tests/lean_model.rs` (2) |

### Public API (all additive)

- `crmath::{sin, cos, exp, log, atan2, sinf, cosf, expf, logf, atan2f, SYMBOLS, precision_cap_hits}`;
  `DetFloatPolicy::with_correctly_rounded_math()` adds the ten `vesper_crmath_*` symbols to
  `allowed_float_calls`.
- `SolverMode::{Scalar, Batched16}`, `BATCH_WIDTH`, `World::{solver_mode, set_solver_mode}`.
- `BroadphaseMode::{MortonCells, Lbvh}`, `World::{broadphase, set_broadphase}`;
  `lbvh::{Lbvh, Aabb, Node, Child, LbvhError, morton64, demorton64, spread21, compact21, brute_force_pairs, MORTON64_AXIS_BITS, MORTON64_AXIS_MAX}`.
- `ContactMaterial { static_friction, dynamic_friction, restitution }`,
  `World::{contact_material, set_contact_material}`.
- `World::step_in(&mut Region<B>)`, `World::step_arena_config() -> RegionConfig`.
- `gjk::{Shape::{Sphere, Capsule, Polytope}, Shape::support, separation, distance, intersects, Separation, GjkError}`.
- `diff::{DiffScene, DiffParticle, DiffConstraint, simulate, gradients, Gradients, Gradients::to_tensors, DiffError, FinalState, MAX_TAPE}`.
- `StepError` gained `Arena(RegionError)`, `Broadphase(LbvhError)` and
  `InvalidMaterial(&str)`. That is the only change to an existing type; a
  downstream exhaustive `match` on `StepError` would need new arms (there is
  none in the workspace).

## 1. Correctly rounded math

`crmath` evaluates each function in exact integer fixed-point arithmetic
(`num-bigint` 0.4.8, already in `Cargo.lock`) with an explicit absolute
error bound per evaluation, rounds both ends of the error interval, and
returns when they agree; otherwise it doubles the precision (Ziv's strategy,
as in MPFR). Argument reduction is exact: `x mod π/2` uses π computed to
`f + lead(x) + 16` bits, so `sin(2^1023)` reduces correctly; `exp` reduces by
`k·ln 2`; `log` folds the mantissa into `[1/√2, √2)` and uses
`2·atanh((m−1)/(m+1))`; `atan2` reduces with two half-angle steps and
handles every C99 Annex F special value. Inputs are decoded into integers
and the result is assembled by integer rounding code, so no hardware float
operation influences the bits: the output is a function of the input bits
only, on every target. The f32 functions run the same evaluator with the
binary32 rounding target (the f32 input is exact in f64).

Decision: this is not a port of CORE-MATH. Binding CORE-MATH's C needs a C
toolchain in every build (Bazel, Cargo on MSVC; CORE-MATH also uses GCC
builtins MSVC lacks), and the pure-Rust ports we checked (`pxfm` 0.1.30)
describe themselves as "mostly" below 0.5 ULP rather than correctly rounded
on every input, which would not give cross-ISA bit equality. The cost is
speed:

| Function | ns/call (release, x86_64 cloud host, concurrent with a workspace build) |
| --- | --- |
| `sin` / `cos` | 6,500 / 6,600 |
| `exp` / `log` | 2,250 / 3,870 |
| `atan2` | 7,000 |
| `sinf` / `expf` | 4,350 / 1,600 |
| platform `f64::sin` | 7 |

Every call allocates (bignum limbs, a per-thread constant cache). These are
for DetFloat code that needs reproducible transcendentals, not hot loops;
the XPBD solver still calls none.

Evidence:

- `tests/data/crmath_refs.txt`: 4,888 references (all ten functions, at
  least 300 each) from `mpmath` at 400 and 700 bits, rounded exactly with
  Python integers, the two precisions required to agree
  (`tests/data/gen_crmath_refs.py`). Includes subnormals, values near
  overflow/underflow, `1 ± ulp`, huge trigonometric arguments (`1e22`,
  `2^1023`, the Kahan–McDonald argument `6381956970095103·2^797`) and
  multiples of π. All match bit for bit.
- Locally, a further 64,374 references (`SEED=7 N=4000`, passed through
  `VESPER_CRMATH_EXTRA_REFS`) also matched bit for bit, with no precision
  cap hits.
- Annex F specials (`exp(±inf)`, `log(±0)`, `sin(-0)`, the eight `atan2`
  zero/infinity cases, `3π/4` rounded from the exact value).
- `output_sweep_hash_is_identical_on_every_architecture`: SHA-256 of 4,000
  outputs over a pseudo-random sweep, committed as
  `31a7d9a5e658c80918e6440949c62124f304e7b3dcf33093bce5084f28daaef6`.
  CI runs it on Linux x86_64, macOS AArch64 and Windows x86_64.

What is not proved: the error bounds are argued in comments, not
machine-checked; a wrong bound could in principle return a misrounded
result in a near-halfway case. The 69,262 reference comparisons are the
check. The DetFloat allowlist is by symbol name only; nothing yet makes the
native backend emit calls to these symbols (integration B).

## 2. 16-lane constraint batches

`SolverMode::Batched16` walks each colour once, gathering up to 16 two-body
constraints (joints and sphere contacts) into one lane tile and ground
contacts into another, solving each tile with lane loops over `[f32; 16]`
columns (distance, coincident fallback, `Δλ`, normal, corrections), then
scattering the active lanes. Every lane performs exactly the scalar
`World::project` operation sequence (including the NaN-sensitive
comparisons), and the constraints of one colour touch disjoint bodies, so
the batch order cannot change a bit. Portable lane arrays only; no
`std::arch` and no `unsafe`.

Evidence: `batched_solver_lbvh_and_arena_match_the_scalar_golden_hash` runs
the golden scene (600 ticks) in all eight combinations of solver mode,
broadphase and arena and requires the committed wave-five golden SHA-256
and BLAKE3 values, which are unchanged by this lane. With friction and
restitution on, scalar and batched agree bit for bit and are pinned at
`d9c065c8b8e798da74a4bb566108d1ad0d3656ce7e7610e2b6e0bb5674928d5d` (300 ticks).

Measured (release, 1,000 resting spheres, same noisy host): Scalar 2.77 ms,
Batched16 3.25 ms per tick with the cell broadphase; 0.49 ms and 0.48 ms with
the LBVH. Batching does not speed the solver up yet: gather/scatter through
the AoSoA body tiles dominates, and the compiler is not required to
vectorise the lane loops. Fiber parallelism over colours (§36.3) is not
implemented.

## 3. LBVH broadphase

Centroids are quantised to 21 bits per axis over the centroid bounds and
interleaved into 63-bit Morton codes. Leaves are sorted by `(code, index)`,
a strict total order, so the sort and tree are deterministic with duplicate
codes. The Karras 2012 construction builds the `n − 1` internal nodes with
`δ(i, j)` = common prefix length, ties between equal codes broken by
`64 + clz(i ⊕ j)`. Bounds are filled children-first from a pre-order list;
the pair query traverses from each leaf and reports a pair once, from its
lower sorted leaf, then sorts the output. All storage is reserved by
`Lbvh::with_capacity`.

In the world, `BroadphaseMode::Lbvh` builds boxes of half-extent
`radius + sweep + margin/2`, inflated by `1/128` relative plus `2^-20 ×
(max |coordinate| + 1)` so box rounding can never drop a pair, then applies
the same exact reach test as the cell broadphase. It has no search-radius
limit, so bodies too fast for the bounded cell search still find their
partners (`lbvh_broadphase_has_no_search_radius_limit`).

Evidence: `tests/lbvh.rs` compares against the brute-force pair set for
0, 1, 2, 17, 64, 300 and 600 random boxes (dense and sparse), checks that
the tree visits each leaf and node exactly once, that 40 identical boxes
sort by index and give all 780 pairs, Morton64 against a bit-by-bit
reference, and typed errors for NaN, inverted boxes, capacity and pair
overflow. In the world the LBVH reproduces the golden hash exactly.

The Ignis GPU broadphase stays unavailable: there is no GPU in CI or on
the cloud host, and nothing here claims it.

## 4. GJK/EPA, friction and restitution

`gjk` treats every shape as a core (point, segment, vertex set) plus a
radius. GJK runs on the cores; the closest point of each simplex comes from
checking every sub-simplex (Johnson's enumeration solved with partial
pivoting), and separated shapes get `core distance − radii`, normal and
witness points. When the cores overlap, EPA runs on the cores' polyhedral
Minkowski difference, so it terminates exactly, and the radii are added to
its depth. A flat core difference (two spheres' centres, crossing capsule
segments) gives zero core depth along its normal; for two flat polytopes
with no radius that is `GjkError::Degenerate`. Binary64 throughout, no FMA.

Evidence (`tests/gjk.rs`): sphere pairs (separated, off-centre penetration,
concentric) to 1e-9; 300 random box pairs against the exact AABB distance,
witness gap, and penetration depth (smallest axis overlap); 300 sphere-box
cases inside and outside; 150 random capsule pairs against a segment
distance reference; crossing and parallel capsules; typed errors.

The world's colliders are still spheres; GJK/EPA is a library narrowphase,
not yet wired to capsule or polytope colliders in `World`.

`ContactMaterial` adds position-level Coulomb friction (Macklin et al.
2014): after each contact's normal correction `corr`, the relative
tangential displacement of the substep is cancelled if it is below
`μ_s · corr`, otherwise reduced by `μ_k · corr`, shared by inverse mass. The
velocity pass (Müller et al. 2020 §3.6) sets each pushing contact's normal
relative velocity to `−e · v̄n` of the pre-substep approach speed, with
`e = 0` when that approach speed is below `2·|g|₁·h`. All zero (the default) skips both, which is why
the golden hash is unchanged.

Evidence (`tests/contacts.rs`, both solver modes where relevant): a ball on
a tilted-gravity incline stays put when the pull is `0.3 g < μ_s g` and
slides with acceleration `(0.8 − 0.3) g` (within 8%) when it exceeds it; a
ball sliding at 3 m/s with `μ = 0.5` stops within 8% of `v²/(2μg)`; a drop
rebounds to `e² ×` the drop height (within 10%) for `e = 0.5, 0.8`, and does
not bounce for `e = 0`; out-of-range coefficients are typed errors.

## 5. Region step arena

`World::step_in(&mut Region<B>)` takes a watermark, allocates the step's
scratch (Morton cell keys, contacts and per-contact state, one 16-aligned
byte block cast with `bytemuck`) in the region, runs the same code as
`World::step`, and rewinds to the watermark before returning, on success or
failure. `step_arena_config()` sizes a bounded single-chunk region for the
world. `World::step` keeps the construction-time buffers (so `World` stays
`Send`; a `Region` is not). The colourer, LBVH and sweep buffers remain
construction-time reservations: they persist across steps.

Evidence: `region_arena_steps_do_not_allocate_in_steady_state` (own test
binary, counting global allocator with a self-check) runs 120 steps of the
golden scene through `step_in` with Batched16, LBVH, friction and
restitution after 30 warm-up steps: 0 allocations, and the arena ends with
0 used bytes, 0 tracked allocations, one chunk and unchanged reservation.
A too-small arena gives `StepError::Arena` and leaves world and arena
unchanged. The golden test runs both arena and non-arena paths.

## 6. Gradients

`diff` records the XPBD substep (the World's formula in binary64:
integrate, one Gauss–Seidel pass over distance constraints in list order
with `Δλ = −C/(w_a + w_b + α/h²)`, `v = (x − x_prev)/h`) on a scalar tape and
sweeps it backwards. `gradients(scene, seed_pos, seed_vel)` returns
`dL/dx0`, `dL/dv0` and `dL/dα` for `L = Σ seed·(x_T, v_T)` (any loss supplies
its seeds), and `to_tensors()` exports them as `vesper-tensor` `Tensor::vector`
f32 host tensors named `d_loss_d_pos0`, `d_loss_d_vel0`, `d_loss_d_compliance`.
Contacts are excluded: their activation is not differentiable. Degenerate
(coincident) constraints and oversize tapes are typed errors.

Evidence (`tests/diff.rs`): for a 4-link pendulum plus a free pair over
30 ticks × 4 substeps, every component of `dL/dx0`, `dL/dv0` and all five
`dL/dα` match central finite differences to `1e-6 + 1e-5·|g|`; the forward
model tracks the binary32 `World` for one joint within 1e-4 over 20 ticks.

## 7. Lean

`lean/KineticColoring.lean` (model id bumped to
`vesper.kinetic.coloring-morton-model/2`) adds, about its own `Nat`
definitions:

- `morton_21bit_roundtrip`, `morton_21bit_injective`, `morton_21bit_lt`: the
  bit-by-bit interleave of 21-bit axes deinterleaves exactly, is injective,
  and fits in 63 bits.
- `sorted_prefix_between` and `monotone_keys_prefix_range`: in a sorted
  array, if two keys agree on every bit at position `k` and above, so does
  every key between them; the keys sharing a prefix form one contiguous
  index range, which is what each Karras internal node covers.
- `augKey_lt_iff`, `augKey_injective`, `augKey_div`: the tie-broken key
  `code · 2^32 + index` orders by code then index, is injective, and its
  bits from 32 up are the code, so the sort order is unique.

The axiom report lists only `propext`, `Quot.sound` and `Classical.choice`
(15 theorems audited). The Rust test also feeds 204 random 21-bit triples
to the executable model and requires `interleave 21` to equal
`lbvh::morton64`. That is a differential test, not a proof about the Rust
code (the magic-mask `spread21`, the sort, or the Karras split search);
nothing here proves the tree construction itself or any floating-point
property.

## Root patch

Own commit (`root: Cargo.lock ...`): `Cargo.lock` gains five dependency
edges for `vesper-kinetic` (`bytemuck` 1.25.2, `num-bigint` 0.4.8,
`num-traits` 0.2.19, all already locked, plus the path crates
`vesper-alloc` and `vesper-tensor`). No new external packages, no
`Cargo.toml` or Bazel change (Bazel derives dependencies from Cargo).

## Checks

Rust 1.97.0, Lean 4.24.0 (GitHub release tarball):

- `cargo fmt --all -- --check`: clean.
- `cargo clippy -p vesper-kinetic --locked --all-targets -- -D warnings`: clean.
- `VESPER_LEAN=lean cargo test -p vesper-kinetic --locked`: 73 tests pass
  (debug; the golden file takes about 13 s).
- `cargo clippy --workspace --locked --all-targets -- -D warnings` and
  `VESPER_LEAN=lean cargo test --workspace --locked`: see the PR.

## Review fixes

Codex review of the first push found three issues, all fixed with
regression tests: the restitution low-speed cutoff looked at the
post-projection normal speed, so an impact that projection fully cancelled
never bounced (now uses the approach speed; the friction golden hash was
re-pinned); GJK treated separations below an absolute `1e-9` as overlap, so
tiny disjoint shapes reported a collision (now only a zero core distance,
with purely relative tolerances, counts); and EPA's flatness test could
mistake a long thin hull for a flat one when every fixed probe direction
picked its two endpoints (it now probes the directions perpendicular to the
line and plane found so far).

## Remaining gaps

- crmath is slow (microseconds per call) and allocates; no fast path with a
  proven error bound. Its error bounds are argued, not proved.
- No compiler `eff [DetFloat]` effect or native-backend binding of the
  `vesper_crmath_*` symbols (integration B, lane 15).
- Batched16 is not faster than scalar yet; no `std::arch` kernel and no
  fiber parallelism across colours.
- GJK/EPA is not wired into `World` colliders (spheres only); no contact
  manifolds, rotation, angular dynamics, articulated or soft bodies.
- Friction is position-level per contact with a world-wide material; no
  per-collider materials or rolling friction.
- Gradients cover joints only (no contacts) and use list order rather than
  the World's colour order.
- No Ignis GPU broadphase (no GPU available).
