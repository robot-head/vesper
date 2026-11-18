# Port gaps lane 5: bit-count builtins and total shifts (#988)

Base: `07e7d4e` (main after #983). PR #1000.

## What changed

- **Builtins.** For every integer kind `k` (all ten):
  `leading_zeros_k(x: k) -> u32`, `trailing_zeros_k(x: k) -> u32`,
  `count_ones_k(x: k) -> u32` (over the two's-complement bit pattern; the two
  zero counts are `bits(k)` at zero), and the total shifts
  `wrapping_shl_k(x: k, n: u32) -> k` and `wrapping_shr_k(x: k, n: u32) -> k`
  (amount taken modulo the width, `>>` arithmetic on signed kinds, as Rust's
  `wrapping_shl`/`wrapping_shr`).
- **Manuscript (flagged).** One sentence in the integer-kinds paragraph of
  `docs/manuscript/content.typ`. The names carry the kind as a suffix, like
  `widen_k`/`narrow_k`/`wrapping_k` and the byte-order family. A generic
  `count_ones(x)` would need a new Place IR form in `src/ir.rs`
  (`lower_world`), which the Stage 1 guard forbids.
- **Reference semantics.** The builtins are reserved prelude functions like
  `mul_wide` (`src/integer_widths.rs`): source bodies built only from total
  operators (a SWAR population count, rotations and constant shifts). Every
  backend (evaluator, verified, Wasm, eBPF, Lean) runs them without new
  support. `BitOp::eval` is the Rust model the tests check them against.
- **Checker.** A module cannot define a function with one of these names
  (E0100, like `mul_wide`). No new typing rules: they type as ordinary
  functions.
- **Native lowering.** `integer_widths::lower_bit_builtin_calls` rewrites each
  Place IR call to `Op::Unary`/`Op::Binary` right after `lower_world`, before
  the optimizer and the inliner. Without that, the inliner would copy the
  portable body in place of the call. It runs in `native.rs`,
  `native_session.rs` and `native_export.rs`. `FunctionCompiler::bit_builtin`
  then emits one `clz`, `ctz`, `popcnt`, `ishl`, `ushr` or `sshr` in the
  kind's own width, which Cranelift selects as `lzcnt`/`tzcnt`/`popcnt`/`shlx`/
  `shrx`/`sarx` on x86-64 hosts that have them (with fallbacks otherwise), and
  `clz`, `rbit`+`clz` and `cnt` on AArch64. The AST call path is intercepted
  too.

## Tests

`tests/bit_builtins.rs`:

- Every kind, 13 edge operands × 9 amounts (0, 1, bits-1, bits, bits+1,
  2·bits-1, 2·bits+3, 100, `u32::MAX`), plus 300 pseudo-random operand and
  amount pairs. Each case runs in the evaluator (optimized and not), the JIT
  (optimized and not), x86-64 and AArch64 objects, and an AOT executable, and
  is compared with `BitOp::eval`. The model itself is checked against Rust's
  `leading_zeros`, `trailing_zeros`, `count_ones`, `wrapping_shl` and
  `wrapping_shr` for every kind.
- The names are reserved (E0100) and typed (E0200 on a wrong kind).
- The CLIF of a function using all five builtins, optimized and not, holds
  `clz`, `ctz`, `popcnt`, `ishl` and `sshr` and no call.

`tests/integer_widths.rs` still passes. `cargo +1.100.0 clippy -p vesper
--lib --test bit_builtins -- -D warnings` is clean.

## Stage 1 guard

No edits to `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**`. The rewrite runs
only on the native paths, and only matches calls to these new reserved names,
which no existing program uses. As a check, the BBR port compiled by the
lane's compiler is byte-identical to the one main's compiler produces (below).
The `Bootstrap Stage 1` workflow (`bootstrap/run.sh stage1 --reproduce
--outcome` against `bootstrap/stage1.lock`) was run on the branch; its result
is linked in the PR.

## Repros, before and after

Setup: cloud x86-64 container (4 vCPU), debug in-tree `vesper`, `vesper build
--lib` (optimize on), instructions per exported function counted with
`objdump -d`.

| Function | Before | After |
|---|---|---|
| warp-wheel `shl(x, s)`, rotation form (gap 4) | 16 | 10 (`shlx`) |
| warp-wheel `shr(x, s)`, rotation form (gap 4) | 16 | 10 (`shrx`) |
| trailing zeros, compare loop | 19 static, plus the loop | 7 (`tzcnt`) |
| leading zeros, compare loop | 19 static, plus the loop | 7 (`lzcnt`) |

"After" is `if s < 64 { wrapping_shl_u64(x, wrapping_u32(s)) } else { 0 }`
and `widen_u64(trailing_zeros_u64(x))`. The 7 are the frame push and pop, the
count, a move and `ret`.

## Ports

Same machine, release profile, Rust 1.97. Numbers are Vesper/Rust.

**warp-wheel** (`cargo test --release -p vesper-warp-wheel --features
vesper-port --test abba -- --ignored --nocapture`, 30 epochs × 16 ops).
Local rewrite, not committed (lane 9 commits port changes): `shl`/`shr` in
`util.ves` use `wrapping_shl_u64`/`wrapping_shr_u64`, and `low_bit` in
`wheel.ves` is `widen_u64(trailing_zeros_u64(x))` instead of the de Bruijn
table lookup.

| Build | Median | 95% interval of the median |
|---|---|---|
| main port, run 1 | 1.850x | [1.820, 1.943] |
| main port, run 2 | 1.851x | [1.830, 1.862] |
| with the rewrite, run 1 | 1.807x | [1.777, 1.827] |
| with the rewrite, run 2 | 1.808x | [1.767, 1.828] |

That is about 2% faster. The rewrite also makes the `l_ctz` table and the
de Bruijn constant unused, so lane 9 can delete them.

**BBR** (`ack_path_abba abba <rust> <vesper> 16 200000`). There is no site
where these builtins help. `pow2` is already one `rotate_left`, and the
shifts in `div_wide_sat` have constant amounts. The port compiled by this
branch's compiler is byte-identical to main's (`cmp` of the two `vesper build
--lib` archives of the concatenated `warp_bbr.ves`), so before equals after.
Two ABBA runs on this branch measured a 95% interval of 2.49x-2.95x and
2.55x-3.15x, a gate regression of about 1.75x. Beacon, which asked for
`leading_zeros`, is not re-measured here because it is not one of this
lane's named ports.

## What is left

- **A range fact from a guard** (prompt item 3, optional): `x << s` inside
  `if s < 64` still needs `Fail[ArithOverflow]`. The checker tracks only
  exact constants (`integer_fact`), not ranges, so this is not small. With
  `wrapping_shl_k`, a port writes the guarded form with no handler, and it
  inlines.
- **Generic names** (`count_ones(x)` for any kind) need a Place IR form in
  `src/ir.rs`, which is outside the Stage 1 guard.
- **`mul_wide`** is not protected from the inliner in the same way. Its
  portable body could replace a call before the backend sees it. This lane
  did not check whether that happens. If it does, the same rewrite fixes it
  (it needs its own op, since `mul_wide` returns a tuple).
- `native_batch.rs` keeps the call form (unoptimized, no inliner), which is
  correct but not one instruction.
