# Native continuation shutdown and machine-state ownership

Suggested branch: next/native-lifecycle.

~~~text
You are implementing one bounded milestone in https://github.com/robot-head/vesper.

Start from a fresh fetch of origin/main in a NEW cloud checkout or dedicated git worktree. Minimum baseline: 0e6caf9 (all first-round handoffs integrated). Record the actual base SHA. Do not share a checkout or branch with another agent. Read docs/specification.txt, docs/implementation.md, docs/parallel-work.md, docs/parallel-results/handoff-integration.md, and docs/rfcs/parallel-contracts.md before editing.

Delivery rules:
- Implement working behavior, with meaningful positive and negative tests. Deliver code, not only an RFC, scaffolding or an adapter proposal.
- Edit only your owned paths. Root Cargo.toml/Cargo.lock, src/main.rs, src/lib.rs, CI and global documentation belong to the coordinator unless explicitly assigned below. Read other lanes' code, but do not change it. Report any necessary cross-boundary patch separately.
- Existing workspace members stay registered: do not restore nested workspaces or lockfiles. A NEW crate may temporarily use its own [workspace] and Cargo.lock for independent testing; the coordinator will register it and regenerate the root lockfile. Match current validated dependency versions. Do not edit the existing sandbox guest workspace/lockfile.
- Do not downgrade dependencies to match illustrative design snippets. Keep public module paths and existing supported behavior, except the explicitly assigned arithmetic migration.
- Keep ownership and failures explicit. Unsupported, ambiguous, overflow, invalid evidence and exhausted budgets must never become success. Preserve recoverable owners on error where the API takes ownership.
- A byte hash is not provenance; a residual effect row is not an authority audit; executable Lean extraction is not a preservation/termination proof; seeded replay is not DPOR. Do not claim kernel/hardware validation from mocks or object emission.
- Preserve the user-approved field restriction k_i ⊑ m_i. Do not modify the Google Doc. Record decisions and remaining gaps in your assigned report.
- Run formatting, relevant tests and strict all-target Clippy. Use real Lean when extraction changes; report unavailable toolchains, kernel permissions or platforms explicitly. For source fixtures outside the arithmetic lane, avoid depending on implicit wrapping arithmetic: that lane is migrating +, - and * to checked effects.
- Commit and push YOUR branch only. Do not merge into main, publish packages or message third parties. Return the base SHA, commit SHA/PR, public API, exact checks/results, integration steps and remaining limitations. If blocked at an ownership boundary, finish independent work and provide a precise coordinator patch/request.
- Other agents are running concurrently. Do not broaden your scope or start overlapping subagents.

Assignment: make the native fiber prototype's lifecycle suitable for later source lowering. This is real runtime work, not the source backend integration.

Exclusive ownership:
- crates/vesper-fiber-abi/**
- experiments/native-pulse/**
- crates/vesper-pulse/src/scheduler.rs only if necessary for an ownership-preserving adapter change; keep its existing public interface compatible
- docs/rfcs/native-pulse-abi.md and docs/parallel-results/native-lifecycle.md
Do not edit src/native.rs, source IR/checker/backends, io_driver.rs, data-frame storage, root manifests or the source-simulation lane.

Read the ABI report and fiber.rs/pulse.rs/switch.rs. The current implementation actually switches pinned Linux stacks and schedules hand-written CLIF entries. It does not lower source programs. Dropping a parked continuation currently cancels once; if native code parks again during cancellation, the fiber is abandoned and counted. Native machine stacks remain pinned. The FP environment is not saved.

Implement:
1. An explicit owned cancellation/shutdown/drain API. If cancellation resumes a fiber and it parks again, return or retain the live continuation in a recoverable owner; do not turn this into successful completion or a string error that drops the owner. Bounded draining must return pending owners on budget exhaustion.
2. Define and implement scheduler-adapter behavior for cancellation before start, after yield, and during I/O. A cancellation-command acknowledgement never releases the original request, file, buffer or continuation. Preserve original-I/O-completion ownership. Fixture drivers must exercise the distinction; real kernel tests must report skips.
3. Audit Drop and rejected-resume paths. Specify what can safely happen when an embedding Rust caller drops an unresolved owner. Do not promise source linear exhaustion from Rust RAII, leak counters or mem::forget behavior. Safe APIs must not free executable code, stack or data while they can still be used.
4. Preserve the target ABI's required floating-point control/environment state across stack switches on Linux x86_64 and AArch64. Review saved general/vector registers, stack alignment and unwind restrictions. Version any changed repr(C) layout and update CLIF offsets/object fixtures together.
5. Keep machine stacks pinned and fibers local to their originating thread. Preserve stable request IDs and separate I/O buffers. No physical relocation, Send/work-stealing claim, or unmeasured switch-latency claim.
6. Update the executable experiment to demonstrate explicit clean shutdown and recoverable incomplete shutdown, rather than only the happy path.

Acceptance:
- A fixture entry that re-parks after cancellation; a drain-budget exit that returns an owner and later completes; repeated/stale resume rejection; code/evidence lifetime across shutdown; submission and completion errors.
- Original I/O completion delayed past cancel acknowledgement; owners remain retained until the real completion.
- Floating-point control-state isolation across two fibers and the scheduler on a host that can execute the target. Cross-target ABI/object layout tests cover AArch64; actual AArch64 execution only if available.
- Existing runtime/object/compile-fail tests, strict Clippy and formatting. Report kernel EPERM/ENOSYS and target limits.

Publish the revised ABI and a small source-lowering checklist in your report. Do not implement source lowering in this branch: that follows the checked-arithmetic lane and this lifecycle milestone.
~~~

