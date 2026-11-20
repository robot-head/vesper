# Self-hosting SH1 lane 2: drop glue on every exit

Claim #1145. Base `e29e63d` (origin/main). PR #1161.

The toolchain pilots (docs/parallel-results/self-host-toolchain-pilots.md)
found three leaks that every compiler module would hit. All three still
leaked on main after P3b gaps lane 10 (#1095), so all three stayed in scope.

## Repros on main

Probed in the JIT (optimize on and off), counting sequence and string
buffers left for teardown:

| Shape | Leaked on main |
| --- | --- |
| Gap 1: `Fail::raise` with a live `P { out: String, err }` | 1 buffer |
| Gap 2: return `r.found` of `R { found, scope }` (strings or seqs) | 1 buffer |
| Gap 3: `if c { return 0; }` after a scratch `P` | 1 buffer |
| A plain `String` local on raise or early return | 0 (already fixed) |
| `f(1).len()` on an unbound `String` call result | 1 buffer (found here) |
| A handled body's local when its `Fail` handler aborts it | 1 buffer (found here) |

The repros are committed as tests before the fix (commit `1d37f49`).

## What changed

Files: `src/fip.rs` (`seq_drop_plan`, `temporary_drops`),
`src/seq/native.rs` (`seq_field_offsets`, `seq_drop_field_moved`,
`seq_drop_slot_kind`), `src/native.rs` (sink `Read` lowering, `Fail` clause
emission in `perform`).

1. **Structs that own strings are drop slots.** `seq_drop_plan` planned a
   `let`-bound struct as `SeqDrop::Fields` only when it held a sequence. It
   now also plans structs that own strings (`needs_drop`), still only when
   every owning leaf sits in a product field. Every exit that already freed
   `Fields` slots (return, `return` from a nested block, `Fail` exits, `?`)
   now frees them. This fixes gaps 1 and 3.
2. **Partial moves.** A sink read of an owning field (`r.found`) used to
   refuse the whole slot. For a `Fields` or `Owned` (sink parameter) slot,
   the read now copies the field out and zeroes its bytes in the slot's own
   storage, so the slot's drop frees only what is still there. The zeroed
   bytes are the drop flag, so the analysis stays flow-sensitive with no new
   state: a field moved on one path is still dropped on the other. This fixes
   gap 2, nested fields included.
3. **Handler aborts.** A lexical `Fail` handler that ends its body now frees
   the body graphs' string and sequence slots (as it already did for boxes),
   and emits its clause with those slots taken out, so a clause that raises
   again does not free them twice.
4. **Unbound strings read into a scalar.** `temporary_drops` counted only
   call arguments as borrows. A `Seq`/`Str` operation whose result is a
   scalar or `bool` (`f().len()`, `== `) now borrows its `in` operands too,
   so the temporary is freed after it. An operation whose result could be a
   view (`as_str`, slicing) is not counted.

The evaluator owns its values in Rust and never leaked; the tests check
that the JIT agrees with it.

## Tests

`tests/exit_drops.rs` (its own binary: the counters are per thread, but the
lane asked for one). Each program runs in the JIT with optimize on and off,
asserts no buffer left for teardown, no stray free, no box leak, agreement
with the evaluator, and builds aarch64 and x86_64 objects:

- gap 1 (`raise_drops_a_live_struct_string_field`), gap 2
  (`returning_a_field_drops_the_other_fields`), gap 3
  (`early_return_drops_a_scratch_struct`);
- `?` on `Result` with a struct owning a string and a sequence;
- a `Fail` handler aborting a body that owns a struct, in a loop;
- a clause that raises again (freed once);
- an early return in a loop; a nested field return; a field moved on one
  path only; a sink parameter with a moved field; an unbound `String`
  result.

Also run locally and green: ffi_affine_drops, fip, native_aggregates,
native_block_param_homes, native_drop_glue, native_empty_seq,
native_fail_exit_drops, native_fail_status, native_handler_loops,
native_handlers, native_inline, native_inline_reach, native_seq_access,
native_seq_drop, native_seq_runtime, native_specialized_sink, native_sroa,
native_write_in_place, seq, strings.

## Workarounds removed

`crates/vesper-lean-syntax/vesper/src/lib.ves` (separate commit):
`render_term` and `render_module` raise without `p.out = String::new()`;
`free_references` and `references` return `r.found`; `app` builds its
scratch printer inline and returns early (the `args_error` helper is gone).
`crates/vesper-span/vesper/**` had no workaround for gaps 1-3 (its gaps were
4, 5 and 7), so it is unchanged.

`crates/vesper-lean-syntax/port/test-port.sh` passes (crate tests on the
port and the seed, differential, all three divergence checks), and
`tests/port_memory.rs` (`port_calls_do_not_leak`, which covers the error
paths) passes with the workarounds gone.

Callgrind, 5 batches, `scripts/bench.sh` with `CALLGRIND=1`, both on this
branch's compiler:

| Workload | With workarounds | Without | Change |
| --- | --- | --- | --- |
| term | 419,266,818 | 419,495,948 | +0.05% |
| module | 331,878,426 | 332,054,821 | +0.05% |
| free | 293,498,872 | 293,498,687 | -0.0001% |

The small rise on term and module is the struct drop now emitted at each
exit of `render_*` and of the inlined scratch printer. ABBA still passes the
2x gate on term and module (0.39x and 0.40x of Rust); free stays about 2x
(Inconclusive gate on this noisy run, Pass on the run before).

## Stage 1 guard

No file under the guarded paths changed (src/verified_codegen/**,
`lower_world` and the Place IR types in src/ir.rs, `ir_opt::optimize`,
lean/**, bootstrap/**, packages/vesper-compiler/**). The bootstrap-stage1
workflow was dispatched on the branch; see the PR for its result.

## Compiler gaps

1. **Re-initializing a moved field is refused.** `let n = take(r.a);
   r.a = String::new(); r.a.len()` is E0303 ("use of moved place 'r.a'") at
   the later read, so the moved-then-replaced path could not be tested.
2. **Non-`Fail` handler aborts.** A clause of another effect that never
   calls its continuation ends the body too, but its body slots are not
   freed there (only `Fail` clauses are known to abort).
3. **Enum payloads.** A struct whose owning field is an enum payload (for
   example `Option[String]`) is still not a `Fields` slot.

## What is left

Gaps 4-7 of the pilots belong to other lanes. The three above are open.
