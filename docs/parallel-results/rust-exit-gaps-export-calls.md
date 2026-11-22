# Rust-exit gap wave 1, lane 5: cheap calls across the export ABI

Claim [#1502](https://github.com/robot-head/vesper/issues/1502). Prompt:
[05-export-calls.md](../agent-prompts/rust-exit-gaps-1/05-export-calls.md).
Base `main` at `66255e17`. Every number is from one shared 4-vCPU cloud
container: callgrind instruction counts (Ir) are exact, wall-clock ABBA
ratios move by about 0.1x between runs.

## What the gap really was

The Kinetic/Resonance report (gap 7) put about 15 ns per call on the export
boundary: the status buffer, the word conversions and the lost inlining.
Measured on its own, the boundary costs about 1 ns:

| Call (20M calls, best of 7) | Rust (`#[inline(never)]`) | Vesper export |
|---|--:|--:|
| `nop(x: u64) -> u64` | 0.38 ns | 1.26 ns |
| `add(a, b)` with `Fail[ArithOverflow]` (status buffer) | 0.68 ns | 1.23 ns |
| `first(in xs: Slice[u64])` (slice view built by the binding) | 0.84 ns | 2.22 ns |
| `half(x) -> Option[u64]` (`abi 3 options`, result pointer) | 0.94 ns | 1.95 ns |
| `ramp_next(inout r: Ramp)` (mirror, no conversion) | | 1.76 ns |

The exported symbol is the function itself, so a leaf export is a plain C
call plus Cranelift's frame-pointer prologue. The binding is a few stores
(`[0u64; N]` status words, a three-word slice view) and one branch on the
status. That is within a nanosecond or two of a Rust call already, so a
second "fast path" calling convention would not buy anything measurable.

Callgrind on Resonance's `pid` workload (200,000 `PidController::step`
calls) shows where the time went instead:

| Function | Ir per step |
|---|--:|
| `vesper_resonance_pid_step` (the export body) | 204 |
| `kernels::f` (the slice accessor, 8 calls per step) | 120 |
| the binding and `with_slice` (not inlined) | 62 |
| the crate's wrapper (`PidController::step`, inlined into the caller) | 42 |
| **port total** | **450** |
| Rust total | 85 |

