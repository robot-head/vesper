# Kinetic batches and contacts (wave seven, lane 05)

Claim: #328. Prompt: `docs/agent-prompts/wave-seven/05-kinetic-batches-contacts.md`.
Base: `origin/main` at 283c811. Branch: `claude/project-thread-p3c2l2`.
All paths touched are under `crates/vesper-kinetic/**` plus this report. No
root patch: no new crate, no new dependency, `Cargo.lock` unchanged.

## What was built

### 1. std::arch Batched16 kernels and colour-parallel solve (`src/simd.rs`, `src/world.rs`)

- `SimdBackend::{Portable, Avx2, Avx512, Neon}` with runtime detection
  (`is_x86_feature_detected!`, `is_aarch64_feature_detected!`).
  `World::set_simd_backend` returns the typed `SimdUnavailable` for a backend
  the CPU lacks. The default is `SimdBackend::detect()`.
- One generic kernel (`pair_kernel`, `ground_kernel`) over a `Lanes` trait,
  instantiated in `#[target_feature]` functions for AVX2 (2×8 lanes),
  AVX-512F (1×16) and NEON (4×4). The portable instantiation is the scalar
  reference. Each kernel uses only IEEE add, sub, mul, div, sqrt, min/max and
  compares: no FMA, no reciprocal estimates. That is why every backend is
  bit-identical to the scalar reference. `unsafe` exists only in `simd.rs`,
  and the crate denies it everywhere else.
- `SolverMode::Batched16` now builds a per-tick `BatchPlan` after colouring:
  AoSoA tiles of 16 constraints per colour, gathered into `PairLanes` or
  `GroundLanes`, solved by the kernel and applied serially in plan order.
- `World::set_solver_threads(1..=64)` splits each colour's tiles across
  scoped threads. There is one scope per substep, a spin barrier between
  colours, and the apply step stays serial in plan order, so the results are
  bit-identical for any thread count. Below `PARALLEL_MIN_CONSTRAINTS` (256,
  adjustable with `set_parallel_threshold`) the solve stays on one thread.
- `World::last_timings()` returns a `StepTimings` with broadphase,
  colouring, solve and total time.

### 2. GJK/EPA rigid bodies (`src/rigid.rs`, `src/gjk.rs`)

`RigidWorld` is a separate world type. `World` keeps its frozen tile layout
and golden hashes, which were unchanged by any of this work.

- Colliders: `ColliderShape::{Sphere, Box, Capsule (y axis), ConvexHull (≤ 64
  vertices)}`. Each is a core plus a rounding radius, collided with the
  existing GJK/EPA.
- Manifolds of up to 4 points (`ContactManifold`, normal from b to a). The
  points come from the GJK/EPA closest pair, plus the vertices of each body
  against the other's support plane, filtered by a GJK footprint check and
  reduced to 4. Anchors sit on the shape's core with a per-manifold radius,
  so a rolling sphere's contact stays under its centre.
- Orientation and angular velocity, using Müller et al. 2020 rigid XPBD:
  substeps, the generalized inverse mass, positional impulses on x and q,
  and velocities taken from the projection correction only. A body nothing
  touches keeps its velocities bit for bit, and free spin is conserved to
  1e-4. There is no gyroscopic term.
- Per-collider `RigidMaterial`: static friction, dynamic friction,
  restitution and rolling friction. Pairs combine friction by the mean and
  restitution by the max. The ground plane has its own material.
- Patch friction at the manifold centroid:
  - static friction is applied in the position pass while the impulse is
    below μ_s·Σλ;
  - dynamic friction applies an impulse of min(μ_d·Σλ/h, the impulse that
    stops the slide);
  - torsional friction acts at the patch's mean radius;
  - rolling friction is bounded by μ_r·Σλ/h.
- Restitution is per point from the approach speed before the substep, with
  a bounce floor of 2·|g|₁·h.
- The broadphase is LBVH or sort-and-sweep, and colouring uses the existing
  `Colorer`.
- Typed `RigidError`s cover invalid bodies, colliders and materials, unknown
  collider indices, hulls that are too large, manifold overflow (a failed
  step changes nothing) and narrowphase failures.
- The world hash is SHA-256 plus BLAKE3 over 76-byte body records, and it is
  pinned in `tests/rigid.rs`.
