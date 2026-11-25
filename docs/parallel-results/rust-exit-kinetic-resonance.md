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

`aspect crate resonance test-port` passes in full: every test of
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

`aspect crate resonance test-port` passes in full (all tests of
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
- `aspect crate resonance test-port`: the divergent port fails one
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

**Sweep and prune.** `pair_total` and `write_pairs` now read each row's
bounds once instead of once per candidate: 16% fewer instructions, `sap`
1.84x to 1.67x (range 1.50-1.92). What is left is the slice header that
every `try_get` reloads (base and offset, three loads per element read),
which the code-size lane (#1426) owns.

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

## Part 6: Kinetic `gjk` and `sap` flipped; `lbvh` + `sap` radix speedup

Base `main` at `cf28be7a`. Target: flip Kinetic modules meeting Rule 8 (`<= 1.50x` ABBA upper bound, `0` differential differences, all tests green).

### Measurements (`n = 30`, `RUSTFLAGS="-C target-cpu=native"`, Windows x86_64 MSVC)

| Workload | Baseline (`0f5a9eba`) | After `sap` + `lbvh` radix/traversal edits | Status |
|---|---:|---:|---|
| `gjk` | `1.10x [1.03x, 1.15x]` | `1.122x [1.053x, 1.168x]` | **Flipped** (`seed/src/engine/gjk.rs`) |
| `sap` | `1.31x [1.25x, 1.35x]` | `1.088x [1.045x, 1.155x]` | **Flipped** (`seed/src/engine/sap.rs`) |
| `scene` | `1.23x [1.18x, 1.25x]` | `1.180x [1.123x, 1.250x]` | Sub-1.5x integrated scene |
| `pile-lbvh` | `1.85x` | `1.365x [1.250x, 1.430x]` | Sub-1.5x integrated pile |
| `pile-lbvh-4t` | `1.79x` | `1.261x [1.201x, 1.315x]` | Sub-1.5x 4-thread pile |
| `lbvh` | `2.09x [2.05x, 2.15x]` | `1.569x [1.491x, 1.626x]` | Kept in `src/lbvh.rs` (`> 1.50x`) |
| `fastmath` | `1.70x [1.56x, 1.78x]` | — | Above bar on Windows MSVC (`1.42x` prior Linux) |
| `morton` | `1.88x [1.82x, 1.96x]` | — | Above bar |
| `crmath` | `2.27x [2.22x, 2.35x]` | — | Above bar |

### What changed

- **`vesper/src/sap.ves` & `vesper/src/lbvh.ves`**: Split `sap::radix_sort` into branchless `radix_pass_vals` / `radix_pass_keys` passes removing inner-loop `if` checks and duplicate histogram reloads, exported `sap::radix_sort` / `sap::radix_sort_pairs`, switched `lbvh::build` / `lbvh::pairs` onto `sap`'s radix sort with preallocated `LbvhPort` scratch buffers (`src/lbvh.rs`), hoisted box bounds / primitive indices out of `lbvh` loops, and checked `q > p` before loading leaf bounds (and updated `build.rs` + `BUILD.bazel` `lbvh` divergent hook to `"if q > p {"`).
- **Frozen engines moved to `seed/src/engine/`**: Moved `separation_rust`, `gjk_cores`, `epa`, `closest_on_simplex`, and helpers out of `crates/vesper-kinetic/src/gjk.rs` into `crates/vesper-kinetic/seed/src/engine/gjk.rs`, and moved `sortable`, `SweepAndPrune::build`/`overlapping_pairs`, `exclusive_scan`, `radix_sort`, and `SortScratch` out of `crates/vesper-kinetic/src/sap.rs` into `crates/vesper-kinetic/seed/src/engine/sap.rs` (790 lines removed / 742 net lines deleted from `crates/vesper-kinetic/src/{gjk.rs,sap.rs}`).
- **Differential & gate**: Added `gjk_matches_the_rust_build` (501 cases, bit-identical `f64` witness/normal/distance), `sap_matches_the_rust_build` (248 cases), and `lbvh_matches_the_rust_build` (360 cases) to `crates/vesper-kinetic/tests/port_differential.rs` (`0` differences across all 1,709 total differential cases with `coloring`). Added `gjk` and `sap` ABBA entries and `seed/src/engine/{gjk,sap}.rs` to `crates/vesper-kinetic/flip-gate.toml`.

### Resonance: `schedule`, `pid` and `confine` flipped

After `#1516`, `#1526` and `#1527` landed on `main` (`0f5a9eba`), re-measuring the remaining Resonance modules with `RUSTFLAGS="-C target-cpu=native"` (`n = 30`, Windows x86_64 MSVC) cleared three more modules cleanly under 1.50x:

| Workload | Ratio (vesper / rust) | 95% interval | Previous | Decision |
|---|--:|---|--:|---|
| `schedule` (`pdc::compile_schedule`) | **1.02x** | `[0.94x, 1.13x]` | 1.50x `[1.45x, 1.53x]` | **Flipped** |
| `pid` (`kernels::PidController`) | **1.27x** | `[1.17x, 1.30x]` | — | **Flipped** |
| `confine` (`confine::verify_realtime`, 3 names) | **1.35x** (now **1.401x**) | `[1.294x, 1.496x]` | 1.66x | **Flipped** |
| `confine-many` (`confine::verify_realtime`, 256 distinct `Effect::Other`) | **1.335x** | `[1.232x, 1.398x]` | quadratic intern | **HashMap intern** |
| `slab` (`slab::DelaySlab`) | 1.89x | `[1.79x, 2.00x]` | 2.1x | Stays above bar |
| `events` | 8.00x | `[7.76x, 8.98x]` | 5.6x | Stays above bar |

Their `#[cfg(not(vesper_port))]` Rust implementations moved into `crates/vesper-resonance/seed/src/engine/` (`confine.rs`, `pdc.rs`, `kernels.rs`) so `vesper-resonance` runs only `@vesper/resonance` for all five flipped paths while `vesper-resonance-seed` stays frozen for the compiler until Stage 2. Effect interning in `confine.rs` uses a `HashMap<&Effect, u64>` alongside `effects: Vec<&Effect>` so graphs with many distinct custom effects (including unreachable functions) stay `O(N)`. `tests/port_differential.rs` keeps 0 differences across `PidController` (50,000 steps + fault cases), `compile_schedule` (400 randomized graphs) and `verify_realtime` (randomized graphs and indirect targets).

## Part 7: Kinetic `fastmath` flipped; `lbvh`, `morton`, `crmath` re-measured

Base `main` at `7f9b71c` (2026-11-24), after the Cranelift SIMD/LICM
waves. Cloud Linux x86_64 (4 vCPUs), `RUSTFLAGS="-C target-cpu=native"`,
release compiler built from that commit in `VESPER_PORT_COMPILER`.

### Measurements

`scripts/ci/flip_gate.py vesper-kinetic` (`n = 30`), plus `abba compare 9`
for the modules still above the bar:

| Workload | Part 6 (Windows MSVC) | Now (Linux, `main`) | Decision |
|---|--:|--:|---|
| `fastmath` | 1.70x `[1.56x, 1.78x]` | **1.08x `[1.07x, 1.09x]`** | **Flipped** |
| `colorer` | 0.78x | 0.82x `[0.81x, 0.82x]` | flipped earlier |
| `multi-colorer` | 1.35x | 1.30x `[1.28x, 1.31x]` | flipped earlier |
| `gjk` | 1.12x `[1.05x, 1.17x]` | 1.46x `[1.44x, 1.48x]` | flipped earlier; now close to the bar |
| `sap` | 1.09x `[1.05x, 1.16x]` | 1.07x `[1.07x, 1.08x]` | flipped earlier |
| `lbvh` | 1.57x `[1.49x, 1.63x]` | 1.54x `[1.53x, 1.56x]` | stays (above the bar) |
| `morton` | 1.88x | 2.07x (9 epochs) | stays: per-call export ABI (fail and result buffers) on a ~10 instruction body |
| `crmath` | 2.27x | 1.92x (9 epochs) | stays: bignum arithmetic |

The `fastmath` gap since part 6 is the inliner fix for gap 8 (`#1527`) and
the later codegen waves: `sin`/`cos` no longer carry a cold-path sequence
in their frames. `gjk` went the other way on this host (1.12x on Windows in
part 6, 1.46x here); it still passes, but it is the next one to watch.

### What changed

- `src/fastmath.rs`: the fdlibm Rust bodies of `exp`, `log`, `sin`, `cos`
  and `atan2` and all their constants and helpers moved to
  `seed/src/engine/fastmath.rs`. The package keeps the public API, the ULP
  bounds, the DetFloat symbol list and `ulp_distance`; the port-backed build
  calls `@vesper/kinetic`, the seed build calls the frozen engine
  (563 to 142 non-comment lines in `src/fastmath.rs`).
- `tests/port_differential.rs`: `fastmath_matches_the_rust_build`, bit for
  bit on 752,532 cases (special values, both edges of every binade, random
  arguments inside and outside the fast reduction's range, random bit
  patterns; all ten binary64 and binary32 functions). 0 differences.
- `flip-gate.toml`: the `fastmath` workload and the new engine file.

### Finding: NaN payloads

The first run of the new differential found 2 differences, both in
`atan2(NaN, NaN')` (and its binary32 form) with two different NaN inputs:
the port returned `x`'s payload, the Rust build `y`'s. Both builds compute
`x + y`; LLVM treats `fadd` as commutative and swapped the operands, so the
Rust build's payload is itself an accident of codegen (Rust leaves NaN
payloads of arithmetic unspecified). The differential therefore treats any
two NaNs as equal; every non-NaN result is compared bit for bit.

### Checks

Flip gate PASS: every test of the port and the seed
(`cargo test --release -p vesper-kinetic -p vesper-kinetic-seed`), 0
differences on 754,241 differential cases, all five ABBA workloads under
1.50x. The existing `fastmath` divergent edit (the cosine kernel's tail
sign) stays in `build.rs`.

## Part 8: Kinetic `morton`, `lbvh` and `crmath` flipped

Base: `main` `7f9b71c`, the same host and compiler as part 7; the gate
ran before part 7's `fastmath` was stacked under it (on the stacked branch
the seven differentials cover 1,960,401 cases, 0 differences). All three modules were above the bar in part 7; each needed a
change in the port, not in the compiler.

