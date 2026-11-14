# Wave seven lane 21: Resonance BIBO and torque-clamp proofs, SIMD SVF and slab packing (§32)

Claim issue #344. Design coverage: §32.1 item 4 (TiledSeq slab for the
graph's scratch buffers), §32.1 item 5 (Lean BIBO filter stability and
actuator torque clamping), §32.3 (SIMD SVF voice tile, 0 Alloc).

- Base: `283c8115d2203232d0e9b473cc4bb0312c3a8509` (origin/main, a descendant
  of the required `c025b9a`).
- Branch: `claude/project-thread-av73wm`.
- Paths touched: `crates/vesper-resonance/**`, new
  `lean/ResonanceStability.lean`, this report. No root patch (no new crate,
  no new dependency; `tools/bazel/sync_build_files.py` reports no change).

## What was built

### Lean: `lean/ResonanceStability.lean`

Model `vesper.resonance.stability-model/1`, 19 theorems, no
`sorry`/`admit`/`native_decide`/axioms, standard axioms only. It uses only
Lean core (`Rat`, `grind`); no Mathlib. Checked by
`tests/lean_model.rs` (source audit always; compilation and axiom report
under `VESPER_LEAN`). Compiles in about 3.5 s.

**SVF model.** Exact rational arithmetic. State `(ic1eq, ic2eq)`, input `x`,
`D = 1 + g² + g k`, `v1 = (ic1eq + g (x - ic2eq)) / D`, `v2 = ic2eq + g v1`,
`ic' = 2 v - ic` — the same closed form `SvfBank` evaluates
(`a1 = 1/D`, `a2 = g a1`, `a3 = g a2`). `svf_step_implicit` shows it solves
the trapezoidal equations `v1 = g (x - k v1 - v2) + ic1eq`, `v2 = g v1 + ic2eq`.

Stated coefficient range: damping fixed with `0 < k < 2` (`Q > 1/2`), cutoff
free to change every sample within `0 ≤ g[n] ≤ G`.

The proof uses the Lyapunov function `V = ic1² + k ic1 ic2 + ic2²`
(positive definite on that range) and three exact facts about the
trapezoidal step:

- `svf_energy_identity`: `V' - V = 4 g (-(k/2) W(v) + x v1 + (k/2) x v2)`,
  where `W` is the same quadratic form evaluated at the midpoint `v`.
- `svf_cross_bound` (Cauchy–Schwarz in the `W` geometry) and
  `svf_stretch_identity` (`W(N v) = D W(v)` for `ic = N v - g x e1`, which
  gives `svf_state_from_mid`).
- Combined: `svf_lyapunov_step`, i.e.
  `V' ≤ (1 - g k / (2D)) V + (g³ k / D + 4 g / k) x²`.

Results:

- `svf_step_bounded`: `V ≤ K`, `x² ≤ M²` and
  `(2 g² k² + 8 D) M² ≤ k² K` imply `V' ≤ K`.
- `svf_bibo`: for every input sequence with `x[n]² ≤ M²`, every cutoff
  sequence in `[0, G]` and `V(c 0) ≤ K` with
  `(2 G² k² + 8 (1 + G² + G k)) M² ≤ k² K`, `V(c n) ≤ K` for all `n`.
- `svf_state_bounded`, `svf_output_bounded`: `(2 - k)(ic1² + ic2²) ≤ 2K` and
  `(2 - k) v2² ≤ 2K`, so states and the low-pass output are bounded.

Not covered: damping that changes over time (the Lyapunov function depends
on `k`; `SvfBank` can ramp `k`), `Q ≤ 1/2`, and `f32` rounding.

**Torque-clamp model.** `clamp lo hi x` is `f32::clamp`'s definition
(`x < lo → lo`, `x > hi → hi`, else `x`). A PID sample is
`some (setpoint, measurement)` or `none` (a non-finite sample, which the
Rust controller turns into a `PidFault`); faults command safe torque `0` and
keep the state.

- `clamp_mem`, `pid_torque_clamped`, `pid_integral_clamped`.
- `pid_invariant`: along any sample sequence, faults included, every state
  has `-ι ≤ integral ≤ ι` and every command `-τ ≤ u ≤ τ`.

These are theorems about the models over `Rat`. They are not theorems about
the Rust code or about IEEE-754 arithmetic. `tests/stability.rs` checks the
same bounds on concrete `f32` runs (below).

### `std::arch` SIMD SVF (`src/simd.rs`)

