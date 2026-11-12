# Wave five lane 10: Vesper Kinetic (§36)

Claim: #130. Baseline: `af03122ea74d403f203170926945f9d35786b289` (origin/main at start).
Branch: `claude/project-thread-23qx7p`.

New crate `crates/vesper-kinetic` (depends on `cranelift-codegen` 0.125,
`sha2` and `blake3`, all already in `Cargo.lock`; it does not use
`vesper-simd`, which the contracts allowed but nothing needed) and
`lean/KineticColoring.lean` with its stdin fixture
`lean/KineticColoring/Fixture.lean`.

## What is implemented

| Item | Where | Evidence |
| --- | --- | --- |
| 1. DetFloat auditor over real Cranelift 0.125 functions | `src/detfloat.rs` | `tests/detfloat.rs` (13 tests) |
| 2. Branchless 30-bit 3D Morton codes, optional BMI2 path | `src/morton.rs` | `tests/morton.rs` (5 tests) |
| 3. Deterministic greedy constraint colouring | `src/coloring.rs` | `tests/coloring.rs` (4 tests) |
| 4. AoSoA tiles and XPBD substep solver, 0 alloc per step | `src/tile.rs`, `src/world.rs` | `tests/world.rs` (8), `tests/alloc.rs` (1) |
| 5. Rollback ring with linear `RollbackTicket`s | `src/rollback.rs` | `tests/rollback.rs` (4 tests) |
| 6. Cross-architecture golden hash | `tests/golden.rs` | CI on Linux x86_64, macOS AArch64, Windows x86_64 MSVC |
| 7. Lean colouring disjointness and Morton inverse | `lean/KineticColoring.lean` | `tests/lean_model.rs` (2 tests) |

### Public API

- `detfloat::audit(&Function, &DetFloatPolicy) -> Result<Vec<VerifierFinding>, AuditError>`;
  `DetFloatPolicy { allow_fma, allowed_float_calls, user_names, reduction_order, codegen_canonicalizes_nans }`
  with `with_flags(&settings::Flags)`; `ReductionOrder::{PairwiseTree, Sequential}`;
  `VerifierFinding { function, inst, rule, message }` exactly as the wave-five contracts define it.
- `morton::{encode, decode, encode_masked, decode_masked, encode_fast, decode_fast, spec_encode, spread, compact}`,
  `morton::arch::{bmi2_available, encode_bmi2, decode_bmi2}` (x86_64 only), `MortonError`.
- `Colorer::{new, color, layers, layer, layer_count, color_of, visiting_order}`, `ConstraintBodies`,
  `ColoringError`, `coloring::islands_are_disjoint`.
- `RigidBodyTile` (16 lanes, `#[repr(C, align(64))]`, the §36.2 fields), `TiledBodies`, `Body`.
- `World::{new, step, header, tick, bodies, apply_velocity_change, kinetic_energy, potential_energy, max_joint_error, max_penetration}`,
  `WorldConfig`, `DistanceConstraint`, `SphereCollider`, `StepError`.
- `SnapshotRing::{new, capture, confirm, rewind, rewind_and_resimulate, leaked_tickets, occupied, capacity}`,
  `RollbackTicket`, `RollbackError`, `ResimulateError`.
- `KineticSnapshotWireHdr::{encode, decode}` (48 bytes), `hash_world -> WorldHashes { sha256, blake3 }`.

### DetFloat auditor rules

`detfloat.fma` (the `fma` instruction or the `FmaF32`/`FmaF64` libcall unless
`allow_fma`), `detfloat.libm` (known libm transcendentals not on the
allowlist), `detfloat.unlisted-float-call`, `detfloat.indirect-float-call`,
`detfloat.reduction-order` (a full horizontal `fadd`/`fmul` tree over every
lane of one vector whose shape, modulo operand order, differs from the policy's
pairwise tree or left fold) and `detfloat.nan-escape` (a result of
NaN-producing arithmetic, the same opcode set Cranelift's own NaN
canonicalisation pass rewrites, reaching a store, return, call argument or
integer bitcast without the canonicalisation pattern; tracked through block
parameters to a fixpoint). The IR is first run through Cranelift's verifier;
invalid IR is `AuditError::InvalidIr`. A test runs Cranelift's real
`canonicalize_nans` pass for the host ISA on scalar f32, f64 and f32x4
functions and checks the auditor accepts its output (the pass emits the
`scalar_to_vector` + `bitselect` form on SIMD hosts).

