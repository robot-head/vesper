# Kinetic and Resonance

> Rust-exit wave SH6, lane 9. The [manuscript](../../manuscript/README.md) is
> the sole specification. Rules for every lane are in the [wave plan](README.md).
> Confirm live ownership on the issue board before acting.

Suggested branch: `rust-exit/kinetic-resonance`. Claim issue: #1404.

Finish, speed up and flip `vesper-kinetic` and `vesper-resonance`. Continues #1070 (P3b lane 8, Kinetic: morton 1.83x) and P3b Resonance (5.0-5.3x).

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are working toward a Vesper repository with no Rust left except the irreducible minimum, in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/maintenance/rust-exit-plan.md (the program, the flip bar and the irreducible minimum), docs/maintenance/rust-census.md, docs/agent-prompts/self-host-sh6/README.md (this wave's rules), docs/maintenance/self-hosting-plan.md, docs/maintenance/porting-a-crate.md, and the "Rules for every lane" of docs/agent-prompts/self-host-sh2/README.md with the SH3, SH4 and SH5 additions, which all apply. Then read the latest report of every earlier lane on the crates or modules you own (docs/parallel-results/).

When to start: now.

Claim: GitHub issue #1404 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Earlier claims this lane continues are named below: comment on each that SH6 lane 9 (#1404) carries the rest, and close it from your last PR. Link the claim from every PR ("Part of #1404", and "Closes #1404" on the last).

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
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1404. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Split the work into PRs of reviewable size; one crate per flip PR.
- Report: docs/parallel-results/rust-exit-kinetic-resonance.md with what changed, the flip-gate verdict per crate or module (differential counts, ABBA ratio and interval), Rust lines deleted, a "Compiler gaps" section with minimal repros, and what is left. Update the crate's row in the STATUS tables of scripts/rust-census.py in your PR (that one hunk is shared; merge main first).

Assignment: Kinetic and Resonance.

Finish, speed up and flip `vesper-kinetic` and `vesper-resonance`. Continues #1070 (P3b lane 8, Kinetic: morton 1.83x) and P3b Resonance (5.0-5.3x).

Exclusive ownership:
- `crates/vesper-kinetic`, `crates/vesper-resonance`, their `tests/`, `port/` and `seed/` directories, `tests/*` files named after the crates, report

Implement:
1. Finish the port of `vesper-kinetic` and `vesper-resonance`: every public item the crate's dependents use, with the crate's own Rust tests running against the port through the differential harness (`vesper-port` builds). Start from the latest report of the earlier lane named above and its "not ported yet" list.
2. Close the speed gap to the owner's bar (within 1.5x of Rust, upper end of the ABBA 95% interval) with port rewrites first. Where the compiler is the reason, write a minimal repro into the report's "Compiler gaps" section; fix it in its own commit only if no open lane owns that hunk (check the census and the SH4/SH5 boards), otherwise hand the repro to the coordinator.
3. Flip each crate that passes the flip gate (`scripts/ci/flip_gate.py`, SH6 lane 1; until it merges, run its recipe by hand): the Vesper build becomes the only build, the Rust implementation is deleted, the crate's Rust tests become Vesper tests (SH6 lane 19's recipe) or stay as a differential against the last Rust commit (the Aura pattern, #977). If the compiler links the crate, move the Rust engine into the crate's frozen seed instead (the #1102 pattern) and leave deletion to the retirement (lane 2).
4. `detfloat`, the SIMD kernels and the threaded world port with source vectors (§14) and atomics; the Cranelift IR scanner goes through `@vesper/cranelift`. The compiler links both today (native_verifiers): flip to the seed pattern until lane 3 moves them out.

Acceptance:
- Each crate either flipped (Rust deleted or moved to its seed) or reported with its measured ratio and the gaps that keep it above 1.5x.
- Tests pass on all four platforms; the Stage 1 guard holds where the change reaches the compiler.
- The report above, and the census row updated.
~~~
