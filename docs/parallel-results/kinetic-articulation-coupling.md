# Kinetic motors, Featherstone articulation, threaded RigidWorld, soft-rigid coupling, GPU broadphase (wave nine, lane 25)

> Retained work context for open issues, unmerged branches, or their dependencies.
> The [manuscript](../manuscript/README.md) is the sole specification; use the
> [section map](../section-map.md) for earlier numbering. Confirm live ownership before acting.

Claim: #602. Prompt: `docs/agent-prompts/wave-nine/25-kinetic-articulation-coupling.md`.
Base: `origin/main` at 5a018bdd8b54f9be5974843d9bb244a207a04834. Branch:
`claude/project-thread-lki0sa`.

Paths touched: `crates/vesper-kinetic/**` (including the new crate
`crates/vesper-kinetic/gpu`) and this report. The root patch is its own
commit (see below). No Lean file changed.

Every new feature is off or absent by default. With all of them off, the
worlds step to the same bits as before, and the existing goldens are
unchanged: rigid `dbfa41ed…`, rigid features `81337213…`, World `d0005b68…`.

## What was built

### 1. Featherstone articulations (`src/articulation.rs`)

`Articulation` is a reduced-coordinate tree of up to 4096 links with a fixed
base. Each link has one revolute or prismatic joint to its parent. The
articulation has:

- **Forward dynamics:** the articulated-body algorithm (Featherstone 2008,
  ch. 7) in binary64 spatial algebra: Plücker transforms, the
  articulated-body inertia `I^A` and bias `p^A`, and the three passes.
- **Integration:** semi-implicit Euler over `substeps`.
- **Drives:** `ArticulationDrive{target_position, target_velocity,
  stiffness, damping, max_effort}`.
  - The PD drive is implicit. `k h² + c h` is added to the joint's `d_i`,
    so stiff drives stay stable.
  - The force is clamped to `max_effort`. A clamped drive becomes an
    explicit force.
- **Coulomb joint friction:** two passes.
  1. The algorithm runs without friction, which gives a free `q̈`.
  2. A joint whose rate would cross zero within the substep is held at zero
     (sticking) if the torque it needs is within `μ`. Otherwise it gets
     `−μ·sign(q̇)`.

  This is exact for one frictional joint. With several frictional joints,
  each joint is decided against the others' unfrozen motion, so the result
  is approximate.
- **Other API:** `set_joint_torque`, `set_drive`, `link_poses`, `com_world`,
  `energy`, `friction_torques` and a SHA-256 `hash`.
- **Allocation and errors:** steps do not allocate. Errors are typed
  (`ArticulationError`).

Tests in `tests/articulation.rs` (11 tests):

| Test | Tolerance or result |
| --- | --- |
| ABA vs the closed-form double-pendulum equations of motion | 1e-9 |
| ABA vs the closed-form cart-pole equations of motion | 1e-9 |
| Compound pendulum period vs the elliptic-integral period (amplitude 1 rad) | within 2e-4 |
| Energy drift under halving `dt` | first order (3.19e-2 J to 1.60e-2 J) |
| Velocity motor reaches its target | passes |
| Saturated motor accelerates at `max_effort / I` | passes |
| Servo equilibrium against gravity | passes |
| Coulomb wheel stops at `t = I ω₀ / μ` | 7.5 s |
| Friction holds below the gravity torque and slips above it | passes |
| Typed errors | passes |
| Chain golden | `a25cf9cd…` |

### 2. XPBD joint motors, servos and joint friction (`src/rigid/joints.rs`)

`JointDrive` drives the hinge angle or the prismatic slide:

- `JointDrive::Velocity{target, max_force}`: a velocity motor, solved in the
  velocity pass with an impulse bound of `max_force · h`.
- `JointDrive::Position{target, compliance, damping, max_force}`: a servo.
  It is an XPBD part with bounded `λ`, plus a velocity-pass damping term.

`set_joint_friction(j, f)` adds Coulomb joint friction with a maximum
force or torque `f`, in two places:

- **Dynamic friction** goes in the velocity pass.
- **Static friction** is positional. The coordinate is re-pinned to its
  substep start when the impulse needed is within `f h`. This runs two
  rounds, with an anchor re-pin for hinges. Velocity-pass friction alone
  let a held hinge creep under gravity; with the positional pass, the
  measured creep is 2.4e-3 rad/s.

