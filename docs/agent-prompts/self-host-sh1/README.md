# Self-hosting wave SH1: the front end, and the gaps SH0 found

> Plan written 2026-11-19 against `main` at `b35b515` (part of claim #1130). The
> [manuscript](../../manuscript/README.md) is the sole specification. The
> crate-by-crate inventory and later waves are in
> [self-hosting-plan.md](../../maintenance/self-hosting-plan.md). Confirm live
> ownership on the issue board before acting.

SH0 is merged except two pieces: the `vesper-diagnostics` port (part 2 of
#1128) and the census (#1130). Its seven reports give a measured list of what
compiler-shaped Vesper code hits: leaks on early exits, per-byte bounds
checks, byte-by-byte string copies, no way to hand compiler data across the
export ABI, `.vsys` imports that only the root module can use, slow
compilation of large functions, and a handful of small language gaps. SH1
fixes those and ports the front end: diagnostics, the AST, the formatter and
lexical highlighter, lint and forge, and swaps the parser into the hybrid
Stage 2 build. The census of the checker and IR, which plans SH2, runs beside
it.

Every port here keeps Rust as the default build under the standing owner rule.

## Lanes

Claims: lane 1 is #1128 (SH0 lane 6, part 2), lane 15 is #1130 (SH0 lane 8,
second half). Lanes 2-14 are #1145-#1157 (lane n is issue 1143+n).

### Compiler gaps found by SH0

| Lane | Prompt | Claim | Gap | Found by | Starts | Exclusive boundary |
|---|---|---|---|---|---|---|
| 2 | [Drop glue on every exit](02-exit-drops.md) | #1145 | `Fail::raise`, field returns and early returns leak a local struct's owned fields | pilots gaps 1-3 | now | exit drop elaboration in `src/` (listed in the PR), `tests/exit_drops*.rs`, the pilots' workaround hunks (follow-up commit), report |
| 3 | [Byte loops, slice headers, range facts](03-byte-loops.md) | #1146 | per-byte `try_get` keeps its check; slice headers reloaded; guards do not reach loop bodies | pilots 6-7, front v2 4, std data 2 and 5 | now | `src/bounds_hoist.rs` and the native lowering hunks it needs (never `ir_opt::optimize`), `tests/byte_loop_facts*.rs`, report |
| 4 | [Strings and export-binding costs](04-strings-and-bindings.md) | #1147 | byte-by-byte `push_str`, UTF-8 re-validation and a `Vec` per aggregate result in bindings, no host call helper for `Str` in / sequence out | pilots 4-5, hybrid 1, front v2 7 | now | `push_str`/`extend` lowering, the binding generator text in `src/native_export.rs`, `crates/vesper-port-abi/**`, regenerated bindings, `tests/string_copy*.rs`, `tests/binding_*.rs`, report |
| 5 | [Front-end ergonomics](05-ergonomics.md) | #1148 | no or-patterns, no `-> Str` literal, no bound literal view, no `cmp_bytes`, no `checked_add_k`, fmt DepthLimit on long `else if` | front v2 1, 2, 6; std-io 3, 7; hybrid 2 | now | manuscript hunks (flagged), the parser/checker/lowering hunks for each item, the mirror hunks in `packages/vesper-front/src/parse.ves`, the fmt else-if fix, `tests/or_patterns*.rs`, `tests/str_literals*.rs`, `tests/checked_arith*.rs`, report |
| 6 | [Compiler data across the ABI, borrowed Box trees](06-tree-abi.md) | #1149 | `World`/`ast::Type` cannot cross export ABI v3; trees need hand-written encoders; `in e: Expr` with `Box` children cannot be matched | hybrid 3-4, pilots 8, std data 3 | now | the export ABI version bump and binding generator hunks, borrowed-match checker hunks, manuscript hunks (flagged), `tests/tree_abi*.rs`, `tests/borrowed_box_match*.rs`, report |
| 7 | [.vsys in dependency packages, bool, C strings](07-vsys-packages.md) | #1150 | `.vsys` imports only from the root module of a library; no `bool`, no `CStr`; impossible error arm on non-NULL handles | std-io 1, 2, 5; shim 1, 4, 5 | now | `src/ffi_lowering.rs`/`src/packages.rs` hunks, the Loom manifest field, `packages/vesper-std/{fs,process,env}/Loom.toml`, `crates/vesper-std-driver/vesper/**`, `tests/vsys_packages*.rs`, manuscript §16.3 (flagged), report |
| 8 | [Compile time of large functions](08-large-functions.md) | #1151 | 4 min 50 s to JIT a 1,000-operation function with `--no-opt`; shared values rematerialized per store | std data 1, 6 | now | the passes the profile points at, outside the Stage 1 guard (listed in the PR), `tests/large_function_compile*.rs`, report |

### Front-end ports

| Lane | Prompt | Claim | Rust replaced (lines) | Starts | Exclusive boundary |
|---|---|---|--:|---|---|
| 1 | [vesper-diagnostics](01-diagnostics.md) | #1128 | 600 | now | `crates/vesper-diagnostics/**`, its root seed line, shard 7 entry, report |
| 9 | [The AST and type table](09-ast.md) | #1152 | ~5,800 (`vesper-core` AST) + the AST half of the parser | now | new `packages/vesper-ast/**`, `tests/self_host_ast*.rs`, report |
| 10 | [Front end v2: lexer speed and editor mode](10-front-v2-speed.md) | #1153 | (completes `vesper-syntax`, 6,800) | now | `packages/vesper-front/**` (except lane 5's mirror hunks), `tests/self_host_front_v2*.rs`, report |
| 11 | [Formatter and lexical highlighter](11-fmt-highlight.md) | #1154 | 2,300 + lexical part of 4,500 | now | `crates/vesper-fmt/**` (except lane 5's else-if fix), `crates/vesper-highlight/**`, their root seed lines, shard 7 entries, report |
| 12 | [Parser swap into the hybrid build](12-parser-swap.md) | #1155 | (first real swap) | after 6 and 9 | `crates/vesper-hybrid/**` (new `modules/parse`), the parse swap-in point in `crates/vesper-syntax`, `tests/hybrid_*.rs`, one scoreboard row, report |
| 13 | [vesper-lint](13-lint.md) | #1156 | 1,300 | after 9 | `crates/vesper-lint/**`, its root seed line, shard 7 entry, report |
| 14 | [vesper-forge and sugar/macro expansion](14-forge.md) | #1157 | 5,600 | after 9 | `crates/vesper-forge/**`, its root seed line, shard 7 entry, report |

### Planning

| Lane | Prompt | Claim | Starts | Exclusive boundary |
|---|---|---|---|---|
| 15 | [Checker and IR census, SH2 plan](15-census.md) | #1130 | now | `docs/maintenance/self-hosting-plan.md` (SH2 sections), new `docs/agent-prompts/self-host-sh2/**`, `docs/parallel-results/self-host-census.md` |

**Start now:** lanes 1-11 and 15.
**After lanes 6 and 9:** lane 12.
**After lane 9:** lanes 13 and 14.

**Recommended merge order.**
- Lane 5 before lane 10: both edit `packages/vesper-front/src/parse.ves`
  (lane 5 only its mirror hunks). Merge `main` between.
- Lane 5 before lane 11: both edit `crates/vesper-fmt` (lane 5 only the
  else-if fix).
- Lane 4 before lane 6: both change the binding generator in
  `src/native_export.rs`. Merge `main` between and regenerate bindings.
- Lanes 2, 3 and 8 all touch native lowering. Merge `main` right before
  merging each and re-run its differential tests.
- Lanes 1, 11, 13 and 14 each add root `Cargo.toml` seed lines: serialize
  those merges (no merge queue on this account) and regenerate `Cargo.lock`
  with cargo right before merging.
- Lane 15 last among the planning edits to `docs/maintenance/self-hosting-plan.md`
  (lane 12 adds one scoreboard row there).

## Rules for every lane

- **Claim first.** Each lane's issue is its claim. Check open issues and PRs
  for overlapping work before starting; stop and report on overlap. Link the
  claim from the PR.
- **Stage 1 guard.** Never edit `src/verified_codegen/**`, `src/ir.rs`
  (`lower_world` and the Place IR types), `ir_opt::optimize`, `lean/**`,
  `bootstrap/**` or `packages/vesper-compiler/**`. Show every verified
  artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome`
  equal to `bootstrap/stage1.lock` (run the bootstrap-stage1 workflow on your
  branch).
- **Standing owner rule.** Rust stays the default build of the compiler and
  of every port the owner has not already moved, and no Rust code is deleted
  until the Vesper build matches it in both function and performance. Every
  Vesper replacement in this wave is a non-default build.
- **Reproduce first (gap lanes).** Some SH0 repros predate fixes from the P3b
  gaps wave. Re-run each repro on current `main`, drop what already passes,
  and commit what still fails as a test before the fix.
- **Parser mirror rule.** Any change to `crates/vesper-syntax/src/parser.rs`
  or `lexer.rs` is mirrored in `packages/vesper-front` in the same PR;
  `tests/self_host_front_v2.rs` fails otherwise.
- **Semantics first.** The evaluator, the JIT (optimize on and off) and an
  executable agree, with differential tests. Runtime helpers get leak tests,
  in their own test binary.
- **Measure.** ABBA numbers and callgrind instruction counts against the Rust
  code each lane replaces or wraps; wall clock on these hosts moves by up to
  30% between runs. Each report has a "Compiler gaps" section with minimal
  repros, in the format of the SH0 reports.
- **Spec.** Any new std module, builtin or rule gets the smallest manuscript
  change in the same PR, flagged in the PR body.
- **Build.** Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100
  (`cargo +1.100.0 clippy` before pushing). New crates need
  `tools/bazel/sync_build_files.py`; files a test reads from outside its crate
  must be declared in `BUILD.bazel` with anchored globs (a root `**` glob walks
  the `bazel-*` symlinks and times out). Windows and macOS CI use Cargo. Long
  runs go to GitHub CI from the pushed branch. A new port joins Linux port
  shard 7; rebalance when it passes about 35 minutes.
- **Merge.** The owner has authorized merging once the four required checks
  (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`,
  `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments
  are addressed. If `main` is red and your PR fixes it, cancel the other
  in-progress Actions runs and merge the fix first.
- **Report.** `docs/parallel-results/self-host-<lane slug>.md`.

## Spec calls flagged for the owner

- **Or-patterns, `Str` literals, `cmp_bytes`, `checked_add_k` (lane 5).**
  Each is a small manuscript addition; the lane writes and flags them.
- **Tree ABI and lent Rust handles (lane 6).** How compiler data crosses the
  export ABI (lent read-only views with generated accessors, owned trees
  decoded once, and opaque Rust handles read through `.vsys` accessor
  imports) is a new ABI version and a manuscript change.
- **Package `.vsys` files (lane 7).** A Loom manifest field for a package's
  system libraries, and `CStr` / non-NULL handle results in `.vsys`, are
  manuscript §16.3 changes.
- **`ir_opt::optimize` (lane 8).** If shared-value extraction needs the guarded
  optimizer, the lane stops and writes the design instead; changing it is an
  owner call because it moves the Stage 1 lock.

## Not in this wave

- The checker, IR and backends (SH2 onward). Lane 15 plans SH2.
- Deleting any Rust module or seed copy: an owner call per module at parity.
- The LSP and editor tooling (SH5), which lane 10's editor mode prepares for.
