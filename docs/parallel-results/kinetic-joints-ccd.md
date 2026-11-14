# Kinetic joints, CCD, sleeping, articulated and soft bodies, rigid contact gradients (wave eight, lane 23)

Claim: #472. Prompt: `docs/agent-prompts/wave-eight/23-kinetic-joints-ccd.md`.
Base: `origin/main` at 1094e649c95e4d81eef4e705d13882ab1339542d (a descendant of
the required ea1ed29). Branch: `claude/project-thread-g8cswf`. Codex's PR #434
(the Kinetic clippy fix this lane waited on) was already merged into that base.

All paths touched are under `crates/vesper-kinetic/**`, `lean/KineticColoring.lean`,
`lean/KineticColoring/Fixture.lean` and this report. No root patch: no new
crate, no new dependency, `Cargo.lock` unchanged.

## What was built

### 1. Joints, gyroscopic term, sleeping islands, CCD (`src/rigid.rs`, `src/rigid/`)

Every new rigid feature is off or absent by default. With all of them off
the world steps to the same bits as before, and the wave-seven rigid golden
`dbfa41ed…` is unchanged.

- **Joints** (`rigid/joints.rs`, `RigidWorld::add_joint`): `RigidJoint` with
  `JointKind::{Ball, Hinge { axis_a, axis_b, limits }, Prismatic { axis_a, limits }}`,
  positional and angular compliance, world anchors (`b == None`) and
  `collide_connected`. Each joint is a set of XPBD parts (Müller et al. 2020
  §3.3) projected once per substep:
  - hinge: axis alignment `Δq = a_a × a_b`, then the angle limit (angle from
    `fastmath::atan2f`, target from `sinf`/`cosf`, so it stays
    deterministic), then the anchors;
  - prismatic: relative-orientation lock, then the anchor offset
    perpendicular to the axis and clamped to the limits along it. Between
    two bodies the positional impulses both act at b's anchor, so a's arm
    carries the axial separation (the Jacobian term of the turning slide
    axis) and a free pair keeps its angular momentum;
  - ball: the anchors.

  Joints are coloured together with the contact manifolds (they follow the
  manifolds in the constraint list), so a colour's joints and contacts touch
  disjoint bodies. Adding a joint wakes the bodies it binds.
`joint_force(i)` reports `λ/h²`. Malformed joints give
  `RigidError::InvalidJoint`: unknown or repeated bodies, two static bodies,
  non-finite anchors, negative compliance, zero axes, bad limits.
- **Gyroscopic term** (`rigid/gyro.rs`, `set_gyroscopic`): implicit
  backward Euler with one Newton step in the principal frame (Catto, GDC
  2015). The 3×3 solve is Cramer's rule in fixed-order binary32. A body with
  a locked axis is skipped. `angular_momentum()` reports `L` about the origin.
- **Sleeping islands** (`rigid/sleep.rs`, `set_sleeping(SleepConfig)`):
  - Union-find over the tick's manifolds and joints between dynamic bodies;
    the smaller root wins, so the forest does not depend on visit order.
  - An island sleeps once every member has been below the speed and spin
    thresholds for `ticks` ticks. Its velocities are zeroed, it is not
    integrated, and its constraints are not solved.
  - Collision between two sleeping bodies is deferred. A waking island
    collides the deferred pairs it now reaches, repeating to a fixpoint, so
    a disturbance crosses a resting pile in one tick.
  - `apply_velocity_change` and `wake` wake a body.
  - Sleep state is part of `hash()` only while sleeping is enabled.
- **CCD** (`rigid/ccd.rs`, `set_ccd(CcdConfig)`):
  - A body is "fast" when its sweep this tick exceeds `motion_fraction` of
    its smallest extent.
  - For fast pairs that are apart by more than the margin, conservative
    advancement along the ballistic path (`x + v t + ½ g t²`, orientation
    integrated like a substep) advances by `s += (d − margin/2)/(μ_a + μ_b)`,
    where `μ` bounds how far any point of the body moves per unit of `s`.
  - The manifold is built on the poses at the time of impact, and ground
    contacts get the same treatment. When no impact is found within the
    tick, the speculative contact from the start pose is kept.
  - `last_ccd_hits()` counts the hits.