Cranelift IR has no fast-math or reassociation flags and fixes round-to-nearest-even,
so there is no rounding-mode rule. The auditor checks one function's IR, not
what an ISA backend emits from it.

### Solver

Each tick: Morton-sorted cell broadphase (cell index = floor of position over
`cell_size`, clamped to the 1024³ grid, 27-neighbour search by binary
search over the sorted codes), sphere-sphere and ground-plane contacts with a
speculative margin, colouring of joints plus contacts, then `substeps` XPBD
substeps (integrate, project colours in colour order, derive velocities).
Distance joints and contacts use compliance `alpha / h²`,
`dλ = -C / max(w + α̃, 1e-9)`. All float arithmetic goes through `world::det`
helpers: single binary32 operations, `sqrt` (correctly rounded by IEEE-754),
and a `max` with a fixed answer for every input. No transcendental is called.
Rust never contracts `a * b + c`; `f32::mul_add` is never called. Measured:
building the golden test with `RUSTFLAGS="-C target-cpu=native"` on an
FMA-capable x86_64 host gives the same golden hash, and `objdump` of that
binary shows zero `vfmadd`/`vfmsub`/`vfnmadd`/`vfnmsub` instructions.

A step validates and can fail (contact overflow, colouring overflow, tick
overflow) only before it mutates bodies, so a failed step leaves the world
unchanged (tested).

Solver projects a colour's constraints one at a time (scalar). The colours
are body-disjoint, so any order or parallel schedule inside a colour gives the
same bits; the 16-wide SIMD `VVec` batch kernel and Pulse fibers of §36.3 are
not implemented.

### Rollback

`capture` copies the tiles into a slot reserved at construction (no
allocation). `RollbackTicket` is non-`Clone`, non-`Copy`, `#[must_use]`; its
only exits are `confirm`, `rewind` and `rewind_and_resimulate`. Refused calls
(foreign ticket, world shape mismatch) return the ticket. A dropped ticket is
counted by `SnapshotRing::leaked_tickets` and keeps its slot pinned, so a ring
full of leaked tickets refuses new captures rather than overwriting history.
This is host-library discipline enforced by Rust ownership, **not** an
implementation of the Vesper source typing rules for `linear struct
RollbackCheckpoint`; Rust ownership is not evidence that those rules are
implemented.

Measured on the cloud test host (x86_64 Linux, release): capture + confirm of
the 180-body test scene takes about 63 ns. The design's "10,000 bodies in
under 80 µs" was not measured.

## Cross-ISA bit determinism

`tests/golden.rs` runs the fixed scene (180 bodies: 144 stacked spheres with
sideways velocities, four 8-link pendulum chains, ground plane; up to 461
contacts and 9 colours per tick) for 600 ticks and compares the SHA-256 of
the canonical little-endian state (and the BLAKE3 the header carries) with
committed values, plus the SHA-256 after tick 1:

- tick 1 SHA-256: `36325a53fa4b30863c752ad6f88ce5e6a90ba536640c1c8aa90f5ce7988f5191`
- tick 600 SHA-256: `f8d98c3b51da4d885a0460f7059fb1850cb028caa8865a21dded2100709acc10`
- tick 600 BLAKE3: `f4c91c0d2e95d3e342d92b50e50e06a9cd9dfd3a70c2ba09f62cc8e08b25cbb0`

Locally (x86_64 Linux) the hashes are identical in debug, release and
release with `-C target-cpu=native`. The CI jobs `check` (Linux x86_64),
`macOS (AArch64)` and `Windows (x86_64 MSVC)` all run `cargo test
--workspace`, so a green CI run on this PR is the cross-ISA evidence for this
Rust code on those three targets; see the PR's CI results. It is not
evidence for RISC-V, for GPU backends, or for Vesper source compiled through
Cranelift.

