# Pulse: portable completion drivers (IOCP, kqueue, epoll) behind one scheduler

Suggested branch: `wave7/pulse-portable-drivers`. Claim issue: #325.

~~~text
You are implementing one substantial milestone of the Vesper design (§12) in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or dedicated git worktree. Require baseline c025b9abb3f90c518b9634143c8bc712b9ea9c30 or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Read AGENTS.md if present, docs/parallel-work.md, docs/implementation.md, docs/bazel.md, docs/rfcs/wave-seven-contracts.md, docs/agent-prompts/wave-seven/README.md, docs/parallel-results/windows-fibers.md, docs/parallel-results/native-pulse-abi.md, and the whole of §12 in docs/specification.txt (the snapshot matches the Google Doc revision modified 2026-11-13 04:46 UTC) before editing.

Claim: this work is claimed by GitHub issue #325. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link #325 from your PR ("Closes #325").

Delivery and coordination:
- Deliver working code and meaningful acceptance tests, not only a proposal or scaffold. Finish the supported slice; reject unsupported cases explicitly with typed errors. If an item turns out to be infeasible, deliver the rest and explain why in the report.
- Edit only the exclusive paths below. Other wave-seven lanes run concurrently; never edit their paths and never depend on an unmerged lane's work. Library crates must not depend on the root `vesper` package.
- Other agents (Codex) hold §16.4 attribute/item macros and §42 (including the resource model, issue #322 and PR #323: lean/ResourceModel42*, docs/rfcs/resource-model42-*, src/optimize*, the rewrite tests and their Lean files). Do not edit those paths and do not add attribute or item macro syntax. Do not write the eBPF linear ringbuf/socket builder API (PR #165, excluded by the owner).
- Root patch rule (user-approved 2026-11-12): you MAY commit your own minimal root patch onto YOUR PR branch so CI goes green: new `[workspace.dependencies]` entries and the regenerated Cargo.lock, and, for a new crate, registration in root Cargo.toml `members` AND `default-members` (insert before "experiments/native-pulse") plus its BUILD.bazel from `python3 tools/bazel/sync_build_files.py`. Keep it in its own commit. Root Cargo.toml and Cargo.lock conflict often: merge origin/main into your branch right before merging, regenerate the lockfile with cargo rather than hand-merging it, and wait for CI on the merged head. Merges that touch the root manifests are serialized: if another lane's PR touching them is about to merge, let it land first.
- Shared files that several lanes touch through narrow hunks (src/native.rs, src/check.rs, src/ir.rs, crates/vesper-syntax/**, src/main.rs, the build-command routing) are listed in docs/rfcs/wave-seven-contracts.md with one owner each. Keep your hunk in those files small and in its own commit, and merge origin/main right before merging.
- Bazel: Linux CI runs `bazel test //...` on BuildBuddy with Rust 1.100 (run `cargo +1.100.0 clippy` before pushing if your local toolchain is older). Files a test reads from outside its crate directory must be declared in that crate's BUILD.bazel (`extra_compile_data`, `extra_test_data`); Lean-model tests need `extra_test_data = ["//:lean"]` and usually `copy_test_cwd` (see crates/vesper-chrono/BUILD.bazel). Bazel's rustfmt also checks test fixtures. Windows CI uses Cargo.
- `vesper fmt --check` runs in CI on every tracked .ves file; new .ves fixtures must be canonically formatted unless listed with a reason in scripts/fmt-exclude.txt.
- Prefer mature, widely used crates for cryptography, parsing and kernel/hardware access; pin exact compatible versions; never vendor or hand-roll cryptographic primitives. Reuse versions already in Cargo.lock where possible (Cranelift 0.125, Wasmtime 50, rustls 0.23, arrow 56).
- Use an isolated target directory per checkout. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited.
- Shared core ExprKind, Type and Place-IR Op/Terminator schemas, arithmetic semantics (vesper.arith.checked/1), interface semantics v2 and native/fiber ABI v2 are frozen unless your assignment says otherwise (lane 28 adds vector operations additively; backends must reject operations they do not know with a typed error). Preserve k_i ⊑ m_i and the existing abort/loan/one-shot checks.
- Unsafe code is forbidden except where your assignment allows it: The existing io_uring module; new gated modules for epoll, kqueue and IOCP system calls.
- Linear obligations in host libraries are non-Clone, non-Copy, #[must_use] owners whose only exits are documented consuming calls; a dropped owner is detected (debug panic or counted leak), never silently released. Rust ownership is not evidence that the Vesper source typing rules are implemented; say so in your report.
- Malformed, ambiguous, overflowed and over-budget inputs are typed errors, never success, and never panics on untrusted bytes.
- "0 Alloc" claims require a counting global allocator test over the warmed-up steady state. Hardware and kernel features must be reported as unavailable unless actually exercised (the cloud host has no /dev/kvm, no Intel PT, no hardware PMU, no GPU; perf_event_paranoid is 2; io_uring_setup returns EPERM; it does have PKU). A software model validates the modelled protocol only. Report only performance you measured, with the setup. Interop claims name the peer implementation and version.
- Lean: files must compile with the pinned Lean 4.24.0 (elan downloads are blocked in the cloud; use the GitHub release .zip), contain no `sorry`/`admit`/new axioms, and be checked from your crate's tests when `VESPER_LEAN` is set (follow crates/vesper-simd/tests/model.rs). State exactly what is proved about which model, and that it is not a theorem about the Rust code.
- Do not start overlapping subagents, edit the Google Doc, publish packages, or message third parties.
- Run `cargo fmt --all -- --check`, focused tests, `cargo test --workspace --locked` and strict `cargo clippy --workspace --locked --all-targets -- -D warnings`. Record exact commands and results.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #325. The user has authorized merging this effort's PRs once all required CI checks (`Test all targets`, `Native and acceptance scripts (BuildBuddy)`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. Return base/commit/PR, public API, checks, root patch, decisions and remaining gaps. Your report path below is mandatory.

Assignment: Pulse: portable completion drivers (IOCP, kqueue, epoll) behind one scheduler (§12).

Exclusive ownership:
- crates/vesper-pulse/**
- docs/parallel-results/pulse-portable-drivers.md
Dependencies: Prefer `libc`/`windows-sys` already in Cargo.lock; `mio` is acceptable if pinned exactly.

Implement:
1. Refactor the scheduler, `lifecycle.rs` and `fp_env.rs` so they no longer depend on the io_uring `Request`/`Completion` types: introduce a backend-neutral completion-driver trait and keep io_uring as one implementation, unchanged in behaviour.
2. A Linux epoll (readiness) driver used when `io_uring_setup` is denied (the cloud host returns EPERM), so the real scheduler runs end to end here and on Bazel CI.
3. A Windows IOCP driver and a macOS kqueue driver, so the scheduler, its lifecycle tests and fp-env tests run on the Windows and macOS CI jobs.
4. Driver selection at runtime with a typed report of which driver ran; the deterministic SimPulse path is unaffected.

Acceptance:
- The existing scheduler tests run on Linux (epoll and, where permitted, io_uring), macOS (kqueue) and Windows (IOCP) in CI; record which job ran which driver.
- Fault tests: a driver error surfaces as a typed error, cancellation and shutdown drain every in-flight request.
- Report: docs/parallel-results/pulse-portable-drivers.md, with what was built, checks, decisions and remaining gaps.
~~~