- **Capsules along any axis**: `ColliderShape::Segment { a, b, radius }` and
  `segment_capsule_inertia`, which returns the principal moments and a
  principal-axes frame. `RigidBody` gained `inertia_frame` (identity by
  default; hashed only when it is not the identity).
- `det::min` was added next to `det::max` in `world.rs`.

### 2. Articulated and soft bodies

- **Articulated bodies** are XPBD chains of the joints above (no
  reduced-coordinate solver). `tests/joints.rs` runs a 10-link ball-joint
  chain of boxes from the world.
- **Soft bodies** (`src/soft.rs`): `SoftWorld` holds particles,
  `SoftConstraint::Distance` and `SoftConstraint::Volume` (tetrahedral
  `C = 6(V − V₀)` with the standard gradients), a ground plane with
  positional friction, and XPBD substeps. `tetrahedral_box` builds a
  5-tetrahedra-per-cube mesh with mirrored cells, so faces match, plus
  deduplicated edge constraints.
- **The solve is deterministic for any thread count**
  (`set_solver_threads(1..=64)`). Each colour's corrections are computed in
  parallel from the same values, because a colour's constraints touch
  disjoint particles. They are then applied serially in visiting order, so
  the result is bit-identical for any thread count and equal to serial
  Gauss–Seidel.
- **`MultiColorer`** (`src/coloring.rs`) is a greedy colourer for
  constraints over 1 to 4 bodies, given as flat body lists with offsets.
  Each colour takes the lowest free bit; constraints are visited sorted by
  (min body, max body, index). `sets_are_disjoint` checks a colouring
  independently. `ColoringError::BadArity` is new.

### 3. Contact gradients for boxes, capsules and hulls (`src/rigid_diff.rs`)

- **The model.** A binary64 rigid XPBD with the same structure as
  `RigidWorld`:
  - substepped position and orientation integration;
  - one projection per contact per substep, with the generalized inverse
    mass and compliance;
  - velocities from positions.

  It has normal contacts only: no friction and no restitution.
- **Shapes**: `DiffShape::{Sphere, Box, Capsule { a, b, radius }` (any
  axis), `Hull` (≤ 32 vertices; faces computed from the vertices)}.
- **Features** are found once per tick from the values at the start of the
  tick:
  - ground against every core point;
  - polytope against anything uses face-only SAT. The second body's face is
    the reference only when it separates by more than a tenth of the
    margin, so resting parallel faces do not switch reference. The incident
    face, or the capsule's segment, is clipped against the reference face's
    side planes (Sutherland–Hodgman).
  - a sphere or capsule whose core lies outside every face's prism (over
    an edge or a corner) gets a rounded edge contact instead: the closest
    points of its core and the nearest polytope edge, recomputed every
    substep;
  - capsule against capsule uses closest points of the segments,
    recomputed every substep.

  The clipped points are computed on the dual scalar, so they carry
  derivatives with respect to the poses at the start of the tick.
- **Forward mode.** The same generic code runs on `f64` (`simulate`) and on
  a 16-tangent dual number (`gradients`). Seeds cover all 13 state
  components per body plus the contact compliance, in chunks. Every branch
  (activation, segment clamps, feature choice) is taken on the value, so
  both runs follow the same branches.
- `RigidGradients::activations` counts projections by feature kind.

### 4. Lean model (`lean/KineticColoring.lean`, model `/3`)

- **The model.** `SetConstraint := List Nat` and `colorAllSets` (the same
  greedy choice over body lists) model the k-body batches: ground contacts
  have one body, contacts and joints two, tetrahedra four.
- **New theorems** (axiom-audited, 19 `#print axioms` in all):
  - `colorAllSets_pairwise_disjoint`: same colour ⇒ disjoint body lists;
  - `colorAllSets_constraints`;
  - `same_color_sets_disjoint_bodies`, the index form;
  - `colorAll_eq_colorAllSets`: the pair colouring gives the same colours
    as colouring two-element lists, so joints and contacts coloured as
    batches agree with the old model.
- **Fixture.** It gained `2 k b1..bk …` for length-prefixed body lists.
  `tests/lean_model.rs` compares `MultiColorer` with the model on 150
  random batches of 1–4 bodies and on the soft-body mesh's mixed
  distance/volume constraints.
