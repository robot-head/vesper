# Self-hosting SH2 lane 14: tensor types in Vesper

> The [manuscript](../manuscript/README.md) is the sole specification.
> Confirm live ownership on the issue board before acting.

Claim: #1186. Base: `main` at `e43a56f`. Lane prompt:
[`docs/agent-prompts/self-host-sh2/14-tensor-types.md`](../agent-prompts/self-host-sh2/14-tensor-types.md).
Rust stays the default checker, and no Rust code was changed or deleted.

This is part 1 of the lane: the checker half of `src/tensor_types` except
elaboration. Follow-ups are listed under "What is left".

## Summary

- **`@vesper/check-tensor-types`** (`packages/vesper-check-tensor-types`):
  `infer.rs`, `dim.rs` and `types.rs` in Vesper. That covers shape kinds and
  their fixed point, symbolic dimensions (products of powers with the
  overflow, zero and factor limits), dtypes, substitution and unification,
  body inference for every expression kind and tensor or KV pool operation,
  signature obligations, and the 18-round `analyze` loop with its
  convergence and obligation limits. `wire.rs` stays in Rust, as the prompt
  says. The harness passes its results in the input.
- **Design** follows the census. Dimensions and types are hash-consed `u32`
  ids, names are interned `@vesper/check-data` symbols, and tables are flat.
  The substitution is an association list read newest first, with mark and
  undo where Rust clones the map.
- **Differential**: 0 differences on every source tried, with the optimizer
  on and off. All eleven tensor diagnostics (E0761-E0771) fire.
- **Speed**: the port takes 0.28x of Rust's time on the rejected fixtures
  and 0.49x on the corpus.

## What changed

| Area | Files |
| --- | --- |
| Package | `packages/vesper-check-tensor-types/{Loom.toml,README.md,src/*.ves}` |
| Differential and ABBA test | `tests/self_host_check_tensor_types.rs` (includes `tests/common/check_harness.rs`) |
| Bazel data and timeout for the test | `BUILD.bazel` |
| Scoreboard row | `docs/maintenance/self-hosting-plan.md` |
| Report | `docs/parallel-results/self-host-check-tensor-types.md` |

No `World` fields or `@vesper/check-data` decoder sections were added,
because the pass runs on the `@vesper/ast` tree lowered from source, not on
the checker world. No port-harness shard entry was added either: like
`@vesper/check-data`, the package has no `vesper build --lib` object yet.

## The differential

The test compares one report per source. The report holds `inactive` or
the diagnostics (code, span, message, help and labels), every ADT's shape
kinds, and every signature (generics with kinds, parameters with passing
modes, result, KV pool, obligations). Rust's side is `tensor_types::analyze`
on the Rust parser's module.

| Set | Sources | Active | Differing |
| --- | --: | --: | --: |
| Fixtures (`tests/fixtures/tensor_types`, rejects) and 11 checker test sources | 57 | 55 | 0 |
| The same, optimizer off | 57 | 55 | 0 |
| Seeded mutants of the active sources (40 each, `TENSOR_MUTANTS`) | 1,880 | 1,875 | 0 |
| Corpus (`tests/fixtures`, `examples`, `packages`, `crates`) | 852 | 47 | 0 |

How often each code fired over the mutants: E0761 749, E0762 72, E0763 417,
E0764 370, E0765 60, E0766 61, E0767 144, E0768 115, E0769 24, E0770 118,
E0771 26. The test asserts that every code fires.

The test takes about 8.5 minutes in a debug build, and compiling the
package with the JIT takes most of that. Bazel gives it the `eternal`
timeout.

## Numbers

ABBA timing, `port-compiler` profile, Linux x86-64, optimizer on. Each side
parses and then checks. Rust uses its parser and `analyze`. Vesper uses
`@vesper/ast` lowering and the port. The Vesper side leaves out building
its input, which is copying the source and running the Rust wire checker.

| Workload | Rust | Vesper | Vesper / Rust |
| --- | --: | --: | --: |
| 40 rejected fixtures x 20, parse + check | 83.2 ms | 22.9 ms | 0.28x |
| 852 corpus sources, parse + activity test | 384.8 ms | 187.1 ms | 0.49x |

Callgrind was not run. The hybrid swap (below) is where the whole-corpus
scoreboard numbers come from.

## Stage 1 guard

No guarded path was touched: nothing under `src/verified_codegen/**`,
`src/ir.rs`, `ir_opt::optimize`, `lean/**`, `bootstrap/**` or
`packages/vesper-compiler/**`. The bootstrap-stage1 workflow run on this
branch is linked from the PR.

## Compiler gaps

1. **The native JIT refuses more than 1,024 monomorphized functions.**
   (`src/native.rs`, "native monomorphization exceeds 1024 function
   instances", and the same limit in `src/native_export.rs`). This package
   plus `@vesper/ast` lowering came to 1,054. Writing about 300 one-line
   helper calls inline brought it to 999, so the package now has little
   headroom. Repro: any package whose reachable functions pass 1,024, for
   example 1,025 `fn fN() -> u64 { N }` called from `main`. #1231 has since
   raised the limit to 4,096 on `main`, so the inlining is no longer needed,
   but the combined checker ports should still be measured against it.
2. **A `const` cannot be imported across modules** (check-data gap 1).
   Each module repeats the type kind and code constants.
3. **`eff` is a keyword** until lane 3 (#1175) lands, so a handler local
   that Rust calls `eff` is `hfx` here.
4. **A method on the whole struct cannot lend one of its fields too.**
   `put_name(in st, inout st.err, x)` is E0301, so the port calls
   `st.names.write(x, inout st.err)` directly. Element borrows (lane 1,
   #1173) were not needed: tables are read through `try_get`.

## What is left

- **Elaboration** (`elaborate.rs`, about 470 lines). Its map is
  `pub(super)` in `src/tensor_types`, so no test can compare against it.
  Part 2 adds the swap point there, ports elaboration, and swaps the pass
  into the hybrid build (`crates/vesper-hybrid`, module `tensor_types`) as
  a non-default module, with the scoreboard row's whole-corpus numbers.
- An ABI v4 input instead of the byte encoding, and a `vesper build --lib`
  object that joins Linux port shard 7.
- Callgrind instruction counts.
