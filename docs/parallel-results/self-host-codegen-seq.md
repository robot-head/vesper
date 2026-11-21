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

## Left

- Part 2, after lane 1's first PR: emission of `seq_op`, `str_op`, the byte
  operations (src/seq/native_bytes.rs), hoisted accesses, fused reads, drop
  glue and the AOT sequence runtime emitter (src/seq/aot_runtime.rs), all
  through `@vesper/cranelift`. Done means 0 CLIF differences and
  byte-identical objects, runtime included.
- Part 3: the hybrid module `codegen_seq` with its `seq` hook install, the
  full suite through the swap, ABBA and callgrind numbers for emission and the
  whole compile, and the SH4 scoreboard row.
