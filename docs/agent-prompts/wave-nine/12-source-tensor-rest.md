# Source Tensor rest and wire structs: record `wire` in the item schema, tensors in impls and traits, named axes and region-backed views

> Retained work context for open issues, unmerged branches, or their dependencies.
> The [manuscript](../../manuscript/README.md) is the sole specification; use the
> [section map](../../section-map.md) for earlier numbering. Confirm live ownership before acting.

Suggested branch: `wave9/source-tensor-rest`. Claim issue: #589.

~~~text
Specification authority: docs/manuscript/content.typ is the sole specification.
Earlier section numbers below use the legacy numbering; locate their manuscript
chapters with docs/section-map.md before implementing requirements.

You are implementing one substantial milestone of the Vesper design (§28.2, §13.2) in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or dedicated git worktree. Require baseline 37f7b8bcfa4cd409177856c8fe5deeaa7f2a88ff or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Read AGENTS.md if present, docs/README.md, docs/bazel.md, docs/rfcs/wave-nine-contracts.md, docs/agent-prompts/wave-nine/README.md, crates/vesper-tensor (lane 27 owns it), docs/parallel-results/source-tensor-types.md, and the whole of §28.2, §13.2 in docs/manuscript/content.typ before editing.

When to start: now. The `wire` schema change in crates/vesper-syntax is additive, in its own commit, and merges first if lanes 10 or 11 also touch the crate. If a PR this lane waits on is still open, check its state first; if it has not merged, work only on the parts that do not depend on it and merge origin/main once it lands.

Claim: this work is claimed by GitHub issue #589. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link #589 from your PR ("Closes #589").

