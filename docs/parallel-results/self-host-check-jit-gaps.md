# SH5 lane 6: checker, JIT and wide-scalar gaps

Self-hosting wave SH5, lane 6, claim #1329. Prompt:
[docs/agent-prompts/self-host-sh5/06-check-jit-gaps.md](../agent-prompts/self-host-sh5/06-check-jit-gaps.md)
(SH5 plan, #1344). Base: `main` at `bbda89e`.

Six gaps the SH4 lanes reported they could not fix inside their own
packages. Each fix is its own commit, and each repro is a test in
`tests/sh5_check_gaps.rs`, where the evaluator and the optimized and
unoptimized JIT must return the same value. Rust stays the default build;
no default build changed and no Rust code was deleted.

## What changed

| Gap | Found by | Cause | Fix |
|---|---|---|---|
| `Err(e)?` in a `Fail` function: the JIT refuses the package ("native layout is not yet defined for _") | SH4 lane 15, #1311 | nothing infers the `Ok` type of `Err(E { .. })`, so the checker records `Result[_, E]` and the backends find no layout for `_` | `src/check.rs`, `ExprKind::Try`: an operand whose `Ok` type is still unresolved gets `Unit` (it never succeeds), recorded on the operand with `constrain_literals` |
| A handler clause cannot capture a plain `u32` parameter (E0404 "must be owned copy") | lane 15, #1311 | E0404 asked for `copy` **and** an owned (`sink`) binding | `src/check.rs`: `copy` read through `in` is accepted too. The manuscript (§ multi-invocation state threading) asks only for `copy`; an `in` copy value cannot change while the handler runs. `inout` captures stay E0404 |
| No `ChunkSeq::last` (E1525) | lane 16, #1308 | no such operation | new `SeqOp::Last` on `ChunkSeq[T]` and `Slice[T]`: `Option[T]`, total, receiver unchanged. Checker (E1522 for non-copy elements, as `try_get`), evaluator, native lowering and Lean extraction (`Array.back?`) |
| `p.as_bytes().len()` on a `Str` parameter is E1531 | lane 12, #1299 | a seq method on `p.as_bytes()` evaluated the view as a value, and a string view is second-class outside an `in` argument | `src/seq/check.rs`: `x.as_bytes()` on text `x` is an `in` view receiver, as `s.as_slice()` and `s[a..b]` already were |
| Debug native lowering overflows a 2 MB stack | lane 17, #1319 | see below | `src/native.rs`: `ir_graph` and `expr` continue on a fresh 8 MiB stack segment when less than 512 KiB is left (`stacker::maybe_grow`, constants `LOWERING_RED_ZONE` and `LOWERING_STACK_SEGMENT`) |
| `@vesper/check-data` and `@vesper/check-layout` have no `i128`/`u128` | lanes 6, 8, 9 (#1302, #1301, #1295) | `Known` had no wide names | `Known::is_wide`, `Known::is_scalar` includes it; check-layout lays wide integers out as 16-byte scalars, as `Layout::of` does |

### The stack overflow

The recursion is not in expression nesting. Effect and closure
specialization (`call_function_into`, `invoke_tag`) inline the callee's
graph through `ir_graph`, up to the existing limit of 64 levels, and a debug
build of the compiler spends about 75 KiB of stack per level (measured with
gdb on `abort_through_handlers.ves`):

| Frame | Bytes per level |
|---|---|
| `Codegen::ir_graph_at` | 55,072 |
| `Codegen::call_function_into` | 19,888 |
| closure and `ir_graph` | 880 |

A 2 MB thread overflowed at 26 levels. The depth is already bounded (64
levels, then a typed error), so the fix bounds the stack instead: lowering
grows onto a heap-allocated segment whenever the current one runs low. This
is the same scheme rustc uses for deep recursion (`ensure_sufficient_stack`),
it keeps the lowering's structure (the hooks other lanes are adding to
`src/native.rs` are untouched) and costs one stack-pointer comparison per
call. Rewriting `ir_graph_at` (1,100 lines, one `match` per instruction)
into a worklist would touch every SH4/SH5 lane that edits it.

New dependency: `stacker` 0.1 (with `psm`), in its own root commit.

Corpus scan, every `tests/fixtures` and `examples` program with a `main`,
lowered optimized and unoptimized on a 2 MB thread:

| | Programs that overflow |
|---|---|
| main | 5 (`native_handlers/abort_through_handlers`, `deep_recursion`, `unresumed`, `verified_codegen/region_exhausted`, `regions`) |
| this branch | 0 |

The test lowers those five on a 2 MB thread and requires the same result as
on a 256 MiB thread (the same objects, or the same 64-level refusal, which
three of them reach).

### 128-bit scalars

`tests/common/check_words.rs` encoded no wide type (`encodable` refused
them, so the hybrid build sent them to Rust). The layout encoding now
carries them (`encodable_with(t, true)`); the IR and other checker encodings
keep refusing them, because those Vesper checkers do not handle two-word
integers yet. `tests/hybrid_layout.rs`'s `wide_integers_stay_on_rust`
asserted the old refusal and is replaced by `wide_integers_match_rust`.

No corpus program uses `i128` or `u128`, so the differentials add one world
that does (struct fields, enum and generic payloads, tuples, signatures,
`Option[i128]`):

| Differential | Cases | Differ |
|---|---|---|
| `hybrid_layout` text and words, corpus | unchanged | 0 |
| `hybrid_layout::wide_integers_match_rust` (both forms) | every type of the wide world plus 6 | 0 |
| `self_host_check_data` types and world lookups, corpus | unchanged | 0 |
| `self_host_check_data::wide_integers_match_rust` (types and world tasks) | the wide world | 0 |

Main's `Known::is_integer` gained `i128`/`u128` in #1300 while this lane was open; `is_wide` names the two-word kinds for check-layout.

## Workarounds these fixes make removable

| Fix | Workaround | Where |
|---|---|---|
| `Err(e)?` | `Fail::raise(e)` | SH4 lane 15, wasm backend port (#1311) |
| `in` copy captures | copying the parameter into an owned local before the `handle` | lane 15 (#1311) |
| `last()` | `try_get(len - 1)` | lane 16, eBPF port (#1308) |
| `as_bytes().len()` | `let b = p.as_bytes(); b.len()` | lane 12, export port (#1299) |
| stack growth | the 512 MiB thread in `tests/self_host_backend_checks.rs` | lane 17 (#1319) |
| 128-bit layout | `wide` placeholders and Rust fallbacks for `i128`/`u128` | lanes 6, 8, 9 (#1302, #1301, #1295) |

Those lanes own their packages, so the workarounds are theirs to remove.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`,
`lean/**`, `bootstrap/**` or `packages/vesper-compiler/**` changed, and no
swap point was added. `SeqOp::Last` is a new variant only the Rust backends
match; the verified backend refuses sequences as before. The two checker
relaxations accept programs the verified backend can now see: both repros
build with `vesper build --codegen verified` and exit with the evaluator's
value (`Err(e)?` 3, `in` capture 5). The bootstrap-stage1 workflow result
is in the PR.

## Compiler gaps

1. **`p.as_str().len()`** goes through the string method path, not the
   seq one, and is still E1531 on a `Str` or `String` parameter; `p.len()` works.
   ```vesper
   fn n(in p: Str) -> u64 { p.as_str().len() }   // E1531
   ```
2. **Integer literals above 64 bits in the check-data format.** The
   encoders still carry a sign and a 64-bit magnitude, so worlds holding a
   literal past `u64::MAX` stay on Rust.
3. **The other Vesper checkers** (core typing, fip, IR check) do not
   handle two-word integers; their encodings keep sending `i128`/`u128`
   worlds to Rust.

## What is left

- The SH4 lanes' workaround removals (table above).
- 128-bit integers in the other Vesper checkers (gap 3).
