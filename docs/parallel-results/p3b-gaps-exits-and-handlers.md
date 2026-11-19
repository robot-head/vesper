# P3b gaps lane 10: drops on every exit and handler fixes

Claim #1095. Prompt:
[10-exits-and-handlers.md](../agent-prompts/p3b-gaps/10-exits-and-handlers.md).
Base `9305b24` (origin/main with the wave plan #1098), merged with
origin/main `31f81b9` (#1076) before merging.

Parts 1 and 2 merged in PR #1105 (`c9bd17a`; the bootstrap-stage1
workflow passed on that branch). Part 3 (Box drops on `Fail` exits,
specialized `sink` leaks) merged in PR #1114 (`3a8d446`). Part 4 (affine
extern-type drop imports) is the last PR and closes #1095.

## Part 1: bit builtins on a reassigned `let mut`

**Repro** (port-p3b-kinetic.md gap 1, port-p3b-prism-h3.md gap 1):

```
pub fn f(v: u32) -> u32 { let mut x = v; x = wrapping_shl_u32(x, 16); x }
```

`vesper build --lib` failed with `optimized IR failed ownership validation:
E0410 IR access to '$slot1_x' overlaps active loan '$slot1_x'`; so did
`acc = wrapping_shl_u64(acc, 5)` in a loop, and the JIT and executables on
the same programs.

