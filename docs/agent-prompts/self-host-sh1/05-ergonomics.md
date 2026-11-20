# Front-end ergonomics: or-patterns, Str literals, checked_add and bound literal views

> Self-hosting wave SH1, lane 5. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `self-host-sh1/ergonomics`. Claim issue: #1148.

Writing the front end, the std I/O driver and the hybrid pilot in Vesper hit language gaps that compiler code meets constantly: no or-patterns in `match`, no `-> Str` for a literal, no bound view of a literal, no byte-slice comparison, no total checked arithmetic, and a formatter limit on long `else if` chains. Each is small; together they decide how natural the SH1 and SH2 ports read.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a self-hosted Vesper compiler in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/maintenance/self-hosting-plan.md, docs/agent-prompts/self-host-sh1/README.md (the wave plan and the rules for every lane), docs/maintenance/porting-a-crate.md, docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Then read what this lane builds on: docs/parallel-results/self-host-front-end-v2.md (gaps 1, 2, 6), docs/parallel-results/self-host-std-io.md (gaps 3, 7), docs/parallel-results/self-host-hybrid-build.md (gap 2), and the manuscript chapters on patterns, strings and integer arithmetic.

When to start: now. If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1148 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1148", or "Part of #1148" for a partial PR).

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
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1148. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the numbers, decisions and remaining gaps.

Assignment: Front-end ergonomics: or-patterns, Str literals, checked_add and bound literal views.

Exclusive ownership:
- the manuscript hunks for each item (flag every one in the PR body as a spec call)
- the parser, checker, evaluator and lowering hunks for or-patterns, `Str` literal results, bound literal views, `cmp_bytes` and the `checked_add_k`/`checked_sub_k`/`checked_mul_k` builtins
- the mirror hunks in packages/vesper-front/src/parse.ves (parser mirror rule; merge main first if lane 10 has landed)
- the fmt DepthLimit fix in crates/vesper-fmt (else-if chains only)
- new tests/or_patterns*.rs, tests/str_literals*.rs, tests/checked_arith*.rs
- docs/parallel-results/self-host-ergonomics.md

Implement:
1. Reproduce every gap this lane owns on current main first (the P3b gaps wave merged fixes for nearby shapes after some SH0 reports were written). Record which repros already pass and drop them from the scope; commit each repro that still fails as a test before the fix.
2. Or-patterns `p | q` in `match` arms (no bindings across alternatives in this lane unless they are the same names and types; say which in the manuscript).
3. A string literal can be returned as `Str` (static bytes) and bound as a view (`let w = ".ves".as_bytes();`).
4. `cmp_bytes(in a: Slice[u8], in b: Slice[u8]) -> i32` lowered to memcmp, or `<` on byte slices; pick one in the manuscript.
5. `checked_add_k`, `checked_sub_k`, `checked_mul_k` returning `Option[k]` for every integer kind, total (no `Fail`).
6. `vesper fmt` formats `else if` chains of any length (iterative, not recursive).
7. Use each in packages/vesper-front where it replaces a workaround, keeping the differential test green.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds.
- Each item passes on the evaluator, the JIT (optimize on and off) and objects.
- The manuscript builds (`docs/manuscript` checks) and every spec call is listed in the PR body.
- Report: docs/parallel-results/self-host-ergonomics.md, with what changed, the Stage 1 guard evidence, the numbers before and after, a "Compiler gaps" section with minimal repros, and what is left.
~~~
