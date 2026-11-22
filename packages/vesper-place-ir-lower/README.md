# @vesper/place-ir-lower

Place IR lowering (`ir::lower_world` and `thread_tokens` of `src/ir.rs`,
with the loop lowering of `src/loops/ir.rs`) in Vesper: self-hosting wave
SH3, lane 13 (#1252). It reads a checked world through
[`@vesper/check-data`](../vesper-check-data/README.md) and writes the
program as [`@vesper/place-ir`](../vesper-place-ir) tables.

Rust stays the default lowering, and `src/ir.rs` is never edited (the
Stage 1 guard). Nothing installs this port yet; the hybrid swap through
`src/ir_swap.rs`'s `lower` hook is a later part of the lane.

| File | Rust | Vesper |
|---|---|---|
| `src/ast.ves` | the `World` function table, effect operation signatures and `Expr` / `Pattern` trees | `Ast`: rows of expressions and patterns in post-order, the function, parameter, generic and effect tables (`read_ast`) |
| `src/lower.ves` | `Lower::{expr, emit, place, argument, rename_pattern, prepare_nested, capture_params}`, `check::free_names`, `pattern_params`, `loop_lowering::control`, `thread_tokens`, `state_action` | `L` (the lowering state: working blocks and instructions over one scratch operand pool, the `names` scopes as an undo trail), `expr`, `flush` |
| `src/lib.ves` | | the entries `ir_lower_words` and `ir_lower_read` |

## The input

`tests/common/ir_lower_words.rs` writes check-data's names, types and
world, then the lowering section (its module comment has the format).
Expression types, `partial_ops` and `overflow_ops` come from check-data's
world, as Rust's `World::types` and span sets.

Two things are still read off Rust, and both are written per
call into the input:

- **Payload routing.** A call or index that `seq::resolve`,
  `tiled::resolve_ir_call`, `VectorOp::resolve` or the `Box` builtins turn
  into an `Op::Seq`, `Op::Tiled`, `Op::Vector` or `Op::Fip` is written as
  a foreign expression. The port skips a function that reaches one (it is
  left out of the program). Closures and handlers are written in full and
  lowered into nested functions, as `prepare_nested` does.
- **Intrinsic names.** The wrapping, bitwise, conversion, rotation and
  float intrinsic tables of `vesper-core::numeric` and
  `capability_effects` are a class word on each call of a plain name.

## Output

`ir_lower_words` returns `put_program`'s words (names, types, the program
section) followed by two words: the functions lowered and the functions
skipped. `decode_program` (tests/common/ir_words.rs) reads them back.

## Tests

`tests/self_host_ir_lower.rs`: every function the port lowers must be
`Debug`-equal to Rust's `FunctionIr`, and the IR check must report the
same diagnostics on both. It runs every second corpus world by default; `IR_LOWER_STEP=1` runs
every world and `IR_LOWER_STEP=n` every n-th.
The ignored `ir_lower_speed` (with `CHECK_HARNESS_SPEED=1`) times both
sides ABBA, and `callgrind_one_side` (`IR_LOWER_SIDE=rust|vesper|vesper-read`)
runs one side for callgrind.