**Cause.** `integer_widths::lower_bit_builtin_calls` (#988) rewrites each
call of a suffix bit builtin into an `Op::Unary`/`Op::Binary`, but kept the
operands as `in` reads. The IR checker releases the shared loan of an `in`
argument when a *call* consumes it; an *operator* consumes copy operands and
releases nothing, so the loan outlived the operation and the later write of
the same local overlapped it. `wrapping_add` never had the problem because
the front end lowers its operands as by-value reads.

**Fix.** The pass now reads the rewritten operands by value (`Pass::Sink`),
as every other operator's operands are read. They are integers (copy), so
nothing moves. Only that hunk of `src/integer_widths.rs` changes.

**Tests.** `tests/bit_builtin_mut.rs`: every builtin of every integer kind
on reassigned `let mut` locals, straight-line and in `for` loops, over edge
values and 200 pseudo-random operands. The evaluator (optimize on and off),
the JIT (optimize on and off), x86-64 and AArch64 objects, an executable and
`vesper build --lib` all agree with the Rust model `BitOp::eval`. Both port
repros build as libraries and run.

**Port rewrite (local, not committed).** The Prism H3 port's `int.ves`
`shl`/`shr` helpers (the report's workaround) were removed and all 22 call
sites in `huffman.ves`, `prefix.ves`, `varint.ves` and `int.ves` rewritten
to `wrapping_shl_u64(x, wrapping_u32(s))`/`wrapping_shr_u64`. Every call
site keeps its amount below 64 (`prefix.ves` guards `shift > 56`), so the
semantics are unchanged. The result is recorded under "Port checks" below.
The Kinetic `util::shr_u64` helpers are on the unmerged Kinetic PR #1078;
they were not rewritten here.

## Part 2: a resuming handler around a loop hangs natively

**Repro** (port-p3b-dyn-effects-closures.md "Gaps found"):

```
handle Net in { for i in 0..3 { Net::emit(1); } } with {
    return x => 7,
    emit(v, k) => k(v),
}
```

The evaluator returned 7; the JIT (optimize on and off) and an executable
never finished (killed after 20 s).

**Cause.** The specializing backend (`src/native.rs`) inlines a clause at
each operation and resumes by jumping back into the handled body
(`invoke_continuation`). Before the jump it saved the handler's return
selector in one stack slot per operation site and set the selector to that
site's return point; when the handled body finished, the dispatcher went to
that point, which restored the saved selector and returned through the
dispatcher again. In a loop the same site runs again before the first
resumption returns: the second visit saved the selector the first visit had
set (its own return point) over the outer one, so after unwinding one level
the dispatcher returned to the same point forever.

**Fix.**

- A clause whose resumption is its result (`emit(v, k) => k(..)`, also as
  the value of every `if`/`match` arm that ends in it) needs nothing after
  the resumption returns. It now resumes with the clause's own return
  selector: nothing is saved, the rest of the `handle` returns straight to
  whoever the clause returns to, and the operation can run any number of
  times. The check is on the clause's Place IR: the `Op::Indirect` is the
  last instruction of its block and its value reaches `Return` unchanged
  (directly or through join blocks that only pass their parameter on), and
  the clause has no sequence, replacement, temporary or box drops.
- A clause with work after its resumption (`emit(v, k) => k(v) + 1000`)
  still keeps that work in one frame per operation site. The backend tracks
  how many loops of the graphs being emitted enclose the current block
  (`cycle_depth`, from the SCCs of each graph). When such a resumption sits
  at a deeper loop level than the `handle`, it is refused with
  `native_handlers::LOOP_RESUME_REFUSAL`, a typed error, instead of
  returning to the wrong point. Outside a loop, or with the `handle` itself
  inside the loop, it keeps working as before.

**Tests.** `tests/native_handler_loops.rs`: the evaluator (optimize on and
off), the JIT (optimize on and off, with a one-minute hang guard and the
sequence teardown and stray-free counters), x86-64 and AArch64 objects and an
executable (with a hang guard) agree on: the report repro; a loop summing
resumed values; a stateful handler counting every iteration; `break` out of
a `loop` with resumptions in both `if` arms; nested handlers inside an outer
loop (inner `Net`, outer stateful `Log` performed from the inner loop); a
looping callee performing the operation, under handlers installed in a loop;
a `ChunkSeq` built from resumed values in the loop (freed exactly once); and
calls through `inout dyn Transport` in a loop. A non-tail clause in a loop
is refused natively and runs in the evaluator; non-tail clauses outside a
loop still run in every backend.

**Not done (remaining gap).** Non-tail resumptions at an operation inside a
loop need one clause frame per pending resumption (a dynamic stack of return
selectors and clause locals) in the specializing backend, or the handler ABI
backend (`src/native_handlers.rs`, fibers) gaining loops and sequences. Every
clause in the Flux, Chorus and `dyn` tests is tail-resumptive.

## Part 3: Box drops on `Fail` exits and specialized `sink` parameters

**Repros** (port-p3b-loop-owned-boxes.md "Remaining gaps",
port-p3b-dyn-effects-closures.md "Gaps found"), counted with the native box
allocator and the sequence teardown counter, JIT optimize on:

| Program | Before | After |
| --- | --- | --- |
| acyclic `held(7)`: `let b = Box::new(..); risky(x)` fails | 2 boxes allocated, 1 freed | 2 / 2 |
| loop `b = Box::new(P { v: risky(i) })`, fails at `i = 6` | 11 allocated, 10 freed | 11 / 11 |
| box in a handled body whose `Fail` handler aborts, in a loop | 6 allocated, 4 freed | 6 / 6 |
| handler-specialized `take(sink s: ChunkSeq[i64], x) eff [Log]` leaves `s` | 1 sequence left for teardown | 0 |
| same with a sequence, a box and a struct holding a sequence, 3 iterations | 6 sequences left for teardown | 0 |

**Fix: `Fail` exits.** `fip::fail_exit_drops` runs the box drop
elaboration of #1076 (`elaborate_function`, acyclic or the loop fixed point)
and records, at each instruction that can abort the graph (calls, indirect
calls, `?`, handled bodies, sequence and tiled operations, partial
primitives), the boxes the graph owns there: owned temporaries other than
the ones the instruction takes `sink` (the callee drops those) and the
remaining box paths of its owned slots. The native backend keeps that map,
the graph's value registers and its scope on each graph's exit target, and
the instruction it is emitting. A `Fail` that leaves the function
(`fail_exit`) drops what every graph being emitted owns at its current
instruction, the inlined callees and handled bodies included; a `Fail`
handled in the same function drops what the handled body and the graphs
inlined into it own before its clause runs (`raise` never resumes).

**Fix: specialized `sink` parameters.** A function inlined for its handler
effects is a nested graph, and nested graphs did not give their parameters
sequence/string drop slots (a handled body shares its parameters with its
function). An inlined function (a graph not named `$...`) now owns its `sink`
parameters as a separately compiled callee does. `dyn` objects lent `sink`
stay the caller's to free (`LentSinkBlock`). Boxes were already dropped by
the elaboration.

**Tests.** `tests/native_fail_exit_drops.rs` (5) and
`tests/native_specialized_sink.rs` (3): the JIT with optimize on and off frees
every box exactly once and leaves no sequence for teardown, frees no dead
sequence, agrees with the evaluator, and emits x86-64 and AArch64 objects.
They cover an acyclic and a loop function, a box taken `sink` by the failing
callee, boxes in struct fields and a `Box[dyn Trait]`, a handled body inside
a loop, and unconsumed `sink` sequences, boxes and sequence-holding structs.

**Review follow-up (in the part 4 PR).** After a local `Fail` handler
drops what the handled body owns, the body's graphs no longer report an
abort position, so a failure the clause raises again cannot drop those
boxes twice. Three more tests in `tests/native_fail_exit_drops.rs` cover a
re-raising clause and a box moved into a `Log` body that fails before and
after consuming it (each freed exactly once).

## Part 4: drop imports for affine extern handles

**Decision (user, 2026-11-19): drops perform `Ffi[Lib]`.** A drop import
runs at every implicit drop, and code that can drop such a handle needs
the library's capability in its row.

**Syntax.** `affine extern type Probe drop res_release;` in a `.vsys`
names the import that releases a dropped `Probe`. It must be declared
`fn res_release(sink p: Probe) -> ForeignVal[Unit];`; a `linear` type with
`drop` is refused (it is finalized explicitly). The canonical `.vsys`
bytes gain a `drop-imports` section only when some type declares one, so
existing libraries keep their digests.

**Lowering.** The front end (`src/ffi_lowering/drops.rs`) generates two
source functions per droppable type, `$drop$Probe(sink h: Probe)` and
`$drop$Option$Probe(sink o: Option[Probe])`, each calling the import
(parsed at offsets past the module so their expression spans never meet
the source's). After `lower_world`, `drops::insert` places a call of the
right one at each point a graph drops such a value. The points come from
the `Box` drop elaboration of #1076 (`fip::handle_drop_plan`, with the
handle types standing in for `Box[T]` under a thread-local override), so
scope ends, overwrites, discarded results, branch edges, early returns,
loop iterations, `sink` parameters, struct fields and destructured tuple
items all release exactly once. The inserted call is an ordinary foreign
call, so the JIT, objects and the evaluator (which runs Place IR) all run
it without backend changes; native instance discovery adds the drop
functions a graph calls (`drops::drop_callees`).

**Checks.**

- A function that may drop a droppable handle without `Ffi[Lib]` in its
  row is refused (`E0401`, "add Ffi[Res] to its effect row").
- A droppable handle live across an unhandled abortable effect is
  refused (`E0402`), as a linear one is, because an abort would skip the
  drop. (Checked arithmetic counts: use `wrapping_add` or finalize first.)
- Dropping another aggregate that holds such a handle as a whole
  temporary (a discarded `Result[Probe, ForeignInvalid]`) is refused with
  a typed error; bound aggregates release field by field.
- A handle consumed inside a handled body that may abort before consuming
  it is refused (`E0402`, as for a linear value). A struct whose handle
  fields were all closed may cross a checkpoint.
- `let _ = h` releases `h`. A handle discarded by `_` inside a
  destructuring pattern, or moved into a closure (an unused closure would
  not release it), is refused with a typed error.

**Review follow-up (also fixes Boxes).** A temporary used in a later
block (`take(open(1), if flag { 2 } else { 3 })`) was dropped right after
its definition, before the branch, so the call received a released handle.
The same was true of Boxes since #1076: the JIT read freed memory. The
elaboration now leaves a value used outside its block to that use
(`a_box_argument_before_a_branching_argument_is_the_callees`).

The IR checker now lets a plain assignment re-initialize a moved place
(the overwrite drop moves the old handle into the import first). Source
programs never produced that shape before, so no existing diagnostic
changes (`ir_diagnostic_causes`, `two_phase_borrows`,
`runtime_resource_trace`, `bit_builtin_mut` pass).

**Tests.** `tests/ffi_affine_drops.rs` (11): twelve scenarios (scope end,
explicit close, branch, 10-iteration loop, overwrite, discarded result,
`Option` values and a matched handle, struct field and tuple item, early
return, `sink` callee, `let _`, a handle argument before a branching
argument) count opens, explicit closes and drop-import calls.
The JIT (Rust library), the evaluator (Rust stand-in) and, on Linux
x86-64, objects linked with a C library and run as executables all give
the expected value and release count, with opens = closes + releases.
x86-64 and AArch64 objects build for every entry. Diagnostics: the
missing row (and that `in` lends need none), `E0402` (and that closing
first is fine), the whole-aggregate refusal, and the `.vsys` declaration
checks (linear `drop`, wrong mode, wrong result, unknown symbol, plain
affine types still dropping silently). `ffi_handles`, `ffi_lowering` and
`vesper-ffi` tests pass unchanged.

**Manuscript.** The §16.3 "Opaque Extern-Type Handles" bullet now
describes `drop` (flagged in the PR).

**Gaps.**

- A handle dropped inside generic code (`fn keep[T](sink x: T)` with
  `T = Probe`) is not released: the drop plan runs on the generic graph,
  before instantiation.
- Only `H` and `Option[H]` temporaries are released whole; other enums
  holding a handle must be matched first.
- `@fip` functions are not given handle drops.
- Rows with only a tail variable do not satisfy the row check; the
  capability must be named.

## Before and after (parts 1 and 2)

| Repro | Before | After |
| --- | --- | --- |
| `let mut x; x = wrapping_shl_u32(x, 16)`, `vesper build --lib` | E0410 | builds |
| `acc = wrapping_shl_u64(acc, 5)` in a loop, `vesper build --lib` | E0410 | builds |
| handler around `for i in 0..3 { Net::emit(1) }`, JIT on/off | hang (killed at 20 s) | 7 |
| same, executable | hang (killed at 10 s) | exit 7 |
| `total += Net::emit(1)` in the loop, `k(v + 10)`, JIT/exe | hang | 33 (evaluator 33) |
| `k(v) + 1000` in the loop | hang | typed refusal; evaluator 3003 |

These are correctness fixes: the programs did not run before, so there is no
instruction count to compare.

## Port checks

- Prism H3 with the local `shl`/`shr` rewrite (helpers deleted, 22 call
  sites on the bit builtins; patch kept out of the tree):
  `cargo test --locked -p vesper-prism-h3 --features vesper-port` passes
  every test binary (16 of 16, 0 failures), the RFC 9204 / QIFS / golden
  encoder tests included, so the output is identical. (The port's
  `int.ves` comment records that the builtins failed E0410 there before
  part 1; that run is the port lane's, not repeated here.)

## Stage 1 guard

No file under `src/verified_codegen/**`, `lean/**`, `bootstrap/**` or
`packages/vesper-compiler/**` changed, nor `src/ir.rs` or `ir_opt::optimize`
(part 4 calls `ir::lower_world` and `ir::thread_tokens` unchanged; with no
droppable handle in the world the inserted pass is a no-op).
The bootstrap-stage1 workflow was dispatched on the branch; its result is in
the PR.

## Ownership note

The hang was in the specializing backend (`src/native.rs`,
`invoke_continuation` and `ir_graph_at`), not in `src/native_handlers/**`,
which this lane's prompt named. The change there is confined to the handler
resume path (two new `Codegen` fields, the loop-depth bookkeeping in
`ir_graph_at` and the tail check before `invoke_tag`); the refusal text
lives in `src/native_handlers.rs`.
