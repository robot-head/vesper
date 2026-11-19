# Boxes owned across loops and early exits

Claim: #1064 (port wave P3b, lane 2). Follow-up to #1057 (PR #1060, boxed
and stored `dyn Trait`). Baseline: `main` at `8212ddf`.

## What changed

- **Manuscript** (`docs/manuscript/content.typ`, the boxed-object item of §7.3
  and the loops paragraph of §2.2). A function with loops or early exits owns boxes as
  any other function does, in a local, a `sink` parameter or an aggregate's
  fields, plain `Box[T]` and `Box[dyn Trait]` alike. Each box it owns is
  dropped exactly once on every path: at the end of the iteration that binds
  it, when its place is assigned, at a `break` or `return` that leaves it
  live, or after the loop; a box moved on one path is dropped on the others.
  Moving a box the next iteration needs stays `E0312`, and a linear value
  live at `break`/`return` stays `E0302`. A sequence element read through
  an index is lent, never owned. A box lent `sink` to a function whose
  handler effects are specialized at the call is freed when that call
  returns, resumes or is aborted by its handler. The old text (a loop
  function may not own a box; the specialized `sink` lend is unsupported) is
  replaced.
- **Drop elaboration** (`src/fip.rs`, `elaborate_non_fip_drops`). The pass
  used to skip any graph without a topological order, so the native
  backends refused a loop function that owned a box
  (`fip::loop_boxes_reclaimed`, now removed). The per-block analysis is
  unchanged in what it tracks (slot moved paths and owned values) and now
  runs to a fixed point:
  - blocks are visited in topological order when the graph is acyclic (one
    pass, the old behavior) and in reverse postorder otherwise;
  - a block's entry state joins its visited predecessors: a slot exists
    when it exists on every incoming edge, a path is moved when any edge
    moved it, and the edge on which a slot (or a path of it) is still owned
    drops it there. The first pass leaves unvisited back edges out of their
    join; each pass recomputes every drop, and the plan of the pass that
    changes no state is the plan of the solution (64 passes at most);
  - `return` (`Op::Exit`, typed `Never`) drops what the graph still owns
    before it leaves; it used to be skipped as a diverging instruction;
  - a sequence element read in place (`Seq` `Get`/`SliceGet`) is not an
    owned value. This also fixes a double free in acyclic functions from
    #1060: `s[0].area()` freed the element after the call and again with the
    sequence (it segfaulted natively).
  - `fip::loop_drops_elaborated` tells the native backends whether a graph
    was planned; they refuse only what it could not plan
    (`loops::BOX_REFUSAL`).
- **Per-edge drops, not drop flags.** A drop that belongs to one edge goes
  before the source block's `Jump`; on a `Branch` or `Match` edge (the
  `for` header's exit edge, typically) the elaboration splits the edge with
  an empty block that holds the drop (`split_edge`). The loop checker's
  `E0312` keeps the moved paths reaching a loop header from a back edge
  within those reaching it from outside, so a box live at the header is
  owned on every edge into it and the static state is exact: no runtime
  flag, no extra branch, and no instruction inside a loop body that only
  uses its box. Drop flags would have needed native changes on every move
  and a test on every drop; the IR already says which edge owns what.
- **`sink` lends to handler-specialized callees** (`src/native.rs`). The
  inlined body reads the object in its box's block, so the block could not
  be freed before the body ran, and an aborting handler skipped the free
  after it; #1060 refused the lend. Now the block and the lent
  `{data, vtable}` pair are kept in variables, recorded with the handler
  count at the call (`LentSinkBlock`). When the body returns, when an
  operation clause of a handler installed outside the call aborts it, or
  when a `Fail` or trace exit leaves the whole function, the object is
  dropped through its vtable if the callee left it unconsumed (a consumed
  pair's vtable word is 0) and the block is freed; a resumed body has done
  so already (the block variable holds 0). A handler the callee installs
  itself never frees its caller's object. The refusal is gone.

## Tests

`tests/loop_boxes.rs`, each in the JIT with the optimizer on and off (zero
box leaks and no double frees under the counting allocator, no sequence
left for teardown, no stray sequence frees), the evaluator agreeing, and
AArch64 and x86-64 objects:

- a loop that owns a `Box[dyn Step]` and calls and mutates it every
  iteration;
- reassignment inside the loop (each branch assigns a different object);
- a box bound in the body, dropped per iteration and on the `break` that
  leaves with it live;
- `return` from inside a loop with live boxes (one consumed by the return,
  one dropped) and `break b` carrying a box out;
- a box moved on the path that leaves the loop and dropped on the others;
- a `sink` box parameter and a plain `Box[T]` local in the same loop;
- a struct owning a `Box[dyn Step]` and an `Option[Box[dyn Step]]` field,
  with a field reassigned inside the loop;
- `sink` lends to handler-specialized callees that consume, borrow, resume
  and abort (the abort inside a loop), and one whose own handler aborts
  before it keeps using the object;
- `@fip` functions with a loop still may not drop an owned box (`E0806`).

`tests/dyn_box.rs` `loops_own_aggregates_owning_boxes` (was
`loops_refuse_aggregates_owning_boxes`) and `tests/loops.rs`
`native_reclaims_boxes_across_loops` (was `native_refuses_boxes_across_loops`)
now run the programs they used to refuse.

## Measurement

`cargo test --test loop_boxes bench -- --ignored --nocapture`, optimized JIT,
50,000,000 iterations of `x = b.step(x)` through a `Box[dyn Step]`, three
runs, compile time included:

| loop | ns per iteration |
|---|---|
| owns the box (`let b` in the loop function) | 3.5-4.7 |
| borrow-only workaround (`in b` parameter, #1060) | 2.6-3.0 |
| boxes and drops an object every iteration | 427 (debug-built runtime helpers) |

The elaboration adds nothing to the loop: the owning loop's only drop is on
the exit edge, and its body IR is identical to the workaround's. The
remaining gap is the native backend's register choice: `register_locals`
keeps scalar locals and `in` parameters in registers but gives a `Box`
local a stack home, so the owning loop reloads the box word each iteration.
Keeping unaddressed box locals in registers belongs to the native backend's
performance hunks (the port speed wave), not to this lane. The per-iteration
case calls the Rust box allocator, which the test profile builds without
optimization; it is shown only to confirm the drop runs every iteration.

## Stage 1 guard

No edit to `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**`. The elaboration
runs only on the native paths (JIT, objects, export, sessions); the
verified backend never calls it. The bootstrap-stage1 workflow run on this
branch is linked from the PR.

## Which P3b ports it unblocks

- **Chorus** (lane 6, part 2): session and Raft drivers that own a
  `Box[dyn Transport]` and loop on it.
- **Warp** (lane 4): the endpoint that holds a `Box[dyn CidSource]` and
  loops over datagrams.
- **Kinetic** (lane 8): the world that stores its pair backend as a boxed
  object and steps in a loop.

## Remaining gaps

- An inlined (handler-specialized) callee does not drop a `sink` parameter
  of another type it leaves unconsumed (a `ChunkSeq`, for one); boxed
  objects lent `sink` are dropped by the caller.
- Box drops do not run when a `Fail` leaves the function (acyclic functions
  too): a box owned at a failing call is not freed. Sequences already drop on that path (`seq_drop_exit`).
- Bounds-check hoisting skips loops in functions with Box drops.
- A box local lives in a stack slot (see the measurement).
