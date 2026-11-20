# Self-hosting SH2 lane 2: 128-bit integers

Claim: #1174. Prompt: `docs/agent-prompts/self-host-sh2/02-wide-integers.md`.
Base: `c9f228b` (origin/main, 2026-11-20).

Census gap C2 (`docs/parallel-results/self-host-census.md`): the checker's
`Type::IntLiteral(i128)` holds literals up to `u64::MAX` and their negations,
and the corpus has one above `i64::MAX` (`18408377700990114895`). Vesper had
no `i128` or `u128`, so the census sample stored a sign and a `u64`
magnitude. The repro now passes on every backend:

```text
fn big() -> i128 { 18408377700990114895 }
fn main() -> i128 { big() }   // 18408377700990114895
```

## What changed

**Semantics (`crates/vesper-core/src/numeric.rs`, new `numeric/wide.rs`).**
`IntegerKind::I128` and `IntegerKind::U128` join `vesper.arith.checked/1`.
`INTEGER_KINDS` stays the ten one-word kinds, and the new
`WIDE_INTEGER_KINDS` holds the two 128-bit ones, so code that iterates one-word
kinds (the byte-order and bit-builtin tests, atomics) is unchanged. The exact
`i128` domain the checker reasons in holds every value but `u128` values above
`i128::MAX`. For `u128`, `max()` is `i128::MAX`, so a checker fact past it is
unknown, never wrong. `max_u128()` gives the true bound, and `widens_to` uses
it. `checked` now uses `checked_add`/`checked_sub`/`checked_div` (the
`i128` operands of the wide kinds can overflow `i128` itself), and the bit,
shift, rotate and wrapping helpers route 128-bit kinds to `numeric::wide`.
`numeric::wide` is the execution half: every operation on bit patterns
(`u128`), plus `Exact`, an exact integer of any kind for conversions.

**Checker (`src/check.rs`, `src/integer_widths/**`, `src/float_math/check.rs`,
`src/atomics.rs`).** The two type names resolve, and literal typing, the
checked operators, `wrapping_*`, shifts and rotations, `widen_k`/`narrow_k`/
`wrapping_k` between every pair of kinds, literal patterns, and the prelude
builtins (`checked_{add,sub,mul}_k`, `leading_zeros_k`, `trailing_zeros_k`,
`count_ones_k`, `wrapping_{shl,shr,neg}_k`, the byte-order functions over
16-tuples) all accept them. Two small checker changes keep native layouts
right:

- A literal in an aggregate whose expected type holds `i128`/`u128` takes
  that field's kind (`constrain_literals` at the expected-type check and at a
  handler clause's result). A literal-typed tuple has a one-word layout, so
  `(1, -1)` returned as `(i64, i128)` would otherwise copy too few bytes.
  This applies only to types that contain a 128-bit kind.
- `binding_bounds` gives a `u128` binding no bounds, rather than the
  domain's `0..=i128::MAX`. That range would have discharged
  `narrow_i128(x: u128)` without a check.

`Atomic[i128]` and the float conversions (`nearest_f64(x: i128)`,
`saturating_i128`, `narrow_u128(f64)`) are refused with a diagnostic.

**Prelude bodies (`src/integer_widths.rs`).** `checked_mul_i128`/`_u128`
have no wider product to read, so they check by dividing the wrapped
product back. The 128-bit bit counts run the SWAR count over both words.
The one-word bodies are textually unchanged.

**Evaluator (`src/runtime.rs`).** `Value::Wide { pattern: u128, signed }`
carries both kinds. Binary and unary operators, conversions in both
directions, literal coercion (a negative literal below `i64::MIN` is built
as an `i128`), literal patterns, equality and display all have a 128-bit
path. `integer_from_pattern` decodes a native result.

**JIT and native code (`src/native.rs`, new `src/integer_widths/native.rs`,
`src/layout.rs`).** A 128-bit value is one Cranelift `I128`: two registers,
or 16 bytes in memory with the low word first (`Layout` size 16). Addition,
subtraction, multiplication, the bit operators, comparisons, variable
shifts and rotations, and `clz`/`ctz`/`popcnt` are single Cranelift
instructions. Constant shifts are lowered on the two words, because Cranelift
emits its variable-amount form for them. The overflow checks split the
operands into words where Cranelift has no 128-bit form (`umulhi`), and the
multiply reuses the check's partial products. Division has a one-instruction
fast path when both operands fit in 64 bits. Otherwise it is a 128-step
restoring loop, because Cranelift has no 128-bit divide on x86-64 or AArch64.
Each `settings::builder()` in `native.rs` turns on `enable_llvm_abi_extensions`,
so `i128` parameters and results pass as register pairs, as rustc and clang
pass them. Signatures without them are unchanged. Spill slots of scalar
locals take the value's size instead of a fixed 8 bytes.

**Refused.** The export ABI refuses `i128`/`u128` in every version, with a
type error naming the two-word workaround. Lean extraction refuses them.
The verified backend and WebAssembly already refuse every kind they do not
model.

**Spec (`docs/manuscript/content.typ`, flagged for the owner).** The
`PrimType` grammar gains `u128` and `i128`. The integer-kinds paragraph
says there are twelve kinds, that the 128-bit ones are two words (low word
first), that literals stop at `u64::MAX` in magnitude, and that the export
ABI, `Atomic[T]` and the float conversions do not take them yet.

