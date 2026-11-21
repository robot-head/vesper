# Self-hosting SH4 lane 6: native scalars, operators and control flow

Claim #1280. Stacks on #1259 (SH3 lane 1 part 1, `@vesper/place-ir`), which
stacks on #1232. Prompt: `docs/agent-prompts/self-host-sh4/06-codegen-scalar.md`
(plan PR #1293). Rust stays the default compiler; nothing here changes a
default build, edits the Rust lowering or deletes Rust code.

## What changed (part 1: the decisions)

The lane's emission and swap wait on lane 1 (#1275), which lends the Rust
`FunctionBuilder` to Vesper. Part 1 is the work that starts now: the parts of
`Codegen::ir_graph_at` and `Codegen::expr_at` (src/native.rs) that read the
Place IR and decide, before any Cranelift instruction is emitted.

- `packages/vesper-codegen-scalar` (`@vesper/codegen-scalar`), over
  `@vesper/place-ir`, `@vesper/check-data` and `@vesper/check-layout`:
  - `blocks.ves`: the block layout of one graph. The order blocks are lowered
    in (petgraph's `toposort` of the reached blocks, or a reverse
    `DfsPostOrder` from the entry when they have a cycle, then the unreached
    blocks in ascending order), the blocks on a cycle (`tarjan_scc` plus
    self loops), and the blocks the walk seals itself (every predecessor
    already lowered).
  - `lib.ves`: for every `Op::Constant`, `Op::Unary` and `Op::Binary`, the
    `expr_at` arm that lowers it, the operand's integer kind and Cranelift
    lane, and the run-time checks: `Fail[ArithOverflow]` (checked arithmetic
    with the exact overflow test `checked_arithmetic` uses, negation,
    narrowing bounds, shift range) and `Fail[RuntimeError]` (divisor). Also
    `Op::Loop`, `Op::Exit` and the terminators, with the `Never` operands
    that become default values.
  - `codegen_scalar_decisions`: the harness entry, one text dump per program.
- `tests/self_host_codegen_scalar.rs`: the Rust half (`decisions`) and the
  differentials below. `decisions` uses the guards of `expr_at`, `binary`,
  `checked_arithmetic`, `convert`, `shift` and `bit_builtin` with the
  compiler's own predicates (`numeric::conversion`,
  `integer_widths::bit_builtin`, `capability_effects::bit_intrinsic`,
  `strings::is_text`, `Layout::of`, ...) and the same petgraph calls as
  `ir_graph_at`.
- BUILD.bazel: data and a `long` timeout for the new test.

### Why the block order needs petgraph's exact behaviour

`ir_graph_at` lowers blocks in the order petgraph returns, and that order
decides the emitted CLIF. It is not just any topological order:

- A `DiGraphMap` keeps one adjacency list per node, with `(neighbor,
  direction)` entries in `add_edge` order. `toposort` visits the outgoing
  entries in list order.
- `ir_graph_at` removes the unreached blocks with `remove_node`, which
  `swap_remove`s the node from the node `IndexMap` and each entry naming it
  from the other lists. Removing a block that jumps to a reached block moves
  the last entry of that block's list into the hole, which changes the order
  `toposort` visits its successors in.

`blocks.ves` keeps the same lists and the same `swap_remove`s, so it matches
on graphs with unreached blocks (blocks after a `break` or `return`) as well.

## Results

Every corpus world (Cargo corpus), as the native path compiles it: with
optimize on, after `ir_opt`, the inliner, drop elaboration and bounds
hoisting; with optimize off, after `fip_reuse_pass` and drop elaboration
(584 programs, optimized test build, `CODEGEN_SCALAR_STEP=1`).

| Check | Programs | Differences |
|---|--:|--:|
| Vesper decisions = Rust decisions (34,684 functions, 1.65 M lines) | 584 | 0 |
| Operator mutants (below; 2.17 M lines, 62,993 error decisions) | 438 | 0 |
| Programs the format cannot encode | 0 | |