### Measurements

`scripts/ci/flip_gate.py vesper-kinetic` (`n = 30`): **PASS**, every test
of the port and the seed, 0 differences on 1,207,869 differential cases.

| Workload | Part 7 | Now | Decision |
|---|--:|--:|---|
| `morton` | 2.07x | **0.77x `[0.76x, 0.77x]`** | **Flipped** |
| `lbvh` | 1.54x `[1.53x, 1.56x]` | **1.32x `[1.31x, 1.33x]`** | **Flipped** |
| `pile-lbvh` | 1.37x | 1.16x `[1.15x, 1.18x]` | (scene workload over `lbvh`) |
| `crmath` | 1.92x | **1.39x `[1.38x, 1.41x]`** | **Flipped** |
| `colorer` | 0.82x | 0.80x `[0.79x, 0.80x]` | flipped earlier |
| `multi-colorer` | 1.30x | 1.31x `[1.30x, 1.32x]` | flipped earlier |
| `gjk` | 1.46x | 1.41x `[1.39x, 1.41x]` | flipped earlier |
| `sap` | 1.07x | 1.07x `[1.05x, 1.07x]` | flipped earlier |

### What changed

- **`morton`** was bound by the export ABI, not by its body: each call
  passed a fail buffer and a result buffer for a ten-instruction
  interleave. The port's exports now return one packed `u64` (the code, or
  bit 63 with the axis and value of the error; decodes pack
  `x | y << 10 | z << 20`), and the interleave reads generated lookup tables
  (`MORTON_SPREAD10`, `MORTON_COMPACT_B0..B3`) in place of the magic-bits
  shifts. The `MortonError` enum leaves the Vesper side; Rust builds the
  typed error from the packed word. The BMI2 `pdep`/`pext` paths and the
  bit-by-bit specification stay in `src/morton.rs`.
