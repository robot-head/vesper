# Lent callbacks keep their parameters in registers (SH4 lane 4)

Claim #1278, follow-up of #1271 (SH3 lane 3, [self-host-closure-modes.md](self-host-closure-modes.md)).
Base: origin/main `e73cfd6`. Native optimize path only: the unoptimized
path, the verified backend and the Stage 1 lock are unchanged.

## What the repro showed

`vesper emit --emit clif` of `flat_callbacks` (the `flat` driver of
`tests/closure_modes_collections.rs`) had six word-sized stack slots, and
the loop stored and reloaded each of them on every lookup. A dump of the
IR graph the native backend lowers showed where they came from. It was
more than `in_param_registers`:

1. **Every name in scope at a closure literal kept a slot.** An
   `Op::Closure` lists every name in scope in its `aliases`, not just the
   names it uses, and `register_locals` treated all of them as captures. So
   one closure literal pushed the function's accumulator, loop index and
   `n` into memory (slots ss0 to ss2), although `closure_tag` copies each
   capture by value when the closure is made.
2. **The specialized callee's parameters kept slots.** `with_ref` is not
   inlined by `ir_inline` (its `eff [e]` row is refused), so the native
   backend specializes it into its caller as a nested graph. The call path
   binds the callee's parameters to fresh variables before it lowers that
   graph, and `ir_graph` treated every such pre-bound name as shared with
   the enclosing graph and gave it a zeroed home slot: the key (ss5) and
   the closure tag (ss4).
3. **`in` arguments of the specialized call kept slots.** `with_ref` has an
   effect row, so `register_locals` homed every place it borrows, including
   the key and the names a lent closure borrows (§4.2), which the call takes
   as `in` arguments.
4. **The element was copied twice on its way to the closure.** `invoke_tag`
   and then `invoke_closure_body` each evaluated the `in v` argument with
   `expr`, which copies an aggregate.
5. **A lent capture's address went through a slot** (ss3): stored when the
   closure is made, loaded when it is called.

## What changed (`src/native.rs`)