`joint_coordinate` and `joint_rate` read a joint's current coordinate and
rate. Tests in `tests/joint_drives.rs` (9 tests):

- A motor reaches its target rate.
- A saturated motor accelerates at `max_torque / I` (1%).
- A servo holds its target against gravity, and holds only up to its torque.
- Hinge friction holds below the gravity torque and slips above it.
- Prismatic friction on a 45° incline holds the cube when `f > m g sin θ`,
  and below that the cube slides at `a = g sin θ − f / m` (0.2%).
- A prismatic motor and a prismatic servo work.
- A two-body hinge motor conserves angular momentum.
- Errors are typed.

### 3. Threaded RigidWorld narrowphase and re-sweeping CCD (`src/rigid.rs`, `src/rigid/ccd.rs`)

**Threaded narrowphase:**

- `set_solver_threads(1..=64)` splits the candidate pairs into contiguous
  chunks under `std::thread::scope`.
- Each pair's outcome (a manifold, a deferred CCD pair, or an error) is
  computed from `&self` only.
- The outcomes are then applied in pair order on one thread, and the first
  error in pair order is the one returned.

So the result is the same bits for any thread count. Tests in
`tests/rigid.rs` and `tests/rigid_features.rs` step at 2, 3, 8 and 64
threads, with the parallel threshold forced to 0, and match the goldens.
`set_parallel_threshold` sets the pair count below which the work stays on
one thread (default 32).

**Measured** in a release build on this 4-vCPU cloud host. The scene was 256
octahedral hulls on the ground (736 manifolds), timed over 60 ticks after
60 settling ticks:

| Threads | Tick time | Hash |
| --- | --- | --- |
| 1 | 3.56 ms | `43bf64d4…` |
| 2 | 3.15 ms | `43bf64d4…` |
| 4 | 2.64 ms | `43bf64d4…` |

Only the narrowphase is parallel. The solver stays on one thread (it is
coloured but sequential), which bounds the speed-up. The scene was a
temporary test, not committed.

**CCD re-sweep**, behind `set_ccd_resweep(true)`:

1. Each fast pair's time of impact (TOI) becomes a candidate.
2. The candidates are sorted by `(s bits, a, b)`.
3. They are resolved in order: the body moves to the impact, its velocity is
   redirected, and later TOIs are swept again along the overridden paths
   (conservative advancement).

`last_ccd_resweeps()` counts the re-sweeps. In `tests/sleep_ccd.rs`, a fast
ball bounces off the floor into a wall. With the re-sweep it hits the wall
at the redirected position (y ≈ 0.1 vs 0.6 without the re-sweep). This is
one TOI pass per tick: a third impact in the same tick is left to the
discrete contacts of the next tick.

### 4. Two-way soft-rigid coupling (`src/couple.rs`)

`CoupledWorld::new(rigid, soft, CouplingConfig{particle_radius, friction})`
checks that both worlds have the same `dt`. Each step:

1. The rigid world steps.
2. The soft world steps through `SoftWorld::step_with`, with a contact pass
   after the ground contact of every substep. In that pass, each particle
   (a sphere) is queried against each body by binary64 GJK/EPA. The
   penetration is projected with the generalized inverse masses
   `w_p = 1/m_p` and `w_b = 1/m_b + (r × n)ᵀ I⁻¹ (r × n)`, and positional
   Coulomb friction is applied.
3. A dynamic body is kinematic during the soft step, so its share becomes an
   impulse over `dt`. The impulse is applied after the soft step, and the
   particle's substep start is shifted so the two impulses are equal and
   opposite.

Tests in `tests/couple.rs` (5 tests):

- A jelly rests on a static box.
- A jelly holds up a rigid box.
- Linear momentum is conserved in a jelly-box collision without gravity.
- The result is the same bits for (soft, rigid) thread counts (2, 3) and
  (4, 1) as for (1, 1).
- Errors are typed.

### 5. GPU broadphase in World (`src/world.rs`, `crates/vesper-kinetic/gpu`)

**`BroadphaseMode::Gpu`** asks a `PairBackend`, a trait set with
`World::set_pair_backend`, for the overlapping pairs. The world validates
the backend's output before using it:

