# Chrono hardware trace decoding and native activation checkpoints

Suggested branch: `wave6/chrono-hardware-checkpoints`. Claim issue: #244.

~~~text
You are implementing one substantial milestone of the Vesper design (§31) in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or dedicated git worktree. Require baseline cda092f2f6427e23c566d7e63259000d3d6c699f or a descendant; record the actual SHA. Development happens on `main`; open your PR against `main`. Read AGENTS.md if present, docs/parallel-work.md, docs/implementation.md, docs/bazel.md, docs/rfcs/wave-six-contracts.md, docs/agent-prompts/wave-six/README.md, the earlier report(s) for your area under docs/parallel-results/, and the whole of §31 in docs/specification.txt (the snapshot matches the Google Doc revision modified 2026-11-13 04:46 UTC) before editing.

Claim: this work is claimed by GitHub issue #244. Before you start, check the open issues and PRs in robot-head/vesper for overlapping work by other agents; if something overlaps, stop and report instead of duplicating it. Link #244 from your PR ("Closes #244").

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
- Unsafe code is forbidden except where your assignment allows it: One gated module for `perf_event_open` capture of Intel PT / Arm CoreSight ETE (Linux only).
- Linear obligations in host libraries are non-Clone, non-Copy, #[must_use] owners whose only exits are documented consuming calls; a dropped owner is detected (debug panic or counted leak), never silently released. Rust ownership is not evidence that the Vesper source typing rules are implemented; say so in your report.
- Malformed, ambiguous, overflowed and over-budget inputs are typed errors, never success, and never panics on untrusted bytes.
- "0 Alloc" claims require a counting global allocator test over the warmed-up steady state. Hardware and kernel features must be reported as unavailable unless actually exercised (the cloud host has no /dev/kvm, no PKU, no Intel PT and no hardware PMU; perf_event_paranoid is 2). A software model validates the modelled protocol only. Report only performance you measured, with the setup.
- Lean: files must compile with the pinned Lean 4.24.0 (elan downloads are blocked in the cloud; use the GitHub release tarball), contain no `sorry`/`admit`/new axioms, and be checked from your crate's tests when `VESPER_LEAN` is set (follow crates/vesper-simd/tests/model.rs). State exactly what is proved about which model, and that it is not a theorem about the Rust code.
- Do not start overlapping subagents, edit the Google Doc, publish packages, or message third parties.
- Run `cargo fmt --all -- --check`, focused tests, `cargo test --workspace --locked` and strict `cargo clippy --workspace --locked --all-targets -- -D warnings`. Record exact commands and results.
- Commit and push YOUR branch only. Open a ready-for-review PR against main that closes #244. The user has authorized merging this effort's PRs once all required CI checks (`check`, `macOS (AArch64)`, `Windows (x86_64 MSVC)`) are green and review comments are addressed. Return base/commit/PR, public API, checks, root patch, decisions and remaining gaps. Your report path below is mandatory.

Assignment: Chrono hardware trace decoding and native activation checkpoints (§31).

Exclusive ownership:
- crates/vesper-chrono/**
- lean/ChronoReplay.lean and lean/ChronoReplay/**
- docs/parallel-results/chrono-hardware-checkpoints.md
May additionally depend read-only on `vesper-fiber-abi` and `vesper-pulse`. Do not edit them; if they lack an API you need, describe it in your report for a later step.

Implement:
1. An Intel PT packet decoder (PSB, PSBEND, TNT short/long, TIP/TIP.PGE/TIP.PGD/FUP with IP compression, PIP, MODE, CBR, TSC/MTC/CYC, OVF, PAD) producing a typed packet stream, and a flow reconstructor that walks a provided code image (decode it with a mature disassembler crate such as `iced-x86`) to turn TNT/TIP into a branch trace. Malformed or truncated packets are typed errors.
2. An Arm CoreSight ETE/ETMv4 trace-element decoder for the instruction trace subset (address, atom, exception, context, timestamp elements), with the same typed-error rules.
3. Map reconstructed branches onto Chrono's effect-boundary timeline so a reverse watchpoint can name the instruction-level path between two effect records.
4. Gated capture: open an Intel PT (or `arm_spe`/CoreSight) perf event for a child process when the PMU exists in `/sys/bus/event_source/devices`; otherwise return `Unavailable` with the reason (the cloud host has no `intel_pt` PMU). Commit small recorded traces only if they were captured on real hardware; otherwise test against traces you encode with an independent encoder in tests and say so.
5. Native activation checkpoints: capture and restore a parked Pulse/fiber activation (stack bytes between the saved stack pointer and the stack top, the saved register frame, and the fiber's metadata) through the public `vesper-fiber-abi`/`vesper-pulse` APIs, restore it at the same address into a fresh stack, and resume it to the same result. Reject relocation (different base address) with a typed error; saved raw stack pointers do not establish relocation safety. Integrate checkpoints as Chrono snapshot epochs.
6. Extend lean/ChronoReplay.lean with a statement about the checkpoint model (restoring a checkpoint at epoch e and replaying the suffix equals the live run), proved about the model only.

Acceptance:
- Decoder tests against hand-encoded packet streams covering every packet kind, IP compression modes and error cases; a flow-reconstruction test over a small x86_64 code image.
- Checkpoint round-trip tests on x86_64 Linux and macOS AArch64 (CI), including resume-to-same-result and a rejected relocation.
- Hardware capture reported unavailable on hosts without the PMU, with the check recorded.
- Report: docs/parallel-results/chrono-hardware-checkpoints.md, with what was built, checks, decisions and remaining gaps.
~~~
