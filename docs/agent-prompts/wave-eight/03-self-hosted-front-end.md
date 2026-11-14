# Self-hosting Stage 1: a Vesper-source lexer and parser for the core subset, differential against the Rust seed

Suggested branch: `wave8/self-hosted-front-end`. Claim issue: #452.

~~~text
You are implementing one substantial milestone of the Vesper design (§42.3, §38) in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or dedicated git worktree. Require baseline ea1ed2964c2583b5dc2da8e0fd7651daa39b00e2 or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Read AGENTS.md if present, docs/parallel-work.md, docs/implementation.md, docs/bazel.md, docs/rfcs/wave-eight-contracts.md, docs/agent-prompts/wave-eight/README.md, crates/vesper-syntax/src/lexer.rs and parser (read-only), docs/parallel-results/strata-source-regions.md, and the whole of §42.3, §38 in docs/specification.txt (the snapshot matches the Google Doc revision modified 2026-11-13 04:46 UTC) before editing.

When to start: now. If a PR this lane waits on is still open, check its state first; if it has not merged, work only on the parts that do not depend on it and merge origin/main once it lands.

Claim: this work is claimed by GitHub issue #452. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link #452 from your PR ("Closes #452").

Delivery and coordination:
- Deliver working code and meaningful acceptance tests, not only a proposal or scaffold. Finish the supported slice; reject unsupported cases explicitly with typed errors. If an item turns out to be infeasible, deliver the rest and explain why in the report.
- Edit only the exclusive paths below. Other wave-eight lanes and the still-open wave-seven PRs run concurrently; never edit their paths and never depend on an unmerged lane's work. Library crates must not depend on the root `vesper` package.
- Other agents (Codex) hold §16.4 attribute/item macros, the §39 formatter work (#170, #214), and these §42 paths: the resource model (lean/ResourceModel42*, docs/rfcs/resource-model42-*), rewrite witnesses and optimizer certificates (src/optimize*, tests/rewrite_*, scripts/check-rewrite-witness.py, docs/rfcs/rewrite42-*, lean/RewriteWitness*), and the Lean4Lean replay (lean/Lean4Lean*, tests/lean4lean*, issues #402-#448). Do not edit those paths and do not add attribute or item macro syntax. Do not write the eBPF linear ringbuf/socket builder API (PR #165, excluded by the owner).
- Root patch rule (user-approved 2026-11-12): you MAY commit your own minimal root patch onto YOUR PR branch so CI goes green: new `[workspace.dependencies]` entries and the regenerated Cargo.lock, and, for a new crate, registration in root Cargo.toml `members` AND `default-members` (insert before "experiments/native-pulse") plus its BUILD.bazel from `python3 tools/bazel/sync_build_files.py`. Keep it in its own commit. Root Cargo.toml and Cargo.lock conflict often: merge origin/main into your branch right before merging, regenerate the lockfile with cargo rather than hand-merging it, and wait for CI on the merged head. Merges that touch the root manifests are serialized: if another lane's PR touching them is about to merge, let it land first.
- Shared files that several lanes touch through narrow hunks (src/native.rs, src/check.rs, src/main.rs, crates/vesper-syntax/**, crates/vesper-doc/**) are listed in docs/rfcs/wave-eight-contracts.md with one owner each. Keep your hunk in those files small and in its own commit, and merge origin/main right before merging.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (run `cargo +1.100.0 clippy` before pushing if your local toolchain is older). Files a test reads from outside its crate directory must be declared in that crate's BUILD.bazel (`extra_compile_data`, `extra_test_data`); Lean-model tests need `extra_test_data = ["//:lean"]` and usually `copy_test_cwd` (see crates/vesper-chrono/BUILD.bazel). Bazel's rustfmt also checks test fixtures. Windows CI uses Cargo.
- `vesper fmt --check` runs in CI on every tracked .ves file; new .ves fixtures must be canonically formatted unless listed with a reason in scripts/fmt-exclude.txt.
- Prefer mature, widely used crates for cryptography, parsing and kernel/hardware access; pin exact compatible versions; never vendor or hand-roll cryptographic primitives. Reuse versions already in Cargo.lock where possible (Cranelift 0.125, Wasmtime 50, rustls 0.23, arrow 56).
- Use an isolated target directory per checkout. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited.
- Shared core ExprKind, Type and Place-IR Op/Terminator schemas, arithmetic semantics (vesper.arith.checked/1), interface semantics v2 and native/fiber ABI v2 are frozen unless your assignment says otherwise; additions are additive, and every backend rejects IR operations it does not know with a typed error. Preserve k_i ⊑ m_i and the existing abort/loan/one-shot checks.
- Unsafe code is forbidden except where your assignment allows it: None.
- Linear obligations in host libraries are non-Clone, non-Copy, #[must_use] owners whose only exits are documented consuming calls; a dropped owner is detected (debug panic or counted leak), never silently released. Rust ownership is not evidence that the Vesper source typing rules are implemented; say so in your report.
- Malformed, ambiguous, overflowed and over-budget inputs are typed errors, never success, and never panics on untrusted bytes.
- "0 Alloc" claims require a counting global allocator test over the warmed-up steady state. Hardware and kernel features must be reported as unavailable unless actually exercised (the cloud host has no /dev/kvm, no Intel PT, no hardware PMU, no GPU; perf_event_paranoid is 2; it does have PKU). A software model or QEMU validates the modelled behaviour only. Report only performance you measured, with the setup. Interop claims name the peer implementation and version.
- Lean: files must compile with the pinned Lean 4.24.0 (elan downloads are blocked in the cloud; use the GitHub release .zip), contain no `sorry`/`admit`/new axioms, and be checked from your tests when `VESPER_LEAN` is set (follow crates/vesper-simd/tests/model.rs). State exactly what is proved about which model, and that it is not a theorem about the Rust code.
- Do not start overlapping subagents, edit the Google Doc, publish packages, or message third parties.
- Run `cargo fmt --all -- --check`, focused tests, `cargo test --workspace --locked` and strict `cargo clippy --workspace --locked --all-targets -- -D warnings`. Record exact commands and results.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #452. The user has authorized merging this effort's PRs once all required CI checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main itself is red and your PR fixes it, follow the standing rule: cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, public API, checks, root patch, decisions and remaining gaps. Your report path below is mandatory.

Assignment: Self-hosting Stage 1: a Vesper-source lexer and parser for the core subset, differential against the Rust seed (§42.3, §38).

Exclusive ownership:
- NEW packages/vesper-compiler/** (Vesper source, Loom.toml with `capability_ceiling = ["Fail"]`)
- NEW tests/self_host*.rs and tests/fixtures/self_host/**
- docs/parallel-results/self-hosted-front-end.md
Dependencies: None new.

Implement:
1. Begin `@vesper-lang/compiler` (§42.3): write a lexer and a recursive-descent parser for the core expression/statement subset in Vesper itself, producing a flat, index-based CST encoded in byte buffers (use what the language supports today; if strings or byte slices are missing, model the source as an array of byte values and report the gap).
2. Run it with the Stage 0 compiler (evaluator and native backend) over the repository's own `.ves` fixtures and compare token streams and CST shapes with `vesper-syntax` (the Rust seed).
3. Keep each compiler phase inside a lexical region (`Region::scoped`, wave-seven lane 29) where the language allows it, and record which §42.3 requirements (no ambient I/O, bounded arena) are checked by the compiler today and which are only conventions.
4. Write down the language features the self-hosted compiler needs but Vesper lacks (strings, growable buffers, generic collections, traits used by the checker) as a ranked list in the report.

Acceptance:
- Differential tests over at least 50 real fixtures: identical token kinds and spans, and identical CST node kinds for the supported subset; unsupported syntax is reported as a typed error by the Vesper lexer/parser, never a wrong tree.
- `vesper fmt --check` passes on the new `.ves` files.
- Report: docs/parallel-results/self-hosted-front-end.md, with what was built, checks, decisions and remaining gaps.
~~~
