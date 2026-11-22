# @vesper/check-loops-atomics

The loop, try/Option, atomics and stack-bound checks of the compiler in
Vesper (self-hosting wave SH2, lane 10, #1182), on the checker data of
[`@vesper/check-data`](../vesper-check-data/README.md). Rust stays the
default checker; nothing here replaces Rust code.

| File | Rust | Vesper |
|---|---|---|
| `src/control.ves` | `src/loops/check.rs` (control frames: `break_expr`, `continue_expr`, `return_expr`, `for_expr`'s frame, `Range`; the typed iterable rule stays with core typing), `src/loops/try_option.rs` (`try_option`) | `Control` (the frame stack), `visit`, `check_bodies`: E0213 and E0200 |
| `src/atomics.ves` | `src/atomics.rs`: `needs_prelude`, `needs_workers`, `is_prelude_function`, `check_world` (`check_expr`, `check_workers`, `worker_body`), `check_program` | the same names: E0820, E0821, E0822, E0823, E0824 |
| `src/stack.ves` | `src/stack_bound.rs` with `vesper_silicon::stack::analyze_stack` (Tarjan, longest path) and `vesper_ebpf::callgraph::check_stack` | `read_graph`, `validate`, `report`, `ebpf_rules`, `write_report` (with `render`) |
| `src/ast.ves` | `ast::Expr`, `Pattern`, `Function` | `Exprs`: flat expression, pattern and function rows |
| `src/out.ves` | `Diagnostic` | diagnostics as text lines |

## Design

The census design of `@vesper/check-data` (types are `u32` ids in a
hash-consed arena, names are interned, flat tables): a function body is one
row per expression with its children as a range of one shared pool
(`Exprs::kid`), so a walk is a loop over indices. A method name, an order
literal or `Atomic` is one symbol compare, and `Atomic[T]` one node compare.
Read the tables through the accessors (`Exprs::row`, `kid`, `pat`,
`function`), not the raw fields.

The control rules depend only on where an expression sits, so `control.ves`
walks a body with the checker's `ControlFrame` stack (function or closure,
loop, `for`, handled body, region block, handler clause) and reports what
`Checker::control_expr` reports. The loop rules that need the checker's
environment stay with the core typing port (SH2 lane 7): moves on a back
edge (E0312), linear bindings a jump drops (E0302), the merge of `break`
environments (E0309), and jumps across an `inout` loan or linear handler
state (E0402).

## Entries and inputs

Each entry takes the harness input (`tests/common/check_harness.rs`: names,
types, the world) followed by its task words, which
`tests/self_host_check_loops_atomics.rs` writes, and returns text packed by
`pack_text`.

- `check_loops_control`: the world's functions (`read_fns`), written in
  `world.functions` order with `Handle`s marked as region blocks the way
  the checker's `region_block` decides.
- `check_atomics`: the world's `atomic_prelude` and `workers_prelude`, its
  functions, the module's items (`read_items`) and the Place IR's `Read`,
  `Write` and `Project` instructions with each function's locals,
  parameters and value types (`read_ir`).
- `check_stack_bound`: one `StackGraph` and its `StackBoundPolicy`
  (`read_graph`).

## Tests

`tests/self_host_check_loops_atomics.rs` compares every line against Rust
(code, span, message, help and related labels):

- control: every corpus world, two mutants of each (jumps where each rule
  fires; every `for` over a non-range), the loop fixtures of the checker
  tests and `?` sources;
- atomics: the corpus, one generated program per cell type with every
  operation in every order, the worker sources, eight world mutants and an
  IR mutant that touches the private cell;
- stack bound: 4,000 generated call graphs of both origins, which make every
  rule fire.

`loops_atomics_speed` (ignored; `CHECK_HARNESS_SPEED=1`) times each task
against the Rust code with the harness's ABBA.
