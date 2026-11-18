# Atomics and scoped workers (F12)

> Port wave P3a, lane 2. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `port-p3/atomics-workers`. Claim issue: #1037.

Compiled Vesper has no shared-memory concurrency: no atomic cells and no way to run work on several cores and join it. The survey calls this F12. It blocks Kinetic's threaded `RigidWorld`, Resonance's rings, Quarry's latches and SSI, and Flux's multi-worker executor (wave P3b). The manuscript uses atomics only inside Rust listings (§13 ShardSlab, `RtAtomicSlot` in Resonance) and has no source-level rule, so this lane writes one.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are adding a compiler feature that the port wave P3a plan of https://github.com/robot-head/vesper needs for the next crates to port.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Require baseline 3c075a4 or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/port-p3/README.md (the wave plan), "Rules for every lane" in docs/agent-prompts/port-speed/README.md (the compiler-lane rules and the Stage 1 guard), docs/maintenance/vesper-port-survey.md ("Missing compiler and runtime features"), the manuscript's Pulse chapter (§12, worker shards and §12.4-§12.5), §13 (ShardSlab and the remote free stack), the Resonance `RtAtomicSlot` text, issue #599 (Pulse pool) and src/native.rs (how builtins lower).

When to start: now for atomics; the scoped-worker half after wave-nine lane 22 (#599) merges. If a PR this lane waits on is still open, check its state first; work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency.

Claim: GitHub issue #1037 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1037").

Delivery and coordination:
- Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch). The verified backend refuses the new construct with a typed diagnostic.
- Edit only the exclusive paths below. The port speed wave (#1021-#1028) owns the native backend's performance hunks and the port lanes of this wave own their crates; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of every port but Aura. Do not change any port's default build or delete Rust code. You may try the feature in a port locally; do not commit port changes.
- Root patch rule: you MAY commit a minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy` before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel. Windows and macOS CI use Cargo. `vesper fmt` and the Stage 1 self-hosted parser must accept or skip every new .ves fixture (tests/fixtures/self_host/driver.rs skip predicate, one line).
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #1037. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the manuscript change, the Stage 1 guard evidence, tests, decisions and remaining gaps.

Assignment: Atomics and scoped workers (F12).

Exclusive ownership:
- NEW src/atomics.rs and src/atomics/**
- the atomic builtin hunks of src/check.rs and src/native.rs (new builtins only)
- NEW packages/vesper-std/sync/** (the `@vesper-std/sync` package; register it next to the other std packages)
- tests/atomics*.rs, tests/scoped_workers*.rs and their fixtures
- a new manuscript subsection for source atomics and scoped workers, and docs/parallel-results/port-p3-atomics-workers.md

Dependencies: Wave-nine lane 22 (#599) for the scoped-worker half. Do not edit Pulse; if #599 is not close, ship atomics alone and leave the second half as a follow-up named in the report.

Implement:
1. Manuscript: `Atomic[T]` for `bool`, `u8`-`u64`, `i8`-`i64` and `usize`, with `load`, `store`, `swap`, `compare_exchange`, `fetch_add`/`sub`/`and`/`or`/`xor` and explicit orderings (`Relaxed`, `Acquire`, `Release`, `AcqRel`, `SeqCst`). Say which mode an `Atomic[T]` has (shareable through `in` borrows across workers, never `copy`), what effect atomic operations carry, and that plain fields stay race-free by the existing borrow rules.
2. Lower the operations to Cranelift atomics in the native backend and the JIT; the verified backend refuses them with a typed diagnostic.
3. Scoped workers: `scope(|s| { s.spawn(...); ... })`-style fork-join in `@vesper-std/sync` over the Pulse pool from #599, where every spawned body borrows only `in` data or `Atomic` cells and the scope joins before it returns. Panics or `Fail` in a worker propagate to the scope.
4. Tests: counters under contention with every ordering that is allowed, a compare-exchange loop, a scoped parallel sum over a slice, rejection of a non-`Atomic` `inout` shared across workers, and the four platforms.
5. Measure a contended `fetch_add` and a 4-worker parallel sum against the same Rust code.

Stays in Rust: Nothing; this is new compiler work. Lock-free data structures (queues, epoch reclamation) are out of scope.

Acceptance:
- Atomics work in the native backend and the JIT on the four required checks; scoped workers work where #599 has merged.
- Sharing a non-atomic `inout` across workers is a typed diagnostic with a fixture.
- Stage 1 lock reproduces unchanged.
- Report: docs/parallel-results/port-p3-atomics-workers.md, with the manuscript change, the lowering, the benchmarks, and what Kinetic, Resonance, Quarry and Flux still need.
~~~