## Lean

`lean/KineticColoring.lean` (model id `vesper.kinetic.coloring-morton-model/1`)
proves, about its own Lean definitions over `Nat`:

- `colorAll_pairwise_disjoint`: in the list produced by the modelled greedy
  colouring (each constraint, in list order, takes the least colour not used
  by an earlier conflicting constraint, with a provably unused fallback), any
  two constraints with the same colour share no body;
  `same_color_constraints_disjoint_bodies` states it by index;
  `colorAll_constraints`: the colouring keeps the constraints in order;
  `firstFree_not_mem`: the chosen colour is free.
- `morton_deinterleave_interleave`: deinterleaving the bit-by-bit
  interleave recovers each axis modulo `2^n`; `morton_10bit_roundtrip` and
  `morton_10bit_injective` for 10-bit inputs; `interleave_lt`: the code fits
  in `3n` bits.

The axiom report lists only `propext`, `Quot.sound` and (Morton theorems)
`Classical.choice`; no `sorry`, `admit`, `native_decide` or new axioms. The
Rust test compiles the file under `VESPER_LEAN`, checks the axiom report, and
runs the executable fixture on 336 Morton cases and 150 random colourings
(fed in the Rust visiting order) and requires identical answers. That is a
differential test, **not** a proof about the Rust code (the magic-mask
`spread`/`compact`, the BMI2 path, the bitmask colourer or the sort).
Solver stability and IEEE-754 expression determinism are not proved.

## Root patch

Own commit: `crates/vesper-kinetic` added to root `members` and
`default-members` before `experiments/native-pulse`; `Cargo.lock` regenerated
(new package entry only; no new external crates).

## Checks

Run locally with Rust 1.97.0 and Lean 4.24.0:

- `cargo fmt --all -- --check`: clean.
- `cargo clippy --workspace --locked --all-targets -- -D warnings`: see PR.
- `VESPER_LEAN=lean cargo test -p vesper-kinetic`: 38 tests pass.
- `VESPER_LEAN=lean cargo test --workspace --locked`: see PR.

## Decisions and corrections

- Design §36.2 hashes snapshots with BLAKE3; the lane prompt asks for a
  SHA-256 golden hash. `hash_world` streams one canonical byte stream into
  both; the header carries BLAKE3 and the golden test checks both.
- The §36.2 header's 48 bytes agree with its fields (8 + 4 + 4 + 32); kept.
- `RigidBodyTile` keeps exactly the §36.2 fields; per-step scratch (previous
  positions) lives in the world, and sphere radii come from a collider table
  indexed by `collider_asset_id`.
- Contact compliance is the sum of the two bodies' `compliance_alpha`.
- Colouring supports up to 64 colours (one mask bit per colour per body);
  more is `ColoringError::TooManyColors`.
- expansion-37-review correction for §36 ("DetFloat requires cross-backend
  rounding/codegen enforcement before bit-determinism claims"): this lane
  claims bit identity only for this crate's Rust solver on the three CI
  targets, backed by the golden test. The auditor checks Cranelift IR, not
  backend output, and nothing here is a compiler `eff [DetFloat]` claim.

## Remaining gaps

- No compiler `eff [DetFloat]` effect, no `DetFloat::scope`, no Stage 4/5
  codegen pinning; integration B (#136) is to run this auditor on the
  functions `src/native.rs` emits.
- No CORE-MATH correctly rounded trigonometry (the auditor only enforces an
  allowlist), no 64-bit Morton LBVH, no GJK/EPA, no articulated or soft bodies,
  no friction or restitution, no TGS beyond plain XPBD substeps.
- No 16-wide SIMD constraint batches or fiber parallelism; no `Region::scoped`
  arena (storage is reserved at construction instead).
- No Ignis GPU broadphase or `GpuBroadphaseLease`, no Tensor gradients.