- GJK now detects stalled progress. Before, nearly aligned stacked boxes
  returned `NoConvergence`. Now a stall at distance² ≤ 1e-12·size² counts as
  contact, so EPA runs (`tests/gjk.rs`).

### 3. Contact gradients in the World's colour order (`src/diff.rs`)

- `DiffOptions { order: DiffOrder::{List, WorldColors}, contacts }`, with
  `simulate_with` and `gradients_with`.
- `DiffContacts` adds sphere-sphere and sphere-ground contacts. It uses the
  World's reach test with piecewise derivatives on the reverse-mode tape.
- `ContactGradients` returns gradients with respect to the initial state,
  each radius and the contact compliance, plus the contact activations.
- `DiffOrder::WorldColors` projects in the same greedy colour order as
  `World`, so the tape differentiates the schedule the World runs.

### 4. GPU-shaped broadphase on the CPU (`src/sap.rs`)

- `SweepAndPrune` is a data-parallel sort-and-sweep written as workgroup
  dispatches (`WORKGROUP = 256`) over flat arrays:
  - an LSD radix sort (histogram, blocked exclusive scan, stable scatter) on
    sortable u32 keys;
  - a count/scan/write sweep.
- Keys are compressed to 32 bits for n ≤ 65,536, otherwise sorted in 8
  passes.
- It is exposed as `BroadphaseMode::SweepAndPrune` with typed `SapError`s
  (invalid box, capacity exceeded, pair overflow). It makes zero allocations
  per step once warmed up (counting allocator in `tests/alloc_arena.rs`).
- **GPU execution is deferred.** The cloud host and CI have no GPU, so the
  kernel runs on the CPU as the reference.

### 5. Fast deterministic transcendentals (`src/fastmath.rs`)

- `exp`, `log`, `sin`, `cos` and `atan2` in binary64 and binary32 use fdlibm
  algorithms: Cody–Waite reduction and the standard kernels. They are built
  from basic IEEE operations only, so their bits are identical on every
  target.
- Contract: within `F64_MAX_ULP = 1` or `F32_MAX_ULP = 1` of the correctly
  rounded value. For |x| ≥ 1,647,099, sin and cos fall back to
  `crmath::sin`/`cos`.
- The bound is checked against `crmath`, the correctly rounded path, in
  `tests/fastmath.rs`. The default is 1,500 samples per function;
  `VESPER_FASTMATH_SAMPLES` raises it. The tests also check Annex F special
  values and a golden sweep hash.
- `DetFloatPolicy::with_fast_deterministic_math()` allows these symbols in
  the DetFloat audit.

## Acceptance checks

- **Golden hashes, scalar vs SIMD vs parallel**:
  `tests/golden.rs::simd_backends_and_threads_match_the_golden_hashes` runs
  every available backend with 1, 2 and 3 threads at parallel threshold 1,
  plus SweepAndPrune. All runs match the existing pinned hashes: tick-1
  `43dd82d1…`, final SHA-256 `d0005b68…`, BLAKE3 `462d71d0…`, friction
  `d9c065c8…`.
- **Kernel equality**: `tests/batches.rs` checks that every backend's
  kernels are bit-identical to the portable reference on random and
  coincident lanes. It also checks a dense-pile golden
  (`6a6bd491…`) for every backend and thread count.
- **Across CI OSes**: the same pinned hashes are asserted on Linux x86_64
  (BuildBuddy), macOS AArch64 (NEON) and Windows x86_64. The rigid golden
  `dbfa41ed…` is the same in debug and release locally. CI on the other OSes
  is the cross-architecture check.
- **Rigid behaviour** (`tests/rigid.rs`, 12 tests):
  - a box rests flat on a 4-point manifold;
  - a 3-box stack stays up without creeping;
  - a tilted box falls onto a face;
  - a sliding box stops at the Coulomb distance v²/(2μg);
  - static friction holds below the limit on an incline;
  - per-collider materials change the outcome;
  - a ball slips into rolling at exactly 5/7 v₀ (measured 1.4282 vs
    1.4286), and rolling friction stops it;
  - restitution bounces to e² of the height;
  - a capsule lies on 2 points and a hull comes to rest;
  - a ball rests on a static box, and spin is conserved in free flight;
  - malformed input gives typed errors;
  - a mixed scene is bit-identical across both broadphases and matches the
    pinned golden.