The clean corpus has no `i128`/`u128` values, bit builtins, rotations, float
`min`/`max` or `&&`/`||` left in the IR, so the second differential uses
**operator mutants**: every 4th program, three times, with every `Unary`
and `Binary` operator renamed round robin through every family of
`expr_at`'s arms (and names that are none of them), and every scalar value
retyped round robin through all twelve integer kinds, both floats, `String`
and `bool`. The mutants need not type check, since the decisions read only
names and types. They reach every arm except the two noted under "Findings".

Decisions the full corpus run exercises (Rust and Vesper agree on each):

| Decision | Corpus | Mutants |
|---|--:|--:|
| `const int` / `bool` / `string` / `f64` / `f32` | 282,079 / 46,907 / 13,714 / 982 / 202 | 366,063 / 55,812 / 13,692 / 81 / 306 |
| `binary icmp` / `fcmp` | 99,212 / 422 | 42,434 / 14 |
| `binary iadd` / `isub` / `imul` | 50,693 / 14,557 / 10,798 | 7,698 / 6,787 / 13,284 |
| `binary udiv` / `sdiv-guarded` / `urem` / `srem` / `wide-divide` | 1,179 / 24 / 361 / 25 / 0 | 5,282 / 5,280 / 4,065 / 5,115 / 1,538 |
| `binary` float `fadd` / `fsub` / `fmul` / `fdiv` / `fmod` | 558 / 443 / 937 / 102 / 8 | 0 / 0 / 1 / 1 / 2 |
| `binary checked` (overflow test chosen) | 1,217 | 1,269 |
| `binary divisor` (`DivisionByZero` check) | 128 | 61 |
| `binary shift` / `bitop` / `bitwise` / `bit` builtin | 9,343 / 16,192 / 195 / 0 | 23,030 / 23,103 / 9,853 / 10,708 |
| `binary str-compare` / `equal` / `logic` / `float-math` | 72 / 60 / 0 / 0 | 8,443 / 11 / 18,321 / 11,403 |
| `unary convert` / `not` / `bnot` / `fneg` / `ineg` / `float-unary` / `bit` | 28,988 / 7,521 / 415 / 133 / 14 / 1,350 / 0 | 18,949 / 2,857 / 2,376 / 388 / 3,370 / 13,602 / 8,700 |
| `binary error` / `unary error` (Rust's `Err` paths) | 0 / 0 | 56,847 / 5,828 |
| `loop` / `exit` | 21,600 / 12,954 | 24,528 / 10,875 |
| terminators `jump` / `branch` / `match` / `return` / `trap` | 623,935 / 113,408 / 40,907 / 34,684 / 76,089 | |

### Time

The decisions of all 584 programs, best of five, optimized test build, one
4-core cloud machine; the encoding is excluded, as SH3 measures passes:

| | Time | Ratio |
|---|--:|--:|
| Rust reference (`decisions` in the test) | 1.39 s | 1.00x |
| `@vesper/codegen-scalar` (`codegen_scalar_decisions`) | 0.91 s | 0.65x |

The Rust side here is the test's reference, which formats strings; it is not
the time `Codegen` spends deciding, which is folded into emission. The
scoreboard row (emission alone and the whole compile, ABBA and callgrind)
comes with part 2, when the Vesper decisions drive emission.

## Part 2a: emission through the `scalar` hook

Lane 1's swap point (#1309, src/codegen_swap.rs) asks the `scalar` hook to
emit each Place IR instruction of the family at `ir_graph_at`. Part 2a
answers the operators and constants from Vesper:

- `codegen_scalar_plan` (`@vesper/codegen-scalar`, lib.ves) reads the
  program the backend is emitting and plans each instruction it emits:
  the request, the condition code or operation, and the coerce words (the
  narrow kind `coerce` reduces the result and each operand to). It follows
  `expr_at`'s arm order and leaves every arm it does not plan to Rust.
- `scalar_hook` (`packages/vesper-codegen-scalar/emit`,
  `@vesper/codegen-scalar-emit`) emits one planned instruction over the
  lent builder. It checks the operand lanes only the builder knows
  (floats and 128-bit words take other arms, mismatched lanes are an error
  in Rust) and declines before emitting anything when they do not fit.
- crates/vesper-codegen-swap-test/tests/self_host_codegen_scalar_emit.rs:
  the glue encodes the lent program once, looks the site up in the plan,
  passes the plan's words, the optimizer flag and the operand values to
  the hook, and compares with lane 1's harness (`compare`: per-function
  CLIF, object bytes, sampled JIT runs), optimize on and off.