`f` is `fn f(in s: Slice[u32], i: u64) -> f32 eff [Fail[RuntimeError],
Fail[ArithOverflow]] { from_bits_f32(s[i]) }`. The inliner refused it in
`k_pid_step`, whose row also has `Fail[PidFault]`: `site_bindings` inlined
a callee with a non-empty row only when the rows were equal ("effect row
differs from the caller's"). Inlining `f` by hand took the workload from
86.5M to 44.1M Ir and from 2.2x to 1.04x of Rust.

## The fixes

1. **Inliner: a callee whose row is within the caller's.** `site_bindings`
   (`src/ir_inline.rs`, `row_within`) now also inlines a loop-free callee
   whose unlabeled `Fail[E]` instances are all in the caller's row. The raise
   in the inlined body performs the caller's `Fail[E]`, which the native
   backend selects by error type (`instance_capability_key`), the same
   instance the caller's re-raise after the call selected. Callees with a
   loop keep the equal-row rule: inlining Resonance's `accumulate2` kernel
   into `sum_inputs` took the `graph` workload from 1.00G to 1.80G Ir
   (the merged loop nest spills, the "spills in large functions" item of
   #1426), so narrower-row kernels stay calls. The Vesper port
   (`packages/vesper-ir-inline`, `row_within` and `has_loop`) makes the same
   decision; the input stream now carries an instance index per row entry
   (`tests/common/inline_words.rs`, four words per instance), so the port
   compares instances without reading their types. Lane 2 (#1499) agreed to
   this split: its cold-path check sits after `site_bindings`.
2. **vesper-port-abi.** `seq_copy`/`seq_take` (returned `ChunkSeq`s),
   `seq_from` and `with_seq_mut` copy packed scalar elements (`u8`, `u32`,
   `u64`, `i64`, `usize`, `isize`, `f64`) as one block instead of element by
   element. `native::<T>()` compares type ids one by one, so it folds to a
   constant, and the lenders (`with_slice`, `with_slice_mut`, `with_seq`,
   `with_str`, `str_view`) are `#[inline]`: `with_slice` was a separate
   call in every binding before. These functions now take `T: 'static`
   (every generated binding already passes a concrete type).
3. **vesper-span `edits`.** `edits_apply` took and returned bytes, and the
   crate ran `String::from_utf8` over the 64 KiB result (a third of the
   port's instructions). It now takes `in text: Str`, `in pool: Str` and
   builds a `String` from `Str` views with `push_str` (which copies runs of
   16 bytes or more with `memmove`, #1147). Every span and replacement is on
   a char boundary (`edits_check`), and the binding hands a returned
   `String` to Rust without validating it again (`str_view`), so the
   validation is gone. `crates/vesper-span` has no lane; the change is in
   this PR and flagged in its body.

`push_str` with `memmove` and unchecked string results were already on
`main` (#1147, #1160); the prompt's step 2 items for them needed no change.

## Numbers

### Resonance (`crates/vesper-resonance/examples/abba.rs`)

Callgrind, port arm, compiler on `main` against this branch (both with the
new vesper-port-abi; the port-abi change alone moved `pid` by 4%, `slab` by
4%):

| Workload | `main` Ir | This branch Ir | Rust Ir | ABBA port/Rust, `main` | ABBA, this branch |
|---|--:|--:|--:|--:|--:|
| `pid` (200,000 steps) | 86.5M | **44.1M** | 17.0M | 2.24x (1.97-2.76) | **1.05x (1.04-1.06)** |
| `events` | 134.6M | 128.1M | 38.8M | 4.46x (4.41-4.52) | 4.8x (noisy, 2.9-5.4) |
| `slab` | 84.3M | 84.3M | 29.8M | 2.22x (2.17-2.24) | 2.13x (2.11-2.15) |
| `kernel` | 4,122M | 4,122M | 1,813M | | 1.04x |
| `graph` | 1,001.9M | 1,001.9M | 761.9M | | (noise; Ir unchanged) |
| `schedule` | 225.0M | 225.0M | 199.7M | | 1.51x |
| `confine` | 226.8M | 226.8M | 152.0M | | 1.64x |

`pid` now passes the bar. No other workload's instruction count rose.

### vesper-span (`crates/vesper-span/examples/abba.rs abba edits`, 30 epochs)

| | `main` | This branch |
|---|--:|--:|
| `edits` Ir (`once edits vesper 20`, Rust 10.3M) | 19.6M | 14.2M |
| `edits` ABBA Vesper/Rust median | 2.83x (2.01-4.09) | **1.68x (1.36-2.51)** |

`port/test-port.sh` still catches all three divergences, and every
vesper-span test passes.

## What keeps the rest above the bar

- **Resonance `events` (`ParamRamp::next_value`, one export call per
  sample).** The Vesper body is 25 Ir against about 10 for the Rust
  function, but the Rust build inlines `next_value` into the caller's loop
  over 16 ramps; a C call across the boundary cannot be inlined into Rust
  (no cross-language LTO with Cranelift objects). The crate's wrapper also
  converts `ParamRamp` to its mirror and back on every call
  (`mirror()`/`load()`). Closing it needs a coarser entry point (a block of
  samples per call), which is the crate's API, or keeping the ramp in its
  mirror form in the crate.
- **Resonance `slab` (`DelaySlab::read`/`write`).** The bodies are 160 and
  120 Ir per call against about 56 in Rust: checked index arithmetic,
  repeated bounds checks and slice-header reloads (`(view)->header->data`
  for every access). Those are the bounds-hoisting thread's and #1426's
  items, not the boundary.
- **vesper-span `edits`, 1.68x.** `check_edits` (2.8M Ir against 1.3M in
  Rust's `SourceEdits::new`) is the rest: same codegen items.
- **Loom CAS** (codecs 1.2-3.5x, 1 MiB reads 1.8x) is on PR #1493, not on
  `main` yet, so it is not re-measured here. The block copy of returned
  byte sequences applies to its reads once it lands.

## Stage 1 guard

The inliner reaches the compiler. `bootstrap/run.sh stage1 --reproduce
--outcome`: see the PR body for the result.

## Tests

- `tests/export_call_cost.rs` (new): a narrower `Fail` row is inlined and
  each of three errors (two built-in, one with a payload) reaches its own
  handler in the evaluator and the JIT, optimized and not; a narrower row
  with a loop stays a call; a callee with an instance the caller lacks
  still works.
- `tests/self_host_ir_inline.rs`: the Vesper inliner's decisions and
  programs match Rust over the corpus (5 tests).
- `tests/native_inline*.rs`, `cargo test -p vesper-port-abi`,
  `cargo test -p vesper-span`, `crates/vesper-span/port/test-port.sh`.
