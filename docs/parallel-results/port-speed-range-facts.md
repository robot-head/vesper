# Port speed lane 7: range facts from guards

Claim #1027, PR #1030. Base `3c075a4` (main after #1020).

## What changed

Before this change, the checker tracked only exact constants (`integer_fact`). So `x << s` inside
`if s < 64` still raised `Fail[ArithOverflow]`, and a port had to write
`wrapping_shl_u64(x, wrapping_u32(s))` or a rotation by hand to keep the helper
free of handlers.

- `src/check.rs`: each binding now carries inclusive bounds (`IntRange`). The bounds are tied to the
  binding's write identity (`Binding::mutation`). Every write, `inout` borrow,
  loop havoc and swap already renews that identity, so the range is dropped
  there with no edits outside the lane's paths. The one place that drops facts
  without renewing the identity is the join after an abortable handler, where
  an abort may skip the body's writes. That join now clears the range
  explicitly (found in review; `tests/inout_abort.rs` covers it).
- Bounds come from these sources:
  - the integer kind;
  - a comparison with a pure integer expression that dominates the use: the matching branch of `if`, `!`, `&&` in
    the true branch, `||` in the false one, and the code after an `if` whose
    other branch returns;
  - `let t = e` and `t = e`;
  - the hull of both branches at a join;
  - `x & m` with `m >= 0`, which lies in `0..=m`;
  - `x % m` with `x >= 0` and `m > 0`, which lies in `0..m`.
- A condition refines bounds only when it is built entirely from comparisons of pure
  operands (literals, names, operators). Then no part of it can write a name
  that another part read.
- The bounds are used for:
  - shift amounts in `0..bits` (`src/integer_widths/check.rs`);
  - divisors that exclude zero (`nonzero`);
  - signed `/` whose divisor excludes -1 or whose dividend excludes `MIN`;
  - `narrow_k` operands within `k`.
- Manuscript (flagged in the PR): the integer-kinds paragraph now says that an
  operation the checker proves cannot fail needs no `Fail` effect, and gives the
  range-fact rule. The manuscript did not state the static rule before.

`+`, `-` and `*` deliberately do not use ranges. `bootstrap/corpus/fib.ves` computes
`n - 1` in the else branch of `if n < 2`, so a range there would drop that
operation's overflow check and change the program's verified artifact (the
Stage 1 guard).

## Stage 1 guard

There are no edits to `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`, `lean/**`,
`bootstrap/**` or `packages/vesper-compiler/**`. No program in the corpus has a guarded
shift, division or narrowing that a constant does not already prove. The
bootstrap-stage1 workflow runs on the PR branch; its result is in the PR.

## Repros

Setup: a cloud x86-64 container with 4 vCPU and a debug in-tree `vesper`. Each function is
built with `vesper build --lib` (optimize on), and the instructions in each exported function
are counted with `objdump -d`.

| Function | Before | After |
|---|---|---|
| `shl(x, s)`, rotation form (warp-wheel before #988) | 16 | 16 (source unchanged) |
| `shl(x, s)`, with a handler (`handle Fail[ArithOverflow] in { x << s }`) | 22 | 22 (source unchanged) |
| `shl(x, s)`, `if s < 64 { x << s } else { 0 }` | rejected without a `Fail` effect | 10 |
| `shr(x, s)`, `if s < 64 { x >> s } else { 0 }` | rejected | 10 |
| `div(a, d)` for `i64`, with a handler | 31 | 31 |
| `div(a, d)` for `i64`, `if d > 0 { a / d } else { 0 }` | rejected | 25 |
| `low(x)`, `if x < 256 { narrow_u8(x) } else { 0 }` | rejected | 10 |

The guarded `x << s` emits the same 10 instructions as
`wrapping_shl_u64(x, wrapping_u32(s))`. With the plain source, the guarded form now
typechecks, needs no handler, and inlines (`tests/range_facts.rs`,
`guarded_helpers_inline`).

The guarded `i64` division still has 25 instructions, because the native backend
emits its own `MIN / -1` and zero tests before `idiv`, whatever the checker has
proven (see "What is left").

## Ports

Setup: the same machine, with a release `vesper` from this branch passed as
`VESPER_PORT_COMPILER`. The "before" arm is each port's committed source. The
"after" arm uses a local rewrite (not committed; lane 8 commits port changes).
Wall clock on this host moves by up to 30%, so the instruction counts are the
firmer result.

**Unchanged sources compile to the same code.** Each port's concatenated
`.ves`, built with `vesper build --lib` by the debug compilers of main and of this
branch, gives identical instruction streams: warp-wheel 11,535 instructions,
BBR 16,305 and Beacon 6,368, with identical disassembly hashes.

**warp-wheel.** Rewrite: `shl`/`shr` in `util.ves` use `x << s` / `x >> s`
under their existing `s < 64` guard, instead of `wrapping_shl_u64(x,
wrapping_u32(s))`. The object loses the two out-of-line copies of the helpers
(10 and 17 instructions; every call site was already inlined). All exported
functions keep the same instruction counts. Committed ABBA
(`tests/abba.rs --ignored`, both builds in one process), run in the order
before, after, before, after:

| Arm | Median Vesper/Rust | 95% interval of the median |
|---|---|---|
| before, run 1 | 1.833x | [1.728, 1.854] |
| after, run 1 | 1.842x | [1.795, 1.870] |
| before, run 2 | 1.817x | [1.782, 1.840] |
| after, run 2 | 1.841x | [1.776, 1.867] |

The ratio is unchanged within noise (1.77x in the re-score). Callgrind was not run
for warp-wheel.

**BBR.** Rewrite: `pow2` in `arith.ves` uses `one << n` under `n < 64` instead
of `rotate_left(one, n)`. Six internal functions lose 1-4 instructions each, and
`probe_inflight_longterm_upward` and `raise_inflight_longterm_slope` lose 1 and 3.
Callgrind `Ir`, whole process, `ack_path_abba measure 20000`:

| Build | Ir | Vesper/Rust |
|---|---|---|
| Rust | 21,974,284 | |
| port, before | 49,254,499 | 2.2414x |
| port, after | 49,254,468 | 2.2414x |

ABBA `ack_path_abba abba <rust> <port> 16 200000`, in the order before, after, before, after: the
median pair gave 2.67-2.92x, 2.66-2.74x, 2.77-2.78x and 2.84-2.85x. That spread is
noise; the instruction counts above show that the change is negligible.

**Beacon.** Its shifts are not guarded by a plain comparison. The amounts are
`octave - 3` or `63 - k` after subtraction, and `high_bit`'s `one << e` is only
known to be `e != 64`. Its functions also keep `Fail[ArithOverflow]` for their
checked subtractions. So no local rewrite applies, the object is identical, and the ratio
is unchanged. Beacon has no committed ABBA driver.

## What is left

- **Ranges for `+`, `-`, `*`.** These would let `i - 1` under `i > 0` and Beacon's
  `octave - 3` drop their checks. They need either a re-recorded
  `bootstrap/stage1.lock` (`fib.ves` changes) or verified codegen that ignores
  the checker's elision. Both are outside this wave.
- **`for` loop variables.** `for i in 0..64` could bind `0..=63` in `src/loops/check.rs`,
  but that file is outside this lane's paths.
- **Native signed division.** `src/native.rs` still emits its own `MIN / -1` and zero
  tests before `idiv` when the checker has proven both away (25 instructions where
  about 10 would do). This needs a native lowering change.
- **Facts about fields** (`q.f < 64`), match guards, and comparisons of two impure
  operands are not tracked.
