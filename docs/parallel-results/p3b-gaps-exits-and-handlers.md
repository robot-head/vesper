# P3b gaps lane 10: drops on every exit and handler fixes

Claim #1095. Prompt:
[10-exits-and-handlers.md](../agent-prompts/p3b-gaps/10-exits-and-handlers.md).
Base `9305b24` (origin/main with the wave plan #1098), merged with
origin/main `31f81b9` (#1076) before merging.

This report covers parts 1 and 2 (PR #1105). Parts 3 (Box drops on `Fail`
exits, specialized `sink` leaks, after #1076) and 4 (affine extern-type drop
imports, after #1077) follow in their own PRs and extend this report.

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

## Before and after

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
`packages/vesper-compiler/**` changed, nor `src/ir.rs` or `ir_opt::optimize`.
The bootstrap-stage1 workflow was dispatched on the branch; its result is in
the PR.

## Ownership note

The hang was in the specializing backend (`src/native.rs`,
`invoke_continuation` and `ir_graph_at`), not in `src/native_handlers/**`,
which this lane's prompt named. The change there is confined to the handler
resume path (two new `Codegen` fields, the loop-depth bookkeeping in
`ir_graph_at` and the tail check before `invoke_tag`); the refusal text
lives in `src/native_handlers.rs`.