- **Scope.** These are theorems about the Lean model, not about the Rust
  code; agreement on inputs is a differential test.

## Acceptance checks (all local, Linux x86_64)

| Test | What it checks | Measured |
|---|---|---|
| `joints::ball_joint_pendulum_has_the_physical_pendulum_period` | rod pendulum (end pivot, θ₀ = 0.2) against `2π√(I_p/mgd)(1+θ₀²/16)`; tolerance 0.2% | period 1.64387 s vs 1.64409 s (1.3e-4 rel.), anchor gap ≤ 6.7e-6 m, energy never gains, loses ≤ 10% of the swing energy in 10 s (measured 7%) |
| `joints::hinge_keeps_the_bob_in_its_plane` | out-of-plane kick of 1.5 m/s | z ≤ 2.0e-3 m, axis drift ≤ 4.5e-3 rad (ball joint: z > 0.2 m) |
| `joints::hinge_limits_stop_the_swing` | limits ±0.3 rad with a swing that would reach ~1.1 rad | angles stay within [−0.29995, 0.29996]; orientation agrees with position to 2e-3 |
| `joints::prismatic_slides_along_its_axis_and_stops_at_the_limit` | 45° slide | off-axis ≤ 1e-4 m, matches symplectic Euler, rests at the limit within 2e-3 |
| `joints::a_free_prismatic_pair_keeps_its_angular_momentum` | two boxes 1 m apart, one moving sideways, no gravity, 2 s | \|ΔL_z\| < 0.03 (was −50% on the first tick before the arm fix); both bodies turn at the same rate |
| `joints::articulated_chain_holds_together_and_does_not_gain_energy` | 10-link chain, 32 substeps, 4 s | worst joint gap 7.2e-4 m, energy never above the start |
| `joints::gyroscopic_term_…` | intermediate-axis (Dzhanibekov) flip | flips; worst \|ΔL\|/\|L\| = 1.1e-2; energy −2.2%, never gains |
| `sleep_ccd::a_resting_stack_falls_asleep_and_a_falling_ball_wakes_it` | 5-box stack plus a ball | asleep after 152 ticks, bit-frozen while asleep, the launched ball wakes the whole stack, which sleeps again; boxes within 1 cm |
| `sleep_ccd::sleeping_matches_…` | sleeping changes nothing before sleep; deterministic; typed errors | equal |
| `sleep_ccd::adding_a_joint_wakes_the_bodies_it_binds` | joint added to a sleeping body | wakes; pulled to the world anchor within 1e-3 m |
| `sleep_ccd::ccd_hits_the_face_at_the_time_of_impact` | 187 m/s ball past a box corner | CCD normal (−1, 0, 0), speculative normal (−0.71, 0.71, 0); after impact CCD keeps vy = −144 m/s, speculative deflects to (−12, −12) |
| `sleep_ccd::ccd_bullets_do_not_cross_a_thin_wall_or_the_ground` | four 600 m/s bullets against a 2 cm wall; spinning box at 150 m/s into the ground | no crossing |
| `rigid_features::a_capsule_along_any_axis_…` | segment capsule vs the y capsule rotated onto the same axis | positions within 2e-3 m, two-point manifold |
| `rigid_features::feature_scene_…` | every new feature at once, both broadphases | bit-identical; pinned SHA-256 `81337213…`; same in debug and release |
| `soft::a_jelly_cube_lands_and_keeps_its_volume` | 4×4×4-cell jelly (320 tets), 3 s | volume error ≤ 2.0e-4, strain 8.6e-5, rests on the ground |
| `soft::soft_solve_is_bit_identical_for_every_thread_count` | 1, 2, 3, 5 threads at threshold 1 | identical; pinned `5ac5aba4…` |
| `soft::a_rope_hangs_straight_at_its_length` | 20-link rope | strain < 1% at the whip (≈2e-4 at rest), energy damped overall |
| `diff_rigid::*` | box on the ground; tilted off-axis capsule on the ground and a box; hull on a box; capsule–capsule plus box-on-box; sphere on a box edge and capsule on a box corner | 110 derivatives against central differences (tolerance 1e-4 + 1e-3·rel); one is retaken at ε = 1e-7 because a resting contact switches within 1e-6 |
| `lean_model` (with `VESPER_LEAN`) | compile, axiom report, Rust/model differential | pass, 19 audited theorems |

