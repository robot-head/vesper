# Front end v2: lexer speed and editor mode

> Self-hosting wave SH1, lane 10. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh1/front-v2-speed`. Claim issue: #1153.

The v2 parser runs at 0.37x Rust time but the lexer is 2.9x (7.1x instructions), and only batch mode is ported: the editor mode that fmt, highlight and the LSP need (`BLOCK`, `ERROR_NODE` and `INCOMPLETE` nodes, recovery sites, header notes) and macro sources are not.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh1/README.md (the wave plan and the rules for every lane), docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Then read what this lane builds on: docs/parallel-results/self-host-front-end-v2.md (all of it) and crates/vesper-syntax/src/{lexer.rs,parser.rs,cst*.rs}.

When to start: now. Lane 3's bounds facts help the lexer; take its result when it lands, and do the rest (byte-class builtins, table-driven dispatch, `const` tables from #1115) without it. If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1153 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1153", or "Part of #1153" for a partial PR).

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/self-host-sh1/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave and other waves run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of the compiler and of every port the owner has not already moved to Vesper, and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any default build or delete Rust code.
- Owner decision (2026-11-19): Stage 2 keeps Cranelift as a vetted external library behind a C-ABI shim; a Vesper-native backend is a later wave.
- Parser mirror rule: any change to crates/vesper-syntax/src/parser.rs or lexer.rs must be mirrored in packages/vesper-front in the same PR, or tests/self_host_front_v2.rs fails.
- Root patch rule: you MAY commit a minimal root patch (workspace members, [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel (anchored globs only, never a root `**`); new crates need tools/bazel/sync_build_files.py. Windows and macOS CI use Cargo. A new port joins Linux port shard 7.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1153. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: Front end v2: lexer speed and editor mode.

Exclusive ownership:
- packages/vesper-front/** (except the mirror hunks lane 5 adds to parse.ves: merge main before merging if lane 5 has landed)
- tests/self_host_front_v2*.rs
- docs/parallel-results/self-host-front-v2-speed.md

Implement:
1. Move token and node kinds to `const` items (#1115) and the punctuation and precedence tables to `match`es or fixed arrays.
2. Bring the lexer to parity with logos on the repository corpus (byte-class runs, a fixed dispatch table, no per-byte `Option`), measured with callgrind and ABBA.
3. Port the editor mode: recovery sites, `BLOCK`, `ERROR_NODE`, `INCOMPLETE`, header notes, and macro sources, matching vesper-syntax's editor parse event for event.
4. Extend the differential test to the editor mode over the corpus and the fuzz corpus (truncated and corrupted inputs).

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds.
- The differential test passes in both modes on all four platforms.
- Lexer time at or below 1.0x of Rust on the corpus, or the remaining gap with a profile and repros.
- Report: docs/parallel-results/self-host-front-v2-speed.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
