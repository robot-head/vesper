# Self-hosting SH4 lane 3: 128-bit aggregates, division and export cells

Claim: #1277 (fixes #1270). Prompt: `docs/agent-prompts/self-host-sh4/03-wide-aggregates.md`.
Base: `e73cfd6` (origin/main, 2026-11-21).

SH2 lane 2 (#1174) added `i128`/`u128` and left three gaps that the SH4
native lowering ports would otherwise copy: native code read 128-bit array
and tuple elements wrong (#1270, gap 8 of SH3 lane 5), division above 64 bits
was a bit loop (gap 3), and no export ABI version carried a 128-bit value
(gap 5). All three are fixed here, in the Rust lowering, so lanes 8, 11 and
12 start from a lowering that is right.

## What changed

**128-bit aggregate elements (#1270, `src/integer_widths/native.rs`).** The
checker types an integer literal from the binding it flows into, so in
`let t: [i128; 3] = [1, 2, 5]` each element is an `i128` constant, but the
tuple expression keeps its literal type, which defaults to `(i64, i64, i64)`.
So does the local it initializes: `$slot_t` was recorded as `(i64, i64, i64)`
while its reads were `[i128; 3]`. The evaluator never reads those types.
Native code laid the tuple out by them: it stored three one-word elements
(an `ireduce` of each `I128`) into a 24-byte slot that the binding's
two-word layout then read back. That is why `vesper jit` printed 0 where
`vesper run` printed 7. The bug was not in the stride: `native_arrays`
already indexed with 16-byte elements.

`refine_wide_tuples` is a native pre-pass, run right after
`lower_bit_builtin_calls` in the three native pipelines (`native.rs`,
`native_export.rs`, `native_session.rs`). It widens, to a fixed point:

- the type of each `Op::Tuple` from its operands' types,
- the block parameters that a `Jump` passes such a tuple to,
- the locals a `Bind` pattern binds from it,
- a local's type from a whole read of it (`Read` with no path), which
  carries the binding's checked type.

It changes only an element whose operand is 128-bit and whose recorded type
is a narrower integer or a literal, and it returns at once for a function
with no 128-bit value. So every other program, the Stage 1 corpus included,
is lowered exactly as before. `src/ir.rs` is untouched: the pass runs on the
native copy of the program.

`const` tables of 128-bit elements are the other half of #1270. Main's
`src/arrays.rs` `scalar_word` writes one word per element, dropping the high
word. #1264 (SH3 lane 5) stops encoding such tables, so they are built
through the tuple path above. With #1264's `arrays.rs` hunk applied, the
`const TABLE: [i128; 3]` form of #1270 agrees on native code too. That hunk
is #1264's to merge (see "What is left").

**128-bit division (gap 3).** `wide_udivrem` is now the word algorithm on
64-bit `udiv`, as Rust's `u128` division is:

- Both operands fit in one word: one `udiv`. The remainder is
  `n - q * d`, so there is no second divide.
- A one-word divisor: `nh / d`, then the two-word remainder `(nh % d, nl)`
  by `d`. Below 2^32 (powers of ten, small radixes), that takes two 32-bit
  digit steps with nothing to correct. Above it, `wide_div_words` is
  Knuth's algorithm D on 32-bit digits (Hacker's Delight `divlu`, 9-3). Its
  two quotient-digit corrections are selects, so the code is straight-line.
- A two-word divisor: the quotient fits in one word. It is estimated from
  the normalized divisor's top 64 bits and the halved dividend (Hacker's
  Delight 9-5). The estimate is exact or one too large, and one compare
  corrects it.

Signed division keeps the magnitude form, which is unchanged.

**Export cells (gap 5).** An `i128` or `u128` crosses the export boundary as
a two-word cell, low word first, in every ABI version:

- Inside an aggregate, an `Option`/`Result`, a handle or a sequence, it is
  its two words, as in the Vesper layout. `vesper_port_abi::Abi` now has
  `u128`/`i128` impls (`WORDS = 2`, `STRIDE = 16`), so every binding
  marshals it like any other copy value.
- In a v3 mirror, a 128-bit field is `vesper_port_abi::Words<i128, 2>`
  (`binding_v2.rs` `cell`). The words are eight-byte aligned, and Rust's
  `u128` has been 16-byte aligned since 1.77, so the field is never a plain
  `u128`.
- As a parameter or a result, it goes through a pointer to its two words
  (`ExportParam::pointer`, `result_pointer`), never in registers. The C
  ABIs disagree here: Windows x64 passes 128-bit integers by reference, and
  AAPCS64 uses an even register pair. The function's own signature keeps
  its `I128` values, so `define_wide_entry` gives the export symbol to a
  small adapter. The adapter loads each such parameter, calls the function
  (now local), and stores a 128-bit result through the leading result
  pointer. Other parameters, the failure record and the return word pass
  through unchanged. Internal calls still call the function directly.
- `inout` 128-bit parameters were already pointers.

**Manuscript (flagged).** The "Integer kinds" paragraph said the export ABI
does not carry `i128`/`u128`. It now says what the cell is.

## Tests

- `tests/wide_aggregates.rs` runs the #1270 repro, dynamic index reads and
  writes (`t[i] = v`, `t[i] += v`, `n[i] -= v`), mixed and nested tuples,
  tuples through `if` joins, calls and `for` loops, and struct fields and
  enum payloads holding 128-bit tuples. Each runs on the evaluator and the
  JIT, optimized and not, and compiles to x86-64 and AArch64 objects. All
  five tests fail on main and pass here.
- `tests/wide_aggregates_division.rs` checks `u128` and `i128` `/` and `%`
  against Rust on 1,807 unsigned and about 7,200 signed pairs. The pairs include
  dividends and divisors of every bit length, and divisors whose 32-bit
  digits force both quotient-digit corrections (all ones, a lone top bit,
  a top bit and a one, at every word position) with dividends just below
  their multiples. The rest are splitmix pairs. Each runs on the JIT,
  optimized and not, and the evaluator. With one correction step removed,
  the test fails within two seconds.
- `tests/wide_aggregates_export.rs` checks the interface: pointer flags,
  word counts and the binding's `u128` signatures. It checks the v3
  mirror's `Words<i128, 2>`/`Words<u128, 2>` fields. Objects build for all
  four export targets, optimized and not. On Unix, a C caller drives nine
  exports: by-value parameters and results, a mixed scalar and 128-bit
  signature, a tuple result, `inout`, `Option[i128]`, a copy struct with
  128-bit fields (parameter and result), and a `Fail[RuntimeError]` row.
- `vesper-port-abi`: `wide_integers_are_two_words_low_first`.
- `tests/wide_integers.rs`: `export_abi_refuses_wide_integers` is now
  `export_abi_passes_wide_integers_by_pointer`. The other 12 tests pass
  unchanged, the boundary-value division tests included.

## Numbers

Instructions per iteration, measured with callgrind over 1,000,000
iterations of an optimized `vesper build` executable against `rustc -C
opt-level=3 -C overflow-checks=on`, on Linux x86-64. Process startup is
subtracted. Each kernel builds `x` and `d` from the loop index (shifted
words), divides, and adds the low word to an accumulator. "Loop" is the same
kernel with `^` in place of the divide.

| Kernel | Before | After | Rust | After / Rust |
|---|--:|--:|--:|--:|
| loop, operands above 2^64 (`^`) | 30 | 30 | 8 | 3.8x |
| loop, one-word operands (`^`) | 41 | 41 | 8 | 5.1x |
| `u128 /`, both one word | 74 | 76 | 11 | 6.9x |
| `u128 /`, 2^64+ by a divisor below 2^32 | 8,639 | 83 | 29 | 2.9x |
| `u128 %`, 2^64+ by a divisor below 2^32 | 8,638 | 83 | 32 | 2.6x |
| `u128 /`, 2^64+ by a divisor of 2^40-2^60 | (bit loop) | 178 | 26 | 6.8x |
| `u128 /`, two-word divisor | 8,626 | 202 | 53 | 3.8x |
| `u128 %`, two-word divisor | 8,626 | 202 | 57 | 3.5x |

Net of the loop, a divide by a divisor above 64 bits went from about 8,600 Ir
to about 160 (Rust: about 45). A two-word dividend by a small divisor went
from about 8,600 to about 53 (Rust: about 21). The one-word case did not
change: two extra compares route it, and the dropped `urem` pays for one of
them. The rest of the gap is in "Compiler gaps" below. Real time gains more
than Ir does: the old loop ran 128 dependent iterations, and the one-word
fast path now runs one hardware divide instead of two.

## Stage 1 guard

No file under `src/verified_codegen/**`, `crates/vesper-codegen-verified`,
`src/ir.rs`, `ir_opt::optimize`, `lean/**`, `bootstrap/**` or
`packages/vesper-compiler/**` changed. The tuple pass runs only in the native
pipelines and changes nothing in a function with no 128-bit value, and the
Stage 1 corpus has none. The `bootstrap-stage1` workflow run on the branch is
linked from the PR.

## Compiler gaps

1. **Tuple and local types keep the literal default.** The checker records
   `(i64, ..)` for a tuple literal whose elements it typed `i128`, and
   `ir::lower_world` gives the local that type. The native pass repairs the
   128-bit case only. A narrower element (`(u8, u8) = (1, 2)` typed
   `(i64, i64)`) is still mistyped, but harmlessly, since every one-word
   scalar takes one word. The real fix is in the checker's literal typing,
   which is Stage 1 territory.
   ```text
   fn main() -> u64 { let t: (i128, u64) = (-3, 4); let (a, b) = t; .. }
   // IR: Tuple [0, 1] : (i64, i64); local $slot_t : (i64, i64)
   ```
2. **`const` tables of 128-bit elements are one word per element on
   main.** `src/arrays.rs` `scalar_word` drops the high word. #1264 stops
   encoding them, and two-word encoding would keep them in read-only data.
   ```text
   const T: [i128; 3] = [1, -2, 5];
   fn main() -> u64 { let i: u64 = 1; if T[i] == -2 { 1 } else { 0 } } // jit: 0
   ```
3. **No 128/64 hardware divide.** x86-64 `div` divides a two-word dividend by
   one word in one instruction, and Rust uses it. Cranelift does not expose
   it, so the one-word-divisor path above 2^32 costs `divlu`'s roughly 100
   instructions. An `x86_udivmodx`-style instruction, or a `__udivti3`
   libcall where a runtime links, would close most of the 6.8x.
4. **The division paths are inline.** Every `/` and `%` emits all three
   paths (about 120 instructions), which raises register pressure in the
   enclosing loop. A local helper function per module would keep only the
   one-word fast path inline. That needs a runtime-function hook in all
   three native pipelines.
5. **No constant-divisor strength reduction.** Rust turns `x / 10` into a
   multiply-high. Vesper divides.
6. **Mirrors copy 128-bit cells.** `Words<T, 2>` converts on access, and an
   `in` 128-bit parameter goes through a scratch copy. The words are
   eight-byte aligned, so a lent Rust `&u128` would need the binding to
   prove 16-byte alignment.

## What is left

- After #1264 merges: turn its `wide_tables_keep_both_words` test on for
  native code (it runs on the evaluator only). With #1264's `arrays.rs`
  hunk, the native result is already right.
- Gaps 3-5, for SH4 lane 11 (numeric) or a later perf lane.