The pre-existing golden hashes are unchanged: World `d0005b68…`,
dense-pile `6a6bd491…`, rigid `dbfa41ed…`. Hashes on other architectures
are checked by CI on macOS AArch64 and Windows. Every new solver path uses
`det` operations and `fastmath` only, so the same bits are expected there.

## Decisions

- **Everything off by default.** Joints are opt-in through `add_joint`,
  and the gyroscopic term, sleeping and CCD through setters, rather than
  new `RigidConfig` fields. This keeps the struct literals and the rigid
  golden unchanged.
- **The implicit gyroscopic step, not the explicit one.** The explicit form
  gains energy on the intermediate axis; the implicit one loses a little
  (−2.2% over 10 s here).
- **Removing the axial part twice in the prismatic projection.** One
  binary32 pass left a residue of about ulp(|d|) along the axis, which
  biased the slide by about 1%. The second pass makes the slide match
  symplectic Euler.
- **A rod pendulum, not a tiny bob.** A point-like bob on a 1 m lever has a
  1000:1 ratio of rotational to translational weight. One Gauss–Seidel
  pass per substep then converges slowly, and the limits did not hold
  (they did with the rod). Joint tests use well-conditioned bodies; badly
  conditioned ones need more substeps.
- **CCD keeps the speculative contact when no impact is found.** That is
  conservative: speculative contacts already stop straight-line tunnelling
  in the tests. What CCD changes is the impact feature and normal.
- **Rigid contact gradients use forward-mode dual numbers** rather than
  extending `diff.rs`'s tape. One generic model gives `simulate` and
  `gradients` with identical branches. The cost is one pass per 16
  parameters, fine for small differentiable scenes.

## Remaining gaps

- **Joints**: no motors, drives or joint friction. No reduced-coordinate
  (Featherstone) articulation; chains are XPBD and converge slowly for
  extreme mass ratios.
- **RigidWorld is single-threaded.** "Deterministic across thread counts"
  is demonstrated for the soft-body solver (and, from earlier lanes, for
  `World`).
- **Allocation.** `RigidWorld::step` and `SoftWorld::step` allocate (state
  snapshots for rollback on error, manifold candidates); there is no
  zero-allocation claim for them. The soft parallel path spawns scoped
  threads per colour per substep. It is correct, but it only pays off for
  large colours (default threshold 64 constraints).
- **No soft–rigid coupling.** Soft bodies collide only with the ground
  plane.
- **Rigid gradients**:
  - no friction and no restitution;
  - face-only SAT, so edge–edge polytope contacts are not modelled (they
    fall back to face contacts);
  - derivatives are those of the branch taken, undefined on activation
    surfaces (resting contacts sit on them);
  - they differentiate the `rigid_diff` model, which mirrors `RigidWorld`'s
    structure but is a separate binary64 program, not the binary32 world.
- **CCD** advances along the ballistic path. Contacts that change a body's
  path earlier in the same tick are not re-swept.
- **No hardware claims.** Only Linux x86_64 was run locally; macOS NEON and
  Windows results come from CI.
- **Scope of the Lean results.** They are about the model, not the Rust
  code. Ownership-based linearity here is host-library discipline, not an
  implementation of Vesper's source typing rules.

## Commands run

```
cargo fmt --all -- --check
cargo +1.100.0 clippy -p vesper-kinetic --all-targets --locked -- -D warnings
VESPER_LEAN=…/lean-4.24.0-linux/bin/lean cargo test -p vesper-kinetic --locked   # debug, all pass
cargo test --release -p vesper-kinetic --test joints --test sleep_ccd --test soft \
  --test diff_rigid --test rigid_features --test rigid -- --nocapture
(cd lean && lean -o out/KineticColoring.olean KineticColoring.lean)   # Lean 4.24.0 release zip
```

The workspace-wide `cargo test --workspace --locked` and strict clippy
cover the root crate, which uses only `DetFloatPolicy`/`audit` from this
crate (unchanged). They are CI's `Test all targets` run; see the PR checks.
