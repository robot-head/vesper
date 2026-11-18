# Export directives from the root module, `Option`/`Result` across the boundary without conversion, `SeqRef` slices

> Port gaps wave, lane 8. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `port-gaps/export-boundary`. Claim issue: #991.

`// @export abi 3` is read from "file 0 of a package", but packages load files in sorted path order, so a directive in `lib.ves` is silently ignored whenever another file sorts first (Aura, Quarry). `Option` and `Result` arguments and results still convert at the boundary (BBR's glue is 8.5% of its run), `SeqRef` has no slice view so wrappers copy into a `Vec` (Aura), and Prism's adapter allocates per request.

Ports to re-measure: BBR, Aura, Quarry, Prism.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are closing a native-backend gap that the port parity wave of https://github.com/robot-head/vesper measured in the Vesper ports.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/port-gaps/README.md (the wave plan and the rules for every lane), docs/parallel-results/port-parity.md (the scoreboard), the port reports it cites for this lane (port-parity-aura.md gaps 1 and 6, port-parity-quarry.md gap 4, port-parity-bbr.md gap 4, port-parity-prism.md gap 6), docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1"), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`).

When to start: now, with the directive fix as the first commit. If a PR this lane waits on is still open, check its state first; work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #991 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents (above all #972 and PR #983); if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #991").

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/port-gaps/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of every port and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any port's default build or delete Rust code. You may measure ports with local rewrites; do not commit port changes (lane 9 does that).
- Root patch rule: you MAY commit a minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel. Windows and macOS CI use Cargo.
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #991. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the repro and port numbers before and after, decisions and remaining gaps.

Assignment: Export directives from the root module, `Option`/`Result` across the boundary without conversion, `SeqRef` slices.

Exclusive ownership:
- the directive lookup in src/native_export.rs and the driver code that hands it the root module (src/main.rs, src/lib_commands.rs, src/packages.rs: only those hunks)
- src/native_export/** (the Option/Result glue)
- crates/vesper-port-abi/** (`SeqRef::as_slice`)
- tests/native_export*.rs, tests/fixtures/native_export/**
- the ABI sections of docs/maintenance/porting-a-crate.md and the manuscript's export ABI hunk
- docs/parallel-results/port-gaps-export-boundary.md

Implement:
1. Read `@export` directives from the package's root module. If a directive appears in another file, warn (or reject) instead of ignoring it. Ports that moved the directive to work around this (Aura `apply.ves`, Quarry `check.ves`, BBR `types.ves`, Ignis `d3d12.ves`) must still build the same binding; their committed binding checks catch a change.
2. Lend copy `Option[T]`/`Result[T, E]` as v3 mirrors (a tag word and the payload) instead of converting them, for arguments and results.
3. `SeqRef<T>::as_slice()` for packed scalars.
4. Look at Prism gap 6 and fix what is the ABI crate's share.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds.
- Each repro named above is re-measured (instructions in the emitted function, or `objdump`) before and after.
- The ports named above are re-measured with their committed ABBA benchmarks, with local port rewrites where the lane adds a builtin; the report gives each ratio before and after with the setup. Report only what you measured.
- Report: docs/parallel-results/port-gaps-export-boundary.md, with what changed, the Stage 1 guard evidence, the numbers before and after, and what is left.
~~~
