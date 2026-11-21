# Self-hosting SH2 lane 10: loops, try/Option, atomics, stack bound and IO sessions

Claim #1182 (part of). Base `e43a56f` (main after SH2 lane 4, #1217).
Prompt: [10-loops-atomics.md](../agent-prompts/self-host-sh2/10-loops-atomics.md).
Rust stays the default checker; no Rust code changed or was deleted.

## What changed

- New package [`packages/vesper-check-loops-atomics`](../../packages/vesper-check-loops-atomics/README.md)
  (`@vesper/check-loops-atomics`, on `@vesper/check-data`):
  - `control.ves`: the control-target rules of `src/loops/check.rs`
    (`break`, `continue` and `return` targets, stray ranges,
    `break` values in `for`: E0213) and the `?`-on-`Option` rule of
    `src/loops/try_option.rs` (E0200), with the checker's `ControlFrame`
    stack. It also follows the checker's reachability: a block stops at a
    statement of type `Never`, read from the world's expression types.
  - `atomics.ves`: `needs_prelude`, `needs_workers`, `is_prelude_function`,
    `check_world` (E0820 orders, E0821 cell kinds, E0822 struct literals,
    E0823 and E0824 workers) and `check_program` (E0822 on Place IR) of
    `src/atomics.rs`.
  - `stack.ves`: `stack_bound_report` of `src/stack_bound.rs` with the
    algorithms it delegates to: `vesper_silicon::stack::analyze_stack`
    (iterative Tarjan, longest path, the six Silicon rules) and
    `vesper_ebpf::callgraph::check_stack` (512 bytes, 8 frames, the
    analyzer cross-check), plus `validate` and `render`.
  - `ast.ves`: function bodies as flat expression, pattern and function
    rows (children are ranges of one pool), the census design applied to
    `ast::Expr`.
- `tests/self_host_check_loops_atomics.rs`: the differentials, the AST,
  item and IR encoders, the mutants and the ABBA timing. It adds task words
  after lane 4's input; it adds no `World` fields to `@vesper/check-data`
  (none were needed), so no hunk of that package changed.
- `BUILD.bazel`: the test's corpus data and its `long` size.
- One scoreboard row (a new SH2 table) in
  [self-hosting-plan.md](../maintenance/self-hosting-plan.md).

No manuscript change: the package uses only existing language features.

## Differential results

Every line compares code, span, message, help and related labels.

| Task | Inputs | Lines | Differences |
|---|---|--:|--:|
| control (E0213, E0200) | 282 corpus worlds and two mutants of each (jumps where every rule fires, each in `if true { .. }` so a jump does not end its block; every `for` over a non-range), the 9 loop reject fixtures and 15 `?`, jump and handler sources with their mutants (closures whose result comes from a `let` annotation or a call argument, a clause for an undeclared operation, an unknown effect): 903 checked modules | 49,614 | 0 |
| atomics (E0820-E0824, preludes) | corpus, a program per cell type (10 integer kinds, `bool`, `f64`, a tuple, `Option`) with every operation in every order and both call forms, 8 worker sources, 8 world mutants (orders rotated, made non-literal, `Atomic::new` as a struct literal, `inout` data, a non-function body) and an IR mutant that touches the private cell, an order named only in a `let` annotation: 535 worlds | 11,713 | 0 |
| stack bound | 4,000 generated call graphs of both origins and policies (every Silicon rule, the eBPF kernel rule, the analyzer disagreement, invalid graphs) | about 30,000 | 0 |

Cargo runs the whole corpus (Linux, macOS, Windows CI); Bazel's corpus is
what the root package declares.