- `svf_tick(path, &mut SvfVoiceTile, &PanLanes, x, &mut LaneOut)` advances
  all 16 voices one sample. `SvfLanes::{Scalar, Sse2, Avx, Neon}`:
  SSE2 (x86_64 baseline, 4×4), AVX (8×2, chosen with
  `is_x86_feature_detected!`), NEON (AArch64 baseline, 4×4), scalar
  elsewhere. `SvfLanes::best()`, `available()`, `checked()` (a path the host
  cannot run is `SimdUnavailable`, never silently used).
- Bit-identity argument: every path evaluates the same IEEE single-precision
  operations in the same order per lane (add, sub, mul and one correctly
  rounded division, no FMA; Rust does not contract scalar code). The stereo
  sum over voices stays scalar and in voice order (`LaneOut::sum`), so the
  rounding of the reduction matches too. The per-sample parameter ramps stay
  scalar (they are branchy per voice).
- `SvfBank::render` now ramps (scalar), ticks the tile on the selected
  path, and sums. `SvfBank::new` picks `best()`; `with_lanes` /
  `set_lanes` / `SynthEngine::set_lanes` select one. The wave-five fused
  loop is kept as `SvfBank::render_reference`.
- Unsafe is confined to this module (`#![allow(unsafe_code)]`, the crate
  stays `#![deny(unsafe_code)]` elsewhere except `denorm`): unaligned vector
  loads/stores of the tile's `[f32; 16]` arrays and the calls into the
  `#[target_feature]` kernels (SSE2/NEON baseline; AVX only after runtime
  detection), each with a SAFETY comment.

### TiledSeq slab packing (`src/slab.rs`, `pdc.rs`)

- `TiledSlab::new(buffers, block)` makes one zeroed allocation; buffer `i`
  starts on a 64-byte boundary with stride `ceil(block/16)·16` samples, so
  no two buffers share a cache line. `buffer`, `buffer_mut`, `split_out(i)`
  (buffer `i` mutable plus a `SlabRead` of every other buffer, no copying,
  no unsafe), `working_set_bytes`, `fits(l1_bytes)`. Overflowing sizes are
  `SlabError::TooLarge`.
- `CompiledGraph` keeps every block buffer in one `TiledSlab` (was
  `Vec<Vec<f32>>` with `mem::take` swaps) and exposes `slab()`. `new` now
  checks that no op reads the buffer it writes (`PdcError::OutputAliasesInput`,
  an internal invariant of the liveness allocator) and maps slab errors to
  `PdcError::Slab`.
- `SvfNode`: the voice bank as a graph node (input 0 excites every voice,
  output is L+R; scratch preallocated).

Delay lines remain separate ring buffers (not in the slab). There is no
egglog buffer fusion; the "fusing gain/pan/biquad chains in registers" part
of §32.1 item 4 is not done.

## Tests

`crates/vesper-resonance/tests/` now has 71 tests (57 before) plus one
ignored throughput probe.

- `simd.rs` (5 + 1 ignored): host path report and typed refusal of
  unavailable paths; bit-identical tick vs scalar over the filter range
  (200 tiles × 64 samples per path); bit-identical on 5,000 tiles of extreme
  values per path (arbitrary bit patterns, subnormals, ±0, ±∞, ±MAX; NaN
  results are compared as NaN, since IEEE does not fix NaN payloads);
  bit-identical inside `with_const_denorm` with subnormal states; whole-bank
  render with random patches, ramps and sample-accurate events equals the
  wave-five `render_reference` bit for bit (10 trials × 64 blocks per path).
- `stability.rs` (2): the `svf_bibo` bound `K` holds for `f32` runs of every
  path (16 dampings from 0.02 to 1.98, `G` from 0.0013 to 40 with the cutoff
  re-drawn every sample, 20,000 samples, `|x| ≤ 1`, 0.1 % slack); the
  largest `V/K` seen was about 0.21. State and output bounds checked too.
  200,000 PID steps with adversarial inputs (NaN, ±∞, ±MAX, random bit
  patterns): every command finite and within `torque_limit`, integrator
  within `integral_limit`, fault path exercised.
- `slab.rs` (5): clones are re-aligned to 64 bytes (Codex review); every buffer 64-byte aligned, exactly one stride apart in
  one allocation, for block sizes 1–128; `split_out` hands out the right
  buffers and refuses the output index and out-of-range; writes never touch
  neighbours; oversize requests are typed errors; a 64-input mixer at
  64 samples is 65 buffers = 16,640 bytes, fits a 32 KiB L1, and mixes
  correctly.
