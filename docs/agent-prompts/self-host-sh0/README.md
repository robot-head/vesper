# Self-hosting wave SH0: groundwork for a Vesper compiler

> Plan written 2026-11-19 against `main` at `9e801e7` (claim #1122). The
> [manuscript](../../manuscript/README.md) is the sole specification. The
> crate-by-crate inventory, gap list and later waves are in
> [self-hosting-plan.md](../../maintenance/self-hosting-plan.md). Confirm live
> ownership on the issue board before acting.

The compiler is about 157k lines of Rust in `src/` plus about 42k in the
toolchain crates it links. The Vesper compiler package is an 8.4k-line Stage 1
front end that the Stage 1 lock freezes. Before any compiler module can move,
four things are missing: a build that can link a Vesper object into the
compiler without a cycle, std I/O, a way to drive Cranelift from Vesper, and the
data structures compilers are made of. This wave adds those. It also pilots the
port pattern on the three smallest toolchain crates, writes a real front end
beside the frozen one, and measures the gaps on compiler code so SH1 and SH2 can
be planned from numbers.

**Owner decision, 2026-11-19:** Stage 2 keeps Cranelift as a vetted external
library behind a C-ABI shim (lane 4). A Vesper-native backend is wave SH7.

## Lanes

Claims are #1123-#1130 (lane n is issue 1122+n).

| Lane | Prompt | Claim | Gap | Starts | Exclusive boundary |
|---|---|---|---|---|---|
| 1 | [Hybrid Stage 2 build](01-hybrid-build.md) | #1123 | the compiler cannot link a Vesper object of its own code without a cycle | now | new `crates/vesper-hybrid/**`, new `tools/bazel/vesper_hybrid.bzl`, `tests/hybrid_*.rs`, the root `Cargo.toml` member line (root patch rule), `docs/parallel-results/self-host-hybrid-build.md` |
| 2 | [Tool subcommands out of the compiler](02-tool-split.md) | #1124 | 8 of the 18 seed copies are linked only for tool subcommands | now | the moved `src/*_commands.rs` files and their `src/main.rs`/`src/lib.rs` hunks, new `crates/vesper-tools/**`, the seed lines of the root `Cargo.toml` (root patch rule), `tests/tool_dispatch*.rs`, `docs/parallel-results/self-host-tool-split.md` |
| 3 | [std fs, process and env](03-std-io.md) | #1125 | no file, directory, process or environment access in `@vesper-std` | now | new `packages/vesper-std/{fs,process,env}/**`, new `crates/vesper-std-host/**` if needed, `tests/std_fs*.rs`, `tests/std_process*.rs`, `tests/std_env*.rs`, the std catalog hunk of the manuscript, `docs/parallel-results/self-host-std-io.md` |
| 4 | [Cranelift through a C-ABI shim](04-cranelift-shim.md) | #1126 | 38k lines lower to Cranelift through its Rust API | now | new `crates/vesper-cranelift-sys/**`, new `packages/vesper-cranelift/**`, `tests/cranelift_shim*.rs`, the root `Cargo.toml` member line (root patch rule), `docs/parallel-results/self-host-cranelift-shim.md` |
| 5 | [std graph, interning, arenas and hashing](05-std-compiler-data.md) | #1127 | no graph algorithms, interning, typed index arenas or BLAKE3 | now | new `packages/vesper-std/{graph,intern,arena,hash}/**`, `tests/std_graph*.rs`, `tests/std_intern*.rs`, `tests/std_arena*.rs`, `tests/std_hash*.rs`, the std catalog hunk of the manuscript (after lane 3), `docs/parallel-results/self-host-std-data.md` |
| 6 | [Toolchain pilots](06-toolchain-pilots.md) | #1128 | no compiler-linked toolchain crate has a port | now | `crates/vesper-span/**`, `crates/vesper-diagnostics/**`, `crates/vesper-lean-syntax/**`, their root `Cargo.toml` seed lines (root patch rule), `docs/parallel-results/self-host-toolchain-pilots.md` |
| 7 | [Front end v2](07-front-end-v2.md) | #1129 | the self-hosted front end folds its CST into a digest | now | new `packages/vesper-front/**`, `tests/self_host_front_v2*.rs`, `docs/parallel-results/self-host-front-end-v2.md` |
| 8 | [Census and SH1/SH2 plan](08-census.md) | #1130 | the checker and IR gap list is an estimate | after lanes 1-7 merge | `docs/maintenance/self-hosting-plan.md`, new `docs/agent-prompts/self-host-sh1/**` and `self-host-sh2/**`, `docs/parallel-results/self-host-census.md` |

**Start now:** lanes 1-7.
**After lanes 1-7:** lane 8.

**Recommended merge order:** 3 before 5 (both add a std catalog hunk to the
manuscript; merge `main` between). Lanes 1, 2 and 4 each add a root
`Cargo.toml` line: serialize those merges (no merge queue on this account) and
regenerate `Cargo.lock` with cargo right before merging. Lane 6 adds seed
lines next to lane 2's edits: merge 2 first. Lane 8 last.

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
- **Semantics first.** The evaluator, the JIT (optimize on and off) and an
  executable agree, with differential tests. Runtime helpers get leak tests.
- **Measure.** Give ABBA numbers and callgrind instruction counts against the
  Rust code each lane replaces or wraps; wall clock on these hosts moves by up
  to 30% between runs. Each lane's report has a "Compiler gaps" section with
  minimal repros, in the format of the port reports (`docs/parallel-results/port-p3*.md`).
- **Spec.** Any new std module, builtin or rule gets the smallest manuscript
  change in the same PR, flagged in the PR body.
- **Build.** Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100
  (`cargo +1.100.0 clippy` before pushing). New crates need
  `tools/bazel/sync_build_files.py`; files a test reads from outside its crate
  must be declared in `BUILD.bazel`. Windows and macOS CI use Cargo. Long runs
  go to GitHub CI from the pushed branch. A new port joins Linux port shard 7
  (see `docs/parallel-results/` port-shard notes).
- **Merge.** The owner has authorized merging once the four required checks
  (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`,
  `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments
  are addressed. If `main` is red and your PR fixes it, cancel the other
  in-progress Actions runs and merge the fix first.
- **Report.** `docs/parallel-results/self-host-<lane slug>.md`.

## Spec calls flagged for the owner

- **Codegen (decided 2026-11-19).** Cranelift behind an extern shim for
  Stage 2; Vesper-native backend in SH7.
- **Tool dispatch (lane 2).** `vesper <tool>` runs a separate `vesper-<tool>`
  executable. The CLI stays the same; the manuscript's tool chapters do not
  say how tools are packaged, so this needs no spec change unless the lane
  finds one.
- **std I/O capabilities (lane 3).** Which effect a file read performs
  (`Ffi[Lib]` today for `.vsys` imports, or a named `Fs` capability) is a
  spec call the lane writes into the manuscript and flags.

## Not in this wave

- Porting any compiler module (SH1 onward; lane 1 swaps one small pilot
  module only to prove the build).
- Widening the verified backend to the compiler (SH6b).
- A Vesper-native backend (SH7).
