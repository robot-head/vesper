# Qualified paths and submodule items

> P3b gaps wave, lane 8. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `p3b-gaps/modules-and-paths`. Claim issue: #1093.

Module and parser gaps the ports reported:

- **No qualified calls** (port-p3b-prism-h3.md gap 2, port-p3-pact.md gap 3): `self::prefix::decode_int(..)` gives "expected RBrace, found ColonColon"; there is no `use .. as`, so every submodule function needs a unique name (`enc_*`/`dec_*`).
- **`.vsys` imports are visible only in the root module** (port-p3b-kinetic.md gap 5): `fastmath.ves` cannot name `Crmath` (`E0111 module self has no item 'Crmath'`).
- **Exports come only from the root module** (port-p3b-flux.md gap 2): exported functions must sit in `lib.ves`.
- **An imported function named like a scalar type shadows the type** (port-p3-aegis.md gap 4): `use self::reader::{u64}` makes every `u64` mean the function.
- **Package error offsets are global** over files in alphabetical order (kinetic): diagnostics should name the file and line.
- **Parser** fixes: `Result::Ok(())` as a pattern (prism-h3 gap 4), `in q[..]` as an argument (flux gap 1), and contextual use of `state`, `linear` and `with` as names (bench gap 5, pact gap 6, chorus gap 5), or at least a diagnostic that names the keyword.

Ports to re-measure: Prism H3, Pact, Kinetic, Flux, Aegis, Chorus.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are closing a compiler gap that the P3a and P3b ports of https://github.com/robot-head/vesper measured.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Record the actual base SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/p3b-gaps/README.md (the wave plan and the rules for every lane), docs/agent-prompts/stage1-perf/README.md ("Why this work fits in Stage 1", "Rules for every lane"), docs/parallel-results/port-gaps-rescore.md (the measurement setup), these port reports: port-p3b-prism-h3.md (gaps 2, 4), port-p3-pact.md (gaps 3, 6), port-p3b-kinetic.md (gap 5), port-p3b-flux.md (gaps 1-2), port-p3-aegis.md (gap 4), port-p3b-chorus.md (gap 5), port-p3-bench.md (gap 5), bootstrap/stage1.rs and crates/vesper-codegen-verified/src/stage1.rs (`compare_outcome`). Some reports may still be on open port-lane PR branches; read them there (`git fetch origin pull/<n>/head`).

When to start: now. If a PR this lane waits on is still open, work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1093 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1093", or "Part of #1093" for a partial PR).

Delivery and coordination:
- Follow every rule in "Rules for every lane" in docs/agent-prompts/p3b-gaps/README.md, above all the Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch).
- Edit only the exclusive paths below. Other lanes of this wave and the P3b port lanes run concurrently; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of every port the owner has not already moved to Vesper, and is never deleted until the Vesper build matches it in both functionality and performance. Do not change any port's default build or delete Rust code. You may measure ports with local rewrites; do not commit port changes (lane 12 or the port's own lane does that).
- Root patch rule: you MAY commit a minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel; new crates need tools/bazel/sync_build_files.py. Windows and macOS CI use Cargo.
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that links #1093. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the Stage 1 guard evidence, the repro and port numbers before and after, decisions and remaining gaps.

Assignment: Qualified paths and submodule items.

Exclusive ownership:
- package module resolution (src/packages.rs, src/package_graph.rs and the name-resolution hunks of src/check.rs)
- the path-expression, use and pattern hunks of crates/vesper-syntax/src/parser.rs
- the module hunks of src/ffi_lowering/** (where .vsys imports are bound) and of src/native_export/directives.rs (which module's pub fns are exported)
- tests/package_paths*.rs, tests/package_submodule_exports*.rs
- the module hunks of the manuscript
- docs/parallel-results/p3b-gaps-modules-and-paths.md

Implement:
1. Qualified value and type paths (`self::m::f`, `super::m::T`, `@pkg::m::f`) in expressions, types and patterns, and `use path as name`.
2. `.vsys` imports resolvable from every module of the package; `pub fn` items in submodules exported under a documented, collision-free symbol rule (or an explicit `// @export` re-export in `lib.ves`); decide and document which.
3. Separate type and value namespaces in `use` so a function import cannot shadow a scalar type.
4. Per-file diagnostic positions for package builds.
5. The parser fixes above; for keywords, accept them as names where the grammar is unambiguous, otherwise improve the diagnostic.
6. Write the manuscript changes and flag them.

Acceptance:
- Tests pass on all four platforms and the Stage 1 guard holds. The evaluator, the JIT (optimize on and off) and an executable agree, with differential tests.
- Each repro above checks and runs; the Prism H3 `enc_*`/`dec_*` renames and the Flux root-module exports are undone in a local rewrite with identical output.
- Each repro from the reports is measured or run before and after. Report only what you measured; wall clock on these hosts moves by up to 30% between runs, so give instruction counts too.
- Report: docs/parallel-results/p3b-gaps-modules-and-paths.md, with what changed, the Stage 1 guard evidence, the numbers before and after, and what is left.
~~~