- `register_locals` and `in_param_registers`: an `Op::Closure` homes only
  what it moves. Its body never writes the caller's names: an owned
  closure copies its captures, and a lent closure's captures are read-only
  (§4.2). `Op::Handle` keeps the old rule (a handler clause runs with the
  caller's names).
- `register_locals`: a call to a specialized callee (`specialized_function`:
  an effectful or callback-taking function, inlined into the caller) no
  longer homes its `in` place arguments. They are bound as values, nothing
  writes through them, and there is no real call to keep them across. A
  handler that could change such a name while the call runs is a `handle`
  in the same graph or an enclosing one, which already homes every name in
  scope. Other impure calls keep the old rule (see gap 3).
- `bound_params`: the variables the specialized-call path and
  `invoke_closure_body` bind for a nested graph's parameters (and a
  closure's captures) belong to that graph, so `ir_graph` may keep them in
  registers like the graph's own locals. Names a nested control graph shares
  with its enclosing graph are unchanged.
- `in_param_registers`: a function-typed `in` parameter (a lent callback) is
  one tag word and may stay in a register. Its `Sink` read for the call does
  not change it. A nested graph has no incoming argument registers, so the
  "function that calls" rule does not apply to it.
- `invoke_tag` and `invoke_closure_body` lend an `in` aggregate argument by
  address, as `call_function` already did, instead of copying it twice.
- `closure_tag`: a word-sized capture (a scalar, a handle, a function tag, or
  the address a lent aggregate capture borrows) lives in a frontend variable
  (`CaptureHome::Word`) instead of a stack slot. Like the slot, it holds the
  word of the closure made last at that site. Only an owned aggregate copy
  keeps a slot.

After the change the optimized `flat_callbacks` has no word-sized slot. The
16- and 24-byte slots left are the `Option` results and the `as_slice`
views, the same as in the hand-written view form.

## Tests

`tests/callback_registers.rs`:

- `callback_loops_keep_their_words_in_registers`: the `flat_callbacks` shape
  (a `with_ref` loop whose lent closure reads the element, a borrowed
  parameter and the loop index) has no 8-byte stack slot when optimized,
  and keeps its slots unoptimized.
- Differential (evaluator, JIT with the optimizer on and off with the
  sequence double-free counter, x86-64 and AArch64 objects, an executable)
  for closures that snapshot a name changed after they are made,
  `inout` element and scalar callbacks, callbacks forwarded through lent
  parameters into specialized loops, raising callbacks caught by a handler,
  and the lookup loop.

Run locally, all passing: `callback_registers`, `closure_modes`,
`closure_modes_collections`, `metatheory_closures`, `language`, `seq`,
`borrow_paths`, `borrowed_box_match`, `dyn_box`, `dyn_effects`, `dyn_fn`,
and 26 `native_*` suites (`aggregates`, `ascii_in_place`, `batch`,
`block_param_homes`, `bounds_facts`, `byte_classes`, `drop_glue`,
`export_handle_seqs`, `fail_exit_drops`, `fail_status`, `handler_loops`,
`handlers`, `hoist_calls`, `hoist_headers`, `in_params`, `inactive_payload`,
`inline`, `inline_cost`, `inline_inout_slice`, `inline_reach`,
`seq_access`, `seq_drop`, `simd_bytes`, `specialized_sink`, `sroa`,
`write_in_place`). The full suite runs in CI. `cargo +1.100.0 clippy
--all-targets -p vesper` is clean.

## Numbers

Callgrind `Ir` of executables built by `vesper build`, main `e73cfd6`
against this branch, on a 4-vCPU Linux cloud VM.

The collections driver of the closure-modes report: each part of
`tests/closure_modes_collections.rs` 20 times with 2,000 entries, in its
element-view and its callback form.

| Part | Views, main | Views, branch | Callbacks, main | Callbacks, branch | Callback gap, main → branch |
|---|--:|--:|--:|--:|--:|
| `FlatTable` build + 500 `with_mut` + 2,003 `with_ref` | 56,671,675 | 56,631,495 | 58,503,635 | 57,592,781 | +3.23% → +1.70% |
| `CacheBTree`, same | 102,808,408 | 102,668,188 | 104,159,848 | 103,249,128 | +1.31% → +0.57% |
| `IdxMap` `put_with` + `with_mut` + `with_ref` | 83,539,140 | 84,218,714 | 86,979,011 | 84,059,071 | +4.12% → −0.19% |

The `IdxMap` view form got 0.81% slower. Its driver function holds both
forms, and the callback branch's closures used to push every local of the
function into slots. With those names in registers, Cranelift places its
spills in the `put` loop differently (gap 4). The callback form is 3.4%
faster.

Lookups alone (`flat`: a 2,000-entry `FlatTable` built once, 20 × 2,003
lookups):

| Form | Ir | Per lookup over the view form |
|---|--:|--:|
| Hand-written element view | 7,617,492 | |
| The same view returning `Option` and unwrapping it (`with_ref`'s API) | 8,258,572 | +16 |
| Callback, main | 9,299,472 | +42 |
| Callback, branch | 8,538,798 | +23 |

So about 7 instructions per call are left between the callback and the
`Option` form (gap 2), and 16 more are the `Option` itself (gap 1).

**Ports.** Every port package was built with `vesper build --lib --target
x86_64-unknown-linux-gnu` by both compilers: all 40 build units (38
packages plus Warp BBR's and Ignis's concatenated units) give
byte-identical objects. No port uses closure literals or callback
parameters, and their specialized calls take no `in` scalar places. The
SH2 checker ports that use callbacks: none on main has a callback lookup
yet (see the closure-modes report, "Port rewrite"). `vesper-ast`,
`vesper-compiler` and `vesper-cranelift` do not build as libraries with
either compiler.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt`, `lean/**`,
`bootstrap/**` or `packages/vesper-compiler/**` changed, and the verified
pipeline does not reach the native optimize path. The `bootstrap-stage1`
workflow is run on the branch (see the PR).

## Compiler gaps

1. **`Option` results of inlined calls live in memory.** `with_ref` returns
   `Option[R]`: each arm builds the enum in a slot, the join passes its
   address, the caller's `let` copies it, and the `match` reads it back.
   That is 16 instructions per lookup, the same in the hand-written form:

   ```vesper
   let r = match t.vals.try_get(t.position(k)) {
       Option::Some(v) => Option::Some(wrapping_add(weight(in v), probe.n)),
       Option::None => Option::None,
   };
   acc = mix(acc, or5(r));
   ```

   Fix: scalar replacement of small enum values through joins (a tag and
   payload words as block parameters), or a case-of-known-constructor pass
   on the native IR. This is the "Enums and `Option` locals" and "aggregate
   temporaries" item of the aggregate-registers report.
2. **Receiver headers are re-read on every inlined call.** `ir_inline`
   refuses `with_ref` (an `eff [e]` row), so each call specializes its graph
   in the caller's loop, and that graph re-reads `self.vals`, `self.slots`
   and `self.hashes` headers each time. `bounds_hoist::header_hoists` and
   `stable_views` skip any loop or function with an `Op::Closure`,
   `Op::Indirect` or a call whose name contains `::`, so even the caller's
   own reads are not hoisted. About 7 instructions per lookup. Fix: let
   `ir_inline` splice a callee with an inferred-empty effect row and a
   closure-literal argument, and let header hoisting accept lent closures
   (they write nothing) and specialized method calls with no effect.
3. **`in` arguments of real impure calls keep homes.** Dropping that rule for
   every impure call (not only inlined ones) measured: Grammar Protobuf
   window −7.5%, DER −13.2%; Aegis `snp` −4.7%, `tdx` −8.2%, `sgx` −6.6%;
   but Aegis `policy` +0.9%, Silicon `dtb-parse` +0.3% and `dtb-platform`
   +0.2%, because more scalars then live across real calls. It needs a cost
   rule (for example the loop-weighted count `split_locals` uses) before it
   can ship.
4. **Spill placement after slots go away.** Taking names out of slots can
   move Cranelift's spills into a hot loop (`IdxMap` view form +0.81%). A
   cost rule for which scalars stay in registers across calls would cover
   this too.

## What is left

The acceptance bar (within 0.5% of the hand-written view match) is met for
`IdxMap` and nearly for `CacheBTree` (+0.57%), not for `FlatTable` (+1.70%).
The rest of the gap is gaps 1 and 2, which are outside this lane's paths
(`native_aggregates`, `ir_inline` and `bounds_hoist`). Gap 3 is the largest
port speedup found here.
