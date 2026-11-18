# Second-class `dyn Trait` (F7b)

> Port wave P3a, lane 1. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `port-p3/dyn-trait`. Claim issue: #1036.

The manuscript already specifies zero-allocation second-class trait objects: `DynType ::= 'dyn' Path` in the grammar, and in §7.1 `in dyn`, `inout dyn` and `sink dyn` parameters that lower to a 16-byte `(data_ptr, vtable_ptr)` pair with a move slot for `sink self`. §7.3 says `dyn Trait` values stay outside monomorphization "until dictionary passing exists". Nothing in `src/` implements it: the checker has no `dyn` type and static elaboration rejects every bound it cannot monomorphize. The survey calls this F7b. It blocks Chorus (12 traits), Flux sources and sinks, Warp's ring abstraction and Aura's pluggable backends, which are wave P3b.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are adding a compiler feature that the port wave P3a plan of https://github.com/robot-head/vesper needs for the next crates to port.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Require baseline 3c075a4 or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/port-p3/README.md (the wave plan), "Rules for every lane" in docs/agent-prompts/port-speed/README.md (the compiler-lane rules and the Stage 1 guard), docs/maintenance/vesper-port-survey.md ("Missing compiler and runtime features"), the manuscript's grammar (`DynType`), §7.1 ("Zero-Allocation Second-Class Trait Objects"), §7.2 and §7.3 ("Bounded Generics by Monomorphization"), src/check.rs (trait and bound checking) and the native lowering of calls in src/native.rs.

When to start: now. If a PR this lane waits on is still open, check its state first; work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1036 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1036").

Delivery and coordination:
- Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch). The verified backend refuses the new construct with a typed diagnostic.
- Edit only the exclusive paths below. The port speed wave (#1021-#1028) owns the native backend's performance hunks and the port lanes of this wave own their crates; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of every port but Aura. Do not change any port's default build or delete Rust code. You may try the feature in a port locally; do not commit port changes.
- Root patch rule: you MAY commit a minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel. Windows and macOS CI use Cargo. `vesper fmt` and the Stage 1 self-hosted parser must accept or skip every new .ves fixture (tests/fixtures/self_host/driver.rs skip predicate, one line).
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #1036. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the manuscript change, the Stage 1 guard evidence, tests, decisions and remaining gaps.

Assignment: Second-class `dyn Trait` (F7b).

Exclusive ownership:
- NEW src/dyn_trait.rs and src/dyn_trait/** (object safety, vtable layout, coercion)
- the `dyn` hunks of src/check.rs and the parser/AST files that need a `dyn` type node
- the `dyn` call and vtable-emission hunks of src/native.rs and the JIT path
- a typed refusal of `dyn` parameters in src/native_export/** (no ABI support in this lane)
- tests/dyn_trait*.rs, tests/fixtures/dyn_trait/**
- the `dyn` text of manuscript §7.1-§7.3, and docs/parallel-results/port-p3-dyn-trait.md

Dependencies: None.

Implement:
1. Specify what the manuscript leaves open, in the smallest change: object-safety rules (no generic methods, no `Self` by value outside `sink self`, no associated constants), the vtable layout (method order, the drop and move slots, size and alignment words), and the coercion from `in`/`inout`/`sink T` to the `dyn` form where `T: Trait`.
2. Checker: a `dyn Path` type allowed only in `in`, `inout` and `sink` parameter position (second class: it never escapes, is never stored in a field, returned, or captured). Reject everything else with typed diagnostics and fixtures for each.
3. Lowering: one vtable per (type, trait) instance, emitted once per object; calls through `dyn` load the slot and call indirectly; `sink dyn` consumes through the move slot and the drop glue runs once. Effects of the trait's methods flow to the caller as for a static call.
4. Native backend and JIT both; the verified backend refuses `dyn` with a typed diagnostic. Export ABI refuses `dyn` parameters with a typed diagnostic.
5. Tests: dispatch to two impls through one function, `inout dyn` mutation, `sink dyn` consumption with a drop counter, a trait with effects, a generic function that takes `in dyn`, the leak test, and every rejection. Run them on all four platforms.
6. Measure one indirect call against the monomorphized call in a micro-benchmark, and report it.

Stays in Rust: Nothing; this is new compiler work. First-class `dyn` values (boxed, stored, returned) are out of scope: list what they would need in the report.

Acceptance:
- `in dyn`, `inout dyn` and `sink dyn` work in the native backend and the JIT on the four required checks, with zero heap allocation for the object itself (counting-allocator test).
- Every non-object-safe trait and every escaping use is a typed diagnostic with a fixture.
- Stage 1 lock reproduces unchanged.
- Report: docs/parallel-results/port-p3-dyn-trait.md, with the manuscript change, the vtable layout, the benchmark, and which P3b crates it unblocks and what they still need.
~~~