- The pairs must be in range, ordered `a < b`, sorted and unique, and within
  `max_candidate_pairs`.
- The backend must not report a device error.

A failure is a typed `StepError` (`NoPairBackend`, `PairBackend(..)`,
`MalformedPairs{index}`), and the world is left unchanged. The backend sits
behind a `Mutex`, so `World` stays `Sync`.

Tests in `tests/pair_backend.rs`:

- A brute-force backend reproduces the World golden `d0005b68…`.
- A corrupt backend and a failing backend give typed errors.

**The new crate `vesper-kinetic-gpu`** has `GpuPairBackend`, which runs the
`vesper-gpu-compute` LBVH broadphase on Vulkan. On Mesa lavapipe (llvmpipe)
on the cloud host, with `VESPER_GPU_REQUIRE=1`, two tests passed:

- `gpu_broadphase_reproduces_the_golden_hash`: 600 ticks are bit-equal to
  the CPU LBVH, and the final hash is the golden.
- `gpu_broadphase_capacity_errors_are_typed`.

Without a Vulkan device, the tests return early, as the `vesper-gpu-compute`
tests do.

### 6. Rigid contact gradients with friction, restitution and edge-edge SAT (`src/rigid_diff.rs`)

`DiffRigidScene` gained `static_friction`, `dynamic_friction` and
`restitution`, all validated:

- **Positional static friction** runs after each normal projection.
- **A velocity pass** applies dynamic friction and restitution, as in
  RigidWorld.

Box-box and hull pairs now also test edge-pair axes in the SAT (the new
`Feature::EdgePair`). Parallel edges share an axis, so the supporting pair
is chosen as the nearest. `activations` grew a fourth entry for edge pairs.

Tests in `tests/diff_rigid.rs` (8 tests) compare every derivative with
central finite differences:

- **A sliding, bouncing box with μs 0.6, μd 0.4 and e 0.3.** The worst scaled
  error is 0.001. Friction shortens the slide.
- **Two crossed boxes that meet edge to edge.** Only the edge-pair feature
  activates, and the box rests at the edge-contact height (2 cm tolerance).
- **The earlier scenes.**

## Checks run

- `cargo test -p vesper-kinetic -p vesper-kinetic-gpu --locked`: everything
  passes (debug), including the lavapipe tests with `VESPER_GPU_REQUIRE=1`.
- `cargo +1.100.0 clippy -p vesper-kinetic -p vesper-kinetic-gpu --locked
  --all-targets -- -D warnings`: clean.
- `cargo fmt` on both crates.

## Root patch

A separate commit (`Root patch: register crates/vesper-kinetic/gpu in the
workspace`):

- `crates/vesper-kinetic/gpu` is added to `members` and `default-members` in
  the root `Cargo.toml`.
- `Cargo.lock` is updated.
- `crates/vesper-kinetic/gpu/BUILD.bazel` is generated by
  `tools/bazel/sync_build_files.py`.

## Gaps and decisions

- **Articulations:**
  - no floating base;
  - no joint limits;
  - no spherical joints;
  - no coupling to RigidWorld contacts;
  - multi-joint Coulomb friction is approximate (see §1).
- **Soft-rigid coupling:**
  - a dynamic body reacts one tick late (the usual staggered coupling);
  - no broadphase (the cost is particles × bodies × substeps, with a
    bounding-sphere reject);
  - no soft self-collision.
- **CCD re-sweep:** one TOI pass per tick.
- **The lavapipe kinetic-gpu tests run in the GPU compute workflow.** At
  the user's request, `-p vesper-kinetic-gpu` was added to the fmt, clippy
  and test steps of `.github/workflows/gpu-compute.yml`. In the required
  checks the crate builds and its device tests skip without a device.
- **Review fixes (Codex on #858):**
  - CCD redirect now adds gravity only since each body's own path start.
  - Articulation drives saturate on the solved effort, implicit part
    included.
  - `set_joint_friction` wakes the joint's bodies.
  - Edge-pair SAT picks only edges on the support planes.

  The drive and edge-pair fixes have regression tests that fail without
  them.
- **Lean:** no Lean changes. The colouring proof is unaffected, because the
  joint drives add no constraints to the colouring.
