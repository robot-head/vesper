# Facts across calls: callee result ranges, `u128` facts, widened multiplies

Self-hosting wave SH3, lane 6. Claim #1245. Base: `main` at `c8ac321`.

SH2 found three places where the checker or native lowering knew less than
the program said:

- SH2 lane 9 gap 1: `128 / bits(x)` where `bits` returns only 8 or 64 was
  E0401 (unhandled `Fail`), because a callee's result was never a fact.
- SH2 lane 2 gap 2: `u128` constant facts stopped at `i128::MAX`, so
  `a * a` of `a = 2^64 - 1` kept its overflow check.
- SH2 lane 2 gap 4: a checked `u128` multiply ran the full word-split check
  even when an operand was a widened `u64`.

All three repros now pass.

## What changed

**Result ranges across calls** (new `src/check/call_facts.rs`, the
`integer_bounds` call arm in `src/check.rs`). Each non-generic function with
an integer result other than `u128` gets a summary on first use: the hull of
the bounds of every value its body can produce, namely its tail, the arms of
`if`/`else` and `match`, and every `return`. A value is bounded when it is:

- an integer literal or a module `const`;
- a parameter or immutable `let`, bounded by its kind, or by a constant
  comparison (`<`, `<=`, `>`, `>=`, `==`, `!=`) that guards the branch, on a
  name nothing assigns or lends `inout`;
- `x & m`, `x % m`, or `x / m` with nonnegative bounds;
- `widen_k(x)`, or `narrow_k(x)` when `x`'s bounds fit `k` (a narrowing that
  may fail can be resumed with any value of `k`, so it gets `k`'s range);
- a call with a summary of its own.

Anything else, such as a loop, a handler, `+`/`-`/`*` or an unknown call,
gives no summary. A function reached again while its own summary is being
computed (recursion, mutual recursion) has none. The summary depends only on
the callee's body and the summaries of what it calls, never on the call's
arguments, and is cached per checker run. A call site uses it only where the
checker resolves the call to that user function: a plain name that is not a
local binding, a constructor or a wrapping intrinsic.

