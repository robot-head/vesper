# P3b gaps lane 7: continue, literal typing, `?` on Option, wrapping_neg

Claim #1092. Base `9305b24` (origin/main after the wave plan #1098). Prompt:
[07-loops-and-literals.md](../agent-prompts/p3b-gaps/07-loops-and-literals.md).
A core `while` is out of scope (owner decision 2026-11-18, §7.6 `sugar`).

## What changed

| Gap (report) | Before | After |
|---|---|---|
| No `continue` (pact 2, ebpf 8, prism-h3 11, loom audit) | `E0201 unknown name 'continue'` | `continue` in `loop` and `for` |
| Literal range bounds are `i64` (kinetic 2, prism-h3 3, chorus 4) | `for a in 0..3 { s.try_get(a) }` with `s: Slice[u64]` is E0200 | checks; the range is `u64` |
| `let x = if c { 1 } else { 2 }; x` in a `u64` function (pact 4, chorus 4, bench 4) | E0200 | checks; `x: u64` |
| Tuple of literals used as `u8` (prism-h3 9) | E0200 | checks; `(lo, hi): (u8, u8)` |
| `?` only on `Result` (aegis 5) | `E0200 '?' requires Result[T, E]` | `num(in s, 0)?` in an `Option` function returns `None` early |
| No `wrapping_neg` (ebpf 8) | `wrapping_sub(0, x)` | `wrapping_neg_k` for all ten kinds, one `ineg` natively |

### `continue`

- Parser (`crates/vesper-syntax/src/parser.rs`): `continue` is a contextual word, like `break`. It parses as `ExprKind::Continue` only when it is followed by `;`, `}`, `)`, `]`, `,` or EOF, and no binding or function named `continue` is in scope. `fn continue(..)` and `let continue = ..` keep working.
- AST: a new `ExprKind::Continue` (`crates/vesper-core`). About 30 exhaustive traversals were extended to treat it as a leaf, like `break` without a value.
- Checker (`src/loops/check.rs`): `continue` targets the innermost loop frame. It has the same placement errors as `break` (E0213 outside a loop, from a closure, a handled body or a clause) and the same inout-loan rule (E0402). Its environment goes to the back edge: loop-local linear values must be consumed (E0302), and nothing live at loop entry may have moved on its path (E0312).
- Place IR (`src/loops/ir.rs`): in a `loop`, `continue` jumps to the header. In a `for`, every `continue` and the body's end meet in a step block, which runs the closed-range bound test and then the latch. This keeps `for i in lo..=255u8 { if .. { continue } }` from stepping past 255. A `for` without `continue` lowers exactly as before, so loops that do not use it keep the same blocks.
- `src/ir.rs` gets one line: `| ExprKind::Continue` in the arm of `Lower::expr` that already sends `loop`/`for`/`break`/`return` to `loops/ir.rs`. The owner approved this exception to the Stage 1 guard on 2026-11-19. The lowering itself is in `loops/ir.rs`, and no locked program uses `continue`.
- `vesper fmt` formats it unchanged (it is token-based). Chroma Tier 0 classes it as a control keyword where it parses as one (`is_loop_keyword`). The semantic highlighter walks it like `break`.
- Backends without loops (verified codegen, Lean, Wasm, eBPF, batch JIT, FIP functions, handler lowering) already refuse every loop-family node through the same typed paths.

### Integer literals typed from their use

New module `src/integer_widths/literal_typing.rs`, with small hooks in `src/check.rs` (the `let`, name and assignment cases), `src/loops/check.rs` (the `for` range) and `src/integer_widths/check.rs` (conversions).

- An *open binding* is an unannotated `let` whose value has an unsuffixed literal type (or a tuple of them, which covers `if` and `match` arms of literals), or a `for` pattern over a range of literals.
- The checker first checks each function exactly as on `main`. **Only a function that fails and has an open binding is checked again.** Up to four probe runs, in which a use of an open binding reads as a literal, record the kind each use settles to. Each open binding whose uses settle to exactly one kind other than `i64` takes that kind as an annotation, and the function is checked once more. The new result is kept only if it has fewer diagnostics.
- A conversion's operand (`widen_u64(x)`) does not constrain the binding. A value assigned to an open `let mut` is a use, typed from its own operands.
- This keeps the meaning of every program that checks today: such a program never takes the retry path. I checked it differentially (below).
- The cost falls only on failing functions with open bindings: one copy of the checker state and up to six extra checks of that one function.

### `?` on `Option`

`src/loops/try_option.rs`. The checker types `e?` with `e: Option[T]` as `T` when the enclosing function or closure returns `Option` (E0200 otherwise). It records the span. `check_core` then rewrites each recorded `?` to

```text
match e { Option::Some($tryN) => $tryN, Option::None => return Option::None }
```

and checks the rewritten module. Every backend therefore runs an ordinary `match` and `return`, with no new IR or runtime support.

- The `return` takes the span of the `?` token, so a rejected exit is reported at the `?`.
- The two synthetic names get spans past every span of the first check's world, so they never collide with real expressions or the prelude.
- Modules without `?` on `Option` are checked once, as before.

### `wrapping_neg_k`

