# Effects and function values through trait objects (F7c)

> Port wave P3b, lane 1. The [manuscript](../../manuscript/README.md) is the sole
> specification. Rules for every lane are in the [wave plan](README.md). Confirm
> live ownership on the issue board before acting.

Suggested branch: `port-p3b/dyn-effects-closures`. Claim issue: #1063.

Second-class `dyn` (#1036, PR #1051) and boxed objects (#1057, PR #1060) dispatch through a vtable, but a slot whose method performs a handler effect is specialized at the call, so an object whose methods do `Net`, `Storage` or `Ffi[..]` work cannot be stored and called later. Function values are not objects at all: Rust's `&dyn Fn`, `&mut dyn FnMut`, `Box<dyn FnMut>` and `Box<dyn FnOnce>` have no Vesper form. Both reports list this as the first gap the P3b ports need: Chorus transports, Flux sources and sinks, Kinetic's contact pass, and the P3a enum workarounds in Pact, Silicon and Aegis.

~~~text
Specification authority: docs/manuscript/ (Typst) is the sole specification. If
the work needs a language or library decision the manuscript does not make,
write the smallest manuscript change into the same PR and flag it in the PR body.

You are adding a compiler feature that the port wave P3b plan of https://github.com/robot-head/vesper needs for the next crates to port.

Start from a fresh origin/main in a NEW cloud checkout or a dedicated git worktree. Require baseline 8212ddf or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Before editing, read docs/agent-prompts/port-p3b/README.md (the wave plan), "Rules for every lane" in docs/agent-prompts/port-speed/README.md (the compiler-lane rules and the Stage 1 guard), docs/parallel-results/port-p3-dyn-trait.md and docs/parallel-results/boxed-dyn-trait.md ("Remaining gaps" and "What the P3b ports still need"), the manuscript's §7.1-§7.3 trait-object items, its closure typing rule `[T-Abs]` and the handler evidence sections, src/dyn_trait.rs and src/dyn_trait/**, and how src/native_handlers.rs passes handler evidence.

When to start: now. If a PR this lane waits on is still open, check its state first; work only on the parts that do not depend on it, and merge origin/main once it lands. Never merge before a dependency. Stacking a branch on an open dependency's branch is allowed; retarget it to main once the dependency merges.

Claim: GitHub issue #1063 claims this work. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link the claim from your PR ("Closes #1063").

Delivery and coordination:
- Stage 1 guard: never edit src/verified_codegen/**, src/ir.rs (`lower_world` and the Place IR types), `ir_opt::optimize`, lean/**, bootstrap/** or packages/vesper-compiler/**, and show every verified artifact digest of a fresh `bootstrap/run.sh stage1 --reproduce --outcome` equal to bootstrap/stage1.lock (run the bootstrap-stage1 workflow on your branch). The verified backend and WebAssembly refuse the new construct with a typed diagnostic.
- Edit only the exclusive paths below. The port speed wave (#1021-#1028) owns the native backend's performance hunks and the port lanes of this wave own their crates; never edit their paths and never depend on an unmerged lane's work.
- Standing owner rule: Rust stays the default build of every port but Aura. Do not change any port's default build or delete Rust code. You may try the feature in a port locally; do not commit port changes.
- Root patch rule: you MAY commit a minimal root patch (new [workspace.dependencies] entries and the regenerated Cargo.lock) onto YOUR branch, in its own commit. Merge origin/main right before merging and regenerate the lockfile with cargo.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (`cargo +1.100.0 clippy --all-targets -- -D warnings` on what you touched before pushing). Files a test reads from outside its crate must be declared in BUILD.bazel; new crates need `python3 tools/bazel/sync_build_files.py`. Windows and macOS CI use Cargo.
- `vesper fmt --check` runs in CI on every tracked .ves file, and the Stage 1 self-hosted parser must accept or skip every new .ves file (one line in the `fixtures()` skip predicate in tests/fixtures/self_host/driver.rs).
- Lean: any Lean file must compile with the pinned Lean 4.24.0, with no sorry, admit or new axioms.
- Use an isolated target directory. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited. Long runs go to GitHub CI from your pushed branch; local runs cover only targeted tests.
- Do not start overlapping subagents, edit website/**, publish packages, or message third parties.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #1063. The owner has authorized merging once the four required checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main is red and your PR fixes it, cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, what changed, the manuscript change, the Stage 1 guard evidence, tests, the benchmark, decisions and remaining gaps.

Assignment: Effects and function values through trait objects (F7c).

Exclusive ownership:
- the effect-row and evidence hunks of src/dyn_trait.rs and src/dyn_trait/** (vtable slot signatures, stub emission)
- NEW src/dyn_fn.rs and src/dyn_fn/** (closure-to-object coercion, `Fn`/`FnMut`/`FnOnce` object traits)
- the evidence-passing call hunks of src/native_handlers.rs and src/native.rs, and the matching evaluator hunks of src/runtime.rs
- tests/dyn_effects*.rs, tests/dyn_fn*.rs, tests/fixtures/dyn_effects/**, tests/fixtures/dyn_fn/**
- the manuscript's trait-object text for effectful slots and function objects, and docs/parallel-results/port-p3b-dyn-effects-closures.md

Dependencies: None. Lane 2 (#1064) edits the Box drop elaboration in the same area: whichever merges second merges main first.

Implement:
1. Specify what the manuscript leaves open, in the smallest change: a trait method's effect row is part of the object type (`dyn Transport` carries the row of its methods, or a declared row `dyn[Net] Transport`), how a caller's handler evidence reaches a slot, and the object forms of function types (`in dyn Fn(A) -> R eff E`, `inout dyn FnMut(..)`, `Box[dyn FnMut(..)]`, `Box[dyn FnOnce(..)]` with exact-once call).
2. Effectful slots: pass the caller's evidence vector to the slot instead of specializing it, in the native backend and the JIT; a call through `in dyn`, `inout dyn` and `Box[dyn ..]` whose methods perform handler effects and `Ffi[..]` works, and the effect still reaches the caller's row as for a static call.
3. Function objects: a closure coerces to `in dyn Fn`/`inout dyn FnMut` at a call (no allocation, the capture lives in the caller's frame) and to `Box[dyn FnMut]`/`Box[dyn FnOnce]` with `Box::new` (one allocation holding the captures); `FnOnce` boxes are consumed by the call. Linear captures force `FnOnce`.
4. The evaluator agrees with the JIT on every new test.
5. Tests: an effectful trait called through each object form, a handler installed after the object was boxed, a `Vec`-like sequence of `Box[dyn FnMut]` callbacks, `FnOnce` consumption with a drop counter, the leak test, and every rejection as a typed diagnostic with a fixture.
6. Benchmark an effectful slot call and a `Box[dyn FnMut]` call against the specialized and monomorphized calls.

Stays in Rust: Nothing; this is new compiler work. Associated types on objects, several models per (object, type) pair and `Rc`/`Arc` of objects stay out of scope: list what they would need.

Acceptance:
- Effectful slots and the four function-object forms work in the native backend and the JIT on the four required checks.
- `in dyn Fn`/`inout dyn FnMut` allocate nothing for the object (counting-allocator test).
- Stage 1 lock reproduces unchanged.
- Report: docs/parallel-results/port-p3b-dyn-effects-closures.md with the manuscript change, the evidence layout, the benchmark, and which P3b ports (Chorus, Flux, Kinetic) and P3a workarounds it unblocks.
~~~