- **`lbvh`**: `pairs()` pruned too late. A child is now tested before it is
  pushed: a node whose highest leaf (the end of its right spine, filled
  once per query into the scratch it already had) is not above the query
  leaf can never yield a pair, so it is never visited. The leaf test
  `q > p` goes away with it. The divergent edit moves to the push-time
  test.
- **`crmath`** spent its time in bignum arithmetic and allocation (about
  40% allocation: about 242,000 sequence allocations per run against the
  Rust build's 70,000 `malloc`s). `bignum.ves` moves from 32-bit to
  64-bit limbs (`u128` products, two-step 32-bit small division, Knuth D
  for long division; the constant cache keeps its 32-bit layout) and gains
  in-place add, subtract, negate, truncating shift and small division.
  The series loops of `exp`, `log`, `sin`/`cos`, `atan` and the constant
  series use them, so a term no longer allocates three intermediates.
  Instructions fell from 142.6M to 120.6M on the 64-bit limbs alone.
- Each module's Rust body moved to `seed/src/engine/{morton,lbvh,crmath}.rs`
  (1,122 lines); `src/` loses 1,141 lines and gains 83 (dispatch and the
  packed-word unpacking), net **1,058 Rust lines out of the package**.
- `tests/port_differential.rs`: `morton_matches_the_rust_build` (1,133,072
  cases: every 16-bit value through `spread`/`compact`, edges and random
  coordinates and codes through the checked forms, errors compared),
  `lbvh_matches_the_rust_build` gains 12 benchmark-sized rounds with small
  pair budgets (408 cases), and `crmath_matches_the_rust_build` (73,040
  cases: Annex F specials, edges, random inputs in each reduction range and
  random bit patterns, all ten functions, bit for bit).
- Divergent edits: `lbvh` moves to the push-time test, `bignum` to the new
  64-bit low word (`big_low_u64` keeps only its low half; the binary64 ULP
  test fails, binary32 passes). All four of `morton`, `lbvh`, `crmath` and
  `bignum` checked: the named test fails, the other passes.

### Compiler gap found

`mul_wide(a, b) -> (lo, hi)` lowers to one `mul`/`umulhi` only when it
stays a call (`src/native.rs`); once the inliner inlines its portable
schoolbook body it becomes four multiplies. The bignum port uses
`wrapping_mul` on `u128` values instead, which is better but not one
instruction. Reported for the codegen owners; not fixed here.

### Checks

`vesper fmt --check` on the changed `.ves` files,
`rustfmt +1.100.0 --edition 2024 --check` on the changed Rust files,
`cargo +1.100.0 clippy -p vesper-kinetic -p vesper-kinetic-seed
--all-targets -- -D warnings` and `tools/bazel/sync_build_files.py --check`.

## What is left

- **Resonance:** slab (1.89x) and events (8.00x) stay above the bar until codegen removes their hot-path overhead (gap 7). The Rust
  `CompiledGraph` over Rust `Box<dyn DspProcessor>` trait objects, the
  Cranelift JIT (`clif.rs`, which goes through `@vesper/cranelift` per the
  plan), the `rtrb` rings (`spsc.rs`) and the denormal registers are not
  ported yet. The SIMD paths are ported (part 3), but they run at 8-48x and
  are bound by gaps 2 and 6.
- **Kinetic:** every isolated module is flipped (part 8). Next, port `world`/`tile`, `rigid` and `rigid/**`, `soft`,
  `articulation`, `couple`, `diff`, `rigid_diff`, `rollback`, `snapshot`,
  `detfloat`, `simd` (see `port-p3b-kinetic.md`, "Not ported yet"), then the
  speed of the ported modules (1.3-2.8x in P3b).
- The census row in `scripts/rust-census.py` is updated with each part.