Planned: `Constant` (integers, bools, `f64`), `Unit` and `Loop` zeros,
integer compares, `+ - * / %` (unchecked; unsigned `/`, `%`), `& | ^`,
`~`, `!`, unchecked `-`, unchecked shifts and rotations, and the widen,
wrapping and unchecked narrow conversions. Two details decide the bytes:

- `ir_temp` declares a fresh Cranelift variable for every operand, and
  `expr_at`'s `Name` arm reads it back with `use_var` and `coerce`s it to
  the operand's type, so a `u16` operand gets its `band_imm` again before
  each use. The hook declares, defines and reads the same variables (the
  variable numbers decide where the SSA builder puts later zero defaults)
  and coerces each operand.
- `coerce` takes the narrow-move form when the function is emitted with
  the optimizer; the hook reads that from `Lent::optimize` (#1309 added
  it for this lane).

| Check (`CODEGEN_SCALAR_STEP=1`) | Count |
|---|--:|
| Builds compared (149 worlds with a `main`, optimize on and off) | 298 |
| Functions compared (over both builds) | 882 |
| JIT runs compared | 74 |
| CLIF / object / run / one-sided differences | 0 / 0 / 0 / 0 |
| Instructions emitted from Vesper | 12,613 |
| Planned but declined by the hook | 0 |
| Left to Rust | 15,933 |

Emitted per request: `iconst` 8,024, `binop` 2,306, `icmp` 1,427, zero 582,
`convert` 138, `f64const` 122, `not` 14 (the compiled corpus has no
unchecked shift, `~` or unchecked negation). Left to Rust: `Read` 10,307,
`Bind` 3,649, `Write` 295; `+`/`-`/`*` 1,203 and `/`/`%` 129 (checked
arithmetic, divisor checks, signed `/`, floats, operands that are not
one-word integers); comparisons 123 (aggregates, functions, floats, text);
the bitwise intrinsics 84; string constants 51; unary `-` 42 (checked or
float); `f32` constants 34; `Exit` 10, `Try` 6.

## Part 2b (first step): the raising arms' tests