Two bugs the differential found while porting, both in the port: the
checker skips the rest of a block after a `Never` statement (so jumps there
are never reported), and a `let` value's own type field was overwritten by
the `let` type (a closure's result type was lost).

Review found three more, now fixed and covered by new sources: the prelude
scan read a closure's result field instead of the `let` annotation; a
closure with no written result expects the result of the function type it
is checked against (the encoder now writes that expected result for an
annotated `let` and for an argument of a non-generic function; other
contexts, such as struct fields or generic parameters, are not encoded yet);
and `Checker::handle` skips a clause for an undeclared operation (E0405) and
the whole handler for an unknown or capability effect, which the encoder now
marks.

## Numbers

ABBA, best of six per side, release build, Linux x86-64 (cloud host; wall
clock moves by up to 30% between runs). Vesper times include decoding the
harness input (names, types, the world, and every function body or graph),
not encoding it.

| Task | Rust | Vesper | Ratio |
|---|--:|--:|--:|
| atomics (`needs_prelude`, `needs_workers`, `check_world`, `check_program` over the corpus) | 58.2 ms | 134.4 ms | 2.31x |
| stack bound (4,000 graphs) | 12.0 ms | 30.4 ms | 2.54x |
| control pass over the corpus | inside the checker (35.5 s for the whole check) | 79.7 ms | n/a |

The Rust control rules run inside the type checker, so they have no
separate time; the Vesper pass is 0.2% of the whole check's time. Most of
the atomics and stack time is decoding: each atomics input carries every
function body and the Place IR, which Rust reads in place. ABI v4 input
(lane 4's follow-up) removes that.

Callgrind counts are not taken yet (see "What is left").

## Stage 1 guard

Nothing under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt`, `lean/**`,
`bootstrap/**` or `packages/vesper-compiler/**` changed, and no compiler
source changed at all, so the Stage 1 artifacts cannot move; the
bootstrap-stage1 workflow on the branch confirms the digests against
`bootstrap/stage1.lock`.

## What is not ported, and why

- **The environment half of `loops/check.rs`** (havoc of loop facts, moves
  on a back edge E0312, linear bindings a jump drops E0302, merging `break`
  environments E0309, `for` range typing and integer facts, jumps across an
  `inout` loan or linear handler state E0402). These read and write the
  checker's `Env` on every expression, so they move with core typing (SH2
  lane 7), which owns `Env`.
- **`try_option::desugar`** rewrites the module before a second check; it
  belongs with the checker driver that runs that second check (lane 7).
- **`src/io_session.rs`** is not a checker: it is the evaluator's I/O
  embedding (a `Resume` state machine over `runtime::Suspension`,
  `std::fs::File` and the io_uring driver). It moves with the evaluator,
  not with the checker. Nothing in this lane's boundary can host it.
- **`atomics::thread_affine`** and `worker_body`'s JIT use are codegen
  queries, not checks; they move with the native lowering (SH3).

## Compiler gaps

1. **`for` iterates only integer ranges** (SH3 lane 4, #1243, adds arrays,
   `ChunkSeq` and `Slice`, and makes the iterable rule a typed one; this
   port leaves that rule to core typing and only keeps the checker's skip
   of a rejected `for`'s body). `for s in ["Relaxed", "Acquire"]
   { .. }` is E0213 ("'for' iterates an integer range"). The order table
   is five interned symbols instead. Repro:
   `fn main() -> u64 { let mut n: u64 = 0; for s in ["a", "b"] { n = wrapping_add(n, s.len()); } n }`.
2. **A `const` cannot be imported from another module** (lane 4 gap 1):
   `control.ves` and `atomics.ves` repeat the `E_*` kind constants of
   `ast.ves`.
3. **No view of part of an interned name.** Stripping `Atomic::` from a
   symbol copies the rest into a `ChunkSeq[u8]` and looks it up again; a
   `Str` view from `as_bytes()` cannot be kept in a binding (E1531: "a
   string view is a second-class borrow"). Repro:
   `fn main() -> u64 { let s = "ab"; let v = s.as_bytes(); v.len() }`
   (E1531 on `s.as_bytes()` outside an argument).

## What is left

- The hybrid swap: a swap-in table in `src/stack_bound.rs` (the cleanest
  boundary: one call from `vesper ebpf check` and `--verify-stack`) and a
  `modules/loops_atomics` hybrid module, with corpus commands that reach
  it; then `check_world` behind the checker's atomics call once the AST
  crosses ABI v4.
- Callgrind instruction counts of each task, and the ABI v4 input.
- The environment rules above, with lane 7.
