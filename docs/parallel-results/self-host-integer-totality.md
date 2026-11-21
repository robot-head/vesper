# Self-hosting SH5 lane 5: total shifts, guarded division and integer `!`

Claim #1328. Base `origin/main` at `bbda89e`. Prompt:
[05-integer-totality.md](../agent-prompts/self-host-sh5/05-integer-totality.md).
Rust stays the default: nothing here replaces or deletes Rust code.

SH4 lane 17 (#1319, backend checks) reported three integer gaps that made
its package add handlers and wrapping calls that never fire. What main
already did, what it did not, and what this lane changes:

| Repro | Main | This lane |
|---|---|---|
| `if s < 64 { 1 << s }`, `w >> (n & 63)`, `if n >= 64 { return 0; } w << n` | total | total |
| `if i < 8 { (w >> (i * 8)) & 255 }` | `Fail` at `i * 8` and at the shift | total |
| `let hw = (w >> 21) & 3; x << (hw * 16)` | `Fail` twice | total |
| `w >> ((i % 8) * 8)`, `w >> (63 - (i & 63))` | `Fail` twice | total |
| `if b == 0 { return 0; } a / b` (unsigned) | total | total |
| `w / ((i & 15) + 1)`, `if i < 100 { w % (i * 3 + 2) }` | `Fail[RuntimeError]` and overflow | total |
| `if d == 0 { return 0; } a / d` (signed) | `Fail` (`MIN / -1`) | `Fail` (unchanged rule) |
| `if d >= 0 && d < 10 { a / (d * 2 + 1) }` (signed) | `Fail` | total |
| `!x` with `x: u64` | E0200 | `~x` |

So the guard-range shift and division rules were already in main (the
bounds-facts and bit-builtins lanes). The gap was arithmetic: a shift
amount or divisor computed from a bounded value (`i * 8`, `hw * 16`,
`63 - n`, `k + 1`) had no bounds, and its own `*`, `+` and `-` needed
`Fail` because range facts prove those only for `for` loop indices. Lane
17's `dv` helper (`if b == 0 { return 0; }` plus a `handle Fail`) did not
need its handler on main.

## What changed

- **Shift amounts and divisors** (`src/integer_widths/check.rs`,
  `operand_bounds`, `total_operation`, `divisor`; the two hunks of
  `src/check.rs` that call them). A shift amount and the right operand of
  `/` and `%` are checked in a *total-operand* context. There, a pure
  `a + b`, `a - b`, `a * b`, `a / b` (nonnegative bounds) or `a >> b` (amount
  in range) of bounded operands has the interval its operands' bounds give
  when every value of it lies in the expression's kind, and that `+`, `-` or
  `*` raises no `Fail[ArithOverflow]`. The shift then needs no check when
  the interval lies in `0..bits`, and the division none when it excludes
  zero. A signed `/` also uses the divisor's interval for the `MIN / -1`
  rule, in any context. Outside a shift amount or divisor nothing changes:
  `i + 1` under `if i < n` keeps its check (see "Why only there").
- **Integer `!`** (`Checker::integer_not`, `complement_nots`). Prefix `!`
  on an integer operand is the bitwise complement. The checker records its
  span, the module is rewritten to `~` and checked again (the pattern
  `?` on `Option` and `for` over sequences use), so every backend (the
  evaluator, the JIT and objects, Lean, eBPF, wasm, the verified Stage 1
  path) sees the `~` it already supports. The evaluator also maps an
  integer `!` to `~` for worlds evaluated unchecked. `!` on `bool` keeps its
  meaning; `!x` of an integer where a `bool` is expected is E0200. No parser
  change: `!` already parsed on any operand, so front v2 and the AST need no
  mirror.
- **Manuscript (flagged)**: ch. 3, "Integer kinds": `!` on an integer is
  `~`, and the total-operand interval rule with its examples.
- **Tests**: `tests/integer_totality.rs` (8 tests): every repro above
  checks without `Fail` and runs in the evaluator (optimized and not) and the
  JIT (optimized and not), and compiles to x86-64 and AArch64 objects, with
  results equal to a Rust model over 40-70 inputs; narrow and signed kinds;
  programs that could fail (`i < 9`, `63 - i`, a negative signed amount,
  `i - 1`, `i + 1` of an unbounded `i`, `i & 15` as a divisor, signed
  `MIN / -1`) still need `Fail` at the right span; `!` on every kind
  including 128-bit; `!x` and `~x` programs give byte-identical objects.

### Why only there

Range facts could prove any `+`, `-` or `*` (`i + 1` under `if i < n`).
That would drop checks across existing programs, including
`packages/vesper-compiler`, whose Stage 1 artifacts are locked
(`bootstrap/stage1.lock`), and change the objects of programs that use none
of the new rules. Inside a shift amount or a divisor, main always required
`Fail` for such arithmetic, so only programs that the old rules rejected
without a handler, or that installed one, change.

## Default objects

Old (`bbda89e`) and new debug compilers on every `.ves` file and package in
the repository (672 inputs: `tests/**`, `examples/**`, every `Loom.toml`
directory under `crates/` and `packages/`, loose sources): `vesper check`
output and exit code, and `vesper emit --emit object` bytes, log and exit
code.

| Compared | Inputs | Differ |
|---|--:|--:|
| `check` output and exit code | 672 (324 check clean) | 0 |
| `emit --emit object` log and exit code | 672 | 0 |
| object bytes (inputs that emit an object) | 157 | 0 |

No input has arithmetic in a shift amount or divisor that the new rule
proves (main checked it, so ports used `wrapping_*` there instead), and
main rejected an integer `!`, so no output changes.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changed.
The bootstrap-stage1 workflow (`bootstrap/run.sh stage1 --reproduce --outcome`) on this branch is linked from the PR; every verified artifact digest must equal `bootstrap/stage1.lock`.

## SH4 packages on a local rewrite

The rewrite is published as a patch, not committed (SH4 lane 17 owns the
package): `backend-checks-local-rewrite.patch` in the project files
(`sh5-integer-totality/`), against #1319's head `5320490` with this branch
merged in. It changes 6 files (+9/-18):

- `util.ves` `dv`: the `handle Fail[RuntimeError]` after the zero guard goes
  (main already accepted that; the handler was never needed).
- `stack.ves` `byte` and the `movz`/`movk` decoder, `detfloat.ves` lane
  extraction: `wrapping_shr_u64(x, wrapping_u32(wrapping_mul(i % 8, 8)))`
  becomes `x >> ((i % 8) * 8)`, `wrapping_shl_u64(v, wrapping_u32(wrapping_mul(hw, 16)))`
  becomes `v << (hw * 16)`, and `x ^ 18446744073709551615` becomes `!x`.
- `consttime.ves` `slot_s`, `lib.ves` `dump`, `verify.ves`: `1 << slot`
  under `slot < 64`, `1 << bit` over `0..4`.

Sites left on `wrapping_*`: amounts nothing bounds (`rule`, `imm`, `ln`
in `consttime.ves` and `detfloat.ves`), where wrapping is the intended
meaning. On `main` the rewritten package fails to check (12 errors); with
this lane it checks.

`tests/self_host_backend_checks.rs` (`port-compiler` profile, Linux x86-64)
on the rewritten package:

| Test | Runs | Differences |
|---|--:|--:|
| `verifier_fixture_reports_match_rust`, `capability_reports_match_rust`, `corpus_reports_match_rust` (unoptimized package) | all | 0 (pass) |
| `optimized_package_agrees` | 28 | 0 |

Speed (`backend_checks_speed`, ABBA best of six, 145 corpus programs,
optimized package, decode subtracted):

| Package | All verifiers | Without stack | Ratio to Rust (without stack) |
|---|--:|--:|--:|
| #1319 as is | 85.4 ms | 74.1 ms | 0.94x |
| rewrite | 86.5 ms | 74.6 ms | 0.94x |
| rewrite without the `dv` change | 89.7 ms | 75.0 ms | 0.97x |

Within noise: the removed handler and wrapping calls are off the hot paths
(the optimizer already lowered `wrapping_shl_u64` to one instruction and
the handler is entered once per table scan). One earlier run of the rewrite
gave 34 ms without stack while `optimized_package_agrees` ran beside it; it
did not reproduce and is not counted.

The eBPF backend package (#1308) has no site the new rules free: its
shifts by constants (`wrapping_shl_u64(x, 8)`, `rem << 32`) already checked
on main, and `put_bytes` shifts by a loop-carried counter `k * 8` that
nothing bounds (compiler gap 2). No patch for it.

## Compiler gaps

1. **`wrapping_shl_k`/`wrapping_shr_k` amounts are still `u32`.**
   `fn bit(n: u64) -> u64 { wrapping_shl_u64(1, n) }` is E0200. With this
   lane the guarded or masked form `1 << (n & 63)` is total, so the wrapping
   builtins are only needed for a genuinely wrapping amount. Taking any
   integer kind would make the reserved prelude functions generic in their
   amount (their lowering in `lower_bit_builtin_calls` assumes `u32`).
2. **Loop-carried counters have no range.**
   `let mut k: u32 = 0; for .. { x << (k * 8); k = k + 1; if k == 8 { k = 0; } }`
   (eBPF `put_bytes`) cannot be proven: the checker keeps no invariant for a
   local the loop body writes. A `k & 7` mask, or iterating `k in 0..8`, is
   total.
3. **Literal typing does not look through `!`/`~`.**
   `let mask = !(65535 << s); x & mask` with `x: u64` types the literal
   `i64` (E0200); `let mask: u64 = ...` is needed. Same as `~` on main.
4. **Ranges of other operators.** `|`, `^`, `widen_k` of a bounded operand
   and calls inside a shift amount do not get the arithmetic interval
   (calls keep their result range). None of the lane 17 sites needed them.

## What's left

- SH4 lane 17 can adopt the rewrite below in its own package (it owns
  `packages/vesper-backend-checks`): its `dv` helper loses the handler, and
  its shifts by `wrapping_u32(wrapping_mul(i % 8, 8))` become `>>`.
- The gaps above.