Call sites use the range wherever `integer_bounds` feeds a decision:
non-zero divisors, shift amounts, `narrow_k` operands, signed quotients, and
the range of a `let` bound to the call. `+`, `-` and `*` still use only
exact facts outside `for` loops. That rule keeps the Lean reference checker
of `tests/lockstep.rs` and the Stage 1 corpus unchanged (see the range facts
lane, #1027).

**`u128` facts over the whole range** (`src/integer_widths/check.rs`,
`fact_checked`, `fact_neg`, `fact_fits`, `u128_expr`). A fact is still an
`i128`. A `u128` fact is now its value's bit pattern, and arithmetic on
`u128` facts uses `numeric::wide::checked`, so values above `i128::MAX`
compose. A `u128` fact above `i128::MAX` reads as negative, so it is never
used as a bound: `integer_bounds` drops it. That keeps `narrow_i128(p)`,
`narrow_i64(p)` and shifts by such a value checked.

**Widened multiply** (`src/integer_widths/native.rs`, `low_word_of`,
`widened_product`). This is native code and the JIT, which share the
lowering; the evaluator is unchanged. An `I128` operand defined by a
`uextend` of an `I64`, or by an `iconcat` with a zero high word, has a known
zero high word.

- With one such operand, the check is one cross product. It drops the
  second `umulhi`/`imul` pair and the both-high-words test.
- With two such operands, the product always fits in 128 bits, so it is
  `imul` plus `umulhi` with no check.

**Manuscript** (flagged for the owner): the integer-kinds paragraph of
`docs/manuscript/content.typ` gets two sentences. One states the
result-range rule, the other says that constant facts cover the whole range
of every kind, `u128` included.

## Tests

`tests/call_facts.rs` has 10 tests:

- `callee_result_range_is_a_divisor_fact` covers the lane 9 repro, plus a
  `let`, a remainder and a nested call.
- `result_ranges_follow_every_return_shape` covers `match`, constant guards
  (both operand orders), an early `return`, a mask, a remainder and a
  quotient of bounded values, a widening and a `const`.
- `result_ranges_bound_shift_amounts`.
- `no_fact_where_a_divisor_can_be_zero` holds 11 negative cases, each still
  E0401/E0400 without `Fail[RuntimeError]` and accepted with it. The cases
  are:
  - a zero return;
  - a bare parameter;
  - recursion and mutual recursion;
  - a `return` of an unguarded parameter;
  - a `return` of a name a `let` rebinds;
  - a guard on a name the branch assigns;
  - a guard on a name lent `inout`;
  - a narrowing that may fail;
  - a loop;
  - a `-`.
- `local_bindings_and_constructors_shadow_functions`: a closure named like
  the function.
- `u128_facts_cover_the_whole_range` covers the lane 2 repro, plus sums,
  differences, quotients and `wrapping_add` above `i128::MAX`.
- `u128_facts_keep_every_real_failure` covers overflow past `u128::MAX`,
  below zero, zero divisors, `narrow_i128`/`narrow_u64`/`narrow_i64` of a
  value above `i128::MAX`, a shift by one, `-p`, and `-0`.
- `widened_multiply_agrees_at_the_boundaries` runs 98 products, `a *
  widen(b)`, `widen(b) * a` and `widen(a) * widen(b)`, over the word edges.
  It checks against Rust's `checked_mul`.
- `widened_multiply_emits_fewer_partial_products` checks the `umulhi`
  counts in `f`: 3 generic, 2 with one widened operand, 1 with two.
- `executable_agrees_with_the_evaluator`: a `vesper build` executable, the
  JIT and the evaluator agree on a digest that uses all three changes.

Every accepted program agrees in the evaluator and the JIT, optimized and
not, and compiles to x86-64 and AArch64 objects.

These suites pass unchanged: `lockstep` (the Lean reference verdicts),
`range_facts`, `native_bounds_facts`, `wide_integers`, `integer_widths`,
`checked_arithmetic`, `literal_typing` and `bit_builtins`.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changed. The Stage
1 corpus has no division by a call result. Its only calls in arithmetic are
recursive (`pow_mod`, `gcd`), which get no summary. The `bootstrap-stage1`
workflow run on the branch (`bootstrap/run.sh stage1 --reproduce
--outcome`) is linked from the PR.

## Numbers

**Checker cost.** No verdict changed on the 528 `.ves` files under
`tests/fixtures` and `examples`: diagnostic codes and counts are identical
between the release builds of `main` and this branch. Callgrind of `vesper
check` (release builds):

| Input | main Ir | this PR Ir | change |
|---|--:|--:|--:|
| `packages/vesper-front` | 16,884.7M | 16,806.9M | -0.46% |
| `packages/vesper-check-data` | 1,713.0M | 1,713.4M | +0.02% |

Wall time of checking all 528 files plus six packages, ABBA over 8 runs:
16.44 s on `main` and 16.62 s here (+1.1%, within the 15.8-17.4 s spread
between runs). The summaries cost nothing measurable.

**Repros.** Instructions per iteration, from callgrind over 1,000,000
iterations of an optimized `vesper build` executable, with a 0-iteration
build subtracted. Rust is `rustc -C opt-level=3 -C overflow-checks=on`, with
the loop bound through `black_box`.

| Kernel | main | this PR | Rust |
|---|--:|--:|--:|
| `h = wrapping_add(wrapping_mul(h, 31), lanes(narrow_u8(i & 1)))` (`128 / bits(x)`) | 33.5 | 29.5 | n/a |
| `h ^= a * widen_u128(i \| 1)` with a full `u128` `a` | 50.0 | 42.0 | 21.0 |
| `h ^= widen_u128(i) * widen_u128(i \| 1)` | 45.0 | 14.0 | 3.1 (vectorized) |

On `main`, `lanes` needs `Fail[RuntimeError]` in its row and a zero test.
Here the row is optional and the test is gone.

## Compiler gaps

1. **Guards on expressions, not just names, do not refine a summary.** A
   comparison with a non-constant, or of a field, gives the branch no range.
   ```vesper
   fn f(x: u64, y: u64) -> u64 { if x > y { x } else { 1 } }   // no summary
   fn lanes(x: u64, y: u64) -> u64 { 128 / f(x, y) }           // E0401
   ```
2. **`+`, `-` and `*` of bounded values give no range,** in summaries and
   outside `for` loops alike. The Lean reference checker has the exact-fact
   rule, and the Stage 1 lock pins `fib.ves`'s `n - 1`.
   ```vesper
   fn f(x: u8) -> u64 { widen_u64(x & 3) + 1 }
   fn g(x: u8) -> u64 { 128 / f(x) }                           // E0401
   ```
3. **Methods and trait calls have no summary**: only plain-name calls of
   non-generic functions do.
   ```vesper
   enum Kind { A, B }
   impl Kind { fn bits(self: Kind) -> u64 { match self { Kind::A => 8, _ => 64 } } }
   fn lanes(k: Kind) -> u64 { 128 / k.bits() }                 // E0401
   ```
4. **`u128` values have no ranges,** only exact facts. A comparison of a
   `u128` local does not refine it, and a `u128` result gets no summary.
   ```vesper
   fn f(a: u128) -> u128 eff [Fail[ArithOverflow]] { if a < 5 { a << a } else { 0 } }
   ```
   The shift keeps its check. (`if a != 0 { 100 / a }` already checks
   without `Fail`, through the non-zero fact.)
5. **The checker does not know that `widen(a) * widen(b)` cannot overflow.**
   Native code drops the check, but the source still needs
   `Fail[ArithOverflow]` (gap 4 again).
6. **Signed widened multiplies keep the full check.** `widen_i128(x)` of an
   `i64` is a `sextend`, and the signed path works on magnitudes.

## What is left

- The gaps above, starting with method summaries (3) and `u128` ranges (4).
- Ports that branch per value to avoid a `Fail` can now drop the branch.
  `lanes_per_chunk` in the SH2 lane 9 regions/seqs port (#1226) is one, as
  a follow-up of that lane.