- **Gradients**: `tests/diff_contacts.rs` checks against finite differences
  in both orders (tolerance 5e-6 + 5e-5·rel). It also checks that
  WorldColors agrees with `World` within 2e-3, and covers the typed errors.

## Measurements

These are timings, not proofs. Host: cloud VM, Intel Xeon @ 2.10 GHz,
4 vCPU, AVX-512F available, rustc 1.97.0 `--release`. Medians come from
`tests/bench_batches.rs` (ignored by default):
`cargo test --release -p vesper-kinetic --test bench_batches -- --ignored --nocapture`.

| What | Result |
|---|---|
| Pair kernel, one 16-lane batch | AVX-512 50 ns, AVX2 61 ns, Portable 160–231 ns (≈ 3.2–4.6× for AVX-512) |
| Solve phase, 4,096-sphere pile | Scalar 1.26–1.69 ms, Batched16 AVX-512 0.88 ms |
| Solve phase, 16,384-sphere pile | Scalar 5.4 ms, Batched16 AVX-512 4.1 ms |
| Broadphase, 4,096 spheres | LBVH 3.9 ms, SweepAndPrune (CPU) 8.5 ms, MortonCells 16.5 ms |
| Solver threads 2 and 4 | Little or no gain on this 4-vCPU host (see decisions) |
| Transcendentals per call | fastmath 7–10 ns, std 6–10 ns, crmath 3.2–6.5 µs |
| fastmath bound, 2M samples per function | f64 always within 1 ULP, correctly rounded 83–98% of the time; f32 correctly rounded in every sample |

At these sizes the broadphase dominates the whole step. The SIMD speed-up
is real in the kernel and the solve phase, but smaller per tick.

## Decisions

- **RigidWorld is separate from World.** World's tile layout and golden
  hashes are frozen by earlier lanes. Adding orientation to its tiles would
  have changed every pinned hash.
- **The patch friction model.** Friction acts once per manifold, at its
  centroid, bounded by the patch's total normal impulse. Per-point friction
  was about 3× too weak because each point saw only its own share of the
  impulse.
- **Velocities from the correction only** (v += Δx/h and ω from Δq against
  the integrated pose). Re-deriving ω from the whole substep rotation drifted
  2.5e-4 per second in f32.
- **Deterministic threading.** Threads compute tile updates in parallel, but
  the updates are applied serially in plan order, so hashes do not depend on
  the thread count. Spawning once per substep and using a spin barrier beat
  spawning per colour with futex barriers. Even so, on this 4-vCPU host
  threads give little: colours are small, and the serial apply and barriers
  dominate.
- **No GPU.** The broadphase is written in GPU shape (dispatch, scan,
  scatter) and runs on the CPU. On the CPU it is slower than the LBVH.

## Remaining gaps

- No GPU execution: there is no GPU on the host or in CI.
- Thread parallelism is correct and deterministic but brings no measured
  speed-up here. The parallel path allocates every substep, for the
  scoped thread spawns and the list of output slots. The zero-allocation
  claim covers only the single-thread warmed-up steady state, with both
  broadphases.
- RigidWorld has no joints, no gyroscopic term, no CCD and no sleeping. At 8
  substeps a 3-box stack jitters at a few cm/s without creeping, and the
  jitter is smaller at 16 or 32 substeps. Capsules are y-aligned in body
  space.
- Contact gradients cover spheres (pairs and ground), not RigidWorld's
  boxes, capsules or hulls.
- The Lean model (`lean/KineticColoring*`) was not extended. Nothing new
  here is proved, and the existing Lean results are about the model, not
  about the Rust code.
- The NEON kernel was not run locally. It runs only on macOS AArch64 CI.

## Commands run

```
cargo fmt -p vesper-kinetic --check
cargo +1.100.0 clippy -p vesper-kinetic --all-targets --locked -- -D warnings
cargo test -p vesper-kinetic --locked            # debug, ~60 s
cargo test --release -p vesper-kinetic --locked
cargo test --workspace --locked
cargo test --release -p vesper-kinetic --test bench_batches -- --ignored --nocapture
VESPER_FASTMATH_SAMPLES=2000000 cargo test --release -p vesper-kinetic --test fastmath
```
