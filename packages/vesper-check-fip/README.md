# @vesper/check-fip

The fip checks of `src/fip.rs` (`fip::check_world`) in Vesper, on
[`@vesper/check-data`](../vesper-check-data/README.md) (self-hosting wave
SH2, lane 12, #1184). Rust stays the default checker; nothing here
replaces Rust code. Report:
[self-host-check-fip.md](../../docs/parallel-results/self-host-check-fip.md).

| File | Rust | Vesper |
|---|---|---|
| `src/ast.ves` | the bodies of `world.functions` (`ast::Expr`, `Pattern`, `Function`) | flat node tables over one list pool, and `read_ast` |
| `src/layout.ves` | `Layout::of(..).size` | `layout_size`, memoized per type id |
| `src/state.ves` | `FipState`, `BindingState` | rows: slots, name-ordered bindings, (name, path) moves; `Paths` |
| `src/check.ves` | `check_world`, `caller_slot_functions`, `unsafe_non_fip_functions`, `FipChecker`, the `Box` predicates, `slots_match`, `slot_size_label`, `seq::fip_allocation` | `Fip` |
| `src/lib.ves` | | the entries `check_fip` (text), `check_fip_count`, `check_fip_decode` |

The input is lane 4's harness encoding followed by the fip task section
(`FipEnc` in `tests/self_host_check_fip.rs`). `check_fip` returns one line
per diagnostic: code, span start, span end, message.

`tests/cases.txt` holds the fip checker test sources the differential runs
besides the corpus (see the report for how it is made).