Delivery and coordination:
- Deliver working code and meaningful acceptance tests, not only a proposal or scaffold. Finish the supported slice; reject unsupported cases explicitly with typed errors. If an item turns out to be infeasible, deliver the rest and explain why in the report.
- Edit only the exclusive paths below. Other wave-nine lanes run concurrently; never edit their paths and never depend on an unmerged lane's work. Library crates must not depend on the root `vesper` package.
- Other agents (Codex) hold §16.4 attribute/item macros, the §39 formatter work (#170, #214), the §42 resource model (lean/ResourceModel42*, docs/rfcs/resource-model42-*), rewrite witnesses and optimizer certificates (src/optimize*, tests/rewrite_*, scripts/check-rewrite-witness.py, docs/rfcs/rewrite42-*, lean/RewriteWitness*), the Lean4Lean replay (lean/Lean4Lean*, tests/lean4lean*, issues #402-#560), and these open CI repairs: src/check.rs (#567), lean/ReferenceEval.lean and lean/ReferenceEval/Soundness.lean (#568), tests/checked_arithmetic.rs and tests/fixtures/lockstep/** (#569), buildbuddy.yaml (#571), crates/vesper-fiber-abi/tests/unwind.rs (#572), crates/vesper-pulse/src/pool.rs and crates/vesper-pulse/tests/pool.rs (#573), root BUILD.bazel (#574), src/lsp/lens_tests.rs (#575), and src/native.rs, src/native_batch.rs, src/native_session.rs (#576). Do not edit those paths unless your assignment names a hunk that waits for the holder's PR to merge, and do not add attribute or item macro syntax. Do not write the eBPF linear ringbuf/socket builder API (PR #165, excluded by the owner). Egglog fusion (crates/vesper-fusion, lean/FusionRules.lean, #460/PR #487) is held by wave-eight lane 11.
- Root patch rule (user-approved 2026-11-12): you MAY commit your own minimal root patch onto YOUR PR branch so CI goes green: new `[workspace.dependencies]` entries and the regenerated Cargo.lock, and, for a new crate, registration in root Cargo.toml `members` AND `default-members` (insert before "experiments/native-pulse") plus its BUILD.bazel from `python3 tools/bazel/sync_build_files.py`. Keep it in its own commit. Root Cargo.toml and Cargo.lock conflict often: merge origin/main into your branch right before merging, regenerate the lockfile with cargo rather than hand-merging it, and wait for CI on the merged head. Merges that touch the root manifests are serialized: if another lane's PR touching them is about to merge, let it land first.
- Shared files that several lanes touch through narrow hunks (src/main.rs, src/check.rs, src/native*.rs, crates/vesper-syntax/**, crates/vesper-highlight/**) are listed in docs/rfcs/wave-nine-contracts.md with one owner each. Keep your hunk in those files small and in its own commit, and merge origin/main right before merging.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (run `cargo +1.100.0 clippy` before pushing if your local toolchain is older). Files a test reads from outside its crate directory must be declared in that crate's BUILD.bazel (`extra_compile_data`, `extra_test_data`); Lean-model tests need `extra_test_data = ["//:lean"]` and usually `copy_test_cwd` (see crates/vesper-chrono/BUILD.bazel). Bazel's rustfmt also checks test fixtures. Windows CI uses Cargo. Root BUILD.bazel is held by Codex (#574): if a root test target needs a timeout or data change, put it in its own commit and merge after #574's PR.
- `vesper fmt --check` runs in CI on every tracked .ves file; new .ves fixtures must be canonically formatted unless listed with a reason in scripts/fmt-exclude.txt.
- Prefer mature, widely used crates for cryptography, parsing, fonts, compression and kernel/hardware access; pin exact compatible versions; never vendor or hand-roll cryptographic primitives. Reuse versions already in Cargo.lock where possible (Cranelift 0.125, Wasmtime 50, rustls 0.23, arrow 56).
- Use an isolated target directory per checkout. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited.
- Shared core ExprKind, Type and Place-IR Op/Terminator schemas, arithmetic semantics (vesper.arith.checked/1), interface semantics v2 and native/fiber ABI v2 are frozen unless your assignment says otherwise; additions are additive, and every backend rejects IR operations it does not know with a typed error. Preserve k_i ⊑ m_i and the existing abort/loan/one-shot checks.
- Unsafe code is forbidden except where your assignment allows it: none.
- Linear obligations in host libraries are non-Clone, non-Copy, #[must_use] owners whose only exits are documented consuming calls; a dropped owner is detected (debug panic or counted leak), never silently released. Rust ownership is not evidence that the Vesper source typing rules are implemented; say so in your report.
- Malformed, ambiguous, overflowed and over-budget inputs are typed errors, never success, and never panics on untrusted bytes.
- "0 Alloc" claims require a counting global allocator test over the warmed-up steady state. Hardware and kernel features must be reported as unavailable unless actually exercised (the cloud host has no /dev/kvm, no Intel PT, no hardware PMU, no GPU; perf_event_paranoid is 2; io_uring_setup returns EPERM; it does have PKU). The owner chose software stand-ins (Xvfb, Mesa lavapipe, QEMU) where they fit; WinUI, SwiftUI, DirectX 12, Metal, TEE guests, KVM and RDMA stay deferred. A software model or QEMU validates the modelled behaviour only. Report only performance you measured, with the setup. Interop claims name the peer implementation and version.
- Lean: files must compile with the pinned Lean 4.24.0 (elan downloads are blocked in the cloud; use the GitHub release .zip), contain no `sorry`/`admit`/new axioms, and be checked from your tests when `VESPER_LEAN` is set (follow crates/vesper-simd/tests/model.rs). State exactly what is proved about which model, and that it is not a theorem about the Rust code.
- Do not start overlapping subagents, edit the Google Doc, edit website/** (the owner maintains the site), publish packages, or message third parties.
- Run `cargo fmt --all -- --check`, focused tests, `cargo test --workspace --locked` and strict `cargo clippy --workspace --locked --all-targets -- -D warnings`. Record exact commands and results.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #589. The user has authorized merging this effort's PRs once all required CI checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. If main itself is red and your PR fixes it, follow the standing rule: cancel the other in-progress Actions runs and merge the fix first. Return base/commit/PR, public API, checks, root patch, decisions and remaining gaps. Your report path below is mandatory.

Assignment: Source Tensor rest and wire structs: record `wire` in the item schema, tensors in impls and traits, named axes and region-backed views (§28.2, §13.2).

Exclusive ownership:
- src/tensor_types.rs and src/tensor_types/** (including the wire layout checks in src/tensor_types/wire.rs)
- Additive `wire` flag in the frozen item schema in crates/vesper-syntax/** (own commit)
- tests/tensor_types*.rs and tests/fixtures/tensor_types/**
- docs/parallel-results/source-tensor-rest.md
Dependencies: None new.

Implement:
1. Record `wire` on struct items so every `wire struct` gets its layout check, including ones with no wire-only fields (`{ flag: bool }`) and wrappers of nested wire structs.
2. Tensors in `impl` and trait items, closures returning tensors through generic positions, and user effects with dimension parameters.
3. Named axes (`[Batch: dim, ...]`), region-backed `StaticTensor`/`TensorView[r, ..]` views and `LinearSeq[KvPageLease]` inside `ActiveKvSeq`.
4. `vesper run` handles the `Tensor` effect with the host evaluator.

Acceptance:
- Fixtures for each rule, positive and negative, including the wire-struct cases above; a `vesper run` test over a small tensor program.
- Report: docs/parallel-results/source-tensor-rest.md, with what was built, checks, decisions and remaining gaps.
~~~
