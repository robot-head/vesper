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

## What is left

- **Resonance graph to 1.5x:** gaps 2 and 3 above, or a further port rewrite
  of the schedule loop.
- **Resonance flip** (seed pattern, the compiler links it through
  `native_verifiers`): the port becomes the default build of
  `vesper-resonance` and the Rust engine stays in `seed/`. The Rust
  `CompiledGraph` over Rust `Box<dyn DspProcessor>` trait objects, the
  Cranelift JIT (`clif.rs`, which goes through `@vesper/cranelift` per the
  plan), the `rtrb` rings (`spsc.rs`), the denormal registers and the SIMD
  intrinsics are not ported yet.
- **Kinetic:** `world`/`tile`, `rigid` and `rigid/**`, `soft`,
  `articulation`, `couple`, `diff`, `rigid_diff`, `rollback`, `snapshot`,
  `detfloat`, `simd` (see `port-p3b-kinetic.md`, "Not ported yet"), then the
  speed of the ported modules (1.3-2.8x in P3b).
- The census row (`scripts/rust-census.py` is in the plan PR #1415, not yet
  on `main`).
