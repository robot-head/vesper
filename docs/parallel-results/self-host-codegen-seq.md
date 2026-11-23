# Self-hosting SH4 lane 9: native lowering of sequences, bytes and strings

Claim #1283. Stacks on #1259 (SH3 lane 1 part 1, `@vesper/place-ir`), which
stacks on #1232. Prompt: `docs/agent-prompts/self-host-sh4/09-codegen-seq.md`.
Rust stays the default compiler. Nothing here changes a default build,
edits the Rust lowering or deletes Rust code.

Base: #1259 head `8ac0f94`; origin/main was `e73cfd6`.

## Part 1: the decisions (this PR)

Lane 1 (#1275) has not landed its lent Cranelift builders yet, so this part
ports only what native lowering decides before it emits anything. The
prompt allows that before lane 1's first PR.

- `packages/vesper-codegen-seq` (`@vesper/codegen-seq`), over
  `@vesper/place-ir` tables, `@vesper/check-data` types and world, and
  `@vesper/check-layout`:
  - `drop_plan`: `fip::seq_drop_plan`, the function-local `ChunkSeq`,
    `String` and owning-aggregate slots native code frees itself, and what
    each frees (`Seq`, `Str`, `Owned`, `Fields`), with every refusal rule
    (sink reads of partial or string slots, field and compound writes,
    borrow bindings, `Box` patterns, closure aliases and moves, handler
    moves and string aliases, match-arm bindings, `@fip` functions,
    external captures, non-sink parameters). It includes `needs_drop`,
    `may_need_drop` and `seq::holds_sequence`.
  - `borrowed_scrutinees`: `fip::borrowed_scrutinees`, to a fixed point.
  - `seq_rows`: for every `Op::Seq`, the facts `Codegen::seq_op` and the
    `Op::Seq` dispatch in src/native.rs read off the operation: `SeqOp::name`,
    `has_receiver`, `receiver_pass`, `mutates`, `fallible`,
    `place_primitive`, `byte_scan`. It also gives the defaulted element type
    (of the receiver, or of the result when there is no receiver) and its
    packed stride (`seq::elem_bytes`: 1 for `u8`/`bool`, 4 for `u32`, else
    the layout size).
  - The entry `codegen_seq_decisions` writes one text block per function.
- `tests/self_host_codegen_seq.rs`: the differential. Each corpus world is
  lowered as the native backend lowers it, optimize on (lowering, `ir_opt`,
  inliner, drop elaboration, bounds hoisting) and off (`fip_reuse_pass`,
  drop elaboration). The Rust side calls the public `fip::seq_drop_plan`,
  `fip::borrowed_scrutinees`, the `SeqOp` methods and `seq::elem_bytes`.
  The input is place-ir's plus one `fip` flag per function, because
  check-data's world does not carry `world.functions`. `CODEGEN_SEQ_STEP=n`
  runs every n-th world.
- BUILD.bazel: data and `long` size for the new test.

## Numbers

Full corpus, optimized test build (`cargo test --release`), one cloud run:

| Check | Result |
|---|---|
| Programs (292 worlds x optimize on/off) | 584 |
| Programs that differ | **0** |
| Not encodable | 0 |
| Decision lines compared | 201,664 |
| Planned drops | 6,770 |
| Sequence operations | 127,322 |
| Rust decisions | 482 ms |
| Vesper decisions, input decode included | 816 ms (1.69x) |

The Vesper time includes reading names, types, world and program from
words, which Rust does not pay for. SH4 lane 2 (#1276) removes that cost.
Emission is the parity bar, and this part emits nothing, so no callgrind
numbers are reported yet.

## Stage 1 guard

The PR adds only a package, a test, BUILD.bazel entries and this report.
`src/verified_codegen/**`, `src/ir.rs`, `ir_opt::optimize`, `lean/**`,
`bootstrap/**`, `packages/vesper-compiler/**` and every Rust lowering file are
untouched. No hook is installed, so the default compiler's output does not
change.

## Decisions

- **The op's facts come from its `Debug` text.** place-ir keeps `SeqOp` as a
  Rust payload and gives Vesper only its `Debug` label (#1259's design). The
  package decodes the variant name, the `Str(..)`/`Hoisted(..)` inner variant
  and the `total:`/`write:`/`store:`/`masked:`/`bytes:`/`big:` fields from
  that label. That is enough for every decision here and is tested on all
  127,322 operations. Emission also needs `BoundsGuard`'s `IndexForm` and
  `Vector`'s `Shape`. Those come from the same label, or from a structured
  pool kind if place-ir adds one.
- **`fused_reads` waits for emission.** `seq_native::fused_reads` (which
  `try_get` results stay in registers) is private to src/native.rs, and port
  lanes may not edit the Rust lowering. It is ported with emission, where the
  CLIF differential checks it.
- **Element types are the template's.** Native code substitutes an
  instance's generic arguments before it computes a stride. The decisions
  here run on the generic templates, as `seq_drop_plan` does. The stride of a
  generic element is `-` on both sides. Strides per instance come with
  emission, through lane 1's hook arguments.

## Compiler gaps

1. **`handle` is a keyword.** `let handle = ...` fails to parse (E0002,
   "expected pattern", then a long cascade). This is by design (effect
   handlers), but the cascade hit the 128-diagnostic limit before any message
   named the keyword. Repro: `fn f() -> bool { let handle = true; handle }`.
   Workaround: renamed to `is_handle`.
2. **check-data's `Known::is_scalar` predates `i128`/`u128`.**
   `numeric::is_scalar_name` accepts both since #1219. The package checks
   them itself (`Dec::is_scalar_name`). The fix belongs in
   `@vesper/check-data` (`Known::is_integer`).
3. **check-layout has no 16-byte `i128`/`u128` layout.** `Layout::of` gives
   them two words (#1174), but `@vesper/check-layout`'s `layout_of` refuses
   them. The package gives a `ChunkSeq[i128]`/`ChunkSeq[u128]` element a
   16-byte stride itself, and `wide_element_strides_match_rust` tests that
   case, since no corpus file holds such a sequence. A sequence of an
   aggregate that holds a wide integer still gets `stride=-` here, where Rust
   gives its size. The fix belongs in `@vesper/check-layout` (SH2 lane 5).
   No corpus program has such an element yet. Repro:
   `struct P { a: u128 } fn f() -> u64 { let s: ChunkSeq[P] = ChunkSeq::new[P](); s.len() }`.

## Part 2a: the AOT sequence runtime (stacks on #1309)

Lane 1's swap point (#1309) is in. Its `INSTRUCTION` request gives a hook
only the operand values, but sequence emission reads `Codegen` state
(header caches, fused reads, write-back places, fail exits). So, with the
owner's approval, this lane adds its own helper requests to the `seq`
family, the same way lane 1's pilot did for `aggregate`. The first one is
the runtime every object with a sequence defines.

- `src/codegen_swap.rs`: `pub mod seq` with `RUNTIME = 1` and
  `runtime_args` (the Windows flag, then `INLINE_BYTES`,
  `NEW_INLINE_BYTES`, `BUFFER_PREFIX`, `FAULT_STATUS`, `HEADER_BYTES`, so
  the two emitters share one source for the constants).
- `src/seq/native.rs` (flagged dispatch hunk): `seq_define_runtime`
  replaces the two `aot_runtime::define` calls. With no hook, it is the
  old call. With a hook, it first checks `defined` and the 64-bit pointer
  type, then requests `RUNTIME`. A declined request runs the Rust emitter.
- `packages/vesper-codegen-seq/emit` (`@vesper/codegen-seq-emit`):
  `seq_hook`, which defines the live counter, the empty header, the fault
  messages and the six functions into the lent module with builders of its
  own, in the Rust emitter's order. It declines every other request.
  It is a separate package because it needs `Ffi` and the decisions
  package stays pure.
- `crates/vesper-codegen-swap-test/tests/self_host_codegen_seq.rs`: the
  corpus worlds whose host object defines the runtime, compared through
  lane 1's harness (CLIF per function, object bytes, sampled JIT runs,
  optimize on and off), plus Windows, AArch64 Linux and macOS objects, plus
  a check that a hook which defines nothing is caught.

Full corpus, `cargo test --release -p vesper-codegen-swap-test --test
self_host_codegen_seq`, one cloud run (82 s for the three tests):

| Check | Result |
|---|---|
| Corpus worlds whose host object defines the runtime | 8 |
| Host builds compared (optimize on and off) | 16, 112 functions |
| CLIF, object and run differences | **0, 0, 0** (4 JIT runs) |
| Runtimes defined from Vesper | 16 of 16 builds |
| Other requests (`INSTRUCTION` for `Op::Seq`), declined | 491 |
| Windows, AArch64 Linux and macOS objects | 6, **0 differ** |
| A hook that defines nothing | caught |

With no hook installed, `seq_define_runtime` is the old
`aot_runtime::define` call after one relaxed atomic load, so the default
compiler's objects do not change. The runtime is small next to a whole
compile, so a callgrind number waits for part 3, where the hybrid build
installs the hook.

Under Bazel, where "Test all targets" shares one hour with the whole
suite, both seq tests sample: the decisions test compares every twelfth
program, and the runtime test compares the first four runtime worlds,
every fourth corpus world. Cargo runs compare the whole corpus.
`CODEGEN_SEQ_STEP=n` overrides the stride in either test.

Compiler gaps found here:

4. **`copy` is a keyword.** `let copy = ...` fails with E0002 ("expected
   pattern"), and the message does not say why. Repro:
   `fn f() -> u64 { let copy = 1; copy }`. Workaround: renamed.
5. **A `.vsys` extern type cannot be imported** (lane 1's gap 1, E0111):
   the package names `Builder` and `Module` without a `use`.


## Part 2b: `seq`, `str`, `native_bytes`, hoisted, fused, and drop emission

Extends `codegen_swap::seq` and `@vesper/codegen-seq-emit` to emit all sequence, string, byte/SIMD, bounds-hoisted, fused `try_get`/condition, and drop-glue operations through `@vesper/cranelift`, keeping every Rust fallback intact behind `seq_hook_active()`:

- `crates/vesper-cranelift-sys` and `@vesper/cranelift`: added `vcl_declare_anonymous_data`, `vcl_call_memset`, `vcl_call_memmove`, `vcl_isa_triple_arch`, `vcl_has_x86_pshufb_lowering`, `CanMove` (`MemFlags::with_can_move`), `sload8`/`uload16`/`sload16`/`sload32`/`istore16`, and SIMD/overflow wrappers (`ins_umul_overflow`, `ins_swizzle`, `ins_vany_true`, `ins_vall_true`, `ins_vhigh_bits`).
- `src/codegen_swap.rs` (`pub mod seq`): added ops `2..=13` (`STRING_LITERAL`, `STR_OP`, `STR_COMPARE`, `BOUNDS_GUARD`, `HOISTED_ACCESS`, `INVARIANT_WORD`, `EMPTY_HEADER`, `SEQ_OP`, `BYTE_OP`, `FUSED_TRY_GET`, `FUSED_CONDITION`, `DROP_OP`) and sub-op/node constants (`STR_*`, `HOIST_*`, `SEQ_*` including `SEQ_RESERVE_GROW`, `BYTE_*`, `DROP_*`, `DROP_NODE_*`).
- `packages/vesper-codegen-seq/emit/src/{lib,common,strings,hoist,bytes,drops,seq_ops}.ves`: full Vesper emission for ops `1..=13`.
- `src/strings/native.rs`, `src/bounds_hoist/native.rs`, `src/seq/native_bytes.rs`, `src/seq/native.rs`: hooked dispatch hunks with Rule 8 Rust fallbacks preserved.
- `crates/vesper-codegen-swap-test/tests/self_host_codegen_seq.rs`: added `all_seq_ops_match_the_rust_emitter` (corpus + targeted `seq_core_ops`, `seq_strings`, `seq_bytes_simd`, `seq_hoist_and_drops` worlds across optimize on/off, asserting `op1..=op13 > 0`) and `a_wrong_seq_emit_is_caught`.

Local verification (`self_host_codegen_seq`):

| Check | Result |
|---|---|
| `all_seq_ops_match_the_rust_emitter` (24 worlds, 144 functions, 12 runs) | **0** CLIF, **0** object, **0** run, **0** one-sided diffs |
| `codegen_swap::seq` ops exercised (`op1..=op13`) | `op1=24 op2=123 op3=242 op4=76 op5=2 op6=11 op7=2 op8=54 op9=802 op10=126 op11=71 op12=61 op13=618` |
| `the_runtime_matches_on_other_targets` (Windows, AArch64 Linux/macOS) | 6 objects, **0 differ** |
| `a_missing_runtime_is_caught` & `a_wrong_seq_emit_is_caught` | passed |

## Left

- The runtime is also defined from src/regions/native.rs (lane 11's file)
  and the export glue (src/native_export/glue.rs). Those calls still use the
  Rust emitter. They are left to those lanes, or to a follow-up once the
  hook is installed in the hybrid build.
- Part 3: the hybrid module `codegen_seq` with its `seq` hook install, the
  full suite through the swap, ABBA and callgrind numbers for emission and the
  whole compile, and the SH4 scoreboard row.
