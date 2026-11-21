# @vesper/lean-extract

Lean extraction (`src/lean.rs`, `src/lean_ir.rs`, `src/lean_nodes.rs`) in
Vesper, self-hosting wave SH5 lane 18 (#1341). Rust stays the default
extractor; nothing here replaces Rust code.

Part 1 ports the head of the module `lean::extract_report` builds, which is
everything before the first source function:

| Rust (`src/lean.rs`) | Vesper |
|---|---|
| `prelude`, `arithmetic_simp_bundle`, `divergence`, `fail_models` | `src/prelude.ves` |
| `adt_groups`, `lean_nodes::recursive_structs` (petgraph `tarjan_scc` over a `DiGraphMap`), the getters, the effect classes and lifts | `src/head.ves` |
| `lean_nodes::ty_with_fip` (no ambient monad) | `src/ty.ves` |
| `lean_nodes::{ident, source_name, source_ref, global}`, `Ident::new` | `src/build.ves` |
| the module wrapper (`import Std`, the option, `namespace Vesper`) | `src/lib.ves` |

The extractor writes the word stream that the `@vesper/lean-syntax` printer
reads (tags in `crates/vesper-lean-syntax/src/port.rs`) directly, instead of
building a tree and encoding it. Every text is a symbol of the
`@vesper/check-data` names table, so the table's bytes are the printer's
text pool. Errors keep Rust's order: types are checked in Rust's evaluation
order before they are written (`ty_error`), and the first error wins.

Part 2 ports the CFG extractor (`src/lean_ir.rs`), which the module uses
for every pure function without fallible operations (about 60% of the
corpus's functions):

| Rust (`src/lean_ir.rs`) | Vesper |
|---|---|
| `extract_function_nodes`' gate, `uses_defs`, `arm_bindings`, `liveness` | `src/cfg.ves` |
| `extract_closures`, `Emitter` (blocks, instructions, patterns, terminators), `project`, `lean_nodes::{update_place_typed, arith_term, tiled_op_term, typed_literal}` | `src/emit.ves` |
| `world.functions`, `world.calls`, the lifted closures | `src/fn_input.ves` |

The function reads the lowered program through `@vesper/place-ir`. Live
sets are bitsets whose bit order is Rust's `BTreeSet<Local>` order, so
block parameters come out in the same order.

## Input

The `@vesper/check-data` harness input (names, types, the world's ADTs),
then the effect section (`src/input.ves`): per effect, its name, generic
names, and per operation its name, parameters and result. The entry
`extract_head` returns the head's text, or `error: <message>`, packed by
`pack_text`.

The entry `extract_cfg_functions` reads the harness input, then the
@vesper/place-ir program section of the lowered program, then the
function sections (`src/fn_input.ves`): signatures, call types and
lifted closures. The encoder (`cfg_input` in the test) lifts each closure
the way `extract_closures` does (it lowers the world extended with the
lifted function) and adds the lifted function to the program, so Vesper
needs no lowering of its own. It returns, per source function, `none`, the
error, or the definitions as a module.

## Test

`tests/self_host_lean_extract.rs` compares the text with
`lean::extract` of each corpus world with its functions removed, byte for
byte, and checks that this head is a prefix of the full module's
declarations wherever the full extraction succeeds. Mutants cover the
refusals (128-bit fields, rows, effectful function fields, names Lean
cannot spell).

For part 2, `cfg_functions_match_rust_on_the_corpus` compares every
corpus function with `lean::cfg_function_nodes` (a hidden hook over
`extract_function_nodes`), and `cfg_functions_match_rust_on_test_sources`
does the same for the Vesper programs in `tests/lean*.rs` and
`tests/tiled.rs`, tiled-sequence programs, and mutants that rename
functions to names Lean cannot spell.
