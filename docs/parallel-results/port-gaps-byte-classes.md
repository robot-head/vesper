# Port gaps wave, lane 4: byte classes, ASCII case and a two-needle find

Claim: #987. PR: #1002. Branch `claude/project-thread-wwznz4`, from `main` at
`07e7d4e` (which includes #983), with `main` merged in at `2e5e941`.

## What changed

Four total builtins on `ChunkSeq[u8]` and `Slice[u8]` (§13, "Byte classes and
ASCII case", flagged in the PR):

| Builtin | Result | Replaces |
|---|---|---|
| `s.find_not_in(lo: u64, hi: u64, high: bool)` | `Option[u64]`: the first byte outside the class (bit `x` of `lo` for `x < 64`, bit `x - 64` of `hi` for `x < 128`, `high` for `x >= 128`) | `tchar`, field-value, origin-form and identifier loops |
| `s.find_either(a: u8, b: u8)` | `Option[u64]`: the first byte equal to `a` or `b` | CR-then-LF double `find_byte` |
| `s.eq_ignore_ascii_case(in t)` | `bool` | folded literal compares |
| `out.extend_ascii_lowercase(in src)` | `()`, `ChunkSeq[u8]` only, `E0301` against its own sequence | per-byte `push(lower(b))` |

- Reference semantics are in `src/seq/eval.rs`, typing in `src/seq/check.rs`, and the `SeqOp` variants in `src/seq.rs`. Lean refuses all four, like the other byte scans.
- Native code (`src/seq/native_bytes.rs`) runs 128-bit steps with a scalar tail on the optimize path only, on x86-64 and AArch64:
  - `find_byte`, `find_either` and `find_not_in` share one search loop.
  - `find_not_in` looks up the row (`x >> 3`) and the bit (`x & 7`) of its class with two `swizzle`s. `swizzle` reads 0 for indices >= 16, so bytes >= 128 read no class bit and take `high`.
  - Because of the shuffle, `find_not_in` is vectorized on AArch64, and on x86-64 only when the ISA has SSSE3 (`has_x86_pshufb_lowering`). Host builds detect SSSE3. The baseline `x86_64-unknown-linux-gnu` cross target runs `find_not_in` as a scalar loop, so it never reaches Cranelift's `pshufb` libcall. The other three builtins are SSE2 only.
- **Overlapping last step.** An input of 16 bytes or more now finishes with one vector step over its final 16 bytes instead of a scalar tail. This applies to `find_byte`, `find_either`, `find_not_in`, `eq`, `eq_ignore_ascii_case` and `extend_ascii_lowercase`. Bytes the loop has already seen are seen twice, which a search, a compare or an idempotent copy tolerates. On the field-value repro this took the builtin from 446 to 193 instructions. Inputs shorter than 16 bytes stay scalar.
- Tests: `tests/native_byte_classes.rs`. Each program runs in the evaluator, the JIT with and without `optimize`, and an AOT executable, and is checked against a Rust model. Coverage:
  - every length 0..=256 at all 17 starts
  - random and planted bytes at the first, a middle and the last position
  - all 256 byte values for six classes, including the edge bits 0/63/64/127, `high` on and off, the empty class and the full class
  - run-time masks
  - the case-bit neighbours of the letters
  - growth for the lowercase extend
  - objects that hold vector code on both targets, and none unoptimized
  - rejections
- `tests/native_simd_bytes.rs` (lane 6) passes unchanged with the shared loop and the overlapping step.

## Stage 1 guard

No edits under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**`. Every vector path
sits behind `operands_only` (optimize), and the Stage 1 corpus is built
unoptimized. bootstrap-stage1 run
[44255794776](https://github.com/robot-head/vesper/actions/runs/44255794776)
(`bootstrap/run.sh stage1 --reproduce --outcome` against
`bootstrap/stage1.lock`) passed on `077a103`. It is re-run on the final head,
see the PR.

## Setup

- Cloud container: 4 vCPU Intel Xeon @ 2.30 GHz (SSSE3/AVX), Linux 6.18, `rustc 1.97.0`, `--release`.
- Repros: `vesper build` executables (optimized). Each calls the function in a loop. The table gives callgrind `Ir` per call: `(Ir(100000 calls) - Ir(0 calls)) / 100000`, with everything inlined into `main`.
- Ports: the committed ABBA benchmarks. Prism is `examples/abba.rs` (`compare`, 15 epochs of 200 ops, plus callgrind of `measure 20` over all four workloads). Quarry is `examples/planner_abba.rs` (`driver`, 9 epochs, plus callgrind of `window 20`).
- The port rewrites are local and not committed (lane 9 adopts them). They are saved as `byte-classes-prism-rewrite.patch` and `byte-classes-quarry-rewrite.patch` in the project files.

## Repros (instructions per call)

| Repro | Input | Loop | Builtin |
|---|---|---|---|
| Prism `value_ok` (port-parity-prism gap 2) | 64-byte field value | 1,662 | 194 (`find_not_in`) |
| Prism `eq_lit` with `fold` (gap 3) | `Content-Length` vs literal, 14 bytes | 779 | 355 (`eq_ignore_ascii_case`, scalar: under 16 bytes) |
| Prism `line_break` (gap 4) | 45-byte header line, CRLF | 198 (283 before the overlapping step) | 106 (`find_either`) |
| Quarry `lower_ok` (port-parity-quarry gap 6) | 11-byte identifier in an `inout` struct's pool | 236 | 299 (`find_not_in` plus a bounds check, scalar) |

Short inputs gain little or lose: under 16 bytes the builtin is a scalar loop of about 16 instructions per byte, plus the view's bounds check and the `Option`. The Quarry repro is slower with the builtin. That case needs a masked or padded short load, which is not done here (see below).

## Ports

Prism rewrites:
- the field-value check of `parse_head` and `response_plan_header` and the `tchar` checks (`all_token`, `push_token_lower`, `response_plan_header`) use `find_not_in`
- the lowercase name copies use `extend_ascii_lowercase`
- `eq_lit`'s `fold` and the response `eq_fold` use `eq_ignore_ascii_case`
- `line_break` uses `find_either`
- `valid_target` and `valid_origin` skip runs of target characters with `find_not_in`

All Prism port tests pass.

| Prism | Rust | Port before | Port after |
|---|---|---|---|
| Instructions, `measure 20` (all workloads) | 53.31 M | 83.81 M (**1.57x**) | 74.47 M (**1.40x**) |

ABBA wall clock, median port/Rust (95% interval of the median). Both arms
ran back to back from the same binaries:

| Workload | Before | After |
|---|---|---|
| decode | 1.71x (1.55-2.03) | 1.62x (1.54-1.84) |
| route | 3.05x (2.85-3.62) | 2.94x (2.81-3.66) |
| encode | 2.63x (2.42-2.91) | 2.27x (2.18-2.84) |
| connection | 1.98x (1.81-2.20) | 1.88x (1.63-2.09) |

The wall-clock intervals overlap, so the instruction counts (-11%) are the
firmer result. `route` barely uses the new builtins.

Quarry rewrites: the lexer's word scan uses `find_not_in`. Identifiers from
the lexer are validated with `find_not_in` and lowercased from the SQL slice
with `extend_ascii_lowercase`. The catalog path's `identifier` is unchanged,
because `pool.extend_ascii_lowercase(in pool[..])` is `E0301`. All Quarry port
tests pass.

| Quarry | Rust | Port before | Port after |
|---|---|---|---|
| Instructions, `window 20` | 19.36 M | 33.00 M (**1.70x**) | 32.59 M (**1.68x**) |
| ABBA median regression (bp) | | +9,929 | +9,465 |

SQL identifiers and keywords are 2-10 bytes, below one vector step, so the
lexer gains about 1%.

## What is left

- **Short inputs.** Field names, identifiers and keywords are usually under 16 bytes and run the scalar loop. A padded load would bring them onto the vector path: a sequence's capacity rounded up to 16, or a page-safe over-read guarded by the address. That is a runtime-layout change (lane 1's area) or an unsafe-read policy, and it is not done here.
- **x86-64 baseline.** `find_not_in` is scalar for the SSE2-only cross target. A 7-compare range form would cover classes made of few ranges, but not arbitrary classes.
- **In-place lowercase.** There is no `pool.extend_ascii_lowercase(in pool[a..b])` (`E0301`), so Quarry's catalog path still lowercases per byte. Lane 3's `copy_within` pattern, or a `lowercase_in_place` over an `inout` slice, would close it.
- The vector constants of `find_not_in` (the table, the bit row, the `high` splat) are rematerialized inside the loop by Cranelift, about 8 extra instructions per step.
