# Checked-source WAL crash and network simulation

Suggested branch: `wave3/crash-wal-network-simulation`.

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

Assignment: Checked-source WAL crash and network simulation.

Exclusive ownership:
- crates/vesper-sim-session/**.
- Optional NEW crates/vesper-sim-models/** for reusable bounded model state.
- docs/parallel-results/crash-wal-network-simulation.md.
Existing SimPulse kernel/journal, compiler/runtime, native session and allocation crates are read-only. Preserve current record/replay APIs where possible; version changed scenario/record schemas explicitly.

Baseline: sim-session drives real checked source on the reference CEK evaluator under virtual clock/yield/sleep/script/external operations. It preserves parked owners on pre-transition errors; evaluator-internal faults/fuel exhaustion can already consume a machine and remain Terminal. This is not native execution or DPOR.

Implement:
1. Bounded deterministic virtual WAL/storage and network state driven through typed source operations. WAL has a clear volatile-write versus durable-flush boundary; recovery defines append ordering, partial/torn records and validation. Network models at least delay, partition/heal and explicit failure with bounded queued payloads.
2. Define process-crash/restart epochs separately from source cancellation. Unflushed writes disappear; flushed state survives according to the model. A crashed process's lost continuations are explicitly terminal/lost process state, not silently called normal linear consumption. No pretending a dropped Runtime is a valid source cancellation.
3. Reject completions from stale epochs/request ids without losing the submitted response owner; retain recoverable owners on script/replay/budget/pre-transition errors. Keep the existing terminal fault limitation explicit. Never clone opaque Runtime/Suspension owners to implement exploration.
4. Record every failure/partition/crash/recovery choice and model transition in a versioned model/source trace bound to exact source, scenario, model semantics and execution identity. Replaying reproduces outputs AND durable state, or rejects the first mismatch. Keep SimPulse v1 journal unchanged unless the coordinator approves a schema extension.
5. Expose snapshot/report inspection and useful source fixtures. Enforce payload, log, queue, epoch, event, virtual-time and work budgets before retaining data or changing owners. No real filesystem, socket, clock, randomness or sleeps in ordinary model tests.

Acceptance:
- A checked-source protocol that acknowledges before flush has a deterministic crash counterexample; a corrected flush-before-ack variant retains acknowledged data on restart.
- Flushed/unflushed/torn records recover per the documented policy; partitions, delayed failures and stale pre-crash completions are replayed exactly.
- Record/replay rejects changed model state/choice/epoch/durable output, not just changed final scalar output.
- Budget slicing preserves state/owners and matches uninterrupted runs. Existing source simulation tests/CLI and identity rejection still pass.
- No DPOR/exhaustive scheduler, actual crash-consistency filesystem, kernel I/O or source-exhaustion theorem claim.

~~~