- `alloc.rs` (7, two new): the quantum allocates and frees nothing on every
  available SIMD path, including a patch swap; the audio graph (exciter →
  SVF voice-bank node → 37-sample lookahead branch plus 62 dry taps → a
  63-input mixer with a 128-sample feedback send, all in one slab) runs
  2,000 warmed-up blocks with 0 allocations and 0 frees (counting global
  allocator, per thread).
- Existing `pdc.rs`, `realtime.rs` (SVF vs f64 reference), `confinement.rs`,
  `cranelift.rs`, `spsc_stress.rs`, `denorm.rs` pass unchanged on the slab
  and SIMD code.
- `lean_model.rs` (2): both Lean files audited and, under `VESPER_LEAN`,
  compiled with the axiom report checked.

### Hardware evidence and measurements

- x86_64 Linux cloud host (Intel Xeon @ 2.10 GHz, 4 vCPUs, AVX detected):
  Scalar, SSE2 and AVX paths exercised, debug and release.
- AArch64 Linux under QEMU user mode (`qemu-aarch64`, Ubuntu 24.04 package):
  NEON path exercised; `simd`, `stability`, `alloc`, `slab`, `denorm`,
  `realtime` and `pdc` tests pass. Real AArch64 hardware is the macOS CI job.
- Throughput, `cargo test --release --test simd -- --ignored throughput` on
  that x86_64 host, one run, 2,000,000 ticks: Scalar 12.3 ns per 16-voice
  tick, SSE2 12.4 ns, AVX 7.3 ns. LLVM already auto-vectorizes the scalar
  loop to SSE, so SSE2 intrinsics give no speedup there; AVX does. No
  NEON timing (QEMU timing is meaningless).
- No audio device, PipeWire/CoreAudio/WASAPI/ASIO or EtherCAT hardware;
  drivers stay hardware-deferred.

## Checks run

Toolchain: Rust 1.100.0 (as in Bazel CI), Lean 4.24.0 (GitHub release zip).

- `cargo +1.100.0 fmt --all -- --check`: clean.
- `cargo +1.100.0 clippy -p vesper-resonance --locked --all-targets -- -D warnings`:
  clean (also with `--target aarch64-unknown-linux-gnu`).
- `cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings`:
  see the PR (recorded below once finished).
- `VESPER_LEAN=lean cargo +1.100.0 test -p vesper-resonance`: 71 passed,
  1 ignored.
- `cargo +1.100.0 test --release -p vesper-resonance --test simd --test stability --test alloc`: passed.
- `cargo +1.100.0 test -p vesper-resonance --target aarch64-unknown-linux-gnu`
  (QEMU runner) for the test files listed above: passed.
- `lean ResonanceStability.lean` (from `lean/`): compiles, 19 axiom report
  lines, each `[propext, Classical.choice, Quot.sound]`.
- `python3 tools/bazel/sync_build_files.py`: no change.

## Decisions

- BIBO is proved for fixed damping and time-varying cutoff, the strongest
  statement the `k`-dependent Lyapunov function gives; it is stated over
  `Rat`, the closed form matches the Rust kernel's formula, and the `f32`
  behaviour is tested, not proved.
- The design's sketch `proofs/ResonanceRealtimeSafety.lean` is split: the
  wave-five `ResonanceSpsc.lean` keeps the SPSC/PDC proofs and this lane adds
  `ResonanceStability.lean`.
- SIMD covers the filter core and the pan products; the reduction across
  voices is scalar in voice order so the result is bit-identical. A
  horizontal vector sum would be faster but would change rounding.
- Paths are explicit and checked (`SimdUnavailable`), with runtime AVX
  detection; no `-C target-cpu` assumptions.
- The slab uses an over-allocated `Vec<f32>` and `align_offset` for 64-byte
  alignment, and `split_at_mut` for disjoint access, so it needs no unsafe.

## Remaining gaps

- Time-varying damping `k`, `Q ≤ 1/2`, and floating-point rounding are
  outside the BIBO theorem. No linearizability proof of `rtrb`.
- No egglog fusion of gain/pan/biquad chains; delay lines are not packed
  into the slab; parameter ramps are scalar.
- No AVX-512 or SVE path. NEON correctness was checked under QEMU here and
  runs on real hardware only in the macOS CI job.
- No compiler `@realtime` source enforcement (unchanged from wave five) and
  no audio/MIDI/CLAP/EtherCAT drivers.
- Linear owners still rely on Rust ownership plus a leak counter; this is not
  the Vesper source `linear` typing rule.
