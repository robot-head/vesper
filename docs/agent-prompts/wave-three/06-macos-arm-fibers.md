# macOS AArch64 pinned-fiber ABI port

Suggested branch: `wave3/macos-arm-fibers`.

~~~text
You are implementing one substantial milestone in https://github.com/robot-head/vesper.

Start from a fresh origin/main in a NEW cloud checkout or dedicated git worktree. Require baseline 02b3a1d175efb5d2561b7fa5b0aaf80121ae1bc6 or a descendant; record the actual SHA. Read AGENTS.md if present, docs/specification.txt, docs/implementation.md, docs/parallel-results/wave-two-integration.md, docs/rfcs/parallel-contracts.md and docs/rfcs/wave-three-contracts.md before editing.

Delivery and coordination:
- Deliver working code and meaningful acceptance tests, not only a proposal or scaffold. Finish the supported slice; explicitly reject unsupported cases.
- Edit only the exclusive paths below. Root Cargo.toml/Cargo.lock, src/main.rs/src/lib.rs, CI and global docs belong to the coordinator. Supply a precise registration/dependency patch separately. Temporary UNCOMMITTED root dependency/module edits for local testing are allowed; do not include them in your commits.
- Existing crates stay registered. Only a NEW unregistered crate may temporarily declare its own [workspace]/Cargo.lock; integration removes those. Preserve the separate sandbox guest workspace and current validated dependency versions.
- Use an isolated target directory per checkout/worktree. Do NOT share CARGO_TARGET_DIR between source-divergent compiler worktrees. Use CARGO_BUILD_JOBS=2, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0 and CARGO_INCREMENTAL=0 where space is limited.
- Integer arithmetic is vesper.arith.checked/1; possible +, -, *, negation and MIN/-1 overflow uses typed Fail[ArithOverflow]. Explicit wrapping_* calls are total. RuntimeError remains separate. Interface semantics version is 2. Preserve k_i ⊑ m_i and the existing abort/loan/one-shot checks.
- Shared core ExprKind, Type and Place-IR Op/Terminator schemas, arithmetic semantics and native ABI v2 are frozen for this wave. Lower new surface constructs to existing core nodes before backend consumption. Required schema changes go to the coordinator; do not silently add variants or patch another lane.
- src/lean.rs, src/lean_ir.rs, src/lean_nodes.rs and crates/vesper-lean-syntax/** are coordinator-owned. Lean emission now builds a full AST and renders once; never reintroduce raw Lean fragments. crates/vesper-loom-tuf/** is also coordinator-owned.
- Preserve owners on failed consuming operations. Unsupported, ambiguous, invalid evidence, overflow and exhausted budgets are errors, never success. Do not weaken safety checks to fit an example.
- Content hashes are not provenance, public residual effect rows are not authority audits, replay is not DPOR, and executable Lean is not a preservation/termination theorem. Report actual hardware/kernel/toolchain evidence and unavailable environments accurately.
- Read other lanes' APIs without editing them. Keep new tests/examples/report files in your boundary. Do not start overlapping subagents, edit the Google Doc, publish packages, or message third parties.
- Run formatting, focused tests and strict all-target Clippy. For changed execution paths, use real execution; compilation/object generation alone is a narrower result. Record exact commands/results and limitations.
- Commit and push YOUR branch only; do not merge main. Return base/commit/PR, public API, checks, coordinator patch, decisions and remaining gaps. Your report path below is mandatory.

Assignment: macOS AArch64 pinned-fiber ABI port.

Exclusive ownership:
- crates/vesper-fiber-abi/**.
- NEW experiments/native-pulse-macos/** if a standalone direct-fiber probe helps.
- docs/parallel-results/macos-arm-fibers.md and platform appendix updates to docs/rfcs/native-pulse-abi.md ONLY.
No src/native.rs/native_session, Pulse scheduler/driver, compiler, Linux experiment or CI edits. Lane 2 consumes the current fiber API read-only: preserve Linux public signatures and ABI_VERSION=2 layouts/offsets.

Baseline: direct pinned fibers/switch/FP/stack modules are Linux x86_64/AArch64 gated. Evidence/data/prototype object APIs are portable. The scheduler adapter depends on Linux Pulse; widening the direct-fiber gate does NOT make io_uring or that scheduler portable.

Implement:
1. Real Darwin AArch64 pinned stack allocation/guard pages, context switch entry/return and symbol/assembler conventions. Preserve required callee-saved GPR/vector state, SP alignment, Darwin reserved registers and native C ABI. Use cfg-separated platform details instead of weakening Linux behavior.
2. Preserve each fiber's FP control/status, default initial environment and host restoration, with the same explicit ABI v2 contract. Keep panic/unwind across switch boundaries forbidden and fail explicitly on unsupported environments.
3. Enable direct Fiber/ContinuationTable/one-shot operations on macOS where implemented; keep Linux-only Pulse/driver modules gated. Cancellation and manual re-park should preserve owner/code lifetime. No nested fibers, work stealing or physical-stack relocation claim.
4. New platform tests/probe cover actual park/resume, repeated/stale handles, cancellation/re-park, code lifetime, guard allocation and FP/register isolation. Linux tests and public ABI checks must continue passing.
5. Prefer real Apple-silicon execution or existing macOS CI evidence. If only Linux is available, finish code plus cross-compilation/assembly checks and clearly mark runtime validation pending; never present Mach-O generation/QEMU-Linux as Darwin execution. Request the coordinator's CI wiring if needed; do not edit workflows yourself.

Acceptance:
- Apple-silicon run (when available) executes a real Cranelift/direct-fiber round trip, cancellation re-park and independent host/fiber FP modes/status.
- Correct Mach-O/target/calling convention and ABI offsets are verified; !Send/locality boundaries remain.
- No kqueue/IOCP, Windows fibers, source suspension or platform latency claim. Lane 2 remains independent Linux source-native work against the frozen API.

~~~
