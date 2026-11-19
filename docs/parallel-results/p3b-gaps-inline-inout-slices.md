# P3b gaps lane 4: inline helpers that take an `inout` slice

Claim #1089, PR #1109. Base `9305b24` (origin/main when the lane started).
Prompt: [04-inline-inout-slices.md](../agent-prompts/p3b-gaps/04-inline-inout-slices.md).

## Result

`ir_inline` now inlines callees with `inout` slice parameters. The Warp
`put3` repro (port-p3b-warp.md, gap 1) goes from three calls to none.
Instruction counts drop by 12.5% on Warp frames, 19.9% on Warp ack and 9.0%
on the warp-tcp codec. No port source changed; every number below is the
same port source built with the compiler before and after this lane.

## What changed

`src/ir_inline.rs`, `inlinable` refused any callee with a slice parameter
whose pass was not `in` ("inout or sink slice"). A slice can only be an `in`
or `inout` parameter (E1520). The native call lowering passes both as the
address of the view (pointer and length). An `inout` callee writes elements
through the view's pointer and has no way to re-point the view or change its
length, so writes from a copy of the view land in the same buffer. The
bindings `site_bindings` already chose for `in` slices are therefore correct
for `inout`:

- an argument that names a caller local or parameter of the same slice type
  (`w8(inout out, ..)` inside `put3`) replaces the callee's parameter with
  that place;
- any other argument (a sub-range `s[lo..hi]`, `q.as_slice_mut()`) binds a
  copy of the view to a fresh local.

Only `sink` slices stay refused (`Refusal::Callee("sink slice")`). E1520
already rejects them, so the refusal is only a guard. The capability-caller
rule (`ConstTime`/`Realtime`/`DetFloat` rows keep their calls), the `Box`
rules and every growth budget are unchanged. The module documentation says
the same.

No manuscript change.

## Tests

`tests/native_inline_inout_slice.rs`. Every program runs in the reference
evaluator (optimize on and off), the JIT (optimize on and off), x86-64 and
AArch64 object emission, and a `vesper build` executable where a C driver
links:

- `put3_repro_inlines`: the repro, called with `inout s.as_slice_mut()`. No
  `w8` call is left in `put3`, and `put3` is inlined into `main`.
- `put3_object_has_no_helper_calls`: the optimized executable object for
  both targets. Neither `main` nor `put3` calls `w8` or `put3`.
- `writes_reach_the_callers_buffer`: nested writers (`put_u16` inside
  `fill_hdr`) called on sub-ranges, on `as_slice_mut()`, and on short views
  where some `try_set`s fall out of range. The result is checked against a
  Rust model.
- `looping_and_failing_writers`: a loop over a `Slice[u64]` and a writer
  with a `Fail[RuntimeError]` row whose `set` faults. The inlined raise
  reaches the caller's handler.

The capability-caller rule could not get a slice test: a `Realtime` or
`DetFloat` body cannot call any slice method (E0709, "callee cannot be
resolved statically"), so no such caller can reach an `inout` slice helper.

Also run locally and passing: `native_inline`, `native_inline_reach`,
`native_inline_cost`, and the suites that pass `inout` slices
(`ffi_handles`, `native_ascii_in_place`, `native_bytes`,
`native_bytes_builtins`, `native_copy_within`, `seq`, `native_simd_bytes`).

## Stage 1 guard

This lane edits only `src/ir_inline.rs`, its new test and this report. It
touches no verified-codegen, `ir.rs`, `ir_opt::optimize`, Lean, bootstrap or
compiler-package path, and the inliner is never run with `optimize` off,
which is how the Stage 1 lock corpus is built. The bootstrap-stage1 workflow
(`bootstrap/run.sh stage1 --reproduce --outcome` against
`bootstrap/stage1.lock`) was run on the PR branch; its result is linked in
the PR.

## Measurements

Linux x86_64 cloud host, release builds. The compiler is a release `vesper`
built at `9305b24` (before) and at this branch (after), passed to each port
build as `VESPER_PORT_COMPILER`. Warp is the port from PR #1079 (head
`94ab147`, since merged as `d9044a6`). The warp-tcp port is the one on main.

### Repro

`vesper build --lib repro.ves --lib-name repro`, `objdump -d`:

| | `vesper_repro_put3` calls | `vesper_repro_get3` calls |
|---|---|---|
| before | 3 (`w8`) | 0 |
| after | 0 | 0 |

After the change the three stores are inline, with their offsets (0, 1, 2)
folded to constants. The slice header is still reloaded before each store;
that is lane 3's (#1088) header-reload work.

### Port objects

`call` instructions in each port's x86-64 library object (`objdump -d`,
including calls into the runtime). This is the same source built by both
compilers.

| Port | Source | Calls before | Calls after | Object bytes before -> after |
|---|---|---|---|---|
| Warp | PR #1079 | 146 | 68 | 139,528 -> 181,704 |
| warp-tcp | main | 147 | 115 | (concatenated `PARTS`) |
| Kinetic | PR #1078 | 68 | 59 | 64,960 -> 71,128 |
| Resonance | PR #1082 | 1,565 | 1,546 | 236,128 -> 282,976 |
| Prism H3 | main | 2,152 | 2,152 | identical bytes |

Prism H3's object is byte-identical: it has no `inout` slice helpers that fit
the budget.

### Warp and warp-tcp ABBA

Each run is the committed `examples/abba.rs` driver: 12 A/B/B/A epochs,
Rust worker against the Vesper worker. Instructions are callgrind `Ir` for
the whole process. Warp uses `(Ir(once 3) - Ir(once 1)) / 2` per batch, as in
port-p3b-warp.md. warp-tcp uses `(Ir(64 requests) - Ir(1 request)) / 63`
per request of `worker codec`.

| Workload | Ir Rust | Ir before | Ir after | Ir ratio before -> after | Change | Wall ratio before (95% CI) | Wall ratio after (95% CI) |
|---|---|---|---|---|---|---|---|
| Warp frames | 4.18M | 9.57M | 8.38M | 2.29x -> 2.00x | -12.5% | 2.37x (2.36-2.41) | 2.02x (1.38-2.18) |
| Warp ack | 5.54M | 11.23M | 8.99M | 2.03x -> 1.62x | -19.9% | 1.79x (1.73-1.91) | 1.37x (1.08-1.52) |
| warp-tcp codec | 511K | 353K | 321K | 0.69x -> 0.63x | -9.0% | 0.85x (0.79-0.88) | 0.70x (0.50-0.93) |

The "before" Ir ratios match the port reports (Warp frames 2.29x and ack
2.03x in port-p3b-warp.md). Wall clock on this host moves by up to 30%
between runs, and the "after" runs had wide intervals, so the instruction
counts are the firmer result. All digests agreed between the arms.

Resonance and Kinetic are on open port PRs. Only their objects were
compared (above), not their benchmarks.

## What is left

- `sink` slices stay refused. They cannot be written today (E1520).
- Header reloads: each inlined `try_set` reloads the view's pointer and
  length from the caller's place. Lane 3 (#1088) owns that.
- Nested `inout` loans such as `w8(inout out, w8(inout out, 0, 1), 2)` are
  still rejected by the checker (the inner loan overlaps the outer one).
  That is lane 9's (#1094) two-phase argument work, not an inliner limit.
- In Warp, `VESPER_INLINE_LOG` lists 7 call sites still over the size
  budget and 1 partial inline after this change. Many of the other calls
  in the object go to the sequence runtime (`vesper_seq_new`, `_make`,
  `_grow`).