`BitOp::WrappingNeg` in `src/integer_widths.rs`, next to the other bit builtins. Its portable source is `wrapping_sub(0, x)`. The bit-builtin call lowering (from #988) turns it into `Op::Unary`, which `src/native.rs` lowers to one `ineg` in the kind's width. The name is reserved like the other bit builtins.

## Manuscript (flagged)

These are spec changes in `docs/manuscript/content.typ` (§2.2 grammar and the paragraphs after it):

- `ContinueExpr ::= 'continue'`, added to `Expr`.
- The loops paragraph adds `continue` and its rules, and `?` on `Option`.
- The integer-kinds paragraph adds `wrapping_neg_k` and the literal-typing rule, including the rule that it only types programs that would otherwise be rejected.
- The self-hosted Stage 1 checker (`packages/vesper-compiler`) is not changed. It stays stricter, as it already is for receiver loans. Sources that use loop syntax are already left out of its differential corpora.

## Tests

| File | What |
|---|---|
| `tests/continue.rs` (8) | `loop`/`for`/nested/match-arm `continue`; a closed `u8` range continuing at 255; loop-local `ChunkSeq` drops on the continue path (no teardown frees); loop-local and loop-carried `Box` values with `continue` after #1076 (allocations equal frees); contextual names; E0213/E0312/E0302; fmt round trip |
| `tests/literal_typing.rs` (4) | range indexing a `Slice[u64]`, range bound from an open `let`, closed `u8` literal range, `if` branches, a tuple used as `u8`, a `let mut` accumulator and a chain of literal lets, the `i64` default, conflicting uses, E0209 for a hinted kind that does not fit, `wrapping_neg_k` for all ten kinds over edge values against `IntegerKind::wrap(-v)` |
| `tests/try_option.rs` (3) | `Some` continues and `None` returns, `?` inside a `for` loop, E0200 outside an `Option` function and on a non-`Option`/`Result` operand |

Every program runs on the evaluator (optimize on and off), the JIT (optimize on and off), x86-64 and AArch64 objects, and an executable built with `vesper build`. Locally the following also pass: `tests/loops.rs`, `tests/bit_builtins.rs`, `tests/integer_widths.rs`, `tests/fmt_cli.rs`, `tests/highlight_cli.rs`, and the crate tests of `vesper-syntax`, `vesper-highlight`, `vesper-core` and `vesper-fmt`. `cargo +1.100.0 clippy --all-targets` is clean for the touched crates.

### Corpus differential (programs that check on `main` keep their meaning)

I compared `vesper check` (exit status) and `vesper emit --emit ir` (full output) between a `main` build (`9305b24`) and this branch:

- **818 `.ves` files in the repository:** all 295 that check on `main` give byte-identical IR. 486 fail identically. 37 fail on both, but with fewer diagnostics on this branch: they are package members checked standalone, whose literal errors the new rule now types.
- **56 package directories (`Loom.toml`):** I ran this before the conversion and assignment hooks were added: all 37 that check gave identical IR, and 19 failed identically. The re-run after those hooks gave identical IR for every package it reached (22 of 22 checking packages compared; the rest was still running when this report was written).

## Port workarounds removed (local rewrites, not committed)

- **Prism H3:** `for i in 0..static_count()` (twice) and `for _p in 0..static_count()` in `table.ves` become `0..99`. On `main` the package fails with E0200; on this branch it checks. A probe entry hashes the built static table (`hidx`, `next`) and looks up all 99 entries with `static_exact`. JIT result: original `17290829430169458178`, rewrite `17290829430169458178`. Machine code differs only in register allocation inside `static_new`: a constant bound replaces an inlined call.
- **Kinetic** (#1078 branch): all ten `0..u(N)` become `0..N`. The rewritten package gives the same diagnostics as the original: the four errors that exist in both come from the crmath `.vsys` that `build.rs` supplies outside the package directory. No new literal error appears. I did not run Kinetic's ABBA benchmark.

## Performance

These are typing and control-flow forms, so the code generated for existing programs does not change: the IR is identical across the corpus. I did not run callgrind or ABBA on the ports. The rewrites above change only how ports spell loop bounds and early exits. A `for` without `continue` lowers exactly as before. `wrapping_neg_k` is one instruction, against a call to `wrapping_sub` with a constant before.

## Stage 1 guard

- `src/verified_codegen/**`, `lean/**`, `bootstrap/**`, `packages/vesper-compiler/**` and `ir_opt::optimize` are not edited.
- `src/ir.rs` gets the one approved dispatch line, and no Place IR type changes.
- The Stage 1 lock corpus uses none of these forms (the verified backend refuses loops).
- `bootstrap-stage1` on this branch ([run 44441389103](https://github.com/robot-head/vesper/actions/runs/44441389103)): success. Its two clean builds give byte-identical locks.
- Every `programs.verified.artifact` digest in its fresh lock equals the one from the same workflow on lane 1's branch (run 44439934250, P3b gaps lane 1, also based on current main). So this PR changes no verified artifact.
- Both runs differ from the committed `bootstrap/stage1.lock` (recorded 2026-11-16) in the same way: 10 of the 11 verified digests and the Cranelift digests. That drift came before this lane; the ratchet passes because every program still builds.

## Left

- `continue` and `break` take no labels, because `break` has none.
- Literal typing is per function and flows only through bindings: a literal passed to a generic parameter still defaults to `i64` unless the call site constrains it. The result of `a + b` over two open bindings is `i64` (unchanged).
- The self-hosted Stage 1 checker does not accept any of these forms.
