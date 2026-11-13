# Wire/Json: two-stage SIMD on-demand JSON parsing

Suggested branch: `wave6/serde-simd-json`. Claim issue: #249.

~~~text
You are implementing one substantial milestone of the Vesper design (§20.3, §20.5) in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or dedicated git worktree. Require baseline cda092f2f6427e23c566d7e63259000d3d6c699f or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Read AGENTS.md if present, docs/parallel-work.md, docs/implementation.md, docs/bazel.md, docs/rfcs/wave-six-contracts.md, docs/agent-prompts/wave-six/README.md, the earlier report(s) for your area under docs/parallel-results/, and the whole of §20.3, §20.5 in docs/specification.txt (the snapshot matches the Google Doc revision modified 2026-11-13 04:46 UTC) before editing.

Claim: this work is claimed by GitHub issue #249. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link #249 from your PR ("Closes #249").

Delivery and coordination:
- Deliver working code and meaningful acceptance tests, not only a proposal or scaffold. Finish the supported slice; reject unsupported cases explicitly with typed errors. If an item turns out to be infeasible, deliver the rest and explain why in the report.
- Edit only the exclusive paths below. Other wave-six lanes run concurrently; never edit their paths and never depend on an unmerged lane's work. Library crates must not depend on the root `vesper` package.
- Other agents (Codex) hold §42 rewrite-witness and optimizer work (issues #230-#242, PR #238: src/optimize*, the rewrite tests and their Lean files). Do not edit those paths. Do not write the eBPF linear ringbuf/socket builder API (PR #165, excluded by the owner).
- Root patch rule (user-approved 2026-11-12): you MAY commit your own minimal root patch onto YOUR PR branch so CI goes green: new `[workspace.dependencies]` entries and the regenerated Cargo.lock, and, for a new crate, registration in root Cargo.toml `members` AND `default-members` (insert before "experiments/native-pulse") plus its BUILD.bazel from `python3 tools/bazel/sync_build_files.py`. Keep it in its own commit. Root Cargo.toml and Cargo.lock conflict often: merge origin/main into your branch right before merging, regenerate the lockfile with cargo rather than hand-merging it, and wait for CI on the merged head.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy. Files a test reads from outside its crate directory must be declared in that crate's BUILD.bazel (`extra_compile_data`, `extra_test_data`); Lean-model tests need `extra_test_data = ["//:lean"]` and usually `copy_test_cwd` (see crates/vesper-chrono/BUILD.bazel). Windows CI uses Cargo.
- `vesper fmt --check` runs in CI on every tracked .ves file; new .ves fixtures must be canonically formatted unless listed with a reason in scripts/fmt-exclude.txt.
- Prefer mature, widely used crates for cryptography, parsing and kernel/hardware access; pin exact compatible versions; never vendor or hand-roll cryptographic primitives. Reuse versions already in Cargo.lock where possible (Cranelift 0.125, Wasmtime 50, rustls 0.23, arrow 56).
- Use an isolated target directory per checkout. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited.
- Shared core ExprKind, Type and Place-IR Op/Terminator schemas, arithmetic semantics (vesper.arith.checked/1), interface semantics v2 and native/fiber ABI v2 are frozen unless your assignment says otherwise. Preserve k_i ⊑ m_i and the existing abort/loan/one-shot checks.
- Unsafe code is forbidden except where your assignment allows it: `std::arch` structural-index kernels in one gated module (x86_64 AVX2/SSE4.2 and AArch64 NEON) with a scalar reference.
- Linear obligations in host libraries are non-Clone, non-Copy, #[must_use] owners whose only exits are documented consuming calls; a dropped owner is detected (debug panic or counted leak), never silently released. Rust ownership is not evidence that the Vesper source typing rules are implemented; say so in your report.
- Malformed, ambiguous, overflowed and over-budget inputs are typed errors, never success, and never panics on untrusted bytes.
- "0 Alloc" claims require a counting global allocator test over the warmed-up steady state. Hardware and kernel features must be reported as unavailable unless actually exercised (the cloud host has no /dev/kvm, no PKU, no Intel PT and no hardware PMU; perf_event_paranoid is 2). A software model validates the modelled protocol only. Report only performance you measured, with the setup.
- Lean: files must compile with the pinned Lean 4.24.0 (elan downloads are blocked in the cloud; use the GitHub release tarball), contain no `sorry`/`admit`/new axioms, and be checked from your crate's tests when `VESPER_LEAN` is set (follow crates/vesper-simd/tests/model.rs). State exactly what is proved about which model, and that it is not a theorem about the Rust code.
- Do not start overlapping subagents, edit the Google Doc, publish packages, or message third parties.
- Run `cargo fmt --all -- --check`, focused tests, `cargo test --workspace --locked` and strict `cargo clippy --workspace --locked --all-targets -- -D warnings`. Record exact commands and results.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #249. The user has authorized merging this effort's PRs once all required CI checks (`check`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. Return base/commit/PR, public API, checks, root patch, decisions and remaining gaps. Your report path below is mandatory.

Assignment: Wire/Json: two-stage SIMD on-demand JSON parsing (§20.3, §20.5).

Exclusive ownership:
- crates/vesper-serde/**
- docs/parallel-results/serde-simd-json.md
May additionally depend read-only on `vesper-simd`.

Implement:
1. Stage 1: SIMD structural indexing (quote/backslash masks, string spans via carry-less prefix XOR, structural character positions, UTF-8 validation) over 64-byte blocks, bit-identical to the scalar reference.
2. Stage 2: an on-demand cursor over the structural index that decodes only the fields the caller asks for, borrowing strings from the input where no escapes occur, with depth and size limits.
3. §20.5 NDJSON ingestion into AoSoA columns using the existing `event_columns`/`ndjson` modules, now fed by the SIMD index.
4. Runtime ISA dispatch with the scalar path always available; the `Isa[P]` source effect stays a later compiler step.

Acceptance:
- Differential tests: SIMD index equals scalar index on random and adversarial inputs (unbalanced quotes, long backslash runs, invalid UTF-8 at block edges), and parsed values equal `serde_json` on a corpus including the JSONTestSuite cases if fetchable.
- x86_64 and AArch64 CI both run the SIMD path; report measured throughput only with its setup.
- Report: docs/parallel-results/serde-simd-json.md, with what was built, checks, decisions and remaining gaps.
~~~
