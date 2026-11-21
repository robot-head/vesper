# Self-hosting SH5 lane 18: Lean extraction in Vesper, part 1

Claim #1341. Base `bbda89e` (main after #1322). Prompt:
[18-lean-extraction.md](../agent-prompts/self-host-sh5/18-lean-extraction.md).
Rust stays the default extractor: nothing here replaces or deletes Rust
code, and no default build changes.

## What changed

Part 1 of the lane ports the **head** of the module `lean::extract_report`
builds: every declaration before the first source function.

- **`packages/vesper-lean-extract`** (new, on `@vesper/check-data` and
  `@vesper/lean-syntax`):
  - `src/prelude.ves`: `prelude` (`HandlerCPS`, its `Monad` instance,
    `liftAction`, `$arithmetic_semantics`, `inI64`),
    `arithmetic_simp_bundle` (the 24-rule `$arith_simps` theorem),
    `divergence` (`VesperDiverge` and its three instances) and
    `fail_models`.
  - `src/head.ves`: `adt_groups` and `lean_nodes::recursive_structs`,
    including petgraph 0.8.3's `tarjan_scc` over a `DiGraphMap` (node and
    edge insertion order, Pearce's index backtracking) so the groups come
    out in Rust's order; inductives, structures, recursive structures and
    their getters; each effect's class; each effect's `lift_E` abbreviation
    and instance.
  - `src/ty.ves`: `ty_with_fip` with no ambient monad (the only form the
    head uses), and `ty_error`, which finds the error Rust reports first in
    Rust's evaluation order (function results before `inout` parameters
    before the effect check before the parameters in reverse).
  - `src/build.ves`: the builder. It writes the `@vesper/lean-syntax`
    printer's prefix word stream directly (tags in
    `crates/vesper-lean-syntax/src/port.rs`), back-patching application
    sizes and counts, instead of building a tree and encoding it. Every
    text is a check-data symbol, so the names table's bytes are the text
    pool. `Ident::new`'s check runs on the UTF-8 bytes, and the error
    message reproduces Rust's `{:?}` escapes for the characters it refuses.
  - `src/input.ves`: the effect section of the input (lane 4's world has
    effect names only).
  - `src/lib.ves`: the entries `extract_head` (text) and
    `extract_head_words` (timing without the printer).
- **`tests/self_host_lean_extract.rs`** (new): the effect-section encoder
  and the differential.
- `BUILD.bazel`: the test's data (lane 4's anchored globs plus
  `crates/vesper-lean-syntax/vesper/**`) and the `long` tag.

## Differential

The head reads no function bodies, so the Rust side is `lean::extract` of
each world with `functions` and `function_results` emptied: that output is
exactly the head (`import Std`, the option, `namespace Vesper` and the
head's declarations). `head_is_a_prefix_of_the_full_module` checks that it
is the first declarations of the full module for every world whose full
extraction succeeds.

`cargo test --test self_host_lean_extract` (debug, Linux x86-64):

| Test | Cases | Differences |
|---|--:|--:|
| `head_matches_rust_on_the_corpus` | 296 worlds, 2,592 ADTs, 400 effects, 23,114 lines | 0 bytes |
| `mutants_match_rust` | 11 mutants, 9 refused by Rust | 0 bytes |
| `unoptimized_jit_agrees` | every 7th world | 0 |
| `head_is_a_prefix_of_the_full_module` | 209 of 296 worlds extract in full | 0 |

No corpus world is refused in the head, so the refusals come from mutants:
`i128` and `u128` fields, a row field, an effectful function field, an ADT
name and a field type name Lean cannot spell (`A«B`, `T\u{202e}`), and three that pair a bad name with a bad field type, since Rust checks names first. Two more
mutants cover type shapes the corpus rarely has: function fields with
`inout`, `in` and `sink` parameters and zero parameters, `Result`
argument order, `TiledSeq`, nested tuples, `_` and integer literal types.

On those 209 worlds the head is 2,418,260 of the 3,489,674 bytes of
extracted Lean (69%): the prelude alone is repeated in every module.

## Speed

`CHECK_HARNESS_SPEED=1 cargo test --release --test self_host_lean_extract
abba -- --ignored --nocapture`, ABBA, best of six per side, all 296 worlds
per run:

| | Rust `lean::extract` (head world) | Vesper `extract_head` (optimized JIT) | Ratio |
|---|--:|--:|--:|
| Corpus head | 180.9 ms | 61.1 ms | 0.34x |

The Vesper time includes decoding the harness input and unpacking the text;
the Rust time includes the empty `lower_world` and contracts passes that
`extract` runs for a world with no functions. The difference is mostly
that Rust builds and clones `Term` trees and Vesper writes the printer's
words once.

## Stage 1 guard

No file under `src/verified_codegen/**`, `src/ir.rs`, `ir_opt`, `lean/**`,
`bootstrap/**` or `packages/vesper-compiler/**` changes, and no Stage 1
re-lock was run: the diff is a new package, a new test, this report and
`BUILD.bazel` data. The bootstrap-stage1 workflow (`bootstrap/run.sh stage1
--reproduce --outcome` against `bootstrap/stage1.lock`) passed on the branch
(run 44887193178, 956db66); the later commits touch only the package, its
test, `BUILD.bazel` data and this report, plus merges of `main`.

## Compiler gaps

1. **`effect` cannot be a method name.** `fx.effect(i)` is E0002 (the
   keyword is reserved even after `.`); renamed to `effect_at`. Repro:
   `impl S { fn effect(in self) -> u64 { 0 } }`.
2. **A shift by a variable count raises `Fail`.** `(x >> shift) & 15` with
   `shift: u32` is E0401 outside a `Fail` row; `wrapping_shr_u32` works.
   SH5 lane 5 (#1328, guarded shifts) owns this.

## What is left

- **Part 2: functions.** `Generator::function` (the structured extractor,
  about 3,000 lines of `src/lean.rs` and `src/seq/lean.rs`) and the CFG
  extractor `lean_ir::extract_function_nodes` over the lowered IR. They
  need the function bodies in the input (an expression encoding like
  check-fip's, plus `world.calls`, `handler_effects` and the IR), and the
  monad form of `ty_with_fip`.
- **Part 3: ordering, fuel companions, contracts.** `order_definitions`,
  the recursive family and `$fuel` companions, `Totality`, `contracts`,
  the proof module and `omitted_executable_definitions`. Then the full
  module differential over the corpus and the sources of `tests/lean*.rs`.
- **Verify and proof commands** (`src/verify_commands.rs`,
  `src/proof_commands.rs`): lane 12's driver (#1335) has not landed, so
  they are left for after it.
- **Hybrid module `lean_extract`**: Rust has no extraction swap point yet;
  it is added with the full port.
