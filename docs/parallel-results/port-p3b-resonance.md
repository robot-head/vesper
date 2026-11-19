# Port P3b lane 9: vesper-resonance in Vesper

Claim: #1071. Base: `main` at 8212ddf. Lane prompt:
`docs/agent-prompts/port-p3b/09-resonance.md` (plan PR #1072).

## Summary

- `@vesper/resonance` (`crates/vesper-resonance/vesper/`, 10 modules,
  about 2,750 lines) runs the realtime DSP core: the SVF voice-bank kernel
  and the PID controller (`kernels.rs`), the event wire format, event
  validation and parameter ramps (`events.rs`), the delay-compensation
  schedule compiler, delay lines and mix kernels (`pdc.rs`), the tiled and
  delay slabs (`slab.rs`), the output lease (`lease.rs`) and the
  realtime-confinement call-graph verifier (`confine.rs`). A graph of
  `Box[dyn DspProcessor]` objects in one `ChunkSeq` runs entirely in Vesper
  (`graph.ves`).
- Beacon's seed pattern (#901) is used:
  - `vesper-resonance` builds the Rust code by default. Under the feature
    `vesper-port`, build.rs links the port and the ported entry points
    forward to it.
  - `vesper-resonance-seed` (`seed/`) is the frozen Rust build. The root
    `vesper` package and `vesper-fusion` (the only crates in
    `cargo tree -p vesper -e normal,build` that depend on it) use it by
    rename, in their own commit.
  - Rust stays the default and nothing is deleted, by the owner's parity
    rule.
- Every crate test passes on the Rust build, the seed and the port.
  `tests/port_differential.rs` (9 tests) compares the port with the Rust
  build bit for bit on samples, schedules, delay slabs, verifier findings
  and leases.
- Zero allocation on the port's audio path is proved by the native
  realtime verifier on x86_64 and aarch64, with a positive control (see
  "Realtime proof").
- Speed (ABBA, release, 9 epochs): the port takes **5.3x** the Rust build's
  time on the 64-voice kernel block and **5.0x** on the 32-node graph;
  instruction counts are 10.8x and 5.9x. Rust stays the default.

## API inventory

| Rust module | Ported (forwarding under `vesper-port`) | Stays Rust |
| --- | --- | --- |
| `kernels.rs` | `SvfBank::render`, `render_reference`, `set_ramp_lane`; `PidController::new`, `step`; `SvfNode`'s stereo sum (`mix_lr`) | `SvfBank` state layout, `apply_patch`, `SynthEngine` (it owns the SPSC mailboxes) |
| `events.rs` | `RtParamEvent::encode`, `decode`, `decode_at`; `decode_events`; `validate_events`; `ParamRamp::set`, `next_value`; `RampTile::set_lane`, `advance_lane_scalar`, `scalar_next` | `process_with_events` (a Rust callback driver over the ported pieces), the SIMD `RampTile` paths |
| `pdc.rs` | `compile_schedule`; `DelayLine` (len, write, read, advance); `MixGain`, `LatentMix`; new `render_builtin_graph` (all-Vesper `Box[dyn]` graph) | `CompiledGraph` over Rust `Box<dyn DspProcessor>` objects: Rust trait objects cannot cross the export ABI; its checks (`check_graph`) are shared by both builds |
| `slab.rs` | `TiledSlab::new` geometry; `DelaySlab` layout, write, read, advance_all, line accessors | the aligned allocation itself (`Vec` of cache lines) |
| `lease.rs` | `write_frame`, emergency silence, completion and take counts; the driver copy | `RtOutputPort` ownership and its SPSC hand-off |
| `confine.rs` | `verify_realtime` (rules 0-7: forbidden effects, unknown callees, indirect calls, loops, recursion, stack budget), worst-case stack | `CallGraph` construction (the Rust caller's data) |

The public Rust API is unchanged in both builds except one addition,
`pdc::BuiltinKind` and `pdc::render_builtin_graph`, present in both builds
(the Rust build runs `CompiledGraph` over the same built-in processors),
for the differential test and the benchmark.

`f32` state crosses as its bit pattern: the export ABI has no `f32`, so
`&[f32]` is lent in place as `&[u32]` with `bytemuck` and scalars as
`f32::to_bits` (`src/port.rs`). Nothing is copied or allocated per call.

## What stays Rust

As the prompt lists: the Cranelift JIT (`clif.rs`), the `rtrb` SPSC rings
(`spsc.rs`), the denormal control registers (`denorm.rs`), the `simd.rs`
intrinsics and the seed copy. `cranelift.rs`, `spsc_stress.rs`, `denorm.rs`
and `simd.rs` pass in both builds with those Rust edges (in the port build
`simd.rs`'s bank test goes through the ported `render`).

Unsafe code: none in the port. The Rust side adds none outside the
generated binding (`port/binding.rs`, 43 `unsafe` occurrences, all FFI
calls and lent slices, `#[allow(unsafe_code)]` on that module only);
`simd.rs` and `denorm.rs` keep their existing unsafe.

## Unit tests

- All test files run unchanged against the port (`cargo test -p
  vesper-resonance --features vesper-port`); Bazel runs them as
  `//crates/vesper-resonance:port_<file>_test` and `:port_unit_test`.
- `lean_model.rs` stays on the default build only (it checks the Lean model,
  not the code).
- `alloc.rs` counts the Rust global allocator, which cannot see Vesper's
  allocator, so in the port build it only shows that the Rust side of the
  audio path allocates nothing. The port's half is the realtime proof below.

## Realtime proof

`lib.ves` has a private root `rt_audio` that calls every audio-path entry
(`svf_render`, `svf_render_reference`, `pid_step`, event decoding and
validation, ramps, one graph block over the delay slab, the lease write)
with its failures handled. `port/test-port.sh` emits it with

```sh
vesper emit crates/vesper-resonance/vesper --emit object --entry rt_audio \
  --target <x86_64|aarch64>-unknown-linux-gnu \
  --verify-realtime --verify-policy crates/vesper-resonance/port/realtime-policy.json
```

and fails on any finding except `rt-unbounded-loop` (see gap 3). The policy
declares two runtime symbols effect-free: `Memmove` (what `copy_from`
lowers to) and `vesper_seq_fault` (the out-of-range fault path, which
writes the fault and exits). On both targets the only findings are 14
`rt-unbounded-loop`s, one per loop over a caller-supplied block or voice
count; there is no allocator, lock or unknown callee. The positive control
emits `compile_schedule` (which builds sequences) under the same policy and
must report `rt-unknown-callee` for `vesper_seq_make`, `vesper_seq_grow` and
`vesper_seq_free`; it does.

## Divergence checks

`vesper-port-divergent` (build.rs) applies one edit per ported module. With
all of them applied, `port/test-port.sh` asserts one failing test per
module and a passing test in the same file:

| Module | Edit | Fails | Still passes |
| --- | --- | --- | --- |
| kernels | right channel panned with `pan_l` | `simd::bank_render_matches_reference_with_events` | `host_reports_its_paths` |
| events | out-of-order offsets accepted after the first | `realtime::malformed_event_streams_are_typed_errors` | `emergency_silence_zeroes_the_rest_and_drops_are_counted` |
| lease | commit allowed before the frame is written | `realtime::lease_must_be_fully_written_to_commit` | same |
| pdc | the path-latency maximum is computed wrong | `pdc::parallel_branches_get_compensation_delays` | `buffer_liveness_reuses_buffers` |
| graph | the `Gain` processor drops its gain | `port_differential::graph_outputs_match_the_rust_build` | `events_and_ramps_match_the_rust_build` |
| slab | the delay head never advances | `slab::delay_slab_read_write_wraps_and_clones_cleanly` | `clones_are_realigned_and_hold_the_same_samples` |
| confine | allowed effects reported as violations | `confinement::allocation_two_calls_deep_is_rejected_with_its_path` | `loops_follow_the_policy` |

In all, 29 tests fail under the divergent build (every comparison in
`port_differential.rs` but the events and PID ones, the bit-exact tests of
`pdc.rs`, `realtime.rs`, `simd.rs`, `slab.rs` and `confinement.rs`, and the `alloc.rs` and `cranelift.rs` tests over the ported graph and verifier), and
59 pass.

## Speed against Rust

`cargo run --release -p vesper-resonance --features vesper-port --example
abba` on the cloud container (x86_64, Linux). Both builds are in one
binary; A is the seed, B the port, timed A B B A, 9 epochs, and the samples
are checked equal first.

| Workload | Port/Rust wall, median (range) | Rust per window | Port per window | Instructions (callgrind), Rust / port / ratio |
| --- | --- | --- | --- | --- |
| kernel: four 16-lane `SvfBank`s (64 voices), one 64-frame block each, 20,000 blocks | **5.26x** (4.90-5.91) | 224 ms | 1,181 ms | 181.2 M / 1,957.6 M / **10.8x** (2,000 blocks) |
| graph: 32 nodes (8 sources, 8 latent mixes with compensation, 16 gains), `render_builtin_graph`, 64-frame blocks, 10 x 2,000 blocks | **4.97x** (3.96-6.40) | 64 ms | 321 ms | 76.1 M / 449.5 M / **5.9x** (1 x 2,000 blocks) |

Instruction counts are whole-process counts minus an empty run (0.67 M).
`ABBA_ONLY=rust|port ABBA_WORKLOAD=kernel|graph` runs one arm for
callgrind.

Two port-side rewrites were measured on the kernel before these numbers:
updating only the ramps that are still ramping (5.88 G -> 2.42 G
instructions; a ramp lane with no samples left keeps its value, so the
samples are unchanged) and writing the tile with `set` instead of the `put`
helper the inliner left as a call (2.42 G -> 1.96 G). The first run was
18.5x wall.

Where the time goes: the Rust kernel is AVX over 16 lanes; the port is
scalar, and its inner loop spends about 240 instructions per voice-sample,
mostly reloading each `inout` slice's header and bounds-checking every
access (gap 1 and 2). In the graph, 80% is the `Gain` and `Latent`
processors summing `ChunkSeq` inputs element by element.

## Compiler gaps

1. **No `f32` vectors.** The kernel is 16 independent lanes; the Rust build
   uses `simd.rs` (AVX/NEON). Vesper has byte SIMD builtins (#936) but no
   float lanes, so the port is scalar. Repro: any `for v in 0..16 {
   s.set(v, to_bits_f32(from_bits_f32(a[v]) * from_bits_f32(b[v]))) }`
   compiles to 16 `mulss`. Platforms: all.
2. **`inout` slice headers reloaded after every store, one bounds check per
   access.** In `k_render` each of the 11 slice parameters is passed as a
   pointer to a {ptr, offset, len} header; after any `set` on one slice the
   header of every other is reloaded, and each access is checked against
   `len` even inside `for v in 0..16` with an earlier access at index 15.
   Repro: `fn k(inout a: Slice[u32], inout b: Slice[u32]) eff
   [Fail[RuntimeError]] { for v in 0..16 { a.set(v, b[v]); b.set(v, a[v]);
   } }` and read the object: two header loads and two compares per access.
   Platforms: x86_64 and aarch64 native.
3. **The realtime policy's `reject_unbounded_loops: false` has no effect.**
   The native verifier still reports `rt-unbounded-loop` for every loop
   whose bound is a parameter, so `test-port.sh` filters that one rule.
   Repro: the policy above with any root containing `for i in 0..n`.
4. **`Box[dyn Trait]` has no export layout.** A handle that owns
   `ChunkSeq[Box[dyn P]]` is refused ("type dyn P has no fixed word
   layout"), so a persistent graph object cannot live in Vesper across
   calls; `render_graph` builds the processors per call. Repro: `pub struct
   G { procs: ChunkSeq[Box[dyn P]] }` in an `// @export abi 3` package.
5. **No `f32` in the export ABI.** Every `f32` crosses as `u32` bits and
   every `&[f32]` is lent through `bytemuck`. Repro: `pub fn f(x: f32) ->
   f32` in an exported package is refused.
6. **`--verify-realtime` applies only to `vesper emit --entry`**, not to
   `vesper build --lib`, so the proof needs a private root (`rt_audio`)
   that calls every entry with its effects handled, and the verifier
   requires every policy root to be emitted from that one entry.
7. **The slice fault path is an undeclared callee** (`vesper_seq_fault`),
   and `copy_from` lowers to an undeclared `Memmove`; every realtime policy
   must declare both. The runtime could ship their effect declarations.
8. **Verifier time:** each `emit --verify-realtime` of the package takes
   about 90 s with the debug compiler (most of it in the verifier), which
   is why the proof runs in the port harness and not in Bazel.
9. **`copy_from` holds a loan on its source**, so copying within one
   sequence needs a temporary; `copy_within` (#986) covers slices but the
   ring buffer in `Latent` uses element loops instead.

## Files

- `crates/vesper-resonance/vesper/` (the Loom package), `port/binding.rs`
  (generated; `VESPER_PORT_BLESS=1` regenerates it), `port/test-port.sh`,
  `port/realtime-policy.json`, `port/realtime-control.json`.
- `crates/vesper-resonance/build.rs`, `Cargo.toml` (features `vesper-port`,
  `vesper-port-divergent`), `src/port.rs` and the cfg-forwarding in
  `src/{kernels,events,pdc,slab,lease,confine,lib}.rs`.
- `crates/vesper-resonance/seed/` (Cargo.toml, BUILD.bazel).
- `crates/vesper-resonance/tests/port_differential.rs`, `examples/abba.rs`.
- Shared lists: one line in `scripts/test-port-harness.sh` (shard 3), one
  in the `fixtures()` skip predicate of `tests/fixtures/self_host/driver.rs`.
- Root patch (its own commit): the seed in root `Cargo.toml` members and
  the `vesper-resonance` dependency renamed to the seed in root
  `Cargo.toml` and `crates/vesper-fusion/Cargo.toml`, plus `Cargo.lock`.
