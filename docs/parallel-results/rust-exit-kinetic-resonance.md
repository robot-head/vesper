# Rust-exit wave SH6 lane 9: Kinetic and Resonance

Claim [#1404](https://github.com/robot-head/vesper/issues/1404), continuing
#1070 (Kinetic, P3b lane 8) and #1071 (Resonance, P3b lane 9). Prompt:
`docs/agent-prompts/self-host-sh6/09-kinetic-resonance.md` (plan PR #1415).

## Part 1: Resonance speed

Base `main` at `ab965f2a` (2026-11-21). Port rewrites only, in
`crates/vesper-resonance/vesper/src/{kernels,graph}.ves`. No compiler change,
no Rust behaviour change, no new export.

### Measurements

`cargo run --release -p vesper-resonance --features vesper-port --example
abba`, cloud Linux x86_64 VM (4 vCPUs), 9 epochs of A B B A windows, release
compiler in `VESPER_PORT_COMPILER`. The samples are checked equal before
timing.

| Workload | Before (this host, `main`) | After, median (range) |
|---|--:|--:|
| `kernel`: four 16-voice `SvfBank`s, 64-frame blocks | 24.4x (21.0-26.6) | **1.11x (1.07-1.34)** |
| `graph`: 32-node delay-compensated graph | 9.0x (7.3-10.3) | **1.89x (1.77-2.17)** |

The P3b report measured 5.3x and 5.0x. On this host, `main`'s unchanged
port measures 24.4x and 9.0x, and the compiler from the P3b merge
(`f3045ce`) building the P3b sources measures 18.5x and 10.6x, so the P3b
numbers do not reproduce here; the "before" column is today's `main` on the
same host as the "after" column.

Flip gate (the 1.5x bar on the upper end of the interval): the kernel passes
(1.34x); the graph does not (2.17x). Rust stays the default for now: see
"What is left".

### What changed

1. **No `Box` in the package (most of the gain).** The only `Box` was the
   graph's `ChunkSeq[Box[dyn DspProcessor]]`. Any `Box` anywhere in a
   program makes the native backend emit far worse code for every
   function, including ones that never touch a box (compiler gap 1). The
   built-in processors are a closed set (source, latent mix, gain), so the
   graph now keeps each processor as five words of one `ChunkSeq[u64]` and
   dispatches on its kind; latent mixes keep their rings and input sums in
   one shared pool. Removing it alone took the kernel from 24x to 1.9x and
   the graph from 9x to 3.5x.
2. **Kernel: four voices at a time, block-major.** `k_render` used to run
   frame by frame over all 16 voices (the Rust order). It now runs a group
   of four voices over the whole block (`group_frames`), with their state in
   registers, and adds each group's panned samples to the partial sums the
   earlier groups left in `left`/`right`. The stereo sum still adds voices
   in voice order (`0.0 + y0 + y1 + ...`), and the voices are independent,
   so every sample is the Rust build's bit for bit. Four voices per loop keep
   four filter recurrences in flight (one voice per loop is latency bound,
   1.9x). The filter coefficients depend only on g and k, so they are
   computed once per call: frame by frame while one of the group's ramps
   moves (its first `max(remaining)` frames), then once for the rest of the
   block.
3. **Graph: fewer passes.** Inputs are summed two at a time; the first pair
   starts from 0.0 (so -0.0 inputs give the Rust build's +0.0) and the last
   pass applies the gain, so each output sample is written once per pair of
   inputs instead of zero, add, add, scale. The latent ring moves with
   `copy_within`/`copy_from`, the delay line reads straight into its
   scratch buffer in the slab, and the source fills its block in its own
   loop.

`group_frames`, `accumulate`, `accumulate2`, `scale`, `fill_zero` and
`source_fill` declare only `Fail[RuntimeError]` (wrapping index arithmetic),
which also keeps the inliner from merging them into their callers (it
inlines only callees whose row equals the caller's); in one function the
graph's whole schedule loop spills (gap 3).

### Checks

`crates/vesper-resonance/port/test-port.sh` passes in full: every test of
the Rust build, the seed and the port (255 test cases;
`tests/port_differential.rs` compares samples, schedules, findings and
leases with the Rust build bit for bit, including events and ramps through
`SvfBank::render`), the seven divergence checks, and the native realtime
verifier proof on x86_64 and aarch64 with its positive control. Two
divergent edits moved with the code (`build.rs`): the kernel's right channel
now takes voice 0's left pan weight, and the graph's gain nodes skip their
gain. `vesper fmt --check` and `cargo fmt --check` are clean.

### Compiler gaps (with repros)

1. **Any `Box` in the program degrades every function's native code.**
   `vesper build --lib` of

   ```vesper
   pub fn k(inout a: Slice[u32], in b: Slice[u32], n: u64) eff [Fail[RuntimeError]] {
       for i in 0..n {
           a.set(i, to_bits_f32(from_bits_f32(a[i]) * 2.0 + from_bits_f32(b[i])));
       }
   }
   fn main() -> u64 { 0 }
   ```

   gives `k` 122 lines with 4 stack stores (x86_64). Adding, anywhere in the
   file,

   ```vesper
   trait P { fn go(inout self) -> u64; }
   struct S { x: u64 }
   impl P for S { fn go(inout self) -> u64 { self.x } }
   pub fn boxy(n: u64) -> u64 {
       let mut b = Box::new[dyn P](S { x: n });
       b.go()
   }
   ```

   gives `k` 240 lines with 51 stack stores. The Place IR of `k` is the same
   in both (`vesper emit --emit ir`), so the difference is in the native
   lowering (probably the drop machinery that turns on once the program
   uses `Box`). In the Resonance package it made the kernel's frame loop
   ~3x longer, with every loop-carried value spilled each iteration. Handed
   to the coordinator for the native backend's owner.
2. **Vectors are stack aggregates.** `Vec[f32, 4]` code (§14, #1111) keeps
   every vector in a stack slot and copies it through 8-byte words around
   each operation (`src/native_vector.rs`: "store the result into a fresh
   stack slot"). A four-lane `s[k] += x[k]` loop costs ~55 instructions per
   four elements, more than the scalar loop (~14 per element). A vector
   kernel of the voice bank was 16.7x before the `Box` fix. So the port is
   scalar, and the graph's sums (the Rust build's are AVX) are its gap.
   Repro: `fn acc4(inout s: Slice[u32], in x: Slice[u32]) eff
   [Isa[Baseline128], Fail[RuntimeError]] { for j in 0..s.len() / 4 { let k =
   wrapping_mul(j, 4); Vec::store(inout s, k, Vec::to_bits(Vec::add(
   Vec::from_bits(Vec::load[u32, 4](in s, k)), Vec::from_bits(Vec::load[u32,
   4](in x, k))))); } }`.
3. **Large inlined functions spill everything.** `run_blocks` with
   `run_block`, the processors and the delay-line helpers inlined is ~32 KB
   of code and spends ~775 instructions per graph op (the Rust
   `process_block` ~160), spread over the whole function rather than in a
   loop; copy-struct values (an earlier 48-byte `Proc`) go through several
   stack temporaries per copy. There is no way to ask for a call to stay a
   call other than a differing effect row.
4. **An index expression cannot start with a qualified path.**
   `ramps[RampBase::Gain + v]` is a parse error (`expected RBracket, found
   Plus`); `ramps[v + RampBase::Gain]` and `f(in ramps, RampBase::Gain + v)`
   parse. The existing port writes `st[(Pid::HasPrevious)]` for the same
   reason.

## Part 2: Resonance graph under 1.5x

Base `main` at `c46c3a61` (part 1 merged). Port rewrites only, in
`crates/vesper-resonance/vesper/src/graph.ves`. No compiler change, no Rust
behaviour change, no new export.

### Measurements

Same harness and host as part 1, three runs of 9 epochs each:

| Workload | Part 1 | Part 2, median per run (range over all runs) |
|---|--:|--:|
| `kernel` (unchanged) | 1.11x | 1.10x, 1.05x, 1.06x (0.87-1.28) |
| `graph` | 1.89x (1.77-2.17) | **1.10x, 1.10x, 1.08x (0.90-1.36)** |

Both workloads now meet the 1.5x bar at the upper end of the range.
Callgrind (500 blocks of the graph): the port runs 265M instructions where
it ran 357M; the Rust build runs 192M. The rest is mostly the input sums
(scalar, where the Rust build's are AVX: gap 2).

### What changed

1. **The schedule is resolved once.** `run_block` used to read every op's
   inputs from the schedule words with total `get` calls, push their buffer
   indices into a sequence and multiply them by the block length, and the
   delay-line helpers divided to bounds-check the line index on every call.
   `g_render` now builds a plan (`plan`): per op, the processor's word
   offset, the output's slab offset, the line steps with the lines'
   metadata offsets, the inputs' slab offsets and the feeds. `run_block`
   walks it with plain indexing and passes the inputs to the processors as
   a slice of the plan. Lines the schedule names but the slab does not have
   are dropped from the plan, as `DelaySlab::read`/`write` skip them. The
   line steps (`line_steps`, `line_read`, `line_write`) declare only
   `Fail[RuntimeError]`, so they stay out of `run_block` (gap 3).
2. **The source is a copy.** Sample t of a source is a function of the
   residue `(t * 7919 + 31 * seed) % 97`, and the next sample's residue is
   the last one plus 62 (7919 mod 97) while the sum does not wrap. 62 and 97
   are coprime, so the residues form one cycle. `source_table` lays the
   cycle's samples out in order (97 + block of them, computed by the same
   expression as before), with each residue's position in the cycle in
   front, so a block of a source is one `copy_from`. When the sum would
   wrap inside the block, the source falls back to the per-sample residue.

### Checks

`crates/vesper-resonance/port/test-port.sh` passes in full (all tests of
the three builds, including the bit-for-bit differential; the seven
divergence checks; the realtime verifier proof on x86_64 and aarch64 with
its positive control). The graph's divergent edit in `build.rs` moved with
the call it edits. `vesper fmt --check` is clean.

### Compiler gap found in this part

5. **Manual unrolling doubles the instruction count.** Unrolling the
   two-input sum by four (`let k1 = wrapping_add(k, 1); ... x[k1] ...`) took
   it from 130M to 252M instructions. Each indexed access with a computed
   index reloads the slice's base and length and checks the bounds on its
   own; nothing is shared between the four. The plain `for k in 0..s.len()`
   loop stays.

## Part 3: the SIMD paths on Vesper vectors

Base `main` at `0d2438da` (part 2 merged). `simd.rs` was the first module
of the "not ported yet" list: its SSE2, AVX and NEON paths of `svf_tick`
and `ramp_tick` were `std::arch` intrinsics with no port.

### What changed

- **`vesper/src/simd.ves` (new).** `v_tick4`/`v_tick8` and
  `v_ramp4`/`v_ramp8` are the SVF and ramp steps on 4- and 8-lane source
  vectors (§14), and `v_tick_scalar` is the scalar reference. Every path
  evaluates the reference's IEEE-754 operations in its order per lane,
  with no fused multiply-add. The ramps select on the bit patterns, since
  a `Mask` of `u32` lanes selects only `u32` vectors.
- **Exports `svf_tick` and `ramp_tick`** take the path's lane count and
  dispatch with `Isa::supported`. An export cannot carry an `Isa` effect,
  so the checked dispatch sits inside it. The tiles cross as their
  `repr(C)` words: `SvfVoiceTile` is 96 words, `PanLanes` and `LaneOut` 32,
  `RampTile` 64. `simd.rs` views them in place, and the sizes are asserted
  at compile time.
- **`src/simd.rs`** routes `svf_tick`, `tick_scalar` and `ramp_tick` to the
  port under `cfg(vesper_port)`. The `std::arch` modules build only
  without it. Path availability is unchanged: AVX is available where the
  CPU has AVX.
- **Checks:** `tests/port_differential.rs` gains
  `simd_paths_match_the_rust_build`. It runs 200 trials over every path on
  random bit patterns (NaN, infinities, subnormals, `u32::MAX` remaining
  samples) against the Rust build's `std::arch` paths, four samples each.
  A NaN matches any NaN, as in `tests/simd.rs`, because IEEE-754 leaves a
  NaN result's payload to the implementation; the port's x86 code can
  return the other operand's NaN. A new divergent edit points the 4-lane
  path's right channel at the left pan weights, and
  `tick_is_bit_identical_over_filter_range` catches it.
  `port/test-port.sh` passes in full, with eight divergence checks now.

### Measurements

A new `simd` workload in `examples/abba.rs` runs one 16-voice tile and
its ramps, sample by sample, through `ramp_tick` and `svf_tick`
(200,000 samples per window):

| Path | port/rust median |
|---|--:|
| widest (`Avx`, which runs on 4-lane vectors in the port: gap 6) | 48.3x |
| `Sse2` | 38.7x |
| `Scalar` | 7.9x |

The module is far from the bar. Both causes are in the compiler. Vectors
are stack aggregates (gap 2), so the vector paths cost five times the
port's own scalar loop. The scalar loop pays the slice-header reloads and
bounds checks on every access (P3b). Each sample is also one call across
the export ABI, about 16 ns of work on the Rust side. `SvfBank::render`,
which renders whole blocks, is the kernel workload above and is at 1.1x.
So the port of this module is functionally complete and stays
non-default.

### Compiler gap found in this part

6. **`Isa::supported` is decided at compile time.** Native code lowers it
   to a constant from the target the object is built for
   (`src/native_vector.rs`: `isa_enabled(self.m.isa(), p)`), not to a
   runtime CPU check. A `vesper build --lib` object for the x86_64
   baseline therefore never runs an `Isa[Avx2]` path, even on an AVX2
   host. Repro: `pub fn wide() -> bool { Isa::supported[Avx2]() }`
   exported and called from Rust on an AVX2 machine returns false. Also,
   an export cannot carry `Isa[P]` ("only Fail[E] and imported .vsys
   libraries are"), so the dispatch has to sit inside the export.

## Part 4: the speed of every other ported module

Base `main` at `d1b96cd2` (part 3 merged). P3b measured only the kernel
and the graph. `examples/abba.rs` now has one workload for each other
ported module (`ABBA_WORKLOADS=a,b` picks workloads), and the schedule
compiler got one port rewrite.

| Module | Workload | port/rust median (range) | Bar |
|---|---|--:|---|
| kernels (`SvfBank::render`) | `kernel` | 1.10x (0.87-1.28) | passes |
| graph (`render_builtin_graph`) | `graph` | 1.08x (0.90-1.36) | passes |
| pdc (`compile_schedule`) | `schedule`: the 32-node graph | **1.32x (1.22-1.50)**, was 1.50x (1.47-1.56) | passes |
| confine (`verify_realtime`) | `confine`: 64 functions, one root, three findings | 1.66x (1.58-1.80) | fails |
| kernels (`PidController::step`) | `pid`: 200,000 steps | 2.06x (2.04-2.13) | fails |
| slab (`DelaySlab`) | `slab`: 8 lines, 20,000 blocks | 2.13x (2.12-2.30) | fails |
| events (wire codec, `ParamRamp`) | `events`: 32 events round trip, 16 ramps x 64 samples | 5.64x (5.31-5.95) | fails |
| simd | `simd` (part 3) | 8-48x | fails |

**Schedule rewrite.** `p_compile` looked up each input's source with a
binary search over the sorted ids, five times per input. It now resolves
every input's source once (`srcs`, by input) and reads it from there.

**What keeps the rest above the bar.** `pid`, `slab` and `events` make one
export call per step, per line or per sample; `ParamRamp::next_value` is
one call per sample. The work in each call is a few nanoseconds in Rust,
so the cost of a call across the export ABI (the status words, the
binding's conversions and the lost inlining) is most of the port's time:
about 15 ns per call more than Rust. Closing it needs either coarser
calls (a block of PID steps, all of a slab's lines per block, a block of
ramp values), which changes the crate's Rust API, or cheaper calls from
the compiler (gap 7). `confine` spends its time in `verify_root`'s total
`get`/`set` accessors and in the Rust side's interning of effects and
building of the call-graph words.

### Compiler gap found in this part

7. **A call across the export ABI costs about 15 ns more than a Rust
   call.** The binding passes a status buffer and converts every argument
   to words and back, and a Rust caller can't inline the Vesper callee.
   That cost is the gap for a module whose API is one small operation
   (`ParamRamp::next_value`, `PidController::step`, `DelaySlab::read`).
   An inlinable fast path for leaf exports with no `Fail` row, or
   coarse-grained entry points in the crates, would close it.

## Part 5: flip the kernel and the graph

The package `vesper-resonance` is now port-backed, like `vesper-tensor`:
its build script always links the Vesper port, and the `vesper-port`
feature is gone (`vesper-port-divergent` stays). The compiler, vesper-fusion
and vesper-hybrid already link the seed, `vesper-resonance-seed`, so no
dependent changes build. The seed now has its own crate root
(`seed/src/lib.rs`, the #1102 pattern) that includes `../../src/lib.rs` as a
module and adds the frozen Rust engine beside it:

| Module | Moved to the seed | Package `vesper-resonance` |
|---|---|---|
| kernels (`SvfBank::render`, `render_reference`) | `seed/src/engine/kernels.rs` | port only (`vesper/src/kernels.ves`) |
| graph (`render_builtin_graph`, the built-in source) | `seed/src/engine/pdc.rs` | port only (`vesper/src/graph.ves`) |

The shared source calls `crate::engine::…` under `cfg(not(vesper_port))`,
so `src/` has no Rust implementation of either module (124 lines moved).
Bazel builds the package with the port object (`vesper_crate` with the
port's build script data, as vesper-tensor does), so the separate `:port`
targets are gone.

### Flip gate

`python3 scripts/ci/flip_gate.py vesper-resonance`
(`crates/vesper-resonance/flip-gate.toml`; `examples/abba.rs flip-gate
<workload>` prints the pairs line), cloud Linux x86_64 VM, compiler at
`main` 2026-11-22:

```
flip-gate vesper-resonance: PASS (bar: 0 differences, every test, 95% upper <= 1.50x) | tests 1/1 pass | differential 0 differences | ABBA vesper/rust kernel 1.07x [1.02x, 1.13x] n=30; graph 1.07x [1.05x, 1.12x] n=30 | RUSTFLAGS=-C target-cpu=native
```

**The schedule compiler is not flipped.** With the current compiler and
the host CPU, the gate measured it at 1.50x [1.45x, 1.53x] (n=30). The upper
end is over the bar, so its Rust stays in `src/pdc.rs`. In callgrind the
port runs 1.19x Rust's instructions (118k against 99k per compile of the
32-node graph); 79k of them are in the one inlined `p_compile` body, so the
wall-clock gap is mostly that function's spills (gap 3).

### Checks

- `cargo test -p vesper-resonance -p vesper-resonance-seed` (the package
  now runs every test against the port).
- `crates/vesper-resonance/port/test-port.sh`: the divergent port fails one
  test per ported module, and the realtime verifier proves the audio path
  allocation-free on x86_64 and aarch64.
- `vesper build --lib` of the port for `aarch64-apple-darwin`,
  `x86_64-pc-windows-msvc` and `aarch64-unknown-linux-gnu` gives the
  committed binding, so the macOS and Windows jobs can build the package.

### Kinetic: the colouring, flipped

`abba compare 12 1` (`crates/vesper-kinetic/examples/abba.rs`, host CPU) on
the same compiler, median port/rust before this part: fastmath 1.42x,
scene 1.20x, colorer 1.60x, gjk 1.70x, sap 1.84x, morton 2.03x, lbvh
2.07x, multi-colorer 2.21x, pile-lbvh 2.21x, crmath 2.65x.

**Colouring.** The port ran only 1.11x Rust's instructions on `colorer`,
but 2.6x its mispredicted branches (cachegrind: 356k against 138k for five
runs). Nearly all of them were in the comparison sort of the visiting
order (`@vesper-std/sort` keyed quicksort). `coloring.ves` now packs each
constraint's `(min, max, index)` into one word when it fits
(`(min << (bb + nb)) | (max << nb) | index`, which orders as the pair does
and is unique), and sorts those words with an LSD radix sort that
ping-pongs between the two halves of a 2n-word key buffer and counts
digits in the (not yet used) `sorted` buffer. Short inputs (under 256
constraints) and body counts too wide to pack keep the comparison sort.
The Rust adapter now lends a key buffer of two words per constraint.
Mispredicts fell to 100k, under Rust's.

The frozen Rust colourers (`Colorer::color`, `MultiColorer::color`) moved
to `seed/src/engine/coloring.rs` under a new seed crate root, as in
Resonance. `tests/port_differential.rs` compares both colourers with the
seed (visiting order, colours and layers) over 600 cases, from small
graphs to large ones and from 2 bodies to 4 million.

```
flip-gate vesper-kinetic: PASS (bar: 0 differences, every test, 95% upper <= 1.50x) | tests 1/1 pass | differential 0 differences on 600 cases | ABBA vesper/rust colorer 0.78x [0.78x, 0.79x] n=30; multi-colorer 1.35x [1.29x, 1.36x] n=30 | RUSTFLAGS=-C target-cpu=native
```

### Compiler gap found in this part

8. **The inliner moves a cold path's sequence onto the hot path.** If a
   small callee that holds a sequence is spliced into a caller where only
   a rare branch reaches it, the caller's every call pays for the
   sequence: a frame, saves of callee-saved registers, zeroing of the
   sequence slot, and drop branches on constant `false`. Repro: a package
   with

   ```vesper
   fn slow(x: u64) -> u64 {
       let mut s: ChunkSeq[u64] = ChunkSeq::new[u64]();
       s.push(x);
       s.len()
   }

   pub fn hot(x: u64) -> u64 {
       if x > 1000000 {
           return slow(x);
       }
       wrapping_add(x, 1)
   }

   pub fn plain(x: u64) -> u64 {
       wrapping_add(x, 1)
   }
   ```

   `vesper build --lib --lib-name repro` gives `vesper_repro_hot` 74
   instructions, about 20 of them on the fast path, against 6 for
   `vesper_repro_plain`. Kinetic's `fastmath_sin` and `fastmath_cos`
   exports have this shape (a rare correctly rounded branch with an empty
   `ChunkSeq` cache). On every call they set up a 288-byte frame, save 5
   registers and zero 7 words, which accounts for much of fastmath's 1.42x.
   Moving the branch into its own function does not help, because that
   function is inlined back.

## What is left

- **Resonance:** flip the schedule compiler once it passes the gate
  (1.50x), then confine, pid, slab and events (gap 7). The Rust
  `CompiledGraph` over Rust `Box<dyn DspProcessor>` trait objects, the
  Cranelift JIT (`clif.rs`, which goes through `@vesper/cranelift` per the
  plan), the `rtrb` rings (`spsc.rs`) and the denormal registers are not
  ported yet. The SIMD paths are ported (part 3), but they run at 8-48x and
  are bound by gaps 2 and 6.
- **Kinetic:** speed up fastmath (gap 8), gjk, sap, morton, lbvh and
  crmath to the bar and flip them, then port `world`/`tile`, `rigid` and `rigid/**`, `soft`,
  `articulation`, `couple`, `diff`, `rigid_diff`, `rollback`, `snapshot`,
  `detfloat`, `simd` (see `port-p3b-kinetic.md`, "Not ported yet"), then the
  speed of the ported modules (1.3-2.8x in P3b).
- The census row in `scripts/rust-census.py` is updated with each part.