Lane 1's shape for arms that raise: the hook computes the test and Rust
raises. Two family dispatch hunks do that (flagged for lane 1's review):

- `codegen_swap::scalar` (end of src/codegen_swap.rs): request
  `OVERFLOW` (args `[op, kind, a, b]`, answer the `i8` overflow flag) and
  `DIVISOR` (args `[divisor]`, answer the nonzero flag), with `op_word`
  and `kind_word`.
- src/native.rs: `checked_arithmetic` asks `OVERFLOW` before its own
  test and `check_divisor` asks `DIVISOR` before its `icmp_imm`
  (`Codegen::swap_scalar`, `swap_overflow`). A decline, or no hook,
  emits the Rust test unchanged, so the default compiler's objects are
  byte-identical; `check_overflow`/`primitive_failure` and the operation
  after the test stay in Rust.

`scalar_hook` emits every overflow test of `checked_arithmetic` (narrow
signed and unsigned, carry and borrow, `umulhi`/`smulhi`, the signed
`MIN / -1`). Over the same corpus run: 1,145 overflow tests and 92
divisor tests from Vesper, 13,850 requests in all, still 0 CLIF, object
and run differences.

## Stage 1 guard

Parts 1 and 2a add two Vesper packages, two tests, a report and BUILD.bazel
entries. Part 2b adds the two dispatch hunks above (src/codegen_swap.rs,
src/native.rs), which only ask an installed hook. None of it touches
`src/verified_codegen/**`, `src/ir.rs` or `ir_opt::optimize`, nor lean/**,
bootstrap/** or
packages/vesper-compiler/**. Nothing installs a hook, so the default
compiler's objects are byte-identical to the base branch's. The Bootstrap
Stage 1 run on the branch is linked from the PR.

## Compiler gaps

1. **`@vesper/check-layout` has no `i128`/`u128` layout** (finding 1).
2. **The lent request did not say whether the optimizer is on**
   (`Codegen::coerce` emits `ireduce` + `uextend`/`sextend` under
   `operands_only`, a mask or shift pair otherwise). Lane 1 added
   `Lent::optimize` (#1309, 14c6729), which the hook now reads.
3. **The Rust walk's own maps are not lent.** `ir_temp` also enters each
   temporary in `Codegen::vars` and `local_types`; no later code reads
   them for a scalar temporary (the differential is clean), but a family
   that emits reads or binds needs the walk's variables (`vars`,
   `registers`, homes, split locals), which the swap point keeps in Rust.
4. **Arms that raise** (checked arithmetic, divisor checks, checked
   negation, narrowing and shift ranges) call `check_overflow` /
   `primitive_failure`, which build a `RuntimeError` value and perform
   `Fail::raise` through the control family, and `sdiv`'s guard jumps
   through `Codegen::jump`, which records edge value infos. Part 2b
   follows lane 1's shape: the hook answers the overflow or zero-divisor
   flag and the Rust path raises (above). The narrowing and shift-range
   checks and `sdiv`'s guard are next in the same shape.

## Findings

1. **`@vesper/check-layout` has no `i128`/`u128` layout** (gap, minimal
   repro: `Layout::of(&world, &Type::named("i128"))` is a 16-byte scalar in
   Rust; `layout_of` on the same type returns `NONE32`, a layout failure). The package predates #1174 (SH2
   lane 2). It surfaced in the mutants as `error layout` where Rust decided
   `icmp`. This package decides the wide names before asking the layout, as
   `Layout::build` does, so it does not depend on the gap. The fix belongs in
   `packages/vesper-check-layout` (not this lane's path): add the two-word
   scalar before the one-word check.
2. **The `widen_u64` arm of `expr_at`'s `Unary` is unreachable.**
   `numeric::conversion("widen_u64")` is `(Widen, U64)`, and the conversion
   arm comes first, so the later `WIDEN_INTRINSIC` arm never runs (the
   mutants send `widen_u64` through `Unary` 1 in 18 times and always reach
   `convert widen`). Both arms emit the operand unchanged, so this is dead
   code, not a miscompile. The port keeps the arm, as Rust does.
3. **Not in the check-data input**: `Codegen::flushes` reads the function's
   `eff [ConstDenorm]` spans (`self.denorm`), which the harness input does not
   carry, so the float flush-to-zero decision is not compared here. It joins
   with emission (and lane 11 owns the float arms).
4. **Value-dependent decisions** wait for emission: `wide_shift_const` needs
   the shift amount to be a known constant (`constant_of` on the Cranelift
   value), and `coerce`'s narrow move versus mask depends on the optimize
   flag and the value's lane at that point.

## What is left

- Part 2b: `Read`, `Bind` and `Write` (most of what is left), the raising
  arms (gap 4), floats with lane 11, string constants with lane 9.
- Part 3: the swap through the `scalar` hook, the full suite on the hybrid
  build, and the SH4 scoreboard row (emission alone and the whole compile,
  ABBA and callgrind).
