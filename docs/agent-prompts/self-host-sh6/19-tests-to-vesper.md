# Rust tests to Vesper tests

> Rust-exit wave SH6, lane 19. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `rust-exit/tests-to-vesper`. Claim issue: #1414.

355k lines of Rust tests (`tests/` and `crates/*/tests`) exercise the compiler and the libraries. When a module's Rust goes, its tests must not. Build the recipe and the tooling that turns them into Vesper tests run by Anvil (SH5 lane 14's test runner), and convert the CLI-driven compiler tests, which are most of `tests/`.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a Vesper repository with no Rust left except the irreducible minimum, in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/maintenance/rust-exit-plan.md (the program, the flip bar and the irreducible minimum), docs/maintenance/rust-census.md, docs/agent-prompts/self-host-sh6/README.md (this wave's rules), docs/maintenance/self-hosting-plan.md, docs/maintenance/porting-a-crate.md, and the "Rules for every lane" of docs/agent-prompts/self-host-sh2/README.md with the SH3, SH4 and SH5 additions, which all apply. Then read the latest report of every earlier lane on the crates or modules you own (docs/parallel-results/).

When to start: now (recipe); conversions follow each flip.

Claim: GitHub issue #1414 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Earlier claims this lane continues are named below: comment on each that SH6 lane 19 (#1414) carries the rest, and close it from your last PR. Link the claim from every PR ("Part of #1414", and "Closes #1414" on the last).

Delivery and coordination:
- Flip bar (owner, 2026-11-21, standing): a module or crate is flipped to Vesper and its Rust deleted once the Vesper build has full function (0 differences on the differential and every test) and runs within 1.5x of Rust's time (upper end of the ABBA 95% interval). Optimization beyond that comes later. Below the bar, Rust stays the default and the port stays non-default. Run `scripts/ci/flip_gate.py` (lane 1) and paste its verdict into the flip PR; until it merges, run its recipe from rust-exit-plan.md by hand. You do not need to ask the owner per module.
- CI rule (owner, 2026-11-21, standing project rule): no GitHub Actions runs for a branch that cannot merge yet (stacked on an open PR, carrying another open PR's commits, or conflicting with main). Right after such a push, cancel every Actions run it started (`gh api -X POST repos/robot-head/vesper/actions/runs/<id>/cancel`, or the GitHub MCP actions tools). Push rarely; Actions is saturated (5 macOS jobs). Test locally with targeted tests; let CI run only when the branch merges cleanly into main, right before landing.
- Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/stage1* or packages/vesper-compiler/**; show a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock when your change can reach the compiler.
- Edit only the exclusive paths below. Other lanes of this and earlier waves (SH2-SH5, which still own their remaining parts) run concurrently; never edit their paths. If you need a change in another lane's path, write the minimal repro or patch into your report and tell the coordinator.
- Compiler-linked crates: the Rust compiler links some library crates (`cargo tree -p vesper -e normal`). Their Rust cannot be deleted before Stage 2 unless lane 3 has moved them out of the compiler's closure. Flip them with the seed pattern (#1102: the Rust engine moves into the crate's frozen `seed/`), and the retirement PR (lane 2) deletes the seed.
- Tests move, never vanish: a flip PR converts the crate's Rust tests to Vesper tests (lane 19's recipe) or keeps them as a differential against the last Rust commit (the Aura pattern, #977). It never deletes a test without a replacement.
- Owner decision (2026-11-19): Cranelift stays behind the C-ABI shim (`vesper-cranelift-sys`). Owner decision (2026-11-21): the repository bootstraps from a committed wasm seed once the Rust compiler is gone (lane 2). Vetted crypto stays vetted: never hand-roll it; it goes behind lane 17's shim.
- Root patch rule: you MAY commit a minimal root patch (workspace members, [workspace.dependencies], the regenerated Cargo.lock, and removing a deleted crate) in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo. Serialize merges that touch root Cargo.toml or crates/vesper-hybrid (no merge queue).
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel (anchored globs only). New or removed crates need tools/bazel/sync_build_files.py. Windows and macOS CI use Cargo. "Failed in ~10s" on BuildBuddy means the branch conflicts with main.
- Licence: new crates and packages set `license = "MIT OR Apache-2.0"`.
- Use an isolated target directory, CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Hardware work uses software stand-ins (Xvfb, lavapipe, QEMU).
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1414. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Split the work into PRs of reviewable size; one crate per flip PR.
- Report: docs/parallel-results/rust-exit-tests-to-vesper.md with what changed, the flip-gate verdict per crate or module (differential counts, ABBA ratio and interval), Rust lines deleted, a "Compiler gaps" section with minimal repros, and what is left. Update the crate's row in the STATUS tables of scripts/rust-census.py in your PR (that one hunk is shared; merge main first).

Assignment: Rust tests to Vesper tests.

355k lines of Rust tests (`tests/` and `crates/*/tests`) exercise the compiler and the libraries. When a module's Rust goes, its tests must not. Build the recipe and the tooling that turns them into Vesper tests run by Anvil (SH5 lane 14's test runner), and convert the CLI-driven compiler tests, which are most of `tests/`.

Exclusive ownership:
- new `tests/vesper/**` (or the location the recipe picks), new `scripts/convert-rust-tests*`, the test-runner hunks that run them, `docs/maintenance/porting-a-crate.md` (the test section), report

Implement:
1. Classify `tests/*.rs` by shape: run the CLI and compare output; build and run a program; call a Rust API. Most are the first two and can become `.ves` sources with expected-output files.
2. Write the converter and the runner hook; convert one family end to end and show both suites agree on all four platforms.
3. Write the recipe each flip lane follows for its crate's tests, and the rule that a flip PR may not delete a test without a Vesper replacement.

Acceptance:
- The recipe in porting-a-crate.md; one converted family green on all four platforms; a count of what is left.
- Tests pass on all four platforms; the Stage 1 guard holds where the change reaches the compiler.
- The report above, and the census row updated.
~~~
