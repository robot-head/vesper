# Bridge single out-pointers and MPK/POE isolation domains

Suggested branch: `wave6/bridge-pointers-mpk`. Claim issue: #243.

~~~text
You are implementing one substantial milestone of the Vesper design (§13.2, §13.4, §29.3) in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or dedicated git worktree. Require baseline cda092f2f6427e23c566d7e63259000d3d6c699f or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Read AGENTS.md if present, docs/parallel-work.md, docs/implementation.md, docs/bazel.md, docs/rfcs/wave-six-contracts.md, docs/agent-prompts/wave-six/README.md, the earlier report(s) for your area under docs/parallel-results/, and the whole of §13.2, §13.4, §29.3 in docs/specification.txt (the snapshot matches the Google Doc revision modified 2026-11-13 04:46 UTC) before editing.

Claim: this work is claimed by GitHub issue #243. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link #243 from your PR ("Closes #243").

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
- Unsafe code is forbidden except where your assignment allows it: One gated, commented module in vesper-ffi for `pkey_alloc`/`pkey_mprotect`/WRPKRU (x86_64 Linux) and POR_EL0 (AArch64 Linux), compiled only on those targets.
- Linear obligations in host libraries are non-Clone, non-Copy, #[must_use] owners whose only exits are documented consuming calls; a dropped owner is detected (debug panic or counted leak), never silently released. Rust ownership is not evidence that the Vesper source typing rules are implemented; say so in your report.
- Malformed, ambiguous, overflowed and over-budget inputs are typed errors, never success, and never panics on untrusted bytes.
- "0 Alloc" claims require a counting global allocator test over the warmed-up steady state. Hardware and kernel features must be reported as unavailable unless actually exercised (the cloud host has no /dev/kvm, no PKU, no Intel PT and no hardware PMU; perf_event_paranoid is 2). A software model validates the modelled protocol only. Report only performance you measured, with the setup.
- Lean: files must compile with the pinned Lean 4.24.0 (elan downloads are blocked in the cloud; use the GitHub release tarball), contain no `sorry`/`admit`/new axioms, and be checked from your crate's tests when `VESPER_LEAN` is set (follow crates/vesper-simd/tests/model.rs). State exactly what is proved about which model, and that it is not a theorem about the Rust code.
- Do not start overlapping subagents, edit the Google Doc, publish packages, or message third parties.
- Run `cargo fmt --all -- --check`, focused tests, `cargo test --workspace --locked` and strict `cargo clippy --workspace --locked --all-targets -- -D warnings`. Record exact commands and results.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #243. The user has authorized merging this effort's PRs once all required CI checks (`check`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. Return base/commit/PR, public API, checks, root patch, decisions and remaining gaps. Your report path below is mandatory.

Assignment: Bridge single out-pointers and MPK/POE isolation domains (§13.2, §13.4, §29.3).

Exclusive ownership:
- crates/vesper-ffi/**
- crates/vesper-migrate/** (additive API changes only; integration B wires its CLI)
- docs/parallel-results/bridge-pointers-mpk.md
Existing dependencies only, plus mature crates for pkey syscalls if needed (prefer `libc`).

Implement:
1. Extend the `.vsys` parser in `vesper_ffi::sys` so a pointer parameter may name a single scalar or wire struct (`nocapture inout out_crc: u32`, `nocapture out hdr: Header`, `nocapture in key: u64`), not only `CSlice[T]`. Give each form a typed layout (size, alignment, direction) and keep the old forms byte-identical. Reject `nocapture` omitted, nullable single pointers without an explicit `Option`, and pointer-to-pointer with typed errors.
2. Update `vesper-migrate` so the C and Rust lifters emit the new single-pointer form instead of `RefusalReason::PointerParameter` when the parameter is a non-aliased scalar or struct out/inout pointer; keep refusing arrays without lengths, aliasing pairs and `void*`. Regenerate golden outputs (`VESPER_MIGRATE_BLESS=1`).
3. §13.4 isolation domains: an `IsolationDomain` API that allocates a protection key, tags a page-aligned region, and runs a closure with the region's write (or read) access revoked, restoring PKRU on every exit path including panics. On AArch64 Linux use POR_EL0 when the kernel exposes it. Report `Unavailable` with the reason when the CPU or kernel lacks PKU/POE (the cloud host has no `pku` flag); never pretend isolation happened.
4. Wire the domain into `vesper-migrate`'s differential shadow harness as an optional mode: when available, the legacy side runs with Vesper-owned buffers write-protected and a violation is reported as a typed shadow failure; when unavailable the report says so (today's behaviour).

Acceptance:
- Parser tests for every new form, including the design's `crc32_update` example, plus rejection tests.
- Migrate golden tests that previously refused single pointers now lift them; refusal tests for the cases still refused.
- Isolation tests that run on hosts with PKU (CI runners may have it) and assert a SIGSEGV-class fault is caught in a child process when the protected region is written; on hosts without it the test asserts `Unavailable`. Record which CI jobs actually exercised PKU.
- Report: docs/parallel-results/bridge-pointers-mpk.md, with what was built, checks, decisions and remaining gaps.
~~~