## Tests

`tests/wide_integers.rs` checks every case against Rust's own `i128`/`u128`
arithmetic. Every case agrees in the evaluator (optimized and not) and the
JIT (optimized and not), and compiles to x86-64 and AArch64 objects:

- `binary_operators_agree_at_boundaries`: `+ - * / %`, `& | ^`,
  `wrapping_{add,sub,mul}`, `<`, `==`, `>=` and `~` over all pairs of 10
  (`i128`) and 7 (`u128`) boundary values: MIN, MIN+1, ±2^64, ±1, 0,
  2^64−1, 2^127, MAX−1 and MAX.
- `negation_agrees`, `shifts_and_rotations_agree` (amounts 0, 1, 63, 64,
  65, 127, 128, plus a 128-bit amount shifting a `u64`),
  `constant_shifts_agree` (the two-word path).
- `conversions_agree_with_every_kind`: `widen_`, `narrow_` and `wrapping_`
  from every one-word kind to both wide kinds and back, at the boundaries.
- `checked_and_bit_builtins_agree`: `checked_{add,sub,mul}_k`,
  `leading_zeros_k`, `trailing_zeros_k`, `count_ones_k`, `wrapping_neg_k`,
  and `wrapping_shl_k`/`wrapping_shr_k` (amounts up to 200).
- `literals_patterns_and_aggregates`, `sequences_hold_wide_elements`:
  literal patterns, struct fields, tuples and `ChunkSeq[u128]`.
- `executable_agrees_with_the_evaluator`: a `vesper build` executable's
  exit status matches the evaluator on a 300-step digest that mixes
  every operator, division by values above 64 bits and `count_ones_u128`
  (Unix hosts with `cc`).
- `checker_rules`, `export_abi_refuses_wide_integers`.

Unit tests in `numeric::wide` pin the corners: MIN / −1, MIN % −1, carries
and conversions. The existing `integer_widths`, `bit_builtins`,
`checked_arith_builtins`, `checked_arithmetic` and `literal_typing` suites
pass unchanged.

## Numbers

Instructions per iteration, measured with callgrind over 1,000,000
iterations of an optimized `vesper build` executable against `rustc -C
opt-level=3 -C overflow-checks=on`, on Linux x86-64. Process startup is
subtracted, and Rust's loop index goes through `black_box` so LLVM keeps
the loop. Each kernel builds its operands from the loop index with shifts
and `|`, then:

| Kernel | First lowering | This PR | Rust | PR / Rust |
|---|--:|--:|--:|--:|
| `u64` loop baseline (`wrapping_add`) | 6.0 | 6.0 | 5.0 | 1.2x |
| `u128` checked add | 31.0 | 20.0 | 11.0 | 1.8x |
| `u128` checked multiply | 76.0 | 53.0 | 23.0 | 2.3x |
| `i128` compare and count | 42.0 | 20.0 | 13.0 | 1.5x |

"First lowering" is the plain Cranelift `I128` form. The constant-shift path
took add 31 → 20 and compare 42 → 20. Reusing the check's partial products
took multiply 58 → 53. The remaining multiply gap is the generic overflow
check (gap 4).

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changed. The
`bootstrap-stage1` workflow run on the branch is linked from the PR.

## Compiler gaps

1. **Literals stop at `u64::MAX` in magnitude.** The parser and its
   `packages/vesper-front` mirror (which keeps a literal as a `u64`) cap
   them, and `Literal::Int(i128)` could not hold `u128::MAX` anyway.
   ```text
   fn f() -> u128 { 18446744073709551616 }
   // E0002: integer literal exceeds u64 range
   ```
2. **Checker facts stop at `i128::MAX` for `u128`.** A constant product past
   it is unknown, so it keeps its overflow check and needs `Fail`.
   ```text
   fn main() -> u128 { let a: u128 = 18446744073709551615; a * a }
   // E0401: unhandled effect 'Fail'
   ```
3. **Division above 64 bits is a bit loop.** Cranelift has no `I128`
   divide on x86-64 or AArch64. A two-word (Knuth D) division, or a
   `__udivti3` libcall where a runtime links, would be about 10x faster
   when an operand exceeds 64 bits.
4. **The checked multiply is generic.** Nothing tells it that a widened
   `u64` has a zero high word (LLVM knows), so it always runs the full
   word-split check.
   ```text
   fn f(a: u128, b: u64) -> u128 eff [Fail[ArithOverflow]] { a * widen_u128(b) }
   ```
5. **No export ABI, Lean model, atomics or float conversions.** An export
   ABI version would need a two-word cell (Rust's `u128` is 16-byte
   aligned on x86-64 since 1.77).
   ```text
   pub fn f(a: i128) -> u64 { wrapping_u64(a) }
   // export type error: is a 128-bit integer, which no export ABI version carries yet
   ```

## What is left

- Rewrite the census sample's `literal(neg, magnitude)` as one `i128` field
  when lane 7 ports literal typing. This lane must merge first, as the plan
  says.
- The gaps above, starting with literals past `u64::MAX` (an AST and front v2
  change) and a faster divide.
