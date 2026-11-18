# Port speed lane 2: write destinations and block-parameter values built in place

Claim #1022. Base `72209eb` (origin/main). Prompt:
[02-write-in-place.md](../agent-prompts/port-speed/02-write-in-place.md).

**Result.** All four repros lose their copies: `q.f = Rec { .. }` through
`inout q` goes from 25 to 9 instructions, `q.o = Option::Some(v)` from 19 to
8, an `if` between two variants bound to a local and returned from 37 to 17,
and an IR-inlined callee with an early `return` from 25 to 15. On the
committed benchmarks BBR runs 3.8% fewer instructions (2.25x to **2.16x** of
Rust), Quarry 1.0% fewer (1.60x to 1.58x) and Ignis 0.3% fewer. No port reaches parity
from this lane, so Rust stays the default build of every port but Aura, and
no port source changed.

## What changed

All of it is on the native optimize path (`operands_only`), in
`src/native_aggregates.rs` and the home, `Write` and block-parameter hunks
of `src/native.rs`. The verified backend and the unoptimized JIT do not
change.

1. **`Write` destinations as homes** (`Home::Place`).
   - `plan()` gives a single-use constructor or direct call whose consumer is
     `place = value` the home `place` when the value's type drops nothing (no
     sequence, string, box, handle or function word), so no drop of the old
     value has to run before the first store.
   - The value is built at `place`'s address, and the `Write` emits nothing.
   - **The rule** (`drop_unsafe_places`, proved in its doc comment). Call
     the instructions from the first store into the destination (the
     earliest constructor of the value's tree) to the write *the window*.
     Every instruction in the window must be:
     - a member of the tree whose aggregate operands are other members or
       fresh constructor or call results;
     - or a constant, a read of a scalar local, arithmetic that cannot raise,
       or a constructor copying only fresh values.
     So no `Read` of an aggregate place, no `Write`, no place argument, no
     effect and no handler runs in the window, and nothing reads the
     destination after the first in-place store. Scalar operands are SSA
     values read before the window, so `q.f = Rec { a: q.f.c, b: q.f.a, c:
     q.f.b }` is built in place and is still correct.
   - **Faults.** When the destination is observable after a fault (an
     `inout` parameter, a local of an enclosing graph, a capture), raising
     arithmetic and calls whose callee has a non-empty effect row may not
     run in the window. A member call that breaks this loses its own home
     (it is built in a temporary, before the window) and the rest of the
     tree is re-checked. A local of the function's own graph dies on a
     fault, so its window may also hold calls of scalar arguments.
   - When the rule fails, the whole tree keeps its temporary, and the write
     copies it once.
2. **One copy instead of two for every other aggregate write.** A `Write`
   used to read its operand as an expression (a copy into a temporary) and
   then copy that into the place. It now copies from the operand's storage
   into the place directly. Distinct places of one type are equal or
   disjoint (a value cannot contain itself without a `Box`), so the forward
   word copy is exact.
3. **Block-parameter homes.** A single-use aggregate parameter of a join
   block that is not on a cycle, reached only by `Jump`s, shares its
   consumer's home (result, constructor field or write destination) with
   every value that reaches it. Those values are single-use constructors or
   calls in the predecessor blocks, or parameters of the same kind (nested
   joins: `else if` ladders, a `match` inside an `if`). They are assigned
   together, from the parameter, so the consumer skips its copy exactly
   when all of them were built there. Edges whose value is `Never` are
   skipped.
4. **Pass-through locals.** A local of a drop-free type that is bound once
   and read once later in the same block, as a whole move, and named
   nowhere else, is transparent: its read yields the bound storage, and the
   bound value is built in the read's home. This is `let s = if .. { .. }
   else { .. }; s`.
5. **Pointers are not cached across blocks.** `HomePointers` now keeps only
   which values were placed and the stack slot of each parent constructor
   without a home. Each use re-derives its pointer (`stack_addr`, the
   result buffer, or the destination's address), so a home reached from
   several predecessors dominates its uses.

### A miscompile found on the way

The first version computed a parameter's home from each value that reaches
it, with a search depth limit, and separately for the parameter itself. In
Ignis's `graph_add_texture` (an `else if` ladder behind `||` conditions, five
nested joins) the deepest arms ran out of depth while the parameter did
not, so the join skipped the copy of values built elsewhere. The plan
digest (`abba digest`) differed from Rust's, and the port did 2.2x the
instructions. Values reaching a parameter now get their home only from the
parameter's own search, all together. `long_else_if_ladders_agree` is the
shape that miscompiled; it fails on the first version and passes now.

### Two more found in review

Codex's review of the PR found two more, both reproduced and fixed, with a
test each in `tests/native_block_param_homes.rs`:

- A fallible call in one arm of an `if` that fills a field of a write
  through `inout q` lost its own home (the fault rule), but the join and the
  other arm kept theirs, so the join skipped copying the call's result
  (`a_fallible_arm_takes_its_join_out_of_the_destination`). A call that
  loses its home now takes every value sharing that home with it.
- Pointer metadata (`NativeInfo`, the closure targets of stored fields) is
  keyed by SSA value. Re-deriving the pointer to a parent's stack slot
  lost the targets recorded by a nested constructor, and calling the field
  then dispatched to every shape-compatible closure, including an effectful
  one of an exited handler (`closure_fields_of_slot_parents_keep_their_targets`).
  Each new pointer to a slot now starts from the merged metadata of the
  earlier ones. This also narrows some dispatches in the ports: the
  instruction numbers below are from the fixed compiler.

## Repros

Built with `vesper build <file> --lib`, then `objdump -d`, counting the
instructions in the exported function (padding excluded).

| Repro | main `72209eb` | this branch | Change |
|---|---:|---:|---|
| `q.f = Rec { a, b, c }` through `inout q` | 25 (two copies through two temporaries) | 9 (three stores into `q`) | no frame |
| `q.o = Option::Some(v)` | 19 | 8 | two stores into `q` |
| `let s = if c { Dot(x) } else { Box(x, x, x) }; s` | 37 | 17 | each arm stores into the result |
| `outer` calls `make`, inlined, with an early `return` | 25 | 15 | each arm stores into the result |

## Ports

Setup: cloud x86-64 container, 4 vCPU, Linux 6.18, Rust 1.97 `--release`.
Each port's committed example was built with `--features vesper-port` by
`VESPER_PORT_COMPILER` set to main `72209eb`'s release `vesper` or this
branch's (same port sources, `optimize = true`), and once without the
feature for the Rust arm. Instructions are callgrind `Ir`, whole process,
with the commands of [port-gaps-rescore.md](port-gaps-rescore.md). The main
arm reproduces that report's "before" numbers (49.45 M, 31.09 M, 165.65 M).

| Port | Command | Rust Ir | Vesper Ir, main | Vesper Ir, branch | Change | Ratio, main | Ratio, branch |
|---|---|---:|---:|---:|---:|---:|---:|
| BBR | `ack_path_abba measure 20000` | 21.97 M | 49.45 M | 47.55 M | -3.8% | 2.25x | **2.16x** |
| Quarry | `planner_abba window 20` | 19.47 M | 31.09 M | 30.78 M | -1.0% | 1.60x | 1.58x |
| Ignis | `abba measure 20` | 116.17 M | 165.65 M | 165.23 M | -0.3% | 1.43x | 1.42x |

Ignis's plan and lowering digests (`abba digest`) are equal in the Rust,
main and branch builds.

Where BBR's gain is (callgrind, by function, before the review fixes):
`ack_step` 4.74 M to 3.84 M and `vesper_function_56` 7.15 M to 6.41 M, the functions that write
`Option` and state fields of the controller. `vesper_function_126` (15% of
the run) and `_106` did not change.

Wall clock (before the review fixes): each port's committed ABBA driver against its Rust build, run
in the order main, branch, main, branch (BBR `abba <rust> <port> 16 200000`,
Quarry `driver`, Ignis `compare <rust> <port> 15 100`). The 95% interval
of the median ratio is in parentheses. Quarry's driver prints only the
median regression.

| Port, workload | Main, run 1 | Branch, run 1 | Main, run 2 | Branch, run 2 |
|---|---|---|---|---|
| BBR | (2.79-3.00) | (2.50-2.87) | (2.85-3.04) | (2.39-2.79) |
| Quarry | 1.90x | 1.90x | 1.89x | 1.94x |
| Ignis build 64 / 256 | 2.59x / 2.61x | 2.54x / 2.52x | 2.63x / 2.67x | 2.57x / 2.53x |
| Ignis plan 64 / 256 | 1.36x / 0.70x | 1.35x / 0.68x | 1.27x / 0.70x | 1.38x / 0.68x |
| Ignis lower 64 / 256 | 3.44x / 2.78x | 3.38x / 2.69x | 3.40x / 2.74x | 3.37x / 2.75x |

BBR's branch intervals sit below main's in both runs, in line with its
instruction drop. The Quarry and Ignis changes are inside this host's noise
(up to 30% for one binary). BBR's wall-clock ratio here is higher than its
instruction ratio, as in the re-score.

## Tests

- `tests/native_write_in_place.rs`:
  - field writes through `inout q` (structs, `Option`, a nested struct, a
    variant), writes whose operands read the destination as scalars (built
    in place) and as aggregates (temporary kept), and a whole-local write in
    a loop;
  - the rule on both sides: the CLIF of `placed` has no stack slot, the CLIF
    of `kept` (an aggregate operand read from the destination's root) still
    has one;
  - a write whose operand raises (a call, checked arithmetic) after part of
    the destination could be built: the handler sees the old value;
  - a local written around a call in a loop (built in place: it dies on a
    fault).
- `tests/native_block_param_homes.rs`: an `if` bound to a local and
  returned, a three-way `if`, a `match` in a struct field, an inlined callee
  with an early `return` into a write, an `if` between constructors that
  read the destination, joins inside loops, the `else if` ladder, and the
  CLIF of `pick` with no stack slot.
- Every program runs in the evaluator, the JIT with the optimizer on and
  off, x86-64 and AArch64 objects and an executable, and all agree.
  Recursive helpers keep the functions of interest calls.
- The existing native suites still pass: `native_aggregates`,
  `native_seq_access`, `native_seq_drop`, `native_bytes`,
  `native_bytes_builtins`, `native_fail_status`, `native_inline` and
  `native_hoist_headers`.

## Stage 1 guard

No file in the guarded set changed (`src/verified_codegen/**`, `src/ir.rs`,
`ir_opt::optimize`, `lean/**`, `bootstrap/**`, `packages/vesper-compiler/**`).
The bootstrap-stage1 workflow was dispatched on this branch; see the PR
for its run.

## What is left

- Write destinations of types that own something (sequences, strings) still
  copy once: building them in place needs the old value dropped before the
  first store, and the drop slots of `seq_drop_*` follow the copy.
- Write destinations whose window holds a fallible call, or a read of an
  aggregate place, keep the temporary. A finer alias rule (which roots an
  aggregate `Read` can point into) would place `q.p = Outer { inner:
  q.out.inner, .. }`.
- Parameters of loop headers get no home: the value of the next iteration
  would be built where the current one still lives.
- Quarry's and Ignis's hot paths write sequence-owning records or build
  their blocks by copy (the parity-wave workarounds), so they gain little;
  lane 8 can undo the workarounds and re-measure.
- BBR's `vesper_function_126` and `_106` are unchanged: their cost is not
  in write or join copies.
